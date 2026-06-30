//! The ripgrep process wrapper: build argv, spawn `rg`, parse its NUL-separated
//! output, and batch large candidate lists (spec §8.2, §8.3).
//!
//! Only three `rg` modes are ever used (PLAN.md cheat-sheet), all with `--null`:
//! list files **with** a match (`-l`, seed + positive narrowing), list files
//! **without** a match (`--files-without-match`, negative literals in one call),
//! and list files (`--files`, the universe for positive-free clauses).
//!
//! Two correctness rules learned from the ripgrep spike (PLAN.md §0) are enforced
//! here:
//! 1. never spawn `rg` with zero path arguments while narrowing — with no paths it
//!    scans the whole cwd, so an empty candidate list short-circuits to ∅;
//! 2. the result set comes from parsed stdout, not the exit code — `-l` exits 1 on
//!    no match but `--files-without-match` exits 0 even when it lists nothing, so
//!    we treat exit 0/1 as "ran fine" and only ≥2 as a real error.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::process::Command;

use crate::cli::{MatchFlags, ScopeFlags};

/// Default per-invocation budget (in bytes) for the path portion of argv, kept
/// well under a typical `ARG_MAX` once env and the fixed argv are accounted for.
/// Overridable via `RGQ_ARG_MAX` (used by tests to force the batching path).
const DEFAULT_ARG_BUDGET: usize = 128 * 1024;

/// An error from running `rg`.
#[derive(Debug, thiserror::Error)]
pub enum RgError {
    #[error("could not run ripgrep '{bin}': {source} — is rg installed and on PATH? (override the binary with RGQ_RG)")]
    Spawn {
        bin: String,
        #[source]
        source: io::Error,
    },
    #[error("ripgrep failed{}: {stderr}", .code.map(|c| format!(" (exit {c})")).unwrap_or_default())]
    Failed { code: Option<i32>, stderr: String },
}

/// A configured ripgrep front end: the resolved binary plus the precomputed flag
/// fragments that every invocation reuses.
pub struct Rg {
    bin: OsString,
    match_args: Vec<OsString>,
    scope_args: Vec<OsString>,
    arg_budget: usize,
}

