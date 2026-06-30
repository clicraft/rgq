# Security notes for `rgq`

This document records the security review of `rgq`: the threat model, the weak points found,
how each was handled, and the residual risks you should know about — especially if you embed
`rgq` in a larger system.

`rgq` is a small CLI that parses a boolean query, spawns `ripgrep` (`rg`) as a subprocess, and
prints the set of matching file paths. It is written in safe Rust (**no `unsafe`**) and **never
invokes a shell** — `rg` is launched with an explicit argument vector, so there is no shell
metacharacter interpretation at any point.

## Threat model

The realistic adversaries for a search tool are:

1. **A hostile search target.** You run `rgq` over a directory you don't fully trust (a cloned
   repo, an unpacked archive, a shared upload dir). The attacker controls **file names** and
   **file contents**. This is the primary threat.
2. **A hostile query.** `rgq` is embedded behind some interface and an untrusted party supplies
   the query string (or flags).
3. **A hostile environment.** Environment variables or `PATH` are partially attacker-influenced.

`rgq` is an ordinary, non-setuid user tool. It does not drop privileges, sandbox, or run as a
daemon; it runs with exactly the privileges of the user who launched it.

---

## Findings

| # | Finding | Severity | Status |
|---|---------|----------|--------|
| 1 | Terminal escape-sequence injection via crafted filenames | Medium | **Fixed** |
| 2 | Newline-in-filename corrupting downstream pipelines | Low | **Mitigated** |
| 3 | DNF combinatorial blow-up (memory DoS) | Medium | **Mitigated** |
| 4 | Stack overflow via deeply nested query | Medium | **Mitigated** |
| 5 | Non-fatal `rg` I/O error / delete-race causing a hard failure | Low | **Fixed** |
| 6 | `rgq` flag smuggling when embedding an untrusted query | Medium | **Documented** |
| 7 | Arbitrary `rg` binary via `RGQ_RG` / `PATH` | Low | **Documented** |
| 8 | Argument injection into `rg` (terms/paths read as flags) | — | **Not vulnerable** |
| 9 | Shell / command injection | — | **Not vulnerable** |
| 10 | ReDoS via a regex term | — | **Not vulnerable** |
| 11 | `RIPGREP_CONFIG_PATH` → ripgrep `--pre` arbitrary command execution | **Critical** | **Fixed** |
| 12 | Unicode bidi-override / invisible-character filename spoofing ("Trojan Source") | Medium | **Fixed** |
| 13 | Tree renderer: unbounded path depth (stack overflow) | Medium | **Fixed** |
| 14 | TOCTOU symlink race between candidate listing and narrowing | Low | **Documented (inherent)** |
| 15 | Supply-chain: dependency vulnerabilities | — | **Checked: clean** |
| 16 | No predictive check against *aggregate* `--tree` memory use (many large/deep files) | Low | **Added** |

### 1. Terminal escape-sequence injection via filenames — *fixed*

A file name may contain arbitrary bytes, including ANSI escape sequences (e.g.
`evil\e[31m\e[2K\rsafe.txt`). The default flat-list output wrote path bytes verbatim, so on an
interactive terminal those bytes **execute**: an attacker who controls a file name in a tree you
search could recolour text, clear lines, or move the cursor to **spoof `rgq`'s output** — hide a
result, fake a "0 matches" line, or make one path look like another.

**Fix.** When stdout is a **terminal**, control bytes (`0x00`–`0x1f`, including `ESC`, newline,
CR; and `0x7f` DEL) are escaped to a visible `\xHH` form in the human-facing modes (default list
and `--tree`). Bytes `>= 0x80` pass through so legitimate non-ASCII names still display.
- **Piped / redirected** output stays **raw**, so tooling receives exact bytes.
- **`--print0`** is always raw — it is the unambiguous machine form (NUL cannot occur in a path).

See `sanitize_controls` in `src/cli.rs` and the `sanitize_*` unit tests.

### 2. Newline-in-filename corrupting pipelines — *mitigated*

A file name containing a newline breaks line-oriented consumers (`| xargs`, `while read`),
potentially splitting one path into two. `rgq` provides **`--print0`** (NUL-delimited), which is
the correct and safe form for piping into other tools (`rgq -0 … | xargs -0 …`). On a terminal,
newlines in names are escaped (finding 1). Byte-exact round-tripping through `--print0` is
covered by tests.

### 3. DNF combinatorial blow-up — *mitigated*

Converting to disjunctive normal form can expand combinatorially: `(a OR b) AND (c OR d) AND …`
doubles per conjunct. Left unbounded, a crafted query could exhaust memory.

