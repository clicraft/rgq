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
    let root = build_trie(paths);
    let mut out: Vec<u8> = Vec::new();
    out.write_bytes(b".\n");
    render_children(&root, b"", 0, &mut out);
    out
}

fn build_trie<'a, I>(paths: I) -> Node
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut root = Node::default();
    for path in paths {
        root.insert(path);
    }
    root
}

/// Where rendered bytes go. Two implementations: [`Vec<u8>`] (the real output
/// buffer) and [`ByteCounter`] (just tallies lengths). `render_children` is
/// written once, generic over this trait, and used for *both* — so
/// [`estimate_memory_bytes`]'s prediction of the output size can never silently
/// drift from what `render` actually produces; they are, by construction, the
/// same code path.
trait Sink {
    fn write_bytes(&mut self, bytes: &[u8]);
}

impl Sink for Vec<u8> {
    fn write_bytes(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

/// A [`Sink`] that only counts bytes, allocating nothing for the content
/// itself. Used to predict `render`'s output size without paying for it.
#[derive(Default)]
struct ByteCounter(u64);

impl Sink for ByteCounter {
    fn write_bytes(&mut self, bytes: &[u8]) {
        self.0 += bytes.len() as u64;
    }
}

/// Depth-first, pre-order render. `depth` counts levels already descended;
/// reaching [`MAX_DEPTH`] truncates the remaining subtree with a visible marker
/// instead of continuing to recurse, so a pathologically deep path bounds the
/// work done (and is reported, not silently dropped) rather than risking a stack
/// overflow. Cloning `prefix` per level is safe now that depth is capped at
/// [`MAX_DEPTH`] (at most ~400 bytes per clone, ~100 levels — see SECURITY.md
/// for why this used to matter a great deal more).
fn render_children<S: Sink>(node: &Node, prefix: &[u8], depth: usize, out: &mut S) {
    if depth >= MAX_DEPTH {
        if !node.children.is_empty() {
            out.write_bytes(prefix);
            out.write_bytes(ELBOW);
            out.write_bytes(
                format!("... (truncated: nested past {MAX_DEPTH} levels)\n").as_bytes(),
            );
        }
        return;
    }

    let last_index = node.children.len().saturating_sub(1);
    for (i, (name, child)) in node.children.iter().enumerate() {
        let is_last = i == last_index;

        out.write_bytes(prefix);
        out.write_bytes(if is_last { ELBOW } else { TEE });
        out.write_bytes(name);
        out.write_bytes(b"\n");

        let mut child_prefix = prefix.to_vec();
        child_prefix.extend_from_slice(if is_last { GAP } else { PIPE });
        render_children(child, &child_prefix, depth + 1, out);
    }
}

/// Conservative, rough per-trie-node memory overhead: the `Vec<u8>` key
/// struct, the `Node`/`BTreeMap` value struct, and B-tree bookkeeping /
/// allocator padding. Not a precise measurement — deliberately generous, so
/// the safety check ([`crate::membudget`]) errs toward refusing rather than
/// under-predicting.
const TRIE_NODE_OVERHEAD_BYTES: u64 = 128;

/// Predicted peak memory [`render`] would need for `paths`: the trie's own
/// footprint (kept alive for the whole call) plus the rendered output buffer.
/// Cheap and safe to compute — bounded by the same [`MAX_DEPTH`] recursion as
/// `render` itself — so it can run *before* deciding whether to render at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryEstimate {
    /// Exact predicted length of `render`'s output: computed by running the
    /// identical traversal against a [`ByteCounter`] sink instead of an
    /// accumulating buffer, so this is provably equal to `render(paths).len()`,
    /// not an approximation.
    pub output_bytes: u64,
    /// Rough estimate of the trie's own memory footprint while rendering.
    pub trie_bytes: u64,
}

impl MemoryEstimate {
    pub fn total(&self) -> u64 {
        self.output_bytes.saturating_add(self.trie_bytes)
    }
}

/// Estimate the memory `render(paths)` would need, without doing the
/// (potentially large) work of actually rendering.
pub fn estimate_memory_bytes<'a, I>(paths: I) -> MemoryEstimate
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let root = build_trie(paths);

