//! Black-box end-to-end tests for the engine (spec §13.3). They drive the real
//! `rgq` binary against the canonical fixture from TEST_PLAN.md §2.1 and assert the
//! exact path set, the set semantics having been computed by hand and validated
//! against real `rg`.
//!
//! The fixture uses a `.ignore` file and is **not** a git repo — deliberately, so
//! that `--hidden` results aren't polluted by a `.git/` directory (verified).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

/// Build the canonical fixture in a fresh temp dir.
fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    write(p, ".ignore", "*.log\n");
    write(p, "a.txt", "cat dog\n");
    write(p, "b.txt", "cat\n");
    write(p, "c.txt", "dog\n");
    write(p, "d.txt", "bird\n");
    write(p, "e.txt", "bird cage\n");
    write(p, "sub/f.txt", "cat bird\n");
    write(p, "sub/g.md", "TODO FIXME\n");
    write(p, "pkg/h.py", "import os\n");
    write(p, "pkg/i.py", "import __future__\n");
    write(p, ".secret.txt", "cat\n");
    write(p, "build.log", "cat\n");
    dir
}

fn run_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rgq"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn rgq")
}

/// stdout (newline mode) as a sorted set, for order-independent comparison.
fn set(out: &Output) -> BTreeSet<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn want(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|s| s.to_string()).collect()
}

