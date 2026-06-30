# rgq

A boolean-query front end for [ripgrep](https://github.com/BurntSushi/ripgrep). You write a
logical expression over search terms; `rgq` reports the **set of files** satisfying it,
optionally rendered as an ASCII tree.

```sh
rgq '(cat AND dog) OR (bird AND NOT cage)'
rgq -i -t py 'import AND NOT __future__' --tree
```

`rgq` shells out to your installed `rg` for the actual searching and performs all boolean
set logic (intersection, union, difference) in Rust over byte-string path sets — so results
are correct for paths that aren't valid UTF-8, and for arbitrarily nested queries (not just
the shapes that happened to be tested).

> **Status: v1 feature-complete.** Lexer, parser, NNF/DNF normalization, the rg-backed
> engine, the tree renderer, and `--explain`/`--print0` are all implemented and tested
> (unit + property + golden + black-box e2e). See [`PLAN.md`](./PLAN.md) for the build
> log, [`TEST_PLAN.md`](./TEST_PLAN.md) for the test design, and
> [`desing_v0.1.0.md`](./desing_v0.1.0.md) for the full specification.

## Query language

Terms combined with `AND`, `OR`, `NOT`, and parentheses. Precedence is `NOT` > `AND` > `OR`,
so `a AND b OR c` parses as `(a AND b) OR c`. There is **no implicit AND**: `cat dog` is a
parse error, not a silent conjunction.

- A **bareword** runs up to the next whitespace or parenthesis.
- A **quoted string** (single or double) is always a term, never an operator — this is how
  you search for the literal word *and*: `rgq '"AND" OR cat'`.
- Terms are **regexes by default** (ripgrep matches them); pass `-F` to treat them as literal
  fixed strings.

Semantics (with `U` = the files `rg` would search under the current scope flags):

| Query     | Meaning                 |
|-----------|-------------------------|
| `term t`  | files containing `t`    |
| `A AND B` | `⟦A⟧ ∩ ⟦B⟧`            |
| `A OR B`  | `⟦A⟧ ∪ ⟦B⟧`            |
| `NOT A`   | `U \ ⟦A⟧`              |

Matching is **file-level**: `A AND B` means a file containing both terms anywhere, not
necessarily on the same line.

## Options

Flags fall into two load-bearing classes (this distinction is a correctness requirement):

**Match** — *how* a term is matched (`-i` case-insensitive, `-w` whole-word, `-F` fixed
strings, `-s` case-sensitive).

**Scope** — *which files exist*, i.e. the universe `U` (`--hidden`, `--no-ignore`/`-u`/`-uu`,
`-t <TYPE>`, `-g <GLOB>`).

**Output** — `--tree` (ASCII tree), `--print0`/`-0` (NUL-separated, the newline-safe form),
`--explain`/`-n` (print the normalized clauses and execution plan; run nothing).

Run `rgq --help` for the full list.

## Build

```sh
cargo build --release   # produces target/release/rgq
cargo test              # unit + integration tests
```

Requires a `rg` (ripgrep) binary on `PATH` at runtime.

## License

Licensed under either of MIT or Apache-2.0 at your option.