**Mitigation.** `--max-clauses` (default **1024**) caps the clause count; the limit is checked
**before** any large allocation, and the per-step capacity hint is clamped, so exceeding it is a
clean usage error (exit 2), never an OOM. Long `AND`/`OR` chains are parsed iteratively (no
recursion), so only nesting depth recurses — see finding 4.

### 4. Stack overflow via deep nesting — *mitigated*

Deeply nested parentheses or `NOT` chains (`((((…))))`) recurse in the parser. A recursion-depth
limit (**256**) returns a clean error instead of overflowing the stack.

### 5. Non-fatal `rg` I/O error / delete-race — *fixed*

`rg` exits `2` on any error, including a *non-fatal* one such as a single unreadable file or a
file that vanished mid-search. Treating every exit `2` as fatal meant one such file failed the
whole query — and let an attacker **race file deletions** to deny results.

**Fix.** On exit `2` *with* output, `rgq` surfaces a `warning:` (with `rg`'s message) and uses
the results it did get. A genuinely fatal error (bad regex, bad flag) produces empty output and
is still reported as an error with a non-zero exit.

### 6. `rgq` flag smuggling when embedding — *documented*

If you build the argument list as `rgq <untrusted-query>`, a query like `-uu cat` or
`--no-ignore secret` is parsed as **`rgq`'s own flags**, letting the supplier widen the search
scope (read hidden / ignored files) or change behaviour.

**Guidance for embedders:** always separate the untrusted query with `--`:

```sh
rgq -- "$UNTRUSTED_QUERY"
```

After `--`, the whole query is positional and can never be read as an `rgq` flag (a leading-dash
*term* then just becomes a search term). Decide deliberately which scope flags, if any, you
expose to the untrusted caller, and keep the default `--max-clauses` in place.

### 7. Arbitrary `rg` binary via `RGQ_RG` / `PATH` — *documented*

`rgq` runs `rg` from `PATH`, or the binary named by the `RGQ_RG` environment variable. Whoever
controls that environment controls **which program `rgq` executes**. This is standard for any
tool that shells out, and not a privilege boundary here (`rgq` is not setuid and inherits your
environment). Do **not** run `rgq` with an attacker-controlled environment, and note that
`RGQ_RG` will execute any program you point it at. (Rust's `Command` does **not** search the
current directory, so a `./rg` dropped into the tree you're searching is *not* executed unless
`.` is already in your `PATH`.)

### 8. Argument injection into `rg` — *not vulnerable*

A search term or a candidate path could otherwise be read by `rg` as an option (a term like
`--type=foo`, or a file literally named `-rf`). `rgq` prevents this structurally:
- every search pattern is passed after `-e`, so it is always a pattern, never a flag;
- every file path is passed after a `--` end-of-options marker.

Tests cover a leading-dash search term and a file named like a flag.

### 9. Shell / command injection — *not vulnerable*

`rgq` never builds or runs a shell command string. `rg` is spawned via `std::process::Command`
with a structured argument vector, so shell metacharacters in a query or filename have no special
meaning. (The original prototype's `bash`/`xargs` pipeline was deliberately dropped for exactly
this reason.)

### 10. ReDoS via a regex term — *not vulnerable*

Terms are regexes by default, but `rgq` uses ripgrep's **default regex engine**, which guarantees
linear-time matching (no catastrophic backtracking). `rgq` does **not** expose ripgrep's PCRE2
mode (`-P`). Pathologically large regexes are bounded by ripgrep's own `--regex-size-limit`.

### 11. `RIPGREP_CONFIG_PATH` → ripgrep `--pre` arbitrary command execution — *fixed*

**The most serious finding in this review.** Ripgrep itself reads the `RIPGREP_CONFIG_PATH`
environment variable and, if set, loads extra command-line flags from the file it points to —
*before* the flags `rgq` passes. Ripgrep also has a `--pre <program>` flag: when set, ripgrep
runs `<program> <file>` for **every file it searches** and searches the program's output instead
of the file. Put together: if an attacker can get `RIPGREP_CONFIG_PATH` pointed at a file
containing `--pre /some/script`, every `rgq` invocation runs `/some/script` once per file in
scope — **arbitrary command execution**, gated only by control over one environment variable.

This was confirmed with a working proof of concept against bare `rg` during this review (a
preprocessor script that wrote a sentinel file ran successfully via `RIPGREP_CONFIG_PATH` +
`--pre`), then verified end-to-end that the fix below blocks it.

**Fix.** Every `rg` invocation now includes `--no-config`, which makes ripgrep ignore
`RIPGREP_CONFIG_PATH` entirely. `rgq` already sets every flag it cares about explicitly, so it
never relied on the user's ripgrep config for anything — disabling it costs no functionality.
See `base_args` in `src/rg.rs`, `every_mode_disables_rg_config_loading` (unit test), and
`ripgrep_config_path_cannot_inject_a_preprocessor_command` (end-to-end test that plants the
exact attack and asserts the preprocessor never runs).

### 12. Unicode bidi-override / invisible-character filename spoofing — *fixed*

Finding 1 escaped raw control bytes, but a file name can also carry **Unicode** characters that
change how text *displays* without changing the underlying bytes — the "Trojan Source" class
(CVE-2021-42574). For example a file named `cat<RLO>txt.gpj<PDF>` (where `<RLO>`/`<PDF>` are the
right-to-left-override and pop-directional-formatting codepoints) renders as `cat` followed by
text that *looks like* `jpg.txt`, even though the real name ends in `.gpj`. These are multi-byte
UTF-8 sequences (`>= 0x80`) that the original control-byte escaping didn't touch — confirmed by
reproducing the spoofed rendering with a real crafted filename before fixing it.

**Fix.** The same TTY-only sanitizer now also escapes a denylist of bidirectional format
characters (LRE/RLE/PDF/LRO/RLO/LRI/RLI/FSI/PDI) and common invisible characters (zero-width
space/non-joiner/joiner, word joiner, BOM) to a visible `\u{XXXX}` form. This is **deliberately
narrow**: it does not attempt general homoglyph/confusable detection (e.g. Cyrillic vs Latin
lookalikes) — that's a much larger, fuzzy problem with real false-positive risk for legitimate
non-Latin filenames, and is a residual risk (see below), not something this fix claims to solve.
See `DANGEROUS_CODEPOINTS` in `src/cli.rs`.

### 13. Tree renderer: unbounded path depth — *fixed*

The tree renderer recurses once per path-nesting level. A path's depth comes from whatever tree
`rgq` is pointed at, so it's attacker-influenceable; a standalone harness confirmed an unbounded
recursive implementation reliably **stack-overflows around depth 50,000** (synthetic, in-memory —
independent of any filesystem `PATH_MAX`). Real filesystem paths can't reach that depth (Linux
`PATH_MAX` is ~4096 bytes, capping real nesting in the low thousands at most), so this was not
reachable through the normal `rg --files` pipeline — but the function had no guard and silently
depended on that external, implicit limit holding.