/// Assert a query against the fixture returns exactly `expected` (and exits 0).
fn assert_query(args: &[&str], expected: &[&str]) {
    let dir = fixture();
    let out = run_in(dir.path(), args);
    assert!(
        out.status.success(),
        "args {args:?} exited {:?}; stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(set(&out), want(expected), "args {args:?}");
}

// ---- default-scope semantics (spec §5; TEST_PLAN §2.2) ----

#[test]
fn term() {
    assert_query(&["cat"], &["a.txt", "b.txt", "sub/f.txt"]);
}

#[test]
fn and() {
    assert_query(&["cat AND dog"], &["a.txt"]);
}

#[test]
fn or() {
    assert_query(
        &["cat OR bird"],
        &["a.txt", "b.txt", "d.txt", "e.txt", "sub/f.txt"],
    );
}

#[test]
fn and_not() {
    assert_query(&["cat AND NOT dog"], &["b.txt", "sub/f.txt"]);
}

#[test]
fn not_single_term() {
    assert_query(
        &["NOT cat"],
        &[
            "c.txt", "d.txt", "e.txt", "sub/g.md", "pkg/h.py", "pkg/i.py",
        ],
    );
}

#[test]
fn nested_or_of_and() {
    assert_query(
        &["(cat AND dog) OR bird"],
        &["a.txt", "d.txt", "e.txt", "sub/f.txt"],
    );
}

#[test]
fn not_of_disjunction() {
    assert_query(
        &["NOT (cat OR dog)"],
        &["d.txt", "e.txt", "sub/g.md", "pkg/h.py", "pkg/i.py"],
    );
}

#[test]
fn positive_and_negative() {
    assert_query(&["bird AND NOT cage"], &["d.txt", "sub/f.txt"]);
}

#[test]
fn quoted_keyword_is_a_literal_term() {
    // No file contains the literal word "AND", so it contributes nothing.
    assert_query(&["\"AND\" OR cat"], &["a.txt", "b.txt", "sub/f.txt"]);
}

// ---- scope flags (TEST_PLAN §2.3) ----

#[test]
fn scope_hidden() {
    assert_query(
        &["--hidden", "cat"],
        &["a.txt", "b.txt", "sub/f.txt", ".secret.txt"],
    );
}

#[test]
fn scope_no_ignore() {
    assert_query(
        &["-u", "cat"],
        &["a.txt", "b.txt", "sub/f.txt", "build.log"],
    );
}

#[test]
fn scope_no_ignore_and_hidden() {
    assert_query(
        &["-uu", "cat"],
        &["a.txt", "b.txt", "sub/f.txt", "build.log", ".secret.txt"],
    );
}

#[test]
fn scope_type() {
    assert_query(&["-t", "py", "import"], &["pkg/h.py", "pkg/i.py"]);
}

#[test]
fn scope_type_with_negation() {
    assert_query(&["-t", "py", "import AND NOT __future__"], &["pkg/h.py"]);
}

#[test]
fn scope_glob() {
    assert_query(&["-g", "*.md", "TODO"], &["sub/g.md"]);
}

#[test]
fn scope_type_with_not() {
    // A `-t txt` filter overrides ripgrep's default hidden-exclusion for matching
    // dotfiles, so .secret.txt is in the universe; NOT bird = U \ bird-files keeps
    // it. rgq mirrors rg's universe exactly (spec §7), which the
    // `not_is_universe_minus_matches` test independently confirms.
    assert_query(
        &["-t", "txt", "NOT bird"],
        &[".secret.txt", "a.txt", "b.txt", "c.txt"],
    );
}

/// Scope consistency (spec §7): `NOT t` must equal `U \ ⟦t⟧` under the same scope.
/// `cat OR NOT cat` is a tautology that yields the whole universe `U`.
#[test]
fn not_is_universe_minus_matches() {
    let dir = fixture();
    let universe = set(&run_in(dir.path(), &["cat OR NOT cat"]));
    let cat = set(&run_in(dir.path(), &["cat"]));
    let not_cat = set(&run_in(dir.path(), &["NOT cat"]));

    assert!(
        cat.is_disjoint(&not_cat),
        "a term and its negation must be disjoint"
    );
    let reunited: BTreeSet<String> = cat.union(&not_cat).cloned().collect();
    assert_eq!(reunited, universe, "⟦t⟧ ∪ ⟦NOT t⟧ must reconstitute U");
}

// ---- output modes & exit codes (spec §9, §12; TEST_PLAN §2.4/§2.5) ----

#[test]
fn zero_matches_is_success_with_empty_output() {
    let dir = fixture();
    let out = run_in(dir.path(), &["nonexistenttermxyz"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
}

#[test]
fn unsatisfiable_is_success_with_a_note() {
    let dir = fixture();
    let out = run_in(dir.path(), &["cat AND NOT cat"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unsatisfiable"));
}

#[test]
fn print0_framing_is_exact() {
    let dir = fixture();
    let out = run_in(dir.path(), &["-0", "cat"]);
    assert!(out.status.success());
    // Sorted, NUL-terminated, no trailing newline.
    assert_eq!(out.stdout, b"a.txt\0b.txt\0sub/f.txt\0");
}

#[test]
fn default_list_is_sorted() {
    let dir = fixture();
    let out = run_in(dir.path(), &["cat OR bird"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    let mut sorted = lines.clone();
    sorted.sort_unstable();
    assert_eq!(lines, sorted, "default output must already be sorted");
}

#[test]
fn positive_free_clause_warns_on_stderr() {
    let dir = fixture();
    let out = run_in(dir.path(), &["NOT cat"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no positive term"),
        "expected positive-free warning; got: {stderr}"
    );
}

// ---- edge cases (TEST_PLAN §4) ----

/// E3: a clause that narrows to ∅ must return nothing — not the whole tree (which
/// is what an `rg` call with zero path args would scan).
#[test]
fn empty_candidate_short_circuits_to_nothing() {
    let dir = fixture();
    let out = run_in(dir.path(), &["nonexistentxyz AND cat"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stdout.is_empty(),
        "must be empty, got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// E.g. `rg` missing: pointing RGQ_RG at a non-existent binary surfaces a clear
/// error and a non-zero exit (spec §12).
#[test]
fn missing_rg_is_a_clear_error() {
    let dir = fixture();
    let out = Command::new(env!("CARGO_BIN_EXE_rgq"))
        .current_dir(dir.path())
        .env("RGQ_RG", "/nonexistent/definitely/not/rg")
        .arg("cat")
        .output()
        .expect("spawn rgq");
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ripgrep"),
        "expected an rg error; got: {stderr}"
    );
}

/// ARG_MAX batching (spec §8.2): force a tiny budget and confirm the result is
/// identical to the unbatched run.
#[test]
fn arg_max_batching_is_correct() {
    let dir = tempfile::tempdir().unwrap();
    // 60 files that all contain both terms, plus a few that contain only one.
    for i in 0..60 {
        write(dir.path(), &format!("m{i:02}.txt"), "alpha beta\n");
    }
    write(dir.path(), "only_alpha.txt", "alpha\n");
    write(dir.path(), "only_beta.txt", "beta\n");

    let unbatched = Command::new(env!("CARGO_BIN_EXE_rgq"))
        .current_dir(dir.path())
        .args(["alpha AND beta"])
        .output()
        .unwrap();
    let batched = Command::new(env!("CARGO_BIN_EXE_rgq"))
        .current_dir(dir.path())
        .env("RGQ_ARG_MAX", "16") // tiny budget => many batches
        .args(["alpha AND beta"])
        .output()
        .unwrap();

    assert!(unbatched.status.success() && batched.status.success());
    let expected: BTreeSet<String> = (0..60).map(|i| format!("m{i:02}.txt")).collect();
    assert_eq!(set(&unbatched), expected);
    assert_eq!(
        set(&batched),
        expected,
        "batched result must match unbatched"
    );
}

/// Regex-by-default vs `-F` fixed strings (spec §3.2): `a.c` is a regex that
/// matches `abc`; with `-F` it is the literal three characters `a.c`.
#[test]
fn regex_default_and_fixed_strings() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "regexmatch.txt", "abc\n"); // matched by regex a.c
    write(dir.path(), "literalmatch.txt", "a.c\n"); // matched only literally

    assert_eq!(
        set(&run_in(dir.path(), &["a.c"])),
        want(&["literalmatch.txt", "regexmatch.txt"]),
        "regex default: a.c matches both abc and a.c"
    );
    assert_eq!(
        set(&run_in(dir.path(), &["-F", "a.c"])),
        want(&["literalmatch.txt"]),
        "-F: a.c matches only the literal"
    );
}
