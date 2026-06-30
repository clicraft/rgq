# `rgq` — Test Design

The companion to [`PLAN.md`](./PLAN.md). The build spec's §13 lists *what* must be
tested; this document is the concrete *design* of the suites: the layers, the exact
inputs and expected outputs (hand-computed and validated against real `rg 14.1.0`), the
fixtures, the helpers, and which milestone lands each suite.

Correctness is defined by spec §5 (every query denotes a set of file paths). Every test
below exists to pin some part of that definition, or to pin a behavior the engine relies
on.

---

## 0. Layers & tooling

| Layer | Where | What it covers | Speed |
|-------|-------|----------------|-------|
| **Unit** | `#[cfg(test)]` in each module | pure logic: lexer, parser, normalize, tree render, rg-argv builder, batching, cli classification | fast, no I/O |
| **Property** | `proptest` (dev-dep) | invariants over *random* inputs — the normalizer equivalence keystone, tree order-independence, batching invariants | fast |
| **Golden** | `tests/` | exact `--explain` text (§13.2), exact tree output (§10.4) | fast |
| **E2E / black-box** | `tests/` | the real compiled binary driving real `rg` over real fixture trees | slower, spawns processes |

**Tooling decisions (keep deps minimal):**
- Drive the binary with `std::process::Command` + `env!("CARGO_BIN_EXE_rgq")` — already
  in use for the M1 CLI tests, no `assert_cmd` needed.
- `tempfile` (dev-dep, lands M4) builds fixture trees in a temp dir.
- `proptest` (dev-dep, lands M3) for the property suites.
- Golden comparisons via `include_str!` against checked-in `.txt` files (no snapshot
  crate); update deliberately, never auto-bless in CI.

**Testability hooks (small production seams, decided here so tests are deterministic):**
- `RGQ_RG` env var overrides the `rg` binary path → lets the "rg not found" / "rg fails"
  tests point at a missing or fake binary.
- `RGQ_ARG_MAX` env var overrides the argv-size budget → forces the batching path (§8.2)
  with a tiny, deterministic threshold instead of needing millions of real files.
- `--max-clauses N` (and a sane default) → makes the DNF-blow-up guard testable and keeps
  it from OOMing (PLAN.md M3 deviation).
- The batching splitter is a **pure function** `batches(paths, budget)` so it is unit-
  tested with an injected budget, independent of `rg`.

---

## 1. Unit suites

### 1.1 Lexer (`lexer.rs`) — lands M2

Each row is `input → tokens` or `→ Err`. `T(x)` = term, keywords are `AND/OR/NOT`.

| # | Input | Expected | Pins |
|---|-------|----------|------|
| L1 | `cat AND dog` | `T(cat) AND T(dog)` | barewords + keyword |
| L2 | `and And AND` | `AND AND AND` | keyword case-insensitivity (§4.1) |
| L3 | `andy ANDES` | `T(andy) T(andes)` | keyword match must be *exact* word |
| L4 | `"AND" OR cat` | `T(AND) OR T(cat)` | quoted keyword is a term (§4.1) |
| L5 | `'cat dog'` | `T(cat dog)` | quoted term keeps internal whitespace |
| L6 | `(cat)` | `( T(cat) )` | paren adjacency splits tokens |
| L7 | `cat)AND(dog` | `T(cat) ) AND ( T(dog)` | parens break barewords, no whitespace needed |
| L8 | `'he said "hi"'` | `T(he said "hi")` | inner quotes literal inside other quote |
| L9 | `foo"bar"` | `T(foo"bar")` | a quote mid-bareword is a literal char (pinned rule) |
| L10 | `a.*b[0-9]` | `T(a.*b[0-9])` | regex metachars are opaque term bytes (regex-default) |
| L11 | `-foo` | `T(-foo)` | leading dash is a normal term to the lexer |
| L12 | `caté` / non-ASCII | `T(caté)` | non-ASCII bytes pass through |
| L13 | `"cat` | `Err(unterminated quote)` | §4.1 |
| L14 | `''` or `""` | `Err(empty term)` | never emit an empty pattern (pinned rule) |
| L15 | `   ` (ws only) | `Err`/empty stream | whitespace insignificant |

### 1.2 Parser (`parser.rs`) — lands M2

`input → AST` (shown parenthesized) or `→ Err`. Precedence is `NOT > AND > OR` (§4.2).

