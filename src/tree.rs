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
    render_children(&root, b"", &mut out);
    out
}

fn render_children(node: &Node, prefix: &[u8], out: &mut Vec<u8>) {
    let last_index = node.children.len().saturating_sub(1);
    for (i, (name, child)) in node.children.iter().enumerate() {
        let is_last = i == last_index;

        out.extend_from_slice(prefix);
        out.extend_from_slice(if is_last { ELBOW } else { TEE });
        out.extend_from_slice(name);
        out.push(b'\n');

        let mut child_prefix = prefix.to_vec();
        child_prefix.extend_from_slice(if is_last { GAP } else { PIPE });
        render_children(child, &child_prefix, out);
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
