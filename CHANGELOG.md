# Changelog

All notable changes to `rgq` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!--
Maintenance: add changes under "Unreleased" as you make them, grouped by Added / Changed /
Deprecated / Removed / Fixed / Security. On release, rename "Unreleased" to the new version
with a date, add a fresh empty "Unreleased" section, and update the links at the bottom.
-->

## [Unreleased]

_No unreleased changes._

## [0.1.2] - 2026-07-01

A predictive memory-safety check for `--tree`, addressing a gap left by 0.1.1's depth cap: that
cap bounds the cost *per matched file*, but not the *number* of matched files, so a broad-scope
query could still add up to a large rendering cost even with no single path unbounded.

### Added

- **`--min-free-mem-pct <PCT>` (default 20).** Before rendering `--tree`, `rgq` now predicts the
  memory it would need and checks that against real system memory (`/proc/meminfo`), refusing
  with a clear error (exit 2) rather than risking exhaustion, if proceeding would leave less than
  this percentage of *total* system memory free. The prediction for the rendered-output portion is
  exact, not approximate: it's computed by running the identical traversal `--tree` itself uses,
  against a byte-counting sink instead of an accumulating buffer, so the estimate can't silently
  drift from what actually gets rendered.
- Only `--tree` is affected — the default list and `--print0` write each matched path once with no
  size amplification and don't need the check.
- `RGQ_MEM_AVAILABLE_BYTES` / `RGQ_MEM_TOTAL_BYTES` env vars (both must be set) override the
  memory-check inputs — useful inside a memory-limited container, where `/proc/meminfo` reports
  host memory rather than the container's actual limit (a documented residual limitation).

### Tests

- `src/membudget.rs`: meminfo parsing and the proceed/refuse decision, including exact boundary
  cases (the budget is inclusive of using exactly the allowed amount).
- `src/tree.rs`: the output-size estimate is asserted *exactly* equal to `render(...).len()` across
  several fixtures, including one exercising the depth-cap truncation marker and one with real
  shared-prefix deduplication.
- `tests/membudget.rs`: end-to-end, using the env var overrides for full determinism — refuses
  under fake near-zero memory, proceeds under fake abundant memory, the flag relaxes/tightens the
  margin, non-tree output modes are unaffected, and real `/proc/meminfo` works for ordinary runs.

## [0.1.1] - 2026-07-01

A second security review, focused on the attack classes a CLI search tool that spawns a
subprocess and prints attacker-influenceable filenames is exposed to (output spoofing,
environment-driven argument injection into the spawned process, and unbounded-input resource
exhaustion). One finding was Critical; the rest were Medium or lower.

### Security

- **Critical — fixed.** `RIPGREP_CONFIG_PATH` could point ripgrep at a config file injecting
  `--pre <program>`, which runs an external command on every file searched — arbitrary command
  execution gated only by one environment variable. Confirmed exploitable against bare `rg` with
  a working proof of concept before fixing it. Every `rg` invocation now passes `--no-config`,
  verified end-to-end against the same proof-of-concept attack.
- **Medium — fixed.** Unicode bidirectional-override and invisible/zero-width characters in
  filenames (the "Trojan Source" class, CVE-2021-42574) passed through unescaped, unlike plain
  control bytes, allowing a crafted filename to spoof what it displays as. Now neutralized
  alongside the existing terminal-escape sanitization, when output goes to a terminal.
