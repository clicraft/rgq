//! Internal ASCII tree renderer (spec §10), replacing any dependency on the
//! external `tree` program.
//!
//! Two phases:
//! 1. **build a trie** keyed by path component — split each path on `/`, walking
//!    from the root and creating nodes as needed, so shared prefixes are reused;
//! 2. **render** depth-first with box-drawing characters, children in sorted
//!    order, the last child drawn with `└── ` and earlier ones with `├── `.
//!
//! Paths are bytes throughout (spec §2.2): a component may not be valid UTF-8, so
//! the renderer copies component bytes verbatim and only the box-drawing glyphs
//! are UTF-8. Input order doesn't matter — children are stored sorted.

use std::collections::BTreeMap;

/// A trie node: a path component maps to a subtree. `BTreeMap` keeps children in
/// sorted byte order, which makes rendering deterministic regardless of insertion
/// order.
#[derive(Default)]
struct Node {
    children: BTreeMap<Vec<u8>, Node>,
}

impl Node {
    fn insert(&mut self, path: &[u8]) {
        let mut node = self;
        for component in path.split(|&b| b == b'/').filter(|c| !c.is_empty()) {
            node = node.children.entry(component.to_vec()).or_default();
        }
    }
}

const TEE: &[u8] = "├── ".as_bytes(); // non-last child
const ELBOW: &[u8] = "└── ".as_bytes(); // last child
const PIPE: &[u8] = "│   ".as_bytes(); // ancestor line continues
const GAP: &[u8] = b"    "; // ancestor was the last child

/// Maximum path nesting depth the renderer descends into. Mirrors the parser's
/// `MAX_DEPTH` guard (`src/parser.rs`) against the same class of risk: a path's
/// component count comes from whatever tree `rgq` is pointed at (spec §10.3), so
/// it is attacker-influenceable, and unbounded recursion is a stack-overflow risk
/// (confirmed empirically: ~50,000 levels reliably overflows an 8 MiB stack).
/// 100 is far beyond any real directory tree (legitimate nesting rarely exceeds a
/// few dozen levels) but keeps recursion trivially cheap and safe regardless of
/// input. A path deeper than this is truncated, not silently dropped — see
/// `render_children`.
const MAX_DEPTH: usize = 100;

/// Render `paths` as an ASCII tree, returning raw bytes (UTF-8 glyphs plus
/// verbatim component bytes). A single `.` root line is printed above the tree.
pub fn render<'a, I>(paths: I) -> Vec<u8>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut root = Node::default();
    for path in paths {
        root.insert(path);
    }

    let mut out = Vec::new();
    out.extend_from_slice(b".\n");
    render_children(&root, b"", 0, &mut out);
    out
}

