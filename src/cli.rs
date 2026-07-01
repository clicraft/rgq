//! Command-line interface: argument parsing, flag classification, dispatch,
//! and exit codes.
//!
//! The most important job here is classifying flags into two kinds, because they
//! must be applied differently downstream (spec §7):
//!
//! * **match** flags ([`MatchFlags`]) change *how* a term is matched and apply to
//!   every pattern-bearing `rg` invocation;
//! * **scope** flags ([`ScopeFlags`]) change *which files exist* — they define the
//!   universe `U` and must be applied identically to the universe listing and to
//!   every search, or `NOT`/intersections become inconsistent.
//!
//! Getting that split right is a correctness requirement, not a nicety, so it is
//! modelled in the type system rather than passed around as a loose bag of bools.

use std::collections::BTreeSet;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use clap::{ArgAction, Parser};

use crate::ast::Normalized;
use crate::{engine, explain, lexer, membudget, normalize, parser, rg, tree};

const ABOUT: &str = "A boolean-query front end for ripgrep: combine terms with AND, OR, NOT \
and parentheses; rgq reports the set of files satisfying the expression, optionally as a tree.";

const EXAMPLES: &str = "\
EXAMPLES:
    rgq 'TODO AND FIXME'
    rgq '(cat OR feline) AND NOT kitten' --tree
    rgq -i -t py 'import AND NOT __future__'
    rgq -n '(A AND B) OR C'              # show the compiled plan, run nothing
    rgq -0 'error AND NOT timeout' | xargs -0 wc -l
    rgq '\"AND\" OR \"NOT\"'             # quote a keyword to search for it literally

Quote the whole query when it contains parentheses or NOT, otherwise the shell will
interpret them. A query that begins with '-' must be separated with '--', e.g.
    rgq -- '-x OR foo'";

/// Raw, clap-parsed arguments. Converted into a validated [`Config`] by
/// [`Cli::into_config`]; the rest of the program works with `Config`, never with
/// this struct directly.
#[derive(Parser, Debug)]
#[command(name = "rgq", version, about = ABOUT, after_help = EXAMPLES)]
pub struct Cli {
    // ---- output mode ----
    /// Render matching files as an ASCII tree instead of a flat list.
    #[arg(long, help_heading = "Output")]
    tree: bool,

    /// Print the normalized clauses and execution plan; do not run any search.
    #[arg(long, short = 'n', help_heading = "Output")]
    explain: bool,

    /// Emit NUL-separated paths (for `xargs -0`). Conflicts with --tree.
    #[arg(
        long = "print0",
        short = '0',
        conflicts_with = "tree",
        help_heading = "Output"
    )]
    print0: bool,

    // ---- match flags: how a term is matched (apply to every search) ----
    /// Case-insensitive matching.
    #[arg(short = 'i', help_heading = "Match")]
    ignore_case: bool,

    /// Whole-word matching.
    #[arg(short = 'w', help_heading = "Match")]
    whole_word: bool,

    /// Treat terms as literal fixed strings, not regexes.
    #[arg(short = 'F', help_heading = "Match")]
    fixed_strings: bool,

    /// Force case-sensitive matching.
    #[arg(short = 's', help_heading = "Match")]
    case_sensitive: bool,

    // ---- scope flags: which files exist (define the universe U) ----
    /// Include hidden files and directories.
    #[arg(long, help_heading = "Scope")]
    hidden: bool,

    /// Do not respect ignore files (.gitignore, etc.). Repeat (-uu) to also include hidden files.
    #[arg(short = 'u', long = "no-ignore", action = ArgAction::Count, help_heading = "Scope")]
    no_ignore: u8,

    /// Restrict to a ripgrep file type (e.g. py). May be repeated.
    #[arg(short = 't', value_name = "TYPE", help_heading = "Scope")]
    r#type: Vec<String>,

    /// Restrict to a glob (e.g. '*.md'). May be repeated.
    #[arg(short = 'g', value_name = "GLOB", help_heading = "Scope")]
    glob: Vec<String>,

    // ---- limits ----
    /// Maximum number of clauses a query may expand to in normal form. Guards
    /// against the combinatorial blow-up of disjunctive normal form.
    #[arg(
        long = "max-clauses",
        value_name = "N",
        default_value_t = 1024,
        help_heading = "Limits"
    )]
    max_clauses: usize,

    /// Minimum percentage of total system memory to always keep free. Before
    /// rendering --tree (whose output can exceed the matched file set in size),
    /// rgq predicts the memory it would need and refuses rather than risk this
    /// margin if the prediction would eat into it.
    #[arg(
        long = "min-free-mem-pct",
        value_name = "PCT",
        default_value_t = 20,
        value_parser = clap::value_parser!(u8).range(0..=100),
        help_heading = "Limits"
    )]
    min_free_mem_pct: u8,

    // ---- the query ----
    /// Boolean query, e.g. '(cat OR dog) AND NOT bird'. Several words are joined with spaces.
    #[arg(value_name = "QUERY")]
    query: Vec<String>,
}

