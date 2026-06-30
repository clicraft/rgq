# `rgq` — Implementation Plan

Implementation plan derived from [`desing_v0.1.0.md`](./desing_v0.1.0.md) (the build
specification). The spec is the source of truth for **what** to build; this document
is the source of truth for **how and in what order** to build it. Section references
(§) point back to the spec.

Status: **M4 done** (engine: rg wrapper, per-clause narrowing, batching, list +
print0 output, scope/match flag propagation; 26 black-box e2e tests incl. scope
consistency, empty-candidate short-circuit, ARG_MAX batching, rg-not-found; 123 tests
green). Next up: M5 (tree renderer + `--tree`).

---

## 0. Decisions taken up front

These resolve the spec's recommendations into concrete choices. Deviations from the
spec are flagged with **⚠ DEVIATION**.

| Area | Decision | Rationale |
|------|----------|-----------|
| Language / edition | Rust 2021, MSRV pinned to the toolchain in CI | Match spec §1 |
| CLI parser | `clap` v4, derive style | Spec §2.4 |
| Errors | `anyhow` in `main`/binary boundary; `thiserror` for typed `lexer`/`parser` errors | Spec §2.4 |
| Path type | `Vec<u8>` byte strings; sets as `BTreeSet<Vec<u8>>` | Spec §2.2 — ordered, dedup'd, deterministic |
| `rg` discovery | Resolve `rg` from `PATH`; allow override via `RGQ_RG` env var | Testability + clear error (§12) |
| Concurrency | **v1 ships single-threaded**; clause-level parallelism is milestone 7 (optional) behind a flag/feature | Spec §11 says measure first; keep v1 small |
| Term-frequency reordering | **Not in v1.** Preserve author's term order. | Spec §11 — optional optimization |
| Batching | Per-batch budget from a probed `ARG_MAX` (fallback 128 KiB), **minus** current env size and per-arg overhead; always ≥1 path per batch | Spec §8.2 |
| Term semantics | Terms are **regexes by default** (`rg` matches them); `-F` makes them literal. A malformed-regex term is surfaced as a clear per-term error, never auto-escaped | Spec §1/§3.2 |
| Search root | Pass an explicit `.` to seed/universe calls; never invoke `rg` with zero path args (it scans cwd ambiguously — spike 1) | Spike |
| no-ignore mapping | rgq `-u`/`--no-ignore` → rg `--no-ignore`; rgq `-uu` → rg `--no-ignore --hidden`. Emit explicit rg flags, don't forward the raw `-u` count | Spec §3.2 |

### `rg` invocation cheat-sheet (the only ripgrep modes we use)

| Purpose | ripgrep flags | Notes |
|---------|---------------|-------|
| List files **with** a match | `rg -l --null <scope> <match> -e PATTERN -- [PATHS]` | seed + positive narrowing (§8.1) |
| List files **without** a match | `rg --files-without-match --null <scope> <match> -e PATTERN -- [PATHS]` | negative literal in one call (§8.1.3) |
| List files (universe `U`) | `rg --files --null <scope> -- [ROOT]` | seed for positive-free clauses (§7, §8.1) |

`--null` (NUL-separated output) is mandatory everywhere (§2.2). `-e PATTERN` keeps a
leading-dash pattern from being read as a flag; `--` separates paths (§8.3).

**Two hard rules learned from the spike, both correctness-critical:**

1. **Never invoke `rg` with zero path arguments during narrowing.** With no paths, `rg`
   silently scans the whole cwd. So: if the candidate set is empty, short-circuit the
   clause to ∅ — do **not** spawn `rg`. Batching must likewise never emit an empty batch.
2. **Derive the result set from parsed stdout, not the exit code.** Exit codes are
   inconsistent across modes: `-l` exits 1 on no match, but `--files-without-match`
   exits 0 even when it lists nothing. Policy: exit 0/1 = ran fine (use stdout); exit
   ≥2 = real error → surface `rg`'s stderr and propagate (§12).