impl Rg {
    /// Build from the classified flags. The scope flags define the universe and
    /// are applied to every invocation; the match flags apply to pattern-bearing
    /// invocations (spec §7).
    pub fn new(match_flags: &MatchFlags, scope_flags: &ScopeFlags) -> Rg {
        Rg {
            bin: std::env::var_os("RGQ_RG").unwrap_or_else(|| OsString::from("rg")),
            match_args: match_args(match_flags),
            scope_args: scope_args(scope_flags),
            arg_budget: std::env::var("RGQ_ARG_MAX")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|&n| n > 0)
                .unwrap_or(DEFAULT_ARG_BUDGET),
        }
    }

    /// List every file in scope — the universe `U` (spec §7). Used to seed
    /// positive-free clauses.
    pub fn list_files(&self) -> Result<Vec<Vec<u8>>, RgError> {
        let mut base = vec![OsString::from("--files"), OsString::from("--null")];
        base.extend(self.scope_args.iter().cloned());
        self.spawn(&base) // no path args: search the cwd
    }

    /// Files matching `pattern`. With `restrict = None` this seeds over the whole
    /// scope (cwd); with `Some(paths)` it keeps only those candidate paths that
    /// match (intersection), batching to stay under `ARG_MAX`.
    pub fn list_matching(
        &self,
        pattern: &[u8],
        restrict: Option<&[Vec<u8>]>,
    ) -> Result<Vec<Vec<u8>>, RgError> {
        let base = self.pattern_base("-l", pattern);
        match restrict {
            None => self.spawn(&base),
            Some(paths) => self.run_batched(&base, paths),
        }
    }

    /// Among `paths`, the files that do **not** match `pattern` (set difference),
    /// in a single ripgrep mode, batched.
    pub fn list_not_matching(
        &self,
        pattern: &[u8],
        paths: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, RgError> {
        let base = self.pattern_base("--files-without-match", pattern);
        self.run_batched(&base, paths)
    }

    /// Base argv for a pattern-bearing mode: `<mode> --null <scope> <match> -e PAT`.
    /// `-e` guards a leading-dash pattern from being read as a flag (spec §8.3).
    fn pattern_base(&self, mode: &str, pattern: &[u8]) -> Vec<OsString> {
        let mut base = vec![OsString::from(mode), OsString::from("--null")];
        base.extend(self.scope_args.iter().cloned());
        base.extend(self.match_args.iter().cloned());
        base.push(OsString::from("-e"));
        base.push(os_from_bytes(pattern));
        base
    }

    /// Run `base` once per batch of `paths`, unioning the results. Empty `paths`
    /// short-circuits to ∅ without spawning `rg` (rule 1 above).
    fn run_batched(&self, base: &[OsString], paths: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, RgError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut seen: BTreeSet<Vec<u8>> = BTreeSet::new();
        for batch in batches(paths, self.arg_budget) {
            seen.extend(self.spawn_with_paths(base, batch)?);
        }
        Ok(seen.into_iter().collect())
    }

    /// Spawn with explicit candidate paths after a `--` end-of-options marker.
    fn spawn_with_paths(
        &self,
        base: &[OsString],
        paths: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, RgError> {
        let mut args = base.to_vec();
        args.push(OsString::from("--"));
        for p in paths {
            args.push(os_from_bytes(p));
        }
        self.spawn(&args)
    }

    /// Spawn `rg` with `args`, returning parsed paths or an error.
    fn spawn(&self, args: &[OsString]) -> Result<Vec<Vec<u8>>, RgError> {
        let output = Command::new(&self.bin)
            .args(args)
            .output()
            .map_err(|source| RgError::Spawn {
                bin: self.bin.to_string_lossy().into_owned(),
                source,
            })?;
        match output.status.code() {
            // 0 = matches, 1 = no matches; both ran fine. Use stdout, not the code.
            Some(0) | Some(1) => Ok(parse_nul(&output.stdout)),
            other => Err(RgError::Failed {
                code: other,
                stderr: String::from_utf8_lossy(&output.stderr)
                    .trim_end()
                    .to_string(),
            }),
        }
    }
}

/// Map match flags to ripgrep flags (apply to every pattern-bearing call, §7).
fn match_args(m: &MatchFlags) -> Vec<OsString> {
    let mut v = Vec::new();
    if m.ignore_case {
        v.push(OsString::from("--ignore-case"));
    }
    if m.whole_word {
        v.push(OsString::from("--word-regexp"));
    }
    if m.fixed_strings {
        v.push(OsString::from("--fixed-strings"));
    }
    if m.case_sensitive {
        v.push(OsString::from("--case-sensitive"));
    }
    v
}

/// Map scope flags to ripgrep flags (define the universe; apply everywhere, §7).
/// `-uu`'s implied `--hidden` is already folded into `hidden` by CLI classification.
fn scope_args(s: &ScopeFlags) -> Vec<OsString> {
    let mut v = Vec::new();
    if s.hidden {
        v.push(OsString::from("--hidden"));
    }
    if s.no_ignore >= 1 {
        v.push(OsString::from("--no-ignore"));
    }
    for t in &s.types {
        v.push(OsString::from("--type"));
        v.push(OsString::from(t));
    }
    for g in &s.globs {
        v.push(OsString::from("--glob"));
        v.push(OsString::from(g));
    }
    v
}

/// Split `paths` into batches whose total byte cost stays within `budget`, with at
/// least one path per batch (so a single oversized path still makes progress). The
/// union of the batches is exactly `paths`. Empty input yields no batches.
pub fn batches(paths: &[Vec<u8>], budget: usize) -> Vec<&[Vec<u8>]> {
    let mut out = Vec::new();
    if paths.is_empty() {
        return out;
    }
    let mut start = 0;
    let mut acc = 0usize;
    for (i, p) in paths.iter().enumerate() {
        let cost = p.len() + 1; // +1 for the argv separator overhead
        if i > start && acc + cost > budget {
            out.push(&paths[start..i]);
            start = i;
            acc = 0;
        }
        acc += cost;
    }
    out.push(&paths[start..]);
    out
}

/// Split NUL-terminated `rg` output into paths, dropping the trailing empty
/// segment (spec §2.2; the spike confirmed `--null` terminates each path).
fn parse_nul(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

fn os_from_bytes(b: &[u8]) -> OsString {
    OsStr::from_bytes(b).to_os_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[test]
    fn parse_nul_drops_trailing_empty() {
        assert_eq!(parse_nul(b"a.txt\0b.txt\0"), vec![v("a.txt"), v("b.txt")]);
        assert_eq!(parse_nul(b""), Vec::<Vec<u8>>::new());
    }

    #[test]
    fn b1_all_paths_fit_one_batch() {
        let paths = vec![v("a"), v("b"), v("c")];
        let b = batches(&paths, 1024);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0], &paths[..]);
    }

    #[test]
    fn b2_splits_when_over_budget() {
        let paths = vec![v("aaaa"), v("bbbb"), v("cccc")]; // cost 5 each
        let b = batches(&paths, 6); // one path (cost 5) per batch
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn b3_single_oversized_path_is_its_own_batch() {
        let paths = vec![v("x"), v("this_one_is_huge"), v("y")];
        let b = batches(&paths, 4); // most paths exceed 4 alone
                                    // every path ends up in some batch, no batch is empty
        assert!(b.iter().all(|batch| !batch.is_empty()));
        let flat: Vec<&Vec<u8>> = b.iter().flat_map(|s| s.iter()).collect();
        assert_eq!(flat, vec![&paths[0], &paths[1], &paths[2]]);
    }

    #[test]
    fn b4_union_of_batches_equals_input() {
        let paths: Vec<Vec<u8>> = (0..50).map(|i| v(&format!("file{i:02}.txt"))).collect();
        let b = batches(&paths, 30);
        let flat: Vec<Vec<u8>> = b.iter().flat_map(|s| s.iter().cloned()).collect();
        assert_eq!(flat, paths);
        assert!(b.iter().all(|batch| !batch.is_empty()));
    }

    #[test]
    fn b5_empty_input_yields_no_batches() {
        let paths: Vec<Vec<u8>> = vec![];
        assert!(batches(&paths, 1024).is_empty());
    }

    #[test]
    fn match_args_mapping() {
        let m = MatchFlags {
            ignore_case: true,
            whole_word: true,
            fixed_strings: false,
            case_sensitive: false,
        };
        assert_eq!(
            match_args(&m),
            vec![
                OsString::from("--ignore-case"),
                OsString::from("--word-regexp")
            ]
        );
    }

    #[test]
    fn scope_args_mapping() {
        let s = ScopeFlags {
            hidden: true,
            no_ignore: 2,
            types: vec!["py".into(), "md".into()],
            globs: vec!["*.x".into()],
        };
        assert_eq!(
            scope_args(&s),
            vec![
                OsString::from("--hidden"),
                OsString::from("--no-ignore"),
                OsString::from("--type"),
                OsString::from("py"),
                OsString::from("--type"),
                OsString::from("md"),
                OsString::from("--glob"),
                OsString::from("*.x"),
            ]
        );
    }
}