/// Flags that change *how* a term is matched. Applied to every pattern-bearing
/// `rg` invocation (spec §7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchFlags {
    pub ignore_case: bool,
    pub whole_word: bool,
    pub fixed_strings: bool,
    pub case_sensitive: bool,
}

/// Flags that change *which files exist* — they define the universe `U` and must
/// be applied identically to the universe listing and to every search (spec §7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeFlags {
    pub hidden: bool,
    /// `--no-ignore` repetition count: 0 = respect ignores, 1 = `-u`, 2 = `-uu` (also hidden).
    pub no_ignore: u8,
    pub types: Vec<String>,
    pub globs: Vec<String>,
}

/// How the final path set is rendered. `--explain` is orthogonal (it short-circuits
/// execution) and is carried separately on [`Config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Default: one path per line, sorted.
    List,
    /// `--tree`: ASCII tree.
    Tree,
    /// `--print0`: NUL-separated paths.
    Print0,
}

/// The validated, classified configuration the rest of the program runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub query: String,
    pub match_flags: MatchFlags,
    pub scope_flags: ScopeFlags,
    pub output: OutputMode,
    pub explain: bool,
    pub max_clauses: usize,
    pub min_free_mem_pct: u8,
}

impl Cli {
    /// Validate and classify raw args into a [`Config`].
    ///
    /// Returns `Err(message)` for a usage problem we own (currently: an empty
    /// query). clap-level usage errors (unknown flags, conflicts) are handled
    /// earlier, in [`run`].
    fn into_config(self) -> Result<Config, String> {
        let query = self.query.join(" ");
        if query.trim().is_empty() {
            return Err(
                "empty query: provide a boolean expression, e.g.  rgq 'cat AND dog'".to_string(),
            );
        }

        let match_flags = MatchFlags {
            ignore_case: self.ignore_case,
            whole_word: self.whole_word,
            fixed_strings: self.fixed_strings,
            case_sensitive: self.case_sensitive,
        };

        // `-uu` (two repetitions) additionally implies `--hidden` (spec §3.2).
        let scope_flags = ScopeFlags {
            hidden: self.hidden || self.no_ignore >= 2,
            no_ignore: self.no_ignore,
            types: self.r#type,
            globs: self.glob,
        };

        let output = if self.tree {
            OutputMode::Tree
        } else if self.print0 {
            OutputMode::Print0
        } else {
            OutputMode::List
        };

        Ok(Config {
            query,
            match_flags,
            scope_flags,
            output,
            explain: self.explain,
            max_clauses: self.max_clauses,
            min_free_mem_pct: self.min_free_mem_pct,
        })
    }
}

/// Parse arguments, classify them, and dispatch. The single entry point from
/// `main`; owns the mapping from outcomes to process exit codes (spec §12).
pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => return report_clap_error(err),
    };

    let config = match cli.into_config() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("rgq: {message}");
            return ExitCode::from(2);
        }
    };

    dispatch(&config)
}