- **Medium — fixed.** The tree renderer recursed once per path-nesting level with no bound;
  confirmed to stack-overflow around depth 50,000 on a synthetic path (not reachable through a
  real filesystem's `PATH_MAX` today, but unguarded). Bounded to a 100-level depth cap, mirroring
  the parser's existing recursion guard; a path nested past the cap is truncated with a visible
  marker rather than silently dropped.
- **Low — documented.** A TOCTOU symlink race between candidate-listing and narrowing is inherent
  to spawning `rg` with an explicit path list (shared by `xargs`, `grep -f`, and similar tools);
  documented as a residual risk rather than fixed, since fixing it would require a different
  search architecture than this project deliberately chose.
- **Supply chain — checked, clean.** `cargo audit` against the RustSec advisory database reports
  zero vulnerabilities across all 57 resolved dependencies.

See [`SECURITY.md`](./SECURITY.md) findings 11-15 for full detail, including an honest account of
a development-time mistake: an earlier attempt at the tree-renderer fix replaced recursion with
an iterative rewrite alone (no depth bound) and was validated with a test deep enough to hit a
separate `O(depth²)` memory cost in the prefix-string handling, which crashed the development host
via the OOM killer. The lesson — a fix for unbounded input should be validated with bounded
inputs near the actual limit, not yet-larger unbounded ones — is recorded in `SECURITY.md` rather
than scrubbed from the record.

## [0.1.0] - 2026-06-30

Initial release: a boolean-query front end for [ripgrep](https://github.com/BurntSushi/ripgrep).
Write a logical expression over search terms with `AND`, `OR`, `NOT`, and parentheses; `rgq`
reports the set of files satisfying it, optionally rendered as a tree.

### Added

- **Query language** — `AND`/`OR`/`NOT` with parentheses; precedence `NOT > AND > OR`; no
  implicit AND (two adjacent terms are a parse error); case-insensitive operators; single- and
  double-quoted terms (a quoted keyword is a literal term); terms are regexes by default, with
  `-F` for fixed strings.
- **Normalization** — queries are rewritten to disjunctive normal form (NNF then DNF) so
  arbitrary nesting, compound `NOT`, and double negation are handled correctly; clause cleaning
  deduplicates literals and clauses and drops `t AND NOT t` contradictions; an unsatisfiable
  query is reported and exits 0. `--max-clauses` (default 1024) caps the DNF blow-up.
- **Engine** — spawns the user's `rg` and evaluates each clause by progressive narrowing (seed,
  filter positive literals, exclude negative literals), then unions the per-clause results. Match
  flags (`-i`, `-w`, `-F`, `-s`) and scope flags (`--hidden`, `-u`/`-uu`, `-t`, `-g`) are
  propagated so `NOT` and intersections stay consistent against the same file universe. Large
  candidate lists are batched to stay under `ARG_MAX`.
- **Byte-oriented paths** — paths are handled as raw bytes, so non-UTF-8 and newline-containing
  filenames are correct (use `--print0`).
- **Output modes** — default sorted list, `--print0`/`-0` (NUL-separated), `--tree` (internal
  ASCII renderer, no dependency on the external `tree`), and `--explain`/`-n` (print the
  normalized clauses and execution plan without running any search).
- **CLI** — full flag set with a grouped `--help`; clear parse/usage error messages; exit codes
  (0 on success including zero matches and unsatisfiable queries, 2 on usage/parse errors,
  non-zero on a runtime error such as `rg` missing or failing).
- **Environment variables** — `RGQ_RG` (override the `rg` binary) and `RGQ_ARG_MAX` (override the
  argv batching budget).
- **Documentation** — README user guide, `PLAN.md` (build log), `TEST_PLAN.md` (test design), and
  `SECURITY.md` (threat model).
- **Tests** — 144 across unit, property-based (normalizer truth-table equivalence over random
  queries), golden (`--explain` and tree output), and black-box end-to-end suites.

### Security

- Control bytes in attacker-influenceable filenames are escaped in the human-facing output modes
  (default list and `--tree`) when stdout is a terminal, preventing ANSI escape-sequence output
  spoofing; piped output stays raw and `--print0` is the safe form for machine consumption.
- Resilient to a non-fatal `rg` I/O error (e.g. an unreadable file, or one deleted mid-search): a
  warning is emitted and partial results are used instead of failing the whole query.
- No `unsafe` code; no shell is invoked; search patterns are passed after `-e` and paths after a
  `--` end-of-options marker, so neither a query term nor a filename can be misread as a flag. See
  [`SECURITY.md`](./SECURITY.md) for the full threat model.

[Unreleased]: https://github.com/clicraft/rgq/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/clicraft/rgq/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/clicraft/rgq/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/clicraft/rgq/releases/tag/v0.1.0
