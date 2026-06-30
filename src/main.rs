//! `rgq` — a boolean-query front end for [ripgrep].
//!
//! The user writes a logical expression over search terms (`AND`, `OR`, `NOT`,
//! parentheses) and `rgq` reports the set of files satisfying it, optionally
//! rendered as an ASCII tree.
//!
//! **Milestone M1 (CLI skeleton).** This binary currently implements argument
//! parsing, the load-bearing match-vs-scope flag classification (see
//! [`cli::MatchFlags`] / [`cli::ScopeFlags`]), `--help`, and exit-code plumbing.
//! Query lexing/parsing/normalization and the search engine arrive in later
//! milestones (see `PLAN.md`).
//!
//! [ripgrep]: https://github.com/BurntSushi/ripgrep

use std::process::ExitCode;

mod ast;
mod cli;
mod engine;
mod explain;
mod lexer;
mod membudget;
mod normalize;
mod parser;
mod rg;
mod tree;

fn main() -> ExitCode {
    cli::run()
}