/// Render a clap parsing error and pick the right exit code. `--help`/`--version`
/// are "errors" in clap's model but are successful outcomes (exit 0); genuine
/// usage errors exit 2 (spec §12), with an extra hint for unknown flags.
fn report_clap_error(err: clap::Error) -> ExitCode {
    use clap::error::ErrorKind;

    // clap writes help/version to stdout and real errors to stderr.
    let _ = err.print();

    match err.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => ExitCode::SUCCESS,
        ErrorKind::UnknownArgument => {
            eprintln!(
                "\nhint: that looks like an unknown flag. To search for it as a term, quote it \
                 inside the query, e.g.  rgq '\"--foo\" AND bar'"
            );
            ExitCode::from(2)
        }
        _ => ExitCode::from(2),
    }
}

/// Act on a validated [`Config`].
///
/// M3: the query is lexed, parsed, and normalized to DNF. `--explain` renders the
/// normalized clauses and the execution plan; an unsatisfiable query is reported
/// and exits 0 (spec §12) without needing the engine. Actual search execution
/// (M4) is not built yet, so a satisfiable runnable query reports its normal form
/// and exits non-zero rather than pretending to search.
fn dispatch(config: &Config) -> ExitCode {
    let tokens = match lexer::lex(&config.query) {
        Ok(tokens) => tokens,
        Err(err) => {
            eprintln!("rgq: {err}");
            return ExitCode::from(2);
        }
    };

    let ast = match parser::parse(&tokens) {
        Ok(ast) => ast,
        Err(err) => {
            eprintln!("rgq: {err}");
            return ExitCode::from(2);
        }
    };

    let normalized = match normalize::normalize(&ast, config.max_clauses) {
        Ok(normalized) => normalized,
        Err(err) => {
            eprintln!("rgq: {err}");
            return ExitCode::from(2);
        }
    };

    if config.explain {
        print!("{}", explain::explain(&ast, &normalized));
        return ExitCode::SUCCESS;
    }

    // An unsatisfiable query needs no search: report and succeed (spec §12).
    let clauses = match normalized {
        Normalized::Unsatisfiable => {
            eprintln!("rgq: query is unsatisfiable (every clause is self-contradictory); it matches no files");
            return ExitCode::SUCCESS;
        }
        Normalized::Clauses(clauses) => clauses,
    };

    let rg = rg::Rg::new(&config.match_flags, &config.scope_flags);
    let outcome = match engine::run(&clauses, &rg) {
        Ok(outcome) => outcome,
        Err(err) => {
            eprintln!("rgq: {err}");
            return ExitCode::FAILURE;
        }
    };

    for warning in &outcome.warnings {
        eprintln!("rgq: warning: {warning}");
    }

    // --tree's output can exceed the matched file set in size (per-line prefix
    // overhead, §10), unlike List/Print0 which write each path once with no
    // amplification. Predict the memory it would need before committing to it,
    // and refuse cleanly rather than risk exhausting memory (spec: keep
    // min_free_mem_pct of total system memory free, always).
    if config.output == OutputMode::Tree {
        let estimate = tree::estimate_memory_bytes(outcome.files.iter().map(Vec::as_slice));
        match membudget::check(estimate.total(), config.min_free_mem_pct) {
            membudget::CheckResult::Proceed => {}
            membudget::CheckResult::Unknown => {
                eprintln!(
                    "rgq: warning: could not determine available system memory; \
                     proceeding without a memory safety check"
                );
            }
            membudget::CheckResult::Refuse(err) => {
                eprintln!("rgq: {err}");
                return ExitCode::from(2);
            }
        }
    }

    if let Err(err) = emit(&outcome.files, config.output) {
        // A broken pipe (e.g. `| head`) is a normal, quiet exit.
        if err.kind() == io::ErrorKind::BrokenPipe {
            return ExitCode::SUCCESS;
        }
        eprintln!("rgq: error writing output: {err}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

/// Write the final path set in the requested output mode (spec §9).
///
/// Paths come from whatever tree the user pointed `rgq` at, so a filename is
/// attacker-influenceable. To stop a crafted name from injecting terminal escape
/// sequences (recolour, clear-line, cursor moves — output spoofing), control bytes
/// are escaped in the **human-facing** modes (`List`, `Tree`) **when stdout is a
/// terminal**. Piped/redirected output stays raw so tooling gets exact bytes, and
/// `--print0` is always raw — it is the correct, unambiguous form for machine
/// consumption and for paths containing newlines (spec §2.2, §9).
fn emit(files: &BTreeSet<Vec<u8>>, mode: OutputMode) -> io::Result<()> {
    let stdout = io::stdout();
    let escape = stdout.is_terminal();
    let mut out = stdout.lock();
    match mode {
        OutputMode::List => {
            for path in files {
                out.write_all(&display_path(path, escape))?;
                out.write_all(b"\n")?;
            }
        }
        OutputMode::Print0 => {
            // Raw, NUL-delimited: exact bytes, no escaping. A filename cannot
            // contain NUL, so the framing is unambiguous.
            for path in files {
                out.write_all(path)?;
                out.write_all(b"\0")?;
            }
        }
        OutputMode::Tree => {
            if escape {
                let safe: Vec<Vec<u8>> = files.iter().map(|p| sanitize_controls(p)).collect();
                out.write_all(&tree::render(safe.iter().map(Vec::as_slice)))?;
            } else {
                out.write_all(&tree::render(files.iter().map(Vec::as_slice)))?;
            }
        }
    }
    out.flush()
}

/// A path rendered for human display: control bytes escaped when `escape` is set,
/// otherwise the raw bytes.
fn display_path(path: &[u8], escape: bool) -> Vec<u8> {
    if escape {
        sanitize_controls(path)
    } else {
        path.to_vec()
    }
}

/// UTF-8 byte sequences for Unicode codepoints neutralized in human-facing
/// output, beyond plain C0 control bytes: bidirectional **format** characters
/// (the "Trojan Source" class — CVE-2021-42574 — which can reorder how text
/// *displays* without changing the underlying bytes, e.g. making `cat\u{202e}gpj.txt`
/// render as `cat` followed by what looks like `txt.jpg`) and common invisible /
/// zero-width characters (can hide text, or make two different filenames render
/// identically). Deliberately narrow: this does **not** attempt general
/// homoglyph/confusable detection (e.g. Cyrillic vs Latin lookalikes) — that is a
/// much larger, fuzzy problem with real false-positive risk for legitimate
/// non-Latin filenames, and a script-mixing heuristic is a poor substitute for
/// actually reading the bytes. Every entry here is exactly 3 bytes in UTF-8.
const DANGEROUS_CODEPOINTS: &[(u32, &[u8])] = &[
    (0x202a, &[0xe2, 0x80, 0xaa]), // LEFT-TO-RIGHT EMBEDDING
    (0x202b, &[0xe2, 0x80, 0xab]), // RIGHT-TO-LEFT EMBEDDING
    (0x202c, &[0xe2, 0x80, 0xac]), // POP DIRECTIONAL FORMATTING
    (0x202d, &[0xe2, 0x80, 0xad]), // LEFT-TO-RIGHT OVERRIDE
    (0x202e, &[0xe2, 0x80, 0xae]), // RIGHT-TO-LEFT OVERRIDE
    (0x2066, &[0xe2, 0x81, 0xa6]), // LEFT-TO-RIGHT ISOLATE
    (0x2067, &[0xe2, 0x81, 0xa7]), // RIGHT-TO-LEFT ISOLATE
    (0x2068, &[0xe2, 0x81, 0xa8]), // FIRST STRONG ISOLATE
    (0x2069, &[0xe2, 0x81, 0xa9]), // POP DIRECTIONAL ISOLATE
    (0x200b, &[0xe2, 0x80, 0x8b]), // ZERO WIDTH SPACE
    (0x200c, &[0xe2, 0x80, 0x8c]), // ZERO WIDTH NON-JOINER
    (0x200d, &[0xe2, 0x80, 0x8d]), // ZERO WIDTH JOINER
    (0x2060, &[0xe2, 0x81, 0xa0]), // WORD JOINER
    (0xfeff, &[0xef, 0xbb, 0xbf]), // ZERO WIDTH NO-BREAK SPACE / BOM
];

/// Replace C0 control bytes (`0x00`–`0x1f`, including ESC and newline/CR), DEL
/// (`0x7f`), and the [`DANGEROUS_CODEPOINTS`] above with a visible escaped form,
/// so a crafted filename can neither drive the terminal nor spoof what's
/// displayed. Other bytes `>= 0x80` (ordinary UTF-8, or invalid bytes) pass
/// through unchanged so legitimate non-ASCII filenames still display.
fn sanitize_controls(path: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(path.len());
    let mut i = 0;
    while i < path.len() {
        let b = path[i];
        if b < 0x20 || b == 0x7f {
            out.extend_from_slice(format!("\\x{b:02x}").as_bytes());
            i += 1;
            continue;
        }
        if let Some(&(cp, _)) = DANGEROUS_CODEPOINTS
            .iter()
            .find(|(_, seq)| path[i..].starts_with(seq))
        {
            out.extend_from_slice(format!("\\u{{{cp:04x}}}").as_bytes());
            i += 3; // every denylisted sequence above is exactly 3 bytes in UTF-8
            continue;
        }
        out.push(b);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_neutralizes_terminal_escapes() {
        // An ANSI colour + clear-line + CR injected via a filename.
        let evil = b"evil\x1b[31m\x1b[2K\rsafe.txt";
        let safe = sanitize_controls(evil);
        // No raw ESC / CR survive; they become visible \xHH.
        assert!(!safe.contains(&0x1b), "ESC must be escaped");
        assert!(!safe.contains(&b'\r'), "CR must be escaped");
        assert_eq!(safe, b"evil\\x1b[31m\\x1b[2K\\x0dsafe.txt");
    }

    #[test]
    fn sanitize_neutralizes_bidi_override_spoofing() {
        // "Trojan Source" style: cat<RLO>txt.gpj<PDF> renders as cat + "jpg.txt"
        // reversed, spoofing a .txt file as something else. Raw RLO/PDF bytes
        // (e2 80 ae / e2 80 ac) were confirmed to pass through unescaped before
        // this fix.
        let evil = "cat\u{202e}txt.gpj\u{202c}".as_bytes();
        let safe = sanitize_controls(evil);
        assert!(
            !safe.windows(3).any(|w| w == [0xe2, 0x80, 0xae]),
            "RLO must not survive raw"
        );
        assert!(
            !safe.windows(3).any(|w| w == [0xe2, 0x80, 0xac]),
            "PDF must not survive raw"
        );
        assert_eq!(safe, b"cat\\u{202e}txt.gpj\\u{202c}");
    }

    #[test]
    fn sanitize_neutralizes_invisible_characters() {
        // Zero-width space can hide text or make two different names render
        // identically (e.g. "secret" vs "se\u{200b}cret").
        let evil = "se\u{200b}cret".as_bytes();
        assert_eq!(sanitize_controls(evil), b"se\\u{200b}cret");
        // BOM / zero-width no-break space.
        assert_eq!(sanitize_controls("\u{feff}x".as_bytes()), b"\\u{feff}x");
    }

    #[test]
    fn sanitize_does_not_mangle_ordinary_non_ascii() {
        // A 3-byte UTF-8 sequence that is NOT in the denylist (e.g. é = U+00E9
        // is 2 bytes; use a real 3-byte char, the EUR sign U+20AC) must pass
        // through untouched, proving the matcher doesn't over-match by length.
        let euro = "café-€100".as_bytes();
        assert_eq!(sanitize_controls(euro), euro);
    }

    #[test]
    fn sanitize_passes_through_printable_and_high_bytes() {
        // Printable ASCII and UTF-8 / high bytes are preserved verbatim.
        let name = "café/path-1.txt".as_bytes();
        assert_eq!(sanitize_controls(name), name);
        assert_eq!(sanitize_controls(&[0xFF, 0xFE]), vec![0xFF, 0xFE]);
    }

    #[test]
    fn sanitize_escapes_newline_and_tab_and_del() {
        assert_eq!(sanitize_controls(b"a\nb\tc\x7f"), b"a\\x0ab\\x09c\\x7f");
    }

    /// Parse args (with the implicit `rgq` argv[0]) and classify, asserting the
    /// parse itself succeeds.
    fn cfg(args: &[&str]) -> Result<Config, String> {
        let cli = Cli::try_parse_from(std::iter::once("rgq").chain(args.iter().copied()))
            .expect("args should parse at the clap level");
        cli.into_config()
    }

    #[test]
    fn uu_implies_hidden_and_no_ignore_2() {
        let c = cfg(&["-uu", "cat"]).unwrap();
        assert_eq!(c.scope_flags.no_ignore, 2);
        assert!(c.scope_flags.hidden, "-uu must imply --hidden");
    }

    #[test]
    fn single_u_is_no_ignore_1_not_hidden() {
        let c = cfg(&["-u", "cat"]).unwrap();
        assert_eq!(c.scope_flags.no_ignore, 1);
        assert!(!c.scope_flags.hidden);
    }

    #[test]
    fn no_ignore_long_flag_counts_as_one() {
        let c = cfg(&["--no-ignore", "cat"]).unwrap();
        assert_eq!(c.scope_flags.no_ignore, 1);
        assert!(!c.scope_flags.hidden);
    }

    #[test]
    fn explicit_hidden_without_no_ignore() {
        let c = cfg(&["--hidden", "cat"]).unwrap();
        assert!(c.scope_flags.hidden);
        assert_eq!(c.scope_flags.no_ignore, 0);
    }

    #[test]
    fn match_flags_classified() {
        let c = cfg(&["-i", "-w", "-F", "-s", "cat"]).unwrap();
        assert_eq!(
            c.match_flags,
            MatchFlags {
                ignore_case: true,
                whole_word: true,
                fixed_strings: true,
                case_sensitive: true,
            }
        );
    }

    #[test]
    fn types_and_globs_accumulate_in_order() {
        let c = cfg(&["-t", "py", "-t", "md", "-g", "*.rs", "cat"]).unwrap();
        assert_eq!(
            c.scope_flags.types,
            vec!["py".to_string(), "md".to_string()]
        );
        assert_eq!(c.scope_flags.globs, vec!["*.rs".to_string()]);
    }

    #[test]
    fn output_mode_defaults_to_list() {
        assert_eq!(cfg(&["cat"]).unwrap().output, OutputMode::List);
    }

    #[test]
    fn output_mode_tree() {
        assert_eq!(cfg(&["--tree", "cat"]).unwrap().output, OutputMode::Tree);
    }

    #[test]
    fn output_mode_print0_long_and_short() {
        assert_eq!(
            cfg(&["--print0", "cat"]).unwrap().output,
            OutputMode::Print0
        );
        assert_eq!(cfg(&["-0", "cat"]).unwrap().output, OutputMode::Print0);
    }

    #[test]
    fn explain_long_and_short() {
        assert!(cfg(&["--explain", "cat"]).unwrap().explain);
        assert!(cfg(&["-n", "cat"]).unwrap().explain);
        assert!(!cfg(&["cat"]).unwrap().explain);
    }

    #[test]
    fn multi_word_query_is_joined_with_spaces() {
        assert_eq!(cfg(&["cat", "AND", "dog"]).unwrap().query, "cat AND dog");
    }

    #[test]
    fn single_quoted_query_passes_through() {
        assert_eq!(
            cfg(&["(cat OR dog) AND NOT bird"]).unwrap().query,
            "(cat OR dog) AND NOT bird"
        );
    }

    #[test]
    fn empty_query_is_a_usage_error() {
        let cli = Cli::try_parse_from(["rgq"]).unwrap();
        assert!(cli.into_config().is_err());
    }

    #[test]
    fn whitespace_only_query_is_a_usage_error() {
        let cli = Cli::try_parse_from(["rgq", "   "]).unwrap();
        assert!(cli.into_config().is_err());
    }

    #[test]
    fn tree_and_print0_conflict_at_parse_time() {
        let res = Cli::try_parse_from(["rgq", "--tree", "--print0", "cat"]);
        assert!(res.is_err(), "--tree and --print0 must conflict");
    }
}