**Fix.** A depth cap (`MAX_DEPTH = 100`, mirroring the parser's existing recursion-depth guard)
truncates a path nested past the cap with a visible `... (truncated: nested past 100 levels)`
marker instead of continuing to recurse. 100 is far beyond any real directory tree.

**A note on how this fix was arrived at, in the interest of an honest record:** the first attempt
replaced the recursion with an iterative, heap-stack-based traversal and was validated with a
test that rendered a *synthetic, 2-million-level-deep* path. That test itself allocated on the
order of terabytes (the per-level prefix string is cloned at every level, an `O(depth²)` cost
independent of recursive-vs-iterative implementation) and **crashed the development host via the
OOM killer**. The mistake was conflating "remove the stack-overflow class" with "depth should be
unbounded" — they're different problems, and once a sane depth cap is in place (which is needed
regardless, to bound the `O(depth²)` prefix-copy cost), plain bounded recursion is simpler and
sufficient; the iterative rewrite was reverted. The lesson generalizes: a fix for an
unbounded-input class should itself be validated with *bounded* inputs near the actual limit, not
yet-larger unbounded ones.

### 14. TOCTOU symlink race between listing and narrowing — *documented, inherent*

Per-clause narrowing (spec §8.1) seeds a candidate file list from one `rg` invocation, then passes
those same paths back to `rg` in a later invocation. If an attacker who can write into the
directory being searched swaps a candidate file for a symlink to a sensitive file (e.g.
`/etc/shadow`) between those two calls, the later call may search the symlink's target. The result
returned is still the original filename (not the target's), so what could leak is a single bit:
whether the targeted file matches a given term.

This is a classic local TOCTOU race shared by virtually every CLI tool that passes an explicit
file list to a second command (`xargs`, `grep -f`, etc.) — not specific to `rgq`'s design. Fixing
it would require not passing paths back to `rg` as arguments at all (e.g. opening file descriptors
directly and feeding ripgrep's library crates instead of the `rg` binary), which is a different
architecture than the one this project deliberately chose (PLAN.md §2.1: spawn `rg`, don't
reimplement search) and is noted there as a possible future direction. Documented here rather than
"fixed" because no fix is possible without that larger architectural change; it requires local
write access to the search target, which is already a high level of access.

### 15. Supply chain: dependency vulnerabilities — *checked, clean; now automated*

`rgq`'s dependency tree is small (`clap`, `thiserror`, plus their transitive deps — all
widely-used, actively maintained crates; see `cargo tree`). `cargo audit` (RustSec advisory
database, 1,146 advisories at time of scan) reports **zero vulnerabilities** across all 57
resolved dependencies. This is a point-in-time result, not a guarantee — new advisories are
published continuously, so a clean scan today says nothing about a CVE disclosed next month
against a crate already in the dependency tree.

**Automated in CI** (`.github/workflows/audit.yml`): `cargo audit` now runs on every push/PR that
touches `Cargo.toml`/`Cargo.lock`, *and* on a weekly schedule independent of any code change —
the schedule is what catches a newly published advisory against an already-merged dependency,
which a merge-time-only check would miss entirely. `.github/dependabot.yml` complements this with
weekly automated PRs bumping outdated `cargo` and `github-actions` dependencies, so a flagged
vulnerability usually already has a fix available to merge.

### 16. No predictive check against aggregate `--tree` memory use — *added*

Finding 13's `MAX_DEPTH` cap bounds the cost *per matched file* but not the *number* of matched
files: a query with very broad scope flags (`-uu` over a huge tree) and a permissive term could
match enough files, each individually near the depth cap, to add up to a large amount of memory
during `--tree` rendering — even though no single path is unbounded anymore.

**Mitigation.** Before rendering, `rgq` now computes an exact prediction of the rendered output
size and a conservative estimate of the trie's own memory footprint (`tree::estimate_memory_bytes`
— the output-size estimate is *exact*, not approximate: it runs the identical traversal `render`
itself uses, against a byte-counting sink instead of an accumulating buffer, so the two can never
silently drift apart), checks that against the system's real available memory (`/proc/meminfo`,
via `src/membudget.rs`), and refuses with a clear, actionable error rather than risk exhausting
memory — by default never letting estimated usage push *total* free system memory below **20%**
(configurable: `--min-free-mem-pct`). The check is scoped to `--tree`, the one output mode whose
memory footprint can exceed the size of its input; the default list and `--print0` modes write
each matched path once with no amplification and don't need it.