| # | Input | Expected AST | Pins |
|---|-------|--------------|------|
| P1 | `a AND b OR c` | `(a AND b) OR c` | AND binds tighter than OR |
| P2 | `a OR b AND c` | `a OR (b AND c)` | same, other order |
| P3 | `NOT a AND b` | `(NOT a) AND b` | NOT binds tightest |
| P4 | `NOT (a OR b)` | `NOT (a OR b)` | paren overrides; NOT wraps a compound |
| P5 | `(a OR b) AND c` | `(a OR b) AND c` | paren overrides precedence |
| P6 | `a AND b AND c` | n-ary `AND(a,b,c)` after flatten | §4.2 flattening |
| P7 | `NOT NOT cat` | `NOT (NOT cat)` (collapses in NNF) | double NOT parses |
| P8 | `((((a))))` | `a` | deep nesting within limit |
| P9 | `cat dog` | `Err(adjacency / no implicit AND)` | §4.2 — leftover-token check |
| P10 | `cat AND` | `Err(dangling operator)` | §12 |
| P11 | `AND cat` | `Err(leading operator)` | §12 |
| P12 | `a AND AND b` | `Err(operator where term expected)` | §12 |
| P13 | `(cat` / `cat)` | `Err(unbalanced paren)` | §12 |
| P14 | `()` | `Err(empty parens)` | §12 |
| P15 | `NOT` | `Err(dangling NOT)` | §12 |
| P16 | `(((((((…)))))))` past depth cap | `Err(too deeply nested)` | recursion-depth guard (no stack overflow) |

### 1.3 Normalize (`normalize.rs`) — lands M3

**NNF** (`input → NNF`):

| # | Input | Expected | Pins |
|---|-------|----------|------|
| N1 | `NOT (a AND b)` | `(NOT a) OR (NOT b)` | De Morgan |
| N2 | `NOT (a OR b)` | `(NOT a) AND (NOT b)` | De Morgan |
| N3 | `NOT NOT a` | `a` | double-negation elimination |
| N4 | `NOT NOT NOT a` | `NOT a` | odd negations collapse |
| N5 | `NOT a` | `NOT a` | literal unchanged |
| N6 | `NOT (a AND NOT b)` | `(NOT a) OR b` | nested De Morgan + double-neg |

**DNF + cleaning** (`input → clause list`):

| # | Input | Expected clauses | Pins |
|---|-------|------------------|------|
| D1 | `a AND (b OR c)` | `{a,b}`, `{a,c}` (2) | distribution |
| D2 | `(a OR b) AND (c OR d)` | 4 clauses | §13.1 count |
| D3 | `(a OR b) AND (c OR d) AND (e OR f)` | 8 clauses | blow-up is expected, not a bug |
| D4 | `a AND a` | `{a}` | literal dedup within clause |
| D5 | `(a) OR (a)` | `{a}` | whole-clause dedup |
| D6 | `a AND NOT a` | `[]` (dropped) → unsatisfiable | contradiction drop (§6.3) |
| D7 | `a OR NOT a` | `{a}`, `{NOT a}` (kept) | tautology is **not** a contradiction |
| D8 | `NOT (a OR b) OR c` | `{NOT a, NOT b}`, `{c}` | NNF then DNF end-to-end |

**Property test (the keystone, `proptest`):** generate a random AST over a small term
alphabet (e.g. `{a,b,c,d}`); assert the normalized clause list is (a) **structurally** a
flat OR-of-ANDs-of-literals, and (b) **semantically equal** to the source AST by
brute-forcing the truth table over the distinct terms (2ᵏ assignments, k≤4). This is what
actually proves "correct for arbitrary nesting" (§1) — example rows D1–D8 cannot.

### 1.4 Tree (`tree.rs`) — lands M5

| # | Input paths | Expected | Pins |
|---|-------------|----------|------|
| T1 | `README.md`, `src/a/main.py`, `src/a/util.py`, `src/b/test.py` | the exact §10.4 block | the golden (§13) |
| T2 | same, shuffled order | identical to T1 | out-of-order renders sorted (§13.3) |
| T3 | `a.txt` only | `.`\n`└── a.txt` | single file |
| T4 | `x/y/z/deep.txt` | nested chain, all `└──` | single deep path |
| T5 | `a`, `b`, `c` | three children, last is `└──` | last-vs-earlier connector (§10.2) |
| T6 | empty set | `.` only | define + pin the empty case |
| T7 | path with non-UTF-8 byte component | renders (lossy), **no panic**, structure intact | byte-orientation (§2.2) |
| T8 | component containing a space | space preserved | no surprise splitting |