### Verified ripgrep behaviors (spike, rg 14.1.0)

Checked empirically against the installed `rg`; the engine design depends on them.
Re-verify if the pinned `rg` version changes.

| # | Probe | Result | Consequence for `rgq` |
|---|-------|--------|------------------------|
| 1 | `rg -l -e PAT` with no path args | scans cwd | empty-candidate short-circuit (hard rule 1) |
| 2 | `rg -l --null` framing | NUL **terminates** each path (`p1\0p2\0`) | split on `0x00`, drop trailing empty |
| 3 | `-t`/`-g` with explicit file paths | filter **still applies** | re-applying scope flags while narrowing is safe & idempotent (satisfies §7) |
| 4 | ignore rules with explicit file paths | **not** applied (explicit path overrides ignore) | fine — candidates are already in-scope `U`, so never an ignored file |
| 5 | `--files-without-match` | lists given files that don't match; exit 0 even when empty | negatives in one call; rely on stdout (hard rule 2) |
| 6 | bad-regex term (e.g. `(`) | exit 2, error on stderr | terms are regexes by default; map compile errors to a clear per-term message |

(`.gitignore` is honored only inside a real git repo; `.ignore`/`.rgignore` always.
Irrelevant to correctness since `U` and all searches share scope, but explains why a
`target/` entry can appear when testing outside a repo.)

---

## 1. Repository layout

```
rgq/
├── Cargo.toml
├── README.md                 # user-facing; examples mirror spec §3.3
├── PLAN.md                   # this file
├── desing_v0.1.0.md          # the spec (source of truth)
├── .gitignore                # /target
├── src/
│   ├── main.rs               # thin: parse args → dispatch → map errors to exit codes
│   ├── cli.rs                # arg parsing, flag classification (match vs scope), dispatch
│   ├── lexer.rs              # query string → tokens
│   ├── parser.rs             # tokens → AST (precedence)
│   ├── ast.rs                # AST + normalized clause types
│   ├── normalize.rs          # NNF, DNF, clause cleaning
│   ├── engine.rs             # spawn rg, per-clause narrowing, clause union
│   ├── rg.rs                 # rg process wrapper: argv building, NUL parsing, batching
│   ├── tree.rs               # trie build + ASCII render (replaces astree)
│   └── explain.rs            # render normalized clauses + execution plan
└── tests/
    ├── explain_golden.rs     # §13.2 golden --explain outputs
    ├── tree_golden.rs        # §10.4 exact tree output
    └── integration.rs        # §13.3 real queries against a fixture tree
```

Unit tests live in `#[cfg(test)] mod tests` inside each module (lexer, parser,
normalize). Cross-cutting golden/integration tests live in `tests/`.

---

## 2. Core data types (`ast.rs`)

```text
Token            = LParen | RParen | And | Or | Not | Term(Vec<u8>)
Ast              = Term(Vec<u8>) | Not(Box<Ast>) | And(Box<Ast>, Box<Ast>) | Or(Box<Ast>, Box<Ast>)
Literal          = { term: Vec<u8>, negated: bool }
Clause           = ordered set of Literal   (AND of literals)
ClauseList       = Vec<Clause>              (OR of clauses; the DNF top-level shape)
```

Terms are bytes from the start (§2.2): even though the query arrives as a `String`,
store term payloads as `Vec<u8>` so the same type flows all the way to `rg`.

`MatchFlags` and `ScopeFlags` are two separate structs (§7) so the type system keeps
the load-bearing distinction explicit:

```text
MatchFlags  = { ignore_case, whole_word, fixed_strings, case_sensitive }   // -i -w -F -s
ScopeFlags  = { hidden, no_ignore: u8, types: Vec<String>, globs: Vec<String> }  // --hidden -u/-uu -t -g
```

`ScopeFlags` is the only thing that defines the universe `U`; it is threaded into
**every** `rg` call (§7).

---

## 3. Milestones

Each milestone compiles and is independently testable (spec §14). Each ends with a
commit. Tests named in §13 are landed in the milestone that makes them pass.

