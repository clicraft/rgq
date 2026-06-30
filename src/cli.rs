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
use std::io::{self, Write};
use std::process::ExitCode;

use clap::{ArgAction, Parser};

use crate::ast::Normalized;
use crate::{engine, explain, lexer, normalize, parser, rg, tree};

const ABOUT: &str = "A boolean-query front end for ripgrep: combine terms with AND, OR, NOT \
and parentheses; rgq reports the set of files satisfying the expression, optionally as a tree.";

const EXAMPLES: &str = "\
EXAMPLES:
    rgq 'TODO AND FIXME'
    rgq '(cat OR feline) AND NOT kitten' --tree
    rgq -i -t py 'import AND NOT __future__'
    rgq -n '(A AND B) OR C'              # show the compiled plan, run nothing
    rgq -0 'error AND NOT timeout' | xargs -0 wc -l

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
    #[arg(long = "print0", short = '0', conflicts_with = "tree", help_heading = "Output")]
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
    #[arg(long = "max-clauses", value_name = "N", default_value_t = 1024, help_heading = "Limits")]
    max_clauses: usize,

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

/// Write the final path set in the requested output mode (spec §9). Paths are
/// written as raw bytes (byte-oriented, spec §2.2); `--print0` is the form that is
/// also correct for paths containing newlines.
fn emit(files: &BTreeSet<Vec<u8>>, mode: OutputMode) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    match mode {
        OutputMode::List => {
            for path in files {
                out.write_all(path)?;
                out.write_all(b"\n")?;
            }
        }
        OutputMode::Print0 => {
            for path in files {
                out.write_all(path)?;
                out.write_all(b"\0")?;
            }
        }
        OutputMode::Tree => {
            out.write_all(&tree::render(files.iter().map(Vec::as_slice)))?;
        }
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(c.scope_flags.types, vec!["py".to_string(), "md".to_string()]);
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
        assert_eq!(cfg(&["--print0", "cat"]).unwrap().output, OutputMode::Print0);
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
        assert_eq!(cfg(&["(cat OR dog) AND NOT bird"]).unwrap().query, "(cat OR dog) AND NOT bird");
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
