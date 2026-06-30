//! End-to-end tests for the predictive memory-safety check on `--tree` (see
//! `src/membudget.rs` and SECURITY.md). Uses the `RGQ_MEM_AVAILABLE_BYTES` /
//! `RGQ_MEM_TOTAL_BYTES` test-only env var overrides so these are fully
//! deterministic and never attempt a genuinely large allocation — that's
//! exactly the mistake this feature exists to prevent (see the OOM incident
//! recorded in SECURITY.md).

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
    fs::write(dir.path().join("b.txt"), "needle\n").unwrap();
    dir
}

fn rgq_tree(dir: &Path, args: &[&str], available_bytes: u64, total_bytes: u64) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rgq"))
        .current_dir(dir)
        .env("RGQ_MEM_AVAILABLE_BYTES", available_bytes.to_string())
        .env("RGQ_MEM_TOTAL_BYTES", total_bytes.to_string())
        .args(["--tree"])
        .args(args)
        .arg("needle")
        .output()
        .expect("spawn rgq")
}

#[test]
fn refuses_when_estimate_would_leave_less_than_the_margin_free() {
    let dir = fixture();
    // Pretend the system has almost no memory at all: any nonzero estimate
    // exceeds the default 20% margin.
    let out = rgq_tree(dir.path(), &[], 10, 1000);
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("refusing"), "got: {stderr}");
    assert!(
        stderr.contains("--min-free-mem-pct"),
        "should hint at the override flag; got: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "must not render any output when refusing"
    );
}

#[test]
fn proceeds_when_plenty_of_memory_is_available() {
    let dir = fixture();
    let out = rgq_tree(dir.path(), &[], 1_000_000_000_000, 1_000_000_000_000);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).starts_with(".\n"));
}

#[test]
fn min_free_mem_pct_flag_relaxes_the_margin() {
    let dir = fixture();

    // available=1000, total=1000: at a strict 99% margin, usable = 1000 - 990
    // = 10 bytes, far below what even this tiny tree needs -> refuse.
    let strict = rgq_tree(dir.path(), &["--min-free-mem-pct", "99"], 1000, 1000);
    assert_eq!(strict.status.code(), Some(2));

    // Same fake memory, but a relaxed 0% margin: usable = all 1000 bytes
    // currently available, comfortably enough for two tiny files -> proceed.
    let relaxed = rgq_tree(dir.path(), &["--min-free-mem-pct", "0"], 1000, 1000);
    assert!(
        relaxed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&relaxed.stderr)
    );
}

#[test]
fn min_free_mem_pct_rejects_out_of_range_values() {
    let dir = fixture();
    let out = Command::new(env!("CARGO_BIN_EXE_rgq"))
        .current_dir(dir.path())
        .args(["--tree", "--min-free-mem-pct", "101", "needle"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn real_system_memory_is_used_when_no_override_is_set() {
    // Without the env var overrides, rgq reads real /proc/meminfo (or warns
    // and proceeds if unavailable). A tiny fixture must comfortably fit under
    // the default 20% margin on any reasonable machine, including CI.
    let dir = fixture();
    let out = Command::new(env!("CARGO_BIN_EXE_rgq"))
        .current_dir(dir.path())
        .args(["--tree", "needle"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn non_tree_output_modes_are_unaffected_by_the_budget_check() {
    // The default list and --print0 never build a trie, so even a fake
    // near-zero memory budget must not block them.
    let dir = fixture();
    let out = Command::new(env!("CARGO_BIN_EXE_rgq"))
        .current_dir(dir.path())
        .env("RGQ_MEM_AVAILABLE_BYTES", "1")
        .env("RGQ_MEM_TOTAL_BYTES", "1000")
        .args(["needle"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
