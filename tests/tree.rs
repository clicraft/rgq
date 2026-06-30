//! End-to-end `--tree` test: drive the real binary over a fixture whose matching
//! files are exactly the spec §10.4 example, and assert the box-drawing output
//! byte-for-byte (spec §13.3).

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

const GOLDEN: &str = "\
.
├── README.md
└── src
    ├── a
    │   ├── main.py
    │   └── util.py
    └── b
        └── test.py
";

#[test]
fn tree_output_matches_spec_10_4() {
    let dir = TempDir::new().unwrap();
    // Every file contains the term `needle`, so `rgq --tree needle` selects them all.
    for rel in [
        "README.md",
        "src/a/main.py",
        "src/a/util.py",
        "src/b/test.py",
    ] {
        write(dir.path(), rel, "needle\n");
    }

    let out = Command::new(env!("CARGO_BIN_EXE_rgq"))
        .current_dir(dir.path())
        .args(["--tree", "needle"])
        .output()
        .expect("spawn rgq");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), GOLDEN);
}
