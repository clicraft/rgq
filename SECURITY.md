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

## Reporting

This is a portfolio/spec project. If you find an issue, open an issue on the repository.
