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