### M1 — CLI skeleton  *(spec §3, §12)*
- `clap` derive struct with every flag in §3.2, grouped into output/match/scope.
- Join multi-arg queries with spaces (§3.1).
- `--help` text carries the §3.3 examples.
- Classify flags into `MatchFlags` / `ScopeFlags` / output mode enum.
- Exit-code plumbing: usage error → 2, runtime error → nonzero, success → 0 (§12).
- Unknown-flag handler hints "quote it to search literally" (§12).
- **Tests:** flag classification table; `-uu` sets `no_ignore=2` **and** `hidden`.
- **Done when:** `rgq --help` prints, all flags parse, exit codes wired.

### M2 — Front end: lexer + parser + AST  *(spec §4, §13.1)*
- **Lexer** (§4.1): punctuation `()`; case-insensitive keywords `AND/OR/NOT`;
  barewords run to whitespace/paren; single+double quoted strings always terms;
  unterminated quote → error. **Quote rules pinned for robustness (spec is silent):** a
  quote opens a quoted term only at a token boundary (start, or after whitespace/paren);
  a quote inside a bareword is a literal character; the matching close-quote ends the
  term; no escape sequences in v1 (to include the other quote char, use the opposite
  quote style). An **empty term** (`""`, or an empty bareword) is a lex/parse error —
  never emit an empty pattern to `rg` (it matches every line ⇒ every file).
- **Parser** (§4.2): recursive descent matching the grammar; precedence
  `NOT > AND > OR`; parentheses to any depth; **no implicit AND** — adjacency is caught
  by a "leftover tokens after a complete parse" check (e.g. `cat dog` leaves `dog`).
  Guard against stack overflow on pathological nesting (`((((…))))`) with an explicit
  recursion-depth limit that errors cleanly rather than crashing.
- Early `--explain` that prints the parsed AST (pre-normalization) to prove the front
  end end-to-end.
- **Tests (§13.1 lexer/parser):** keyword case-insensitivity; quoted keyword is a
  term; `'"AND" OR cat'`; unterminated quote errors; `a AND b OR c` ⇒ `(a AND b) OR c`;
  `NOT` binds tightest; n-ary flatten of `a AND b AND c`; adjacency / dangling /
  leading-operator / unbalanced-paren all error.
- **Done when:** §13.1 lexer+parser tests pass.

### M3 — Normalization  *(spec §6, §13.1 NNF/DNF, §13.2 golden)*
- **NNF** (§6.1): push `NOT` to leaves; the four De Morgan/double-neg rewrites.
- **DNF** (§6.2): distribute AND over OR → `Vec<Clause>`. ⚠ **DEVIATION (robustness):**
  the spec says document DNF blow-up rather than defeat it (§6.2/§11); we keep that, but
  add a **clause-count cap** (`--max-clauses`, sensible default) that aborts with a clear
  error *before* exhausting memory. Failing safe on a 2ⁿ expansion is not "defeating" the
  blow-up — an unbounded expansion that OOMs the process is itself a robustness bug.
  Document the cap in `--help`/README.
- **Cleaning** (§6.3): dedup literals in a clause; drop `t ∧ ¬t` contradictions;
  dedup whole clauses; all-clauses-dropped ⇒ unsatisfiable (stderr note, exit 0).
- **`explain.rs`** (§9 `--explain`): print normalized clause list (one clause per line,
  positives + `NOT`-negatives joined by `AND`) **and** the execution plan (seed,
  narrowing order, union). Output must be **stable** — it is golden-tested.
- **Tests:** §13.1 NNF (each rule, double-neg, `NOT (A OR B)`); DNF clause counts
  (`(a OR b) AND (c OR d)` ⇒ 4); dedup; contradiction drop. §13.2 golden `--explain`
  for all 8 listed queries.
