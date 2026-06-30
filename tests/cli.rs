//! Binary-level tests for the M1 CLI skeleton: `--help`, exit codes, and the
//! usage-error paths (spec §12). Driven through the real compiled binary via
//! cargo's `CARGO_BIN_EXE_rgq` env var, so no test-harness dependency is needed.

use std::process::{Command, Output};

fn rgq(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rgq"))
        .args(args)
        .output()
        .expect("failed to spawn rgq binary")
}

#[test]
fn help_succeeds_and_lists_examples() {
    let out = rgq(&["--help"]);
    assert!(out.status.success(), "--help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("rgq"), "help should name the tool");
    assert!(
        stdout.contains("TODO AND FIXME"),
        "help should carry the spec §3.3 examples"
    );
}

#[test]
fn version_succeeds() {
    let out = rgq(&["--version"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("rgq"));
}

#[test]
fn no_query_is_usage_error_exit_2() {
    let out = rgq(&[]);
    assert_eq!(out.status.code(), Some(2), "empty query must exit 2");
    assert!(String::from_utf8_lossy(&out.stderr).contains("empty query"));
}

#[test]
fn unknown_flag_exits_2_with_hint() {
    let out = rgq(&["--definitely-not-a-real-flag"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("hint"),
        "unknown flag should suggest quoting it as a term; got: {stderr}"
    );
}

#[test]
fn tree_and_print0_conflict_exits_2() {
    let out = rgq(&["--tree", "--print0", "cat"]);
    assert_eq!(out.status.code(), Some(2));
}

// ---- M2: parse/lex errors are usage errors (exit 2, spec §12) ----

#[test]
fn adjacency_is_a_parse_error() {
    // Two terms with no operator between them: no implicit AND.
    let out = rgq(&["cat dog"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no implicit AND"));
}

#[test]
fn dangling_operator_is_a_parse_error() {
    assert_eq!(rgq(&["cat AND"]).status.code(), Some(2));
    assert_eq!(rgq(&["AND cat"]).status.code(), Some(2));
}

#[test]
fn unbalanced_paren_is_a_parse_error() {
    assert_eq!(rgq(&["(cat"]).status.code(), Some(2));
}

#[test]
fn unterminated_quote_is_a_parse_error() {
    let out = rgq(&["\"cat AND dog"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unterminated"));
}

#[test]
fn explain_prints_parsed_query_and_exits_0() {
    let out = rgq(&["--explain", "(cat AND dog) OR bird"]);
    assert!(out.status.success(), "--explain should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Parsed form is fully parenthesized by precedence.
    assert!(stdout.contains("(cat AND dog) OR bird"), "got: {stdout}");
}

#[test]
fn explain_does_not_run_on_a_quoted_keyword() {
    // '"AND"' is a literal term, so the query is well-formed (a single term).
    let out = rgq(&["--explain", "\"AND\" OR cat"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("AND OR cat"));
}
