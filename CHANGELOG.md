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

[Unreleased]: https://github.com/clicraft/rgq/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/clicraft/rgq/releases/tag/v0.1.0