- **Robustness test (beyond spec):** a **property-based** check (`proptest`) that
  generates random ASTs and asserts the normalized DNF is (a) structurally a flat
  OR-of-ANDs-of-literals and (b) **semantically equivalent** to the source AST by
  brute-forcing the truth table over its distinct terms. This directly targets the
  spec's stated goal — correctness for *arbitrary* nesting, not just the shapes the
  author happened to test (§1, §6) — which example-based tests alone can't guarantee.
- **Done when:** `rgq --explain '<q>'` is byte-stable, the 8 golden cases pass, and the
  property test holds.

### M4 — Engine  *(spec §7, §8, §13.3)*
- **`rg.rs`** process wrapper:
  - Build argv for the three modes (cheat-sheet above), always `--null`, always `--`
    before paths, `-e` before patterns (§8.3).
  - Parse NUL-separated stdout → `Vec<Vec<u8>>` (§2.2).
  - **Batching** (§8.2): when restricting to candidate paths, split into batches under
    an argv-size budget; union per-batch outputs. A pure helper
    `batches(paths, budget) -> Vec<&[Vec<u8>]>` is unit-tested independently of `rg`.
  - Exit-code policy (spike hard rule 2): the result set comes from parsed stdout; exit
    0/1 = ran fine, exit ≥2 = real error → forward `rg`'s stderr and propagate (§12).
    `rg` missing → clear error. Map regex-compile failures (exit 2 on a bad term) to a
    message naming the offending term and clause.
  - **Never spawn with an empty path batch** (would scan cwd); callers short-circuit ∅.
- **Shared `ExecutionPlan` type (architectural):** build one plan value (per clause: the
  seed choice, the ordered narrowing steps, the outer union) that **both** `engine`
  executes and `explain` renders, so `--explain` can never drift from what actually runs.
  `explain.rs` formats the plan; `engine.rs` interprets it.
- **`engine.rs`** per-clause narrowing (§8.1):
  1. Seed: first positive literal via `-l`; if no positive literal, seed from `U`
     (`--files`) and emit the positive-free **warning** to stderr (§8.1.1).
  2. Apply remaining positives via `-l` restricted to candidates.
  3. Apply negatives via `--files-without-match` restricted to candidates.
  - **Short-circuit:** as soon as the candidate set is empty, stop narrowing this clause
    (result ∅) — never spawn `rg` with zero paths.
  - Outer OR: union clause results into the final `BTreeSet` (§8.4).
- **Flag propagation (§7):** `ScopeFlags` go to the universe call **and** every
  pattern call; `MatchFlags` go to every pattern call. This invariant gets its own
  integration test.
- **Default output:** one path per line, sorted (§9).
- **Tests (§13.3):** AND/OR/NOT/nested exact path sets vs hand-computed §5 semantics on
  a `tempfile` fixture tree; scope-flag consistency (`NOT` = universe − matches under
  same scope); large-candidate-set batching correctness.
- **Done when:** §13.3 integration tests pass against the fixture.

### M5 — Tree module  *(spec §10, §13)*
- **Phase 1** trie (§10.1): split paths on `/`, descend/create nodes.
- **Phase 2** render (§10.2): DFS with prefix; last child `└── `, others `├── `;
  descend extends prefix by 4 spaces (last) or `│   ` (not last); root line `.`.
- Wire `--tree` to render the engine's final set.
- Standalone stdin filter (§10.3): accept NUL- or newline-separated input, detect NUL
  and prefer it.
- **Tests:** §10.4 exact box-drawing golden, **including** an out-of-order input that
  must still render sorted.
- **Done when:** the §10.4 golden matches byte-for-byte.

### M6 — Hardening  *(spec §3, §8.3, §9, §12)*
- Leading-dash guarding audited across **all** `rg` calls (`-e` + `--`) (§8.3).
- Positive-free clause warning verified on stderr (§8.1).
- `--print0` output framing (NUL-separated, no trailing newline conversion) (§9).
- Error-message pass: parse/lex (empty query, dangling/leading operator, unbalanced
  paren, unterminated quote, adjacency) → exit 2 with a clear message naming the
  problem (§12); unknown flag hint; `rg` failure surfaced.