**Limitation:** the check reads host-level `/proc/meminfo`, so inside a memory-limited container
(e.g. `docker run --memory=512m`) it sees the host's full memory, not the cgroup limit, and would
not catch an OOM kill imposed by a tighter container limit. If memory introspection is unavailable
at all (non-Linux, sandboxed), `rgq` warns and proceeds rather than refusing to run.

---

## Residual risks & operator guidance

- **Expensive scans are still possible.** A positive-free clause (e.g. `NOT foo`) must scan every
  file in scope; `rgq` warns on stderr but still runs it. If you expose `rgq` to untrusted
  queries, impose your own time/CPU limits and keep the default `--max-clauses`.
- **Scope flags widen exposure by design.** `-u`/`--no-ignore` and `--hidden` intentionally
  include ignored/hidden files (which may hold secrets). Don't expose them to untrusted callers.
- **`rgq` mirrors `rg`'s view of the filesystem.** It respects `.gitignore` (inside a git repo),
  `.ignore`, and hidden-file rules exactly as `rg` does. It never follows symlinks for content
  (it doesn't read file contents at all — `rg` does), and never writes, deletes, or opens the
  paths it reports.
- **Use `--print0` for any machine consumption** of the output. Line-based parsing of the default
  output is only safe for filenames without newlines.
- **General homoglyph/confusable spoofing is out of scope.** Finding 12 neutralizes bidi-override
  and invisible characters specifically, not visual lookalikes in general (e.g. Cyrillic `а` vs
  Latin `a`). Don't rely on `rgq`'s output to visually distinguish two filenames that are
  designed to look alike.
- **The TOCTOU race in finding 14** means a result set is a snapshot, not a guarantee — if the
  search target is being concurrently modified by an untrusted party, don't treat `rgq`'s output
  as proof of what those files currently contain.
- **The `--tree` memory check (finding 16) sees host memory, not container limits.** If you run
  `rgq` inside a memory-limited container, set `--min-free-mem-pct` conservatively (or use
  `RGQ_MEM_AVAILABLE_BYTES`/`RGQ_MEM_TOTAL_BYTES` to supply the real cgroup limit), since
  `/proc/meminfo` alone won't reflect it.

## Reporting

This is a portfolio/spec project. If you find an issue, open an issue on the repository.
