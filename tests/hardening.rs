//! Hardening / edge-case e2e tests (spec §8.3, §12, §15; TEST_PLAN §4): byte-exact
//! handling of non-UTF-8 and newline-containing paths, leading-dash terms and
//! filenames, surfaced ripgrep errors, and the exit-code audit.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn rgq_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rgq"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn rgq")
}

/// Create a file whose name is raw bytes (possibly non-UTF-8 / containing a
/// newline) with the given contents.
fn write_bytes_name(dir: &Path, name: &[u8], body: &str) {
    let path = dir.join(OsStr::from_bytes(name));
    fs::write(path, body).unwrap();
}

// ---- E1: non-UTF-8 path bytes survive byte-for-byte through --print0 ----

#[test]
fn non_utf8_filename_roundtrips_through_print0() {
    let dir = TempDir::new().unwrap();
    let name: &[u8] = b"inv\xFFalid.txt"; // 0xFF is not valid UTF-8
    write_bytes_name(dir.path(), name, "cat\n");

    let out = rgq_in(dir.path(), &["-0", "cat"]);
    assert!(out.status.success());
    let mut expected = name.to_vec();
    expected.push(0); // NUL-terminated
    assert_eq!(out.stdout, expected, "non-UTF-8 path must survive verbatim");
}

// ---- E2: a newline in a filename is only safe under --print0 ----

#[test]
fn newline_in_filename_roundtrips_through_print0() {
    let dir = TempDir::new().unwrap();
    let name: &[u8] = b"two\nlines.txt";
    write_bytes_name(dir.path(), name, "cat\n");

    let out = rgq_in(dir.path(), &["-0", "cat"]);
    assert!(out.status.success());
    let mut expected = name.to_vec();
    expected.push(0);
    assert_eq!(
        out.stdout, expected,
        "newline path must survive under --print0"
    );
}

// ---- E4: leading-dash term and a file named like a flag (spec §8.3) ----

#[test]
fn file_named_like_a_flag_survives_narrowing() {
    // Two-term query forces the dash-named file to be passed back to rg as a
    // candidate path (after `--`), exercising the end-of-options guard.
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("-dash.txt"), "cat dog\n").unwrap();

    let out = rgq_in(dir.path(), &["cat AND dog"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "-dash.txt\n");
}

#[test]
fn leading_dash_search_term_matches() {
    // The term `-x` begins with a dash; `-e` keeps rg from reading it as a flag.
    // The query itself must be passed after `--` so clap doesn't read it either.
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("hit.txt"), "value -x here\n").unwrap();
    fs::write(dir.path().join("miss.txt"), "nothing\n").unwrap();

    let out = rgq_in(dir.path(), &["--", "-x"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hit.txt\n");
}

// ---- E7: a malformed-regex term surfaces ripgrep's error (spec §12) ----

#[test]
fn bad_regex_term_surfaces_rg_error() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "anything\n").unwrap();

    // '("' is a well-formed *query* (a single quoted term) but an invalid regex.
    let out = rgq_in(dir.path(), &["\"(\""]);
    assert_ne!(out.status.code(), Some(0), "a bad regex must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ripgrep"),
        "should surface rg's error; got: {stderr}"
    );
}

// ---- exit-code audit (spec §12) ----

#[test]
fn exit_codes_audit() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "cat\n").unwrap();

    // matches -> 0
    assert_eq!(rgq_in(dir.path(), &["cat"]).status.code(), Some(0));
    // zero matches -> 0
    assert_eq!(rgq_in(dir.path(), &["zzzznope"]).status.code(), Some(0));
    // unsatisfiable -> 0
    assert_eq!(
        rgq_in(dir.path(), &["cat AND NOT cat"]).status.code(),
        Some(0)
    );
    // parse error -> 2
    assert_eq!(rgq_in(dir.path(), &["cat AND"]).status.code(), Some(2));
    // empty query -> 2
    assert_eq!(rgq_in(dir.path(), &[""]).status.code(), Some(2));
    // clause cap exceeded -> 2
    assert_eq!(
        rgq_in(dir.path(), &["--max-clauses", "1", "(a OR b) AND (c OR d)"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn max_clauses_cap_message_is_clear() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let out = rgq_in(dir.path(), &["--max-clauses", "2", "(a OR b) AND (c OR d)"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("max-clauses"), "got: {stderr}");
}

// ---- security: output handling (see SECURITY.md) ----

/// Piped (non-terminal) output is raw, so tooling receives exact bytes — even an
/// ESC byte embedded in a filename. (The terminal-escape *escaping* applies only
/// when stdout is a TTY; that path is covered by `cli::sanitize_*` unit tests.)
/// This locks the documented contract: pipe = raw, use `--print0` for safety.
#[test]
fn piped_output_is_raw() {
    let dir = TempDir::new().unwrap();
    let name: &[u8] = b"weird\x1bname.txt"; // embedded ESC
    write_bytes_name(dir.path(), name, "cat\n");

    let out = rgq_in(dir.path(), &["cat"]); // captured => not a tty
    assert!(out.status.success());
    let mut expected = name.to_vec();
    expected.push(b'\n');
    assert_eq!(
        out.stdout, expected,
        "piped output must be the exact raw bytes"
    );
}

/// Resilience to a non-fatal ripgrep I/O error: an unreadable file makes `rg` exit
/// 2, but rgq should warn and return the files it *could* read, not hard-fail.
/// This also defuses a race where a file is deleted between listing and narrowing.
/// (On a root test runner the file is readable, so the query simply succeeds — the
/// assertion holds either way.)
#[test]
fn unreadable_file_does_not_fail_the_whole_query() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("readable.txt"), "cat\n").unwrap();
    let unreadable = dir.path().join("unreadable.txt");
    fs::write(&unreadable, "cat\n").unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

    let out = rgq_in(dir.path(), &["cat"]);
    // Restore perms so the tempdir can be cleaned up.
    let _ = fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644));

    assert_eq!(
        out.status.code(),
        Some(0),
        "a single unreadable file must not fail the query"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("readable.txt"),
        "the readable match must still be returned"
    );
}