Property test: inserting the path set in any permutation yields byte-identical output
(render is order-independent because it sorts — §10.1).

### 1.5 rg argv builder + batching (`rg.rs`) — lands M4

**Argv builder** — assert the exact `Vec<OsString>` for representative combos. Invariants
every call must satisfy: contains `--null`; the pattern is introduced with `-e`; paths
follow a `--` end-of-options marker (§8.3).

| # | Mode + flags | Must contain | Pins |
|---|--------------|--------------|------|
| A1 | list-with-match, `-i -w` | `-l --null -i -w -e PAT --` | match-flag mapping |
| A2 | list-without-match (negative) | `--files-without-match --null … -e PAT --` | one-call negatives (§8.1) |
| A3 | universe, `-t py -t md -g '*.x'` | `--files --null -t py -t md -g *.x --` | scope-flag mapping |
| A4 | no-ignore=2 | `--no-ignore --hidden` | `-uu` → explicit rg flags |
| A5 | term `-rf`, path `-weird` | pattern after `-e`, path after `--` | leading-dash guard |

**Batching `batches(paths, budget)`** (pure):

| # | Case | Expected | Pins |
|---|------|----------|------|
| B1 | all paths fit budget | one batch | §8.2 |
| B2 | total exceeds budget | ≥2 batches, each within budget | split correctness |
| B3 | one path alone > budget | that path in a singleton batch (progress guaranteed) | never stall |
| B4 | union of all batches | == input set, no loss / no dup | batching can't change results |
| B5 | empty input | zero batches | caller short-circuits, never spawns `rg` (hard rule 1) |

Property test: for random `(paths, budget)`, `flatten(batches) == paths` and no batch is
empty and (when >1 path) no batch overflows except a forced singleton.

### 1.6 CLI classification (`cli.rs`) — **landed in M1** (15 tests)

`-uu` ⇒ `no_ignore=2` ∧ `hidden`; `-t`/`-g` accumulate in order; output-mode resolution;
`--explain`/`-n`; multi-word query joins with spaces; empty/whitespace query errors;
`--tree`/`--print0` conflict. (See `src/cli.rs::tests`.)

---

## 2. E2E / black-box suites — land M4 (engine), M5 (tree), M6 (hardening)

These spawn the real `rgq` binary against a fixture tree and assert the **exact** path set
(spec §13.3). The set semantics (§5) are computed by hand and were validated against real
`rg 14.1.0` while writing this plan.

### 2.1 The canonical fixture

Built in a temp dir by a helper (§3). It uses a **`.ignore` file, not `.gitignore`, and is
*not* a git repo** — deliberately: `rg` honors `.ignore` without git, so we get the
ignore/`-u` behavior with **no `.git/` directory to pollute `--hidden` results** (verified:
in a git repo, `--hidden cat` leaks `.git/hooks/*.sample`).

| Path | Contents | Note |
|------|----------|------|
| `.ignore` | `*.log` | hidden ignore-file |
| `a.txt` | `cat dog` | |
| `b.txt` | `cat` | |
| `c.txt` | `dog` | |
| `d.txt` | `bird` | |
| `e.txt` | `bird cage` | |
| `sub/f.txt` | `cat bird` | |
| `sub/g.md` | `TODO FIXME` | only `.md` |
| `pkg/h.py` | `import os` | |
| `pkg/i.py` | `import __future__` | |
| `.secret.txt` | `cat` | **hidden** |
| `build.log` | `cat` | **ignored** by `.ignore` |

Default universe `U` (no scope flags) = `{a, b, c, d, e, sub/f, sub/g.md, pkg/h.py,
pkg/i.py}` — `.ignore`, `.secret.txt` (hidden) and `build.log` (ignored) are excluded.

### 2.2 Query → expected set (default scope) — validated

| Query | Expected set |
|-------|--------------|
| `cat` | `a.txt, b.txt, sub/f.txt` |
| `dog` | `a.txt, c.txt` |
| `bird` | `d.txt, e.txt, sub/f.txt` |
| `cat AND dog` | `a.txt` |
| `cat OR bird` | `a.txt, b.txt, d.txt, e.txt, sub/f.txt` |
| `cat AND NOT dog` | `b.txt, sub/f.txt` |
| `NOT cat` | `c.txt, d.txt, e.txt, sub/g.md, pkg/h.py, pkg/i.py` |
| `(cat AND dog) OR bird` | `a.txt, d.txt, e.txt, sub/f.txt` |
| `NOT (cat OR dog)` | `d.txt, e.txt, sub/g.md, pkg/h.py, pkg/i.py` |
| `bird AND NOT cage` | `d.txt, sub/f.txt` |
| `"AND" OR cat` | `a.txt, b.txt, sub/f.txt` (no file contains literal "AND") |
| `cat AND NOT cat` | ∅ — unsatisfiable, stderr info, **exit 0** |