- Final exit-code audit against §12 (incl. unsatisfiable = exit 0, zero matches = 0).
- **Tests:** `--print0` framing; an error-cases table asserting exit 2 + message.

### M7 — Optional (post-v1)  *(spec §10, §11)*
- Clause-level parallelism (`rayon` or threads), results unioned at the end — only if
  measured to help (§11). Behind a feature/flag; must not change results.
- Most-selective-term-first seeding — transparent, result-preserving (§11).
- *(Future, separate efforts, explicitly not v1):* native search via ripgrep library
  crates (`grep`, `ignore`) to drop the `rg` dependency; the line-level same-line
  backend behind the engine interface (§11 seam).

---

## 4. Testing strategy (spec §13) — summary checklist

The concrete suite design — exact inputs/expected outputs, the validated fixture, helpers,
and the edge-case catalogue — lives in [`TEST_PLAN.md`](./TEST_PLAN.md). Checklist:

- [ ] **§13.1 Unit** — lexer, parser, NNF, DNF (in-module `#[cfg(test)]`).
- [ ] **§13.2 Golden `--explain`** — the 8 listed queries, exact output.
- [ ] **§13.3 Integration** — fixture tree (`tempfile`/`assert_cmd`); AND/OR/NOT/nested
      exact sets; scope-flag consistency; `--print0` framing; `ARG_MAX` batching.
- [ ] **§10.4 Tree golden** — exact box-drawing, plus out-of-order-renders-sorted.

Suggested dev-dependencies: `assert_cmd` + `predicates` (drive the built binary),
`tempfile` (fixture trees). The tree golden and most unit tests need neither.

The acceptance criteria in spec §15 are the definition of done; the table above is the
mechanical encoding of it.

---

## 5. Risks & watch-items

1. **Scope/universe drift (§7)** — the single most likely correctness bug. Mitigation:
   one `ScopeFlags → argv` function used by *all three* rg modes; a dedicated test that
   a `NOT` result equals `universe − matches` under identical scope flags.
2. **DNF blow-up (§6.2)** — expected, not a bug. Document in README; do not try to
   defeat it. Cleaning (dedup + contradiction drop) keeps it from being gratuitous.
3. **`ARG_MAX` (§8.2)** — batching must be correct *and* exercised by a test that
   actually crosses the threshold (lower the budget in the test to force batching).
4. **Non-UTF-8 / newline paths (§2.2)** — byte-oriented end to end; lossy conversion
   only at human-readable print. `--print0` is the only newline-safe output.
5. **Leading-dash terms/paths (§8.3)** — easy to regress; the M6 audit + a fixture file
   named like a flag guard it.
6. **Empty candidate → cwd scan** — the sharpest footgun (spike 1). A missing
   short-circuit silently returns the whole tree instead of ∅. Guarded in `engine` +
   `rg.rs`, and a test drives a clause down to ∅ and asserts no spurious results.
7. **Exit-code misuse** — inferring emptiness from `rg`'s exit code is wrong for
   `--files-without-match` (spike 5). Always parse stdout; test both modes at empty.
8. **DNF OOM** — unbounded 2ⁿ expansion can exhaust memory; the `--max-clauses` cap
   fails safe with a clear message (M3 deviation).
9. **Stack overflow on deep nesting** — recursion-depth guard in the parser (M2).
10. **explain/engine drift** — the shared `ExecutionPlan` type (M4) is the structural
    guarantee they stay in sync.

---

## 6. Immediate next steps (when implementation starts)

1. `cargo init --name rgq` (binary), add `clap`, `anyhow`, `thiserror`; dev-deps
   `assert_cmd`, `predicates`, `tempfile`.
2. Land **M1** (CLI skeleton) + its tests; commit.
3. Proceed M2 → M6 in order, each milestone a commit, tests landed with the code that
   makes them pass.
4. Keep the README examples in sync with spec §3.3 and `--help`.
