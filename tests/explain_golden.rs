//! Golden `--explain` outputs (spec §13.2). Each query's full stdout must match a
//! checked-in golden file byte-for-byte. These pin the normalized clause list and
//! the execution-plan rendering so the teaching/debugging output can't drift
//! silently; regenerate the files deliberately when the format intentionally
//! changes.

use std::process::Command;

fn explain(query: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_rgq"))
        .args(["--explain", query])
        .output()
        .expect("spawn rgq");
    assert!(out.status.success(), "--explain {query:?} should exit 0");
    String::from_utf8(out.stdout).expect("explain stdout is utf-8")
}

macro_rules! golden {
    ($name:ident, $query:expr, $file:expr) => {
        #[test]
        fn $name() {
            assert_eq!(
                explain($query),
                include_str!($file),
                "mismatch for query {:?}",
                $query
            );
        }
    };
}

golden!(
    cat_and_dog_or_bird,
    "(cat AND dog) OR bird",
    "golden/explain_cat_and_dog_or_bird.txt"
);
golden!(
    not_cat_or_dog,
    "NOT (cat OR dog)",
    "golden/explain_not_cat_or_dog.txt"
);
golden!(
    not_cage_and_bird,
    "NOT cage AND bird",
    "golden/explain_not_cage_and_bird.txt"
);
golden!(not_not_cat, "NOT NOT cat", "golden/explain_not_not_cat.txt");
golden!(
    cat_and_dog_or_bird_precedence,
    "cat AND dog OR bird",
    "golden/explain_cat_and_dog_or_bird_precedence.txt"
);
golden!(
    unsatisfiable,
    "cat AND NOT cat",
    "golden/explain_unsatisfiable.txt"
);
golden!(
    aorb_and_corc,
    "(a OR b) AND (c OR d)",
    "golden/explain_aorb_and_corc.txt"
);
golden!(
    quoted_and_or_cat,
    "\"AND\" OR cat",
    "golden/explain_quoted_and_or_cat.txt"
);