### 2.3 Query → expected set (scope flags) — validated

| Invocation | Expected set |
|------------|--------------|
| `--hidden 'cat'` | `a.txt, b.txt, sub/f.txt, .secret.txt` |
| `-u 'cat'` | `a.txt, b.txt, sub/f.txt, build.log` |
| `-uu 'cat'` | `a.txt, b.txt, sub/f.txt, build.log, .secret.txt` |
| `-t py 'import'` | `pkg/h.py, pkg/i.py` |
| `-t py 'import AND NOT __future__'` | `pkg/h.py` |
| `-g '*.md' 'TODO'` | `sub/g.md` |
| `-t txt 'NOT bird'` | `.secret.txt, a.txt, b.txt, c.txt` † |

† A `-t txt` type filter overrides ripgrep's default hidden-exclusion for matching
dotfiles, so `.secret.txt` is in the `-t txt` universe (verified). `rgq` mirrors `rg`'s
universe exactly (§7), so `NOT bird` keeps it; the scope-consistency test confirms it.

**Scope-consistency assertion (spec §7):** for any term `t`, the result of `NOT t` must
equal `(rg --files under the same scope) \ (result of t under the same scope)`. The test
computes both sides independently and asserts equality — this is the single check that
catches universe/search drift.

### 2.4 Output modes (black-box)

| Mode | Assertion |
|------|-----------|
| default list | sorted, `\n`-separated, exact set |
| `--print0` | `\0`-separated, exact byte framing, parses back to the same set |
| `--tree` | exact box-drawing for a fixture subset |
| `--explain` | golden text **and** does not execute (point `RGQ_RG` at a fake that records calls → 0 calls) |

### 2.5 Exit codes (black-box, spec §12)

| Scenario | Exit | stderr/stdout |
|----------|------|---------------|
| matches found | 0 | sets on stdout |
| zero matches | 0 | empty stdout |
| unsatisfiable (`cat AND NOT cat`) | 0 | info on stderr, empty stdout |
| parse error (`cat dog`, `cat AND`, `(cat`, `"cat`) | 2 | message naming the problem |
| unknown flag | 2 | hint to quote it |
| `rg` not found (`RGQ_RG=/no/such`) | non-zero | clear "rg not found" |
| bad regex term (`'('`) | non-zero | surfaced rg error naming the term |
| positive-free clause (`NOT cat`) | 0 | **warning** on stderr, result still correct |

### 2.6 Real-usage scenarios ("like real usage")

- **Pipe to xargs:** run `rgq -0 cat` and feed the bytes to `xargs -0 wc -l`; assert it
  consumes the NUL stream without error (the `--print0` contract, §3.3).
- **Run from a subdirectory:** invoke with cwd = `sub/`; results are relative to cwd.
- **Multi-arg == single-arg:** `rgq cat AND dog` and `rgq 'cat AND dog'` give identical
  output.
- **Quoted keyword in anger:** `rgq '"AND" OR cat'` does not treat `AND` as an operator.

---

## 3. Fixtures & helpers (turnkey)

A single test-support module both e2e files share. Builds the §2.1 tree and runs the
binary.

```rust
// tests/support/mod.rs   (include via `mod support;`)
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

/// Build the canonical fixture in a fresh temp dir. NOT a git repo; uses `.ignore`.
pub fn fixture() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path();
    write(p, ".ignore", "*.log\n");
    write(p, "a.txt", "cat dog\n");
    write(p, "b.txt", "cat\n");
    write(p, "c.txt", "dog\n");
    write(p, "d.txt", "bird\n");
    write(p, "e.txt", "bird cage\n");
    write(p, "sub/f.txt", "cat bird\n");
    write(p, "sub/g.md", "TODO FIXME\n");
    write(p, "pkg/h.py", "import os\n");
    write(p, "pkg/i.py", "import __future__\n");
    write(p, ".secret.txt", "cat\n");
    write(p, "build.log", "cat\n");
    dir
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() { fs::create_dir_all(parent).unwrap(); }
    fs::write(path, body).unwrap();
}

/// Run rgq with cwd at the fixture root.
pub fn run_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rgq"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn rgq")
}

/// stdout (newline mode) → sorted Vec for set comparison.
pub fn lines(out: &Output) -> Vec<String> {
    let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines().map(str::to_owned).collect();
    v.sort();
    v
}
```