/// Depth-first, pre-order render. `depth` counts levels already descended;
/// reaching [`MAX_DEPTH`] truncates the remaining subtree with a visible marker
/// instead of continuing to recurse, so a pathologically deep path bounds the
/// work done (and is reported, not silently dropped) rather than risking a stack
/// overflow.
fn render_children(node: &Node, prefix: &[u8], depth: usize, out: &mut Vec<u8>) {
    if depth >= MAX_DEPTH {
        if !node.children.is_empty() {
            out.extend_from_slice(prefix);
            out.extend_from_slice(ELBOW);
            out.extend_from_slice(
                format!("... (truncated: nested past {MAX_DEPTH} levels)\n").as_bytes(),
            );
        }
        return;
    }

    let last_index = node.children.len().saturating_sub(1);
    for (i, (name, child)) in node.children.iter().enumerate() {
        let is_last = i == last_index;

        out.extend_from_slice(prefix);
        out.extend_from_slice(if is_last { ELBOW } else { TEE });
        out.extend_from_slice(name);
        out.push(b'\n');

        let mut child_prefix = prefix.to_vec();
        child_prefix.extend_from_slice(if is_last { GAP } else { PIPE });
        render_children(child, &child_prefix, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_strs(paths: &[&str]) -> String {
        let owned: Vec<Vec<u8>> = paths.iter().map(|s| s.as_bytes().to_vec()).collect();
        let bytes = render(owned.iter().map(Vec::as_slice));
        String::from_utf8(bytes).expect("ascii tree is valid utf-8 for utf-8 inputs")
    }

    /// The exact golden from spec §10.4.
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
    fn t1_matches_spec_10_4_golden() {
        let out = render_strs(&[
            "README.md",
            "src/a/main.py",
            "src/a/util.py",
            "src/b/test.py",
        ]);
        assert_eq!(out, GOLDEN);
    }

    #[test]
    fn t2_out_of_order_input_renders_sorted() {
        // Same paths, shuffled — must produce byte-identical output.
        let out = render_strs(&[
            "src/b/test.py",
            "src/a/util.py",
            "README.md",
            "src/a/main.py",
        ]);
        assert_eq!(out, GOLDEN);
    }

    #[test]
    fn t3_single_file() {
        assert_eq!(render_strs(&["a.txt"]), ".\n└── a.txt\n");
    }

    #[test]
    fn t4_single_deep_path() {
        assert_eq!(
            render_strs(&["x/y/z.txt"]),
            ".\n└── x\n    └── y\n        └── z.txt\n"
        );
    }

    #[test]
    fn t5_connectors_for_three_siblings() {
        assert_eq!(render_strs(&["a", "b", "c"]), ".\n├── a\n├── b\n└── c\n");
    }

    #[test]
    fn t6_empty_input_is_just_the_root() {
        let bytes = render(std::iter::empty());
        assert_eq!(bytes, b".\n");
    }

    /// Build a synthetic path with `n_dirs` directory components plus a trailing
    /// `leaf` file (so `n_dirs + 1` total path components). Small, fixed sizes
    /// only — this exists to probe the [`MAX_DEPTH`] boundary precisely, not to
    /// stress-test depth (see `t9` below for why that's the wrong instinct).
    fn deep_path(n_dirs: usize) -> Vec<u8> {
        let mut path = Vec::with_capacity(n_dirs * 4);
        for i in 0..n_dirs {
            path.extend_from_slice(format!("d{i}/").as_bytes());
        }
        path.extend_from_slice(b"leaf");
        path
    }

    /// Security: a path nested deeper than [`MAX_DEPTH`] total components is
    /// truncated with a visible marker rather than rendered in full or causing
    /// unbounded recursion.
    ///
    /// (A prior version of this test tried to validate the *absence* of a depth
    /// limit by rendering a path with millions of levels — that's exactly what
    /// the cap exists to prevent, and the test itself OOM-killed the host. Keep
    /// depths here small, fixed, and bounded near `MAX_DEPTH`, never huge.)
    #[test]
    fn t9_path_deeper_than_max_depth_is_truncated_not_unbounded() {
        // MAX_DEPTH dirs + 1 leaf = MAX_DEPTH + 1 total components: one past the
        // cap, so exactly MAX_DEPTH names render (d0..d{MAX_DEPTH-1}) and the
        // leaf is cut.
        let path = deep_path(MAX_DEPTH);
        let text = String::from_utf8(render(std::iter::once(path.as_slice()))).unwrap();

        assert!(text.starts_with(".\n"));
        assert!(
            text.contains("truncated"),
            "expected a visible truncation marker, got: {text}"
        );
        assert!(
            text.contains(&format!("d{}", MAX_DEPTH - 1)),
            "last component within the cap must render"
        );
        assert!(
            !text.contains(&format!("d{MAX_DEPTH}")),
            "the first component past the cap must not render"
        );
        assert!(!text.contains("leaf"), "must not render past the depth cap");
    }

    #[test]
    fn max_depth_exactly_at_the_cap_renders_in_full_no_truncation() {
        // (MAX_DEPTH - 1) dirs + 1 leaf = exactly MAX_DEPTH total components.
        // Must render completely, with no truncation marker — the cap must not
        // be off-by-one and clip legitimate, if unusually deep, real trees.
        let path = deep_path(MAX_DEPTH - 1);
        let text = String::from_utf8(render(std::iter::once(path.as_slice()))).unwrap();

        assert!(
            !text.contains("truncated"),
            "exactly-at-cap must not truncate; got: {text}"
        );
        assert!(text.ends_with("leaf\n"));
    }

    #[test]
    fn t7_non_utf8_component_does_not_panic() {
        // Path "dir/inv\xFF.bin" — the 0xFF byte is not valid UTF-8.
        let path: &[u8] = b"dir/inv\xFF.bin";
        let bytes = render(std::iter::once(path));
        // Structure is intact and the invalid byte survived verbatim.
        assert!(bytes.starts_with(b".\n"));
        assert!(bytes.contains(&0xFF));
    }

    #[test]
    fn t8_component_with_space() {
        assert_eq!(
            render_strs(&["my dir/file.txt"]),
            ".\n└── my dir\n    └── file.txt\n"
        );
    }
}