    let mut counter = ByteCounter::default();
    counter.write_bytes(b".\n");
    render_children(&root, b"", 0, &mut counter);

    let (node_count, component_bytes) = trie_footprint(&root, 0);

    MemoryEstimate {
        output_bytes: counter.0,
        trie_bytes: node_count
            .saturating_mul(TRIE_NODE_OVERHEAD_BYTES)
            .saturating_add(component_bytes),
    }
}

/// Bounded recursive walk (same [`MAX_DEPTH`] guard as `render_children`)
/// tallying the trie's real, deduplicated `(node_count, component_bytes)`.
fn trie_footprint(node: &Node, depth: usize) -> (u64, u64) {
    if depth >= MAX_DEPTH {
        return (0, 0);
    }
    let mut nodes = 0u64;
    let mut bytes = 0u64;
    for (name, child) in &node.children {
        nodes += 1;
        bytes += name.len() as u64;
        let (n, b) = trie_footprint(child, depth + 1);
        nodes += n;
        bytes += b;
    }
    (nodes, bytes)
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

    // ---- estimate_memory_bytes: the predictive memory-safety check ----

    #[test]
    fn estimate_output_bytes_exactly_matches_real_render_length() {
        let paths: Vec<Vec<u8>> = [
            "README.md",
            "src/a/main.py",
            "src/a/util.py",
            "src/b/test.py",
        ]
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect();
        let rendered = render(paths.iter().map(Vec::as_slice));
        let estimate = estimate_memory_bytes(paths.iter().map(Vec::as_slice));
        assert_eq!(
            estimate.output_bytes,
            rendered.len() as u64,
            "the estimate must be exact, not approximate — it's the same traversal"
        );
    }

    #[test]
    fn estimate_output_bytes_exactly_matches_with_truncation() {
        // Same property, but exercising the MAX_DEPTH truncation-marker branch.
        let path = deep_path(MAX_DEPTH + 5);
        let rendered = render(std::iter::once(path.as_slice()));
        let estimate = estimate_memory_bytes(std::iter::once(path.as_slice()));
        assert_eq!(estimate.output_bytes, rendered.len() as u64);
    }

    #[test]
    fn estimate_is_exact_for_shuffled_and_shared_prefix_inputs() {
        // A case with real prefix sharing (deduplication matters): two paths
        // share "src/a/".
        let paths: Vec<Vec<u8>> = ["src/a/one.txt", "src/a/two.txt", "src/b.txt"]
            .iter()
            .map(|s| s.as_bytes().to_vec())
            .collect();
        let rendered = render(paths.iter().map(Vec::as_slice));
        let estimate = estimate_memory_bytes(paths.iter().map(Vec::as_slice));
        assert_eq!(estimate.output_bytes, rendered.len() as u64);
    }

    #[test]
    fn estimate_trie_bytes_is_positive_for_nonempty_input_and_total_sums_correctly() {
        let estimate = estimate_memory_bytes(std::iter::once(b"a/b/c.txt".as_slice()));
        assert!(
            estimate.trie_bytes > 0,
            "a non-empty trie must have a positive footprint estimate"
        );
        assert_eq!(
            estimate.total(),
            estimate.output_bytes + estimate.trie_bytes
        );
    }

    #[test]
    fn estimate_for_empty_input_is_just_the_root_line() {
        let estimate = estimate_memory_bytes(std::iter::empty());
        assert_eq!(estimate.output_bytes, 2); // ".\n"
        assert_eq!(estimate.trie_bytes, 0);
    }

    #[test]
    fn estimate_grows_with_more_distinct_unshared_paths() {
        // More files with no shared structure must predict more memory than
        // fewer — a basic monotonicity sanity check on the estimator.
        let few = estimate_memory_bytes(std::iter::once(b"a.txt".as_slice()));
        let many: Vec<Vec<u8>> = (0..50)
            .map(|i| format!("file{i}.txt").into_bytes())
            .collect();
        let many_estimate = estimate_memory_bytes(many.iter().map(Vec::as_slice));
        assert!(many_estimate.total() > few.total());
    }
}