Edge-case helpers (lands M6):
- **non-UTF-8 / newline filenames:** create with `std::os::unix::ffi::OsStrExt` from raw
  bytes (e.g. `b"inv\xFFname.txt"`, `b"two\nlines.txt"`); assert `--print0` round-trips the
  exact bytes and the tree renderer does not panic.
- **ARG_MAX:** create N small matching files and run with `RGQ_ARG_MAX=<tiny>` to force the
  batching path; assert the result equals the un-batched result for the same query.

---

## 4. Edge-case catalogue (and the suite that owns each)

| # | Edge case | Risk | Owned by |
|---|-----------|------|----------|
| E1 | non-UTF-8 path bytes | corruption / panic | §3 helper + e2e (M6) |
| E2 | newline in filename | only `--print0` is correct (default list is unsafe) | e2e print0 (M6) |
| E3 | empty candidate set narrows to ∅ | **sharpest bug**: rg with 0 paths scans cwd → whole tree | e2e `zzz AND cat` ⇒ ∅; unit B5 (M4) |
| E4 | leading-dash term / file named like a flag | option misparse | argv unit A5 + e2e (M6) |
| E5 | quoted keyword `"AND"` | operator vs literal | lexer L4 + e2e 2.6 |
| E6 | regex metachar term vs `-F` | `a.c` matches `abc`; `-F 'a.c'` literal | lexer L10 + e2e |
| E7 | bad regex term `'('` | must surface rg error clearly | e2e 2.5 |
| E8 | DNF blow-up beyond `--max-clauses` | OOM / hang | e2e: exit 2 + message, bounded time (M3/M6) |
| E9 | deep nesting `((((…))))` | stack overflow | parser P16 (M2) |
| E10 | positive-free clause `NOT cat` | expensive full scan, must warn | e2e 2.5 (M4) |
| E11 | tautology `cat OR NOT cat` | must return all of `U` | normalize D7 + e2e |
| E12 | unsatisfiable `cat AND NOT cat` | exit 0, info, empty | normalize D6 + e2e 2.5 |
| E13 | dedup `cat AND cat`, `cat OR cat` | redundant work / wrong count | normalize D4/D5 |
| E14 | Unicode case-fold under `-i` | document rg's behavior, no panic | e2e (M6, verify vs pinned rg) |
| E15 | many `-t`/`-g`, very long term | arg handling | argv A3 + e2e |
| E16 | binary files / symlinks | rg defaults (skip binary, no-follow) | documented; light e2e |

---

## 5. CI & invariants

- `cargo test` runs unit + golden + property + e2e. Heavy tests (ARG_MAX large-tree, DNF
  blow-up) are `#[ignore]`d by default and run via `cargo test -- --include-ignored` in a
  dedicated CI job so the default run stays fast.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` gate merges.
- **rg is a pinned dependency of the e2e layer.** Its behaviors (verified in PLAN.md §0)
  are load-bearing; re-run the spikes and this suite when bumping the supported `rg`.
- Set comparisons sort first (order-independent); the default-output test *additionally*
  asserts the stream is already sorted (spec §9).

---

## 6. Spec coverage map (§13 / §15 → suite)

| Spec requirement | Suite |
|------------------|-------|
| §13.1 lexer | §1.1 (M2) |
| §13.1 parser | §1.2 (M2) |
| §13.1 NNF/DNF | §1.3 (M3) |
| §13.2 golden `--explain` (8 queries) | Golden (M3) |
| §13.3 integration AND/OR/NOT/nested | §2.2 (M4) |
| §13.3 scope-flag consistency | §2.3 + scope-consistency check (M4) |
| §13.3 `--print0` framing | §2.4 (M6) |
| §13.3 tree golden + out-of-order | §1.4 T1/T2 + Golden (M5) |
| §13.3 ARG_MAX batching | §1.5 B1–B5 + e2e ARG_MAX (M4) |
| §15 byte/NUL/newline-safe paths | E1/E2 (M6) |
| §15 leading-dash, quoted keywords | E4/E5 |
| §15 single self-contained binary, rg-only dep | e2e harness (no other runtime dep) |
