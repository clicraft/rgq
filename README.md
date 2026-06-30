# rgq

A boolean-query front end for [ripgrep](https://github.com/BurntSushi/ripgrep). You write a
logical expression over search terms with `AND`, `OR`, `NOT`, and parentheses; `rgq` reports
the **set of files** satisfying it, optionally rendered as an ASCII tree.

```sh
rgq '(cat AND dog) OR (bird AND NOT cage)'
rgq -i -t py 'import AND NOT __future__' --tree
```

`rgq` shells out to your installed `rg` for the actual searching and performs all boolean set
logic (intersection, union, difference) in Rust over byte-string path sets — so results are
correct for paths that aren't valid UTF-8, and for arbitrarily nested queries (not just the
shapes that happened to be tested).

> **Status: v1 feature-complete.** Lexer, parser, NNF/DNF normalization, the rg-backed engine,
> the tree renderer, and `--explain`/`--print0` are all implemented and tested (unit + property
> + golden + black-box e2e). See [`PLAN.md`](./PLAN.md) for the build log,
> [`TEST_PLAN.md`](./TEST_PLAN.md) for the test design, and [`desing_v0.1.0.md`](./desing_v0.1.0.md)
> for the full specification.

---

## Contents

- [Install](#install)
- [Quick start](#quick-start)
- [Query language](#query-language)
- [Options reference](#options-reference)
  - [Match flags — *how* a term is matched](#match-flags--how-a-term-is-matched)
  - [Scope flags — *which files exist*](#scope-flags--which-files-exist)
  - [Output flags](#output-flags)
  - [Limits](#limits)
  - [Information](#information)
- [Output modes in detail](#output-modes-in-detail)
- [`--explain`: see the plan without running it](#--explain-see-the-plan-without-running-it)
- [Exit codes](#exit-codes)
- [Environment variables](#environment-variables)
- [Gotchas & notes](#gotchas--notes)
- [Security](#security)
- [Build & test](#build--test)
- [License](#license)

---

## Install

`rgq` needs a Rust toolchain to build and a `rg` (ripgrep) binary on your `PATH` at runtime.

```sh
git clone https://github.com/clicraft/rgq
cd rgq
cargo build --release        # binary at target/release/rgq
cargo install --path .       # or install into ~/.cargo/bin
```

Check it works:

```sh
rgq --version                # rgq 0.1.0
rg --version                 # ripgrep must be installed (14.x recommended)
```

---

## Quick start

```sh
rgq 'TODO AND FIXME'                      # files containing both words
rgq 'cat OR dog'                          # files containing either
rgq 'cat AND NOT dog'                     # cat, but not dog
rgq '(cat OR feline) AND NOT kitten'      # grouping with parentheses
rgq -i -t py 'import AND NOT __future__'  # case-insensitive, Python files only
rgq '(a AND b) OR c' --tree               # render the result as a tree
rgq -n '(A AND B) OR C'                   # show the compiled plan, run nothing
rgq -0 'error AND NOT timeout' | xargs -0 wc -l   # pipe NUL-safe paths onward
```

The query is one argument — **quote it** so the shell doesn't interpret the parentheses or the
word `NOT`. Multiple bare words are also accepted and joined with spaces (`rgq cat AND dog`),
but quoting is the habit to keep.

---

## Query language

Terms are combined with the operators `AND`, `OR`, `NOT` and grouped with parentheses.

**Precedence is `NOT` > `AND` > `OR`**, so:

```
a AND b OR c    parses as   (a AND b) OR c
NOT a AND b     parses as   (NOT a) AND b
```

Use parentheses to override precedence to any depth. There is **no implicit AND**: two adjacent
terms with no operator between them (`cat dog`) is a *parse error*, not a silent conjunction.

**Terms:**

- A **bareword** runs up to the next whitespace or parenthesis: `cat`, `__future__`, `a.*b`.
- Operators are matched **case-insensitively** — `and`, `And`, `AND` are all the operator.
- A **quoted string** (single or double) is **always** a term, never an operator. This is how
  you search for the literal word *and*: `rgq '"AND" OR cat'`. Whitespace inside quotes is kept
  (`'cat dog'` is one term that matches the phrase).
- Terms are **regexes by default** (ripgrep compiles them). `a.c` matches `abc`; to match the
  literal three characters `a.c`, pass `-F` (fixed strings). A term that is an invalid regex
  (e.g. an unbalanced `'('`) surfaces ripgrep's error.

**Semantics.** Every query denotes a set of file paths. With `U` = the files `rg` would search
under the current [scope flags](#scope-flags--which-files-exist):

| Query     | Meaning              |
|-----------|----------------------|
| `term t`  | files containing `t` |
| `A AND B` | `⟦A⟧ ∩ ⟦B⟧`          |
| `A OR B`  | `⟦A⟧ ∪ ⟦B⟧`          |
| `NOT A`   | `U \ ⟦A⟧`            |

Matching is **file-level**: `A AND B` means a file containing both terms *somewhere* in the
file, not necessarily on the same line.

---

## Options reference

```
rgq [OPTIONS] '<QUERY>'
```

Flags fall into classes that are applied differently — the **match vs scope** distinction is
load-bearing for correctness (a `NOT` is computed against the same file universe as the positive
terms).

### Match flags — *how* a term is matched

Applied to every search. (Mirror the corresponding ripgrep flags.)

| Flag | Description | Example |
|------|-------------|---------|
| `-i` | Case-insensitive matching | `rgq -i 'error'` matches `Error`, `ERROR` |
| `-w` | Whole-word matching | `rgq -w 'cat'` matches `cat` but not `category` |
| `-F` | Treat terms as literal fixed strings, not regexes | `rgq -F 'a.c'` matches only `a.c` |
| `-s` | Force case-sensitive matching | `rgq -s 'Error'` |

### Scope flags — *which files exist*

These define the universe `U`. They are applied identically to the file-listing that seeds the
search and to every term search, so `NOT` and intersections stay consistent.

| Flag | Argument | Description | Example |
|------|----------|-------------|---------|
| `--hidden` | — | Include hidden files and directories | `rgq --hidden 'TODO'` |
| `-u`, `--no-ignore` | — | Don't respect ignore files (`.gitignore`, `.ignore`, …) | `rgq -u 'TODO'` |
| `-uu` | — | `-u` **plus** `--hidden` (repeat the flag) | `rgq -uu 'TODO'` |
| `-t` | `<TYPE>` | Restrict to a ripgrep file type (e.g. `py`, `rust`, `md`). Repeatable | `rgq -t py -t pyi 'import'` |
| `-g` | `<GLOB>` | Restrict to a glob. Repeatable | `rgq -g '*.md' -g '!CHANGELOG.md' 'TODO'` |

`rg --type-list` shows the available type names for `-t`.

### Output flags

| Flag | Description | Example |
|------|-------------|---------|
| `--tree` | Render matching files as an ASCII tree instead of a flat list | `rgq --tree 'cat'` |
| `-0`, `--print0` | Emit NUL-separated paths — the form that is safe to pipe and correct for paths containing newlines. Conflicts with `--tree` | `rgq -0 'cat' \| xargs -0 wc -l` |
| `-n`, `--explain` | Print the normalized clauses and the execution plan; **do not run any search** | `rgq -n '(A AND B) OR C'` |

The default (no output flag) prints one path per line, sorted.

### Limits

| Flag | Argument | Default | Description |
|------|----------|---------|-------------|
| `--max-clauses` | `<N>` | `1024` | Maximum clauses a query may expand to in disjunctive normal form. Guards against the combinatorial blow-up of DNF — e.g. `(a OR b) AND (c OR d) AND (e OR f)` is 8 clauses; deeply nested OR-of-ANDs can explode. Exceeding the cap is a clear error (exit 2), not an out-of-memory crash. |

### Information

| Flag | Description |
|------|-------------|
| `-h`, `--help` | Print usage and exit |
| `-V`, `--version` | Print version and exit |

---

## Output modes in detail

**Default — flat list**, one path per line, sorted:

```sh
$ rgq 'cat'
a.txt
b.txt
sub/f.txt
```

**`--print0`** — paths separated by NUL bytes, no trailing newline. This is the only form that
is correct for filenames containing newlines, and the safe way to feed other tools:

```sh
rgq -0 'error AND NOT timeout' | xargs -0 wc -l
rgq -0 'cat' | xargs -0 rm        # safe deletion of the matching set
```

**`--tree`** — an ASCII tree built by an internal renderer (no dependency on the external
`tree` program):

```sh
$ rgq --tree 'needle'
.
├── README.md
└── src
    ├── a
    │   ├── main.py
    │   └── util.py
    └── b
        └── test.py
```

---

## `--explain`: see the plan without running it

`--explain` (`-n`) compiles the query to normal form and prints the clauses plus the execution
plan, **without searching**. It's the primary way to understand how a query was interpreted:

```sh
$ rgq --explain 'cat AND dog OR bird'
query: (cat AND dog) OR bird
normal form: 2 clause(s), unioned
  clause 1: bird
  clause 2: cat AND dog
execution plan:
  clause 1:
    seed candidates from files matching: bird
  clause 2:
    seed candidates from files matching: cat
    narrow to files also matching: dog
  union the per-clause results
```

A contradictory query is reported as unsatisfiable:

```sh
$ rgq --explain 'cat AND NOT cat'
query: cat AND (NOT cat)
unsatisfiable: every clause is self-contradictory — matches no files
```

---

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success — including **zero matches** and an **unsatisfiable** query (these are not errors) |
| `2` | Usage error — empty query, parse/lex error (dangling operator, unbalanced parenthesis, unterminated quote, adjacency), unknown flag, conflicting flags, or `--max-clauses` exceeded |
| non-zero (`1`) | Runtime error — `rg` not found or `rg` failed (e.g. an invalid-regex term) |

Warnings (such as a positive-free clause like `NOT cat`, which must scan every file in scope)
go to **stderr** and do not change the exit code.

---

## Environment variables

| Variable | Effect |
|----------|--------|
| `RGQ_RG` | Path to the `rg` binary to use instead of resolving `rg` from `PATH`. |
| `RGQ_ARG_MAX` | Override the per-invocation argv byte budget used when batching large candidate lists (default ~128 KiB). Mainly a testing/tuning hook. |

---

## Gotchas & notes

- **Quote your query.** Parentheses and `NOT` are shell metacharacters/words; `rgq '(a OR b)'`,
  not `rgq (a OR b)`.
- **A query that starts with `-`** looks like a flag to the argument parser. Separate it with
  `--`: `rgq -- '-x OR foo'`. (A term *inside* the query that starts with `-` is fine once the
  whole query is past `--` or quoted away from the leading dash.)
- **`-F` is per-invocation, not per-term.** It makes *every* term a literal. There is no
  per-term regex/literal switch in v1.
- **DNF can expand.** `(a OR b) AND (c OR d) AND …` multiplies out; `--max-clauses` bounds it.
  This is expected behavior, not a bug — simplify the query or raise the cap.
- **`.gitignore` is honored only inside a git repository** (ripgrep's own rule); `.ignore` and
  `.rgignore` are honored everywhere. Use `-u` to ignore all ignore files.
- **A `-t <type>` filter can surface matching dotfiles** that the default scan hides (a ripgrep
  behavior). `rgq` mirrors `rg`'s universe exactly, so results stay internally consistent.
- **Paths are bytes.** Non-UTF-8 and newline-containing filenames are handled correctly; use
  `--print0` when paths might contain newlines.

---

## Security

`rgq` is safe Rust (no `unsafe`) and never invokes a shell — `rg` is spawned with a structured
argument vector, search patterns go after `-e`, and paths after `--`, so neither a query term
nor a filename can be misread as a flag. Every invocation also passes `--no-config`, so an
attacker-set `RIPGREP_CONFIG_PATH` cannot inject extra `rg` flags (notably `--pre <program>`,
which would otherwise run an arbitrary command per file searched). Filenames are
attacker-influenceable, so control bytes and Unicode bidi-override/invisible characters in paths
are **escaped when output goes to a terminal** (preventing both ANSI escape-sequence spoofing and
"Trojan Source"-style filename spoofing); piped output stays raw and `--print0` is the safe form
for machine consumption.

If you **embed** `rgq` and pass an untrusted query, always separate it with `--`
(`rgq -- "$query"`) so the query can't smuggle `rgq`'s own flags (e.g. `-uu` to widen scope).

The full threat model, findings, and mitigations are in [`SECURITY.md`](./SECURITY.md).

## Build & test

```sh
cargo build --release            # optimized binary at target/release/rgq
cargo test                       # unit + property + golden + black-box e2e
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The test suite (see [`TEST_PLAN.md`](./TEST_PLAN.md)) covers the lexer, parser, and normalizer
(including a property test that checks the normal form is truth-table-equivalent to the source
for random queries), golden `--explain` and tree outputs, and black-box end-to-end runs against
real fixture trees through the real binary and `rg`.

---

## License

Licensed under either of MIT or Apache-2.0 at your option.
