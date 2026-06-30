# `rgq` — Build Specification (Rust)

A boolean-query front end for [ripgrep](https://github.com/BurntSushi/ripgrep). The user
writes a logical expression over search terms; `rgq` reports the set of files satisfying it,
optionally rendered as a tree.

```
rgq '(cat AND dog) OR (bird AND NOT cage)'
rgq -i -t py 'import AND NOT __future__' --tree
```

This document specifies **what** to build, not how to write it. It is deliberately
implementation-free. Use it as the source of truth; where it gives a recommendation, follow it
unless there is a concrete reason not to, and note the deviation.

---

## 1. Goal & scope

Build a single command-line **Rust binary** named `rgq` that:

1. Parses a boolean query language (terms combined with `AND`, `OR`, `NOT`, parentheses).
2. Normalizes the query to a canonical form so that translation is correct for **arbitrary**
   nesting — not just the shapes the author happened to test.
3. Executes the query by orchestrating `ripgrep`, returning the set of matching file paths.
4. Can render that set as an ASCII tree (replacing any dependency on the external `tree` tool).

### In scope
- File-level matching: `A AND B` means "files containing both terms, anywhere in the file."
- The four output modes in §9.
- A built-in tree renderer (§8).

### Out of scope (for v1)
- Line-level matching (`A AND B` on the *same line*). Leave a clean seam for it (see §11) but do
  not implement it.
- Reading queries from a file or stdin (the query is a CLI argument).
- Any GUI or daemon.

---

## 2. Architecture & approach

### 2.1 Recommended execution model: spawn `rg`, compute sets in Rust

`rgq` shells out to the user's installed `rg` binary for the actual searching, captures the file
paths it prints, and performs all boolean set logic (intersection, union, difference) **in Rust**
using ordered byte-string sets.

Do **not** build and run a shell pipeline string. The earlier prototype emitted a `bash` command
with `xargs`, `sort`, and process substitution; that is fragile (depends on a shell, GNU coreutils,
quoting) and hard to test. Spawning processes directly and holding the intermediate path sets in
memory is more robust and unit-testable.

Rationale for spawning `rg` rather than reimplementing search:
- It reuses the user's ripgrep, including its `.gitignore` handling and performance.
- It keeps v1 small.

A future, fully self-contained version could instead use ripgrep's own library crates
(`grep`, `grep-searcher`, `grep-regex`, `ignore`) to remove the `rg` dependency entirely. Note
this as a follow-up; do not build it in v1.

### 2.2 Path representation

Treat file paths as **raw bytes**, not UTF-8 strings. Filenames on Linux may contain bytes that are
not valid UTF-8 and may even contain newlines. Therefore:
- Always invoke `rg` with NUL-separated output (its null-separator option) and split captured
  output on the `0x00` byte.
- Store path sets as an **ordered set of byte vectors** so results are deduplicated and sorted with
  no extra step, and so tree rendering is deterministic.
- Only convert to text (lossily) at the final moment, when printing for human display.

### 2.3 Suggested module breakdown

Organize the crate into clearly separated concerns:

| Module        | Responsibility                                                          |
|---------------|-------------------------------------------------------------------------|
| `cli`         | Argument parsing, flag classification, dispatch, exit codes.            |
| `lexer`       | Turn the query string into tokens.                                      |
| `parser`      | Tokens → abstract syntax tree (AST). Enforces precedence.               |
| `ast`         | The AST type and the normalized clause representation.                  |
| `normalize`   | NNF and DNF rewrites; clause cleaning.                                  |
| `engine`      | Execute clauses against `rg`; combine results into the final set.       |
| `tree`        | Build a trie from paths and render it as ASCII (the former `astree`).   |
| `explain`     | Render the normalized query and execution plan for `--explain`.         |

### 2.4 Suggested crates
- A CLI argument parser (e.g. `clap`, derive style).
- An error-handling helper (e.g. `anyhow` for the binary, optionally `thiserror` for typed errors).
- Optional, for parallel clause execution (§10): a data-parallelism crate (e.g. `rayon`) or the
  standard threading library.
- Keep the dependency list small. No regex engine is needed in v1 because `rg` does the matching.

---

## 3. CLI specification

### 3.1 Synopsis

```
rgq [OPTIONS] '<QUERY>'
```

The query may be passed as a single quoted argument, or as several arguments that `rgq` joins with
spaces. (Users should quote the whole query when it contains parentheses or `NOT`, because the shell
would otherwise interpret those.)

### 3.2 Options

| Flag                     | Class  | Effect                                                                  |
|--------------------------|--------|-------------------------------------------------------------------------|
| `--tree`                 | output | Render matching files as an ASCII tree (§8) instead of a flat list.     |
| `--explain`, `-n`        | output | Print the normalized clauses and the execution plan; do **not** run.    |
| `--print0`, `-0`         | output | Emit NUL-separated paths (for piping into `xargs -0` or similar).       |
| `-i`                     | match  | Case-insensitive matching.                                              |
| `-w`                     | match  | Whole-word matching.                                                    |
| `-F`                     | match  | Treat terms as literal fixed strings, not regexes.                      |
| `-s`                     | match  | Force case-sensitive matching.                                          |
| `--hidden`               | scope  | Include hidden files.                                                   |
| `--no-ignore`, `-u`      | scope  | Do not respect ignore files. `-uu` additionally implies `--hidden`.     |
| `-t <TYPE>`              | scope  | Restrict to a ripgrep file type (e.g. `py`).                            |
| `-g <GLOB>`              | scope  | Restrict to a glob (e.g. `*.md`).                                       |
| `-h`, `--help`           | —      | Usage.                                                                  |

The **match** vs **scope** distinction is load-bearing — see §7.

### 3.3 Example invocations (for `--help` and the README)

```
rgq 'TODO AND FIXME'
rgq '(cat OR feline) AND NOT kitten' --tree
rgq -i -t py 'import AND NOT __future__'
rgq -n '(A AND B) OR C'              # show the compiled plan, run nothing
rgq -0 'error AND NOT timeout' | xargs -0 wc -l
```

---

## 4. Query language

### 4.1 Lexer (tokens)

The lexer produces this token stream:

- **Punctuation:** `(` and `)`.
- **Keywords:** `AND`, `OR`, `NOT`, matched **case-insensitively** (`and`, `And`, `AND` are all the
  operator).
- **Terms:** everything else.
  - A **bareword** is a run of characters up to the next whitespace or parenthesis. If a bareword
    equals a keyword (case-insensitively) it is the keyword, otherwise it is a term.
  - A **quoted string** (single or double quotes) is **always** a term, never a keyword. This is how
    a user searches for the literal word *and*: `'"AND" OR cat'`. An unterminated quote is a lexer
    error.

Whitespace separates tokens and is otherwise insignificant.

### 4.2 Grammar (precedence: `NOT` > `AND` > `OR`)

```
query    = or_expr ;
or_expr  = and_expr , { "OR"  , and_expr } ;
and_expr = not_expr , { "AND" , not_expr } ;
not_expr = "NOT" , not_expr | atom ;
atom     = "(" , or_expr , ")" | TERM ;
```

Consequences this grammar must guarantee:
- `a AND b OR c` parses as `(a AND b) OR c` (AND binds tighter than OR).
- `NOT a AND b` parses as `(NOT a) AND b` (NOT binds tightest).
- Parentheses override precedence to any depth.
- A chain like `a AND b AND c` is flattened into a single n-ary conjunction during normalization
  (it need not be represented as nested binary nodes downstream).
- There is **no implicit AND**: two adjacent terms with no operator between them (`cat dog`) is a
  **parse error**, not a silent conjunction. This keeps the grammar unambiguous.

### 4.3 AST

Describe (do not necessarily expose) an AST with four node kinds: a **term** (carrying the search
string), a **NOT** wrapping one child, an **AND** of two children, and an **OR** of two children.
AND/OR may be built binary by the parser; the normalizer is responsible for flattening.

---

## 5. Semantics (the definition of "correct")

Every query denotes a **set of file paths**. Let `U` be the universe: the set of files `rg` would
search under the current scope flags (i.e. the output of ripgrep's "list files" mode with the same
scope flags applied — see §7). Then:

- `⟦ term t ⟧`   = the files containing a match for `t`.
- `⟦ A AND B ⟧`  = `⟦A⟧ ∩ ⟦B⟧`.
- `⟦ A OR B ⟧`   = `⟦A⟧ ∪ ⟦B⟧`.
- `⟦ NOT A ⟧`    = `U \ ⟦A⟧`.

**The tool is correct iff, for every query, the set of paths it prints equals the denoted set.**
All implementation choices below must preserve this. The unit and integration tests in §13 exist to
check it.

---

## 6. Normalization

Translation is only tractable and provably general if the query is first rewritten into a canonical
form. Do this in two passes, then clean up.

### 6.1 Negation Normal Form (NNF) — push `NOT` to the leaves

Apply these rewrites until no `NOT` wraps anything but a bare term:

```
NOT (NOT A)      →  A
NOT (A AND B)    →  (NOT A) OR  (NOT B)
NOT (A OR  B)    →  (NOT A) AND (NOT B)
NOT (term)       →  unchanged   (this is a literal)
```

After NNF, "NOT of a compound expression" cannot occur — the case that is otherwise easy to forget
becomes structurally impossible.

### 6.2 Disjunctive Normal Form (DNF) — distribute `AND` over `OR`

Rewrite the NNF expression into an **OR of clauses**, where each **clause** is an **AND of
literals**, and each **literal** is either a positive term or a negated term:

```
A AND (B OR C)   →  (A AND B) OR (A AND C)
```

After DNF, **every** query has the identical top-level shape: a union of conjunctive clauses. This
is what makes the engine general — it only ever has to handle "a clause" and "a union of clauses."

**Cost to be aware of:** DNF can expand combinatorially. For example
`(a OR b) AND (c OR d) AND (e OR f)` becomes eight clauses. This is acceptable for interactive
search and must not be treated as a bug, but the implementation should not be gratuitously wasteful
(see cleaning, next).

### 6.3 Clause cleaning

After producing the clause list:
- **Deduplicate literals** within each clause.
- **Drop contradictory clauses:** a clause containing both `t` and `NOT t` denotes the empty set;
  remove it.
- **Deduplicate whole clauses** (treat a clause as a set of literals for comparison).
- If **every** clause is dropped as contradictory, the whole query is unsatisfiable: print nothing,
  emit an informational message to stderr, exit success.

---

## 7. Flag propagation (match vs scope) — a correctness requirement

Flags fall into two classes and must be applied differently. Getting this wrong produces
**silently incorrect** results, so treat it as a hard requirement.

- **Match flags** (`-i`, `-w`, `-F`, `-s`) change *how a term is matched*. They apply to every `rg`
  invocation that carries a search pattern.
- **Scope flags** (`--hidden`, `--no-ignore`/`-u`/`-uu`, `-t`, `-g`) change *which files exist* from
  the tool's point of view. They define the universe `U`. They must be applied to **both**:
  1. the "list files" invocation that produces `U` (used to seed all-negative clauses, §8/§11), and
  2. **every** pattern-bearing invocation.

If scope flags were applied to the searches but not to the universe (or vice versa), `NOT` would be
computed against a different file set than the positive terms, and intersections would be wrong.
Keep `U` and the searched set identical.

---

## 8. Execution engine

Given the cleaned clause list, compute the final path set as the **union over clauses** of the
**per-clause path set**.

### 8.1 Per-clause evaluation (a "narrowing" intersection)

A clause is an AND of positive and negative literals. Evaluate it by **progressive narrowing**, so
each step searches only the survivors of the previous step:

1. **Seed the candidate set.**
   - If the clause has at least one positive literal, seed by running ripgrep's "list files with a
     match" for the first positive term. (Choice of *which* positive term to use first is an
     optimization — see §10.)
   - If the clause has **no positive literal** (e.g. `NOT a AND NOT b`), seed from the universe `U`
     (ripgrep's "list files" mode). Emit a stderr **warning** that a positive-free clause scans
     every file in scope and is therefore expensive.
2. **Apply each remaining positive literal** by searching for it **restricted to the current
   candidate paths**, keeping only files that match. (Use ripgrep's "list files with a match" mode,
   passing the candidate paths as the files to search.)
3. **Apply each negative literal** by searching for it **restricted to the current candidate
   paths**, keeping only files that do **not** match. Ripgrep has a "list files *without* a match"
   mode that does exactly this in one call; prefer it. (Equivalently, you may search for files that
   *do* match and subtract them in Rust — either is acceptable as long as the result equals the set
   difference.)

The candidate set shrinks monotonically. After all literals are applied, it is the clause's result.

Note that whether a literal is positive or negative — not its position in the source query —
determines how it is applied. This is why `NOT a AND b` works: `b` seeds, `a` filters.

### 8.2 Passing candidate paths to `rg` and the `ARG_MAX` requirement

When restricting a search to the current candidates, the candidate paths are passed to `rg` as the
list of files to search. A candidate set can be large enough to exceed the operating system's
maximum argument length (`ARG_MAX`), which would cause the spawn to fail.

**Requirement:** when the candidate list is large, split it into **batches**, invoke `rg` once per
batch, and take the union of the per-batch outputs. This mirrors what `xargs` does and must be
handled internally. Because each literal's decision is per-file, batching does not change the
result.

### 8.3 Guarding against leading-dash terms and paths

A search term or a file path may begin with a dash and would otherwise be misread as an option.
Ensure that, in every `rg` invocation, the search pattern and the file paths are passed in a way
that prevents option interpretation (ripgrep supports an end-of-options marker for this). A file
literally named like a flag must not break a query.

### 8.4 Combining clauses (the outer OR)

Take the union of all clause result sets into the final ordered set. If there is exactly one clause,
its result is the final set. Clauses are independent and may be evaluated concurrently (§10).

---

## 9. Output modes

After computing the final ordered path set:

- **Default:** print one path per line, in sorted order.
- **`--print0`:** print paths separated by NUL bytes, no trailing newline conversion. This is the
  safe form for downstream tools and the only form that is correct for paths containing newlines.
- **`--tree`:** render the path set as an ASCII tree (§10/tree module). The renderer is internal;
  do not shell out to the external `tree` program.
- **`--explain`:** do not execute. Print:
  1. the normalized clause list, each clause shown readably (e.g. positive and negated terms joined
     by `AND`, clauses listed one per line), and
  2. a description of the execution plan (the seed, the narrowing order, the union).
  This mode is the primary teaching and debugging aid; make its output clear and stable, because it
  is covered by golden tests (§13).

---

## 10. Tree rendering (internal `tree` module)

Render a set of file paths as an indented ASCII tree, equivalent to the external `tree --fromfile`.

### 10.1 Phase 1 — build a trie ("the tree grows dynamically")

Insert each path into a shared tree of nested nodes keyed by path component:
- Split each path on the `/` byte.
- Walk from the root; for each component, descend into the existing child or create it if absent.

Because shared prefixes reuse existing nodes, the tree grows incrementally as paths are inserted;
input order does not matter, since rendering sorts.

### 10.2 Phase 2 — render with box-drawing characters

Depth-first traversal, carrying an indentation **prefix** string:
- Children of a node are visited in sorted order.
- The **last** child of a node is drawn with `└── `; every earlier child with `├── `.
- When descending into a child, extend the prefix by four spaces if that child was the last,
  otherwise by `│   ` (a vertical bar and three spaces) so the ancestor's line continues.
- Print a single `.` as the root line above the tree.

### 10.3 Input handling

The tree module must accept the same path representation as the engine (ordered byte set). When used
as a standalone filter reading stdin (optional, but recommended for parity with the old `astree`),
it should accept **either** NUL-separated **or** newline-separated input — detect NUL and prefer it,
since newline-separation is unsafe for paths containing newlines.

### 10.4 Expected output shape

For the input paths `README.md`, `src/a/main.py`, `src/a/util.py`, `src/b/test.py`, the renderer
must produce exactly:

```
.
├── README.md
└── src
    ├── a
    │   ├── main.py
    │   └── util.py
    └── b
        └── test.py
```

This exact output is a golden test (§13).

---

## 11. Performance & concurrency

- **Narrowing** (the per-clause strategy in §8.1) is the main efficiency win: each literal after the
  seed searches only the surviving candidates, so total bytes scanned drops at every step.
- **Most-selective-term-first (optional optimization):** within a clause, seeding with the *rarest*
  positive term collapses the candidate set fastest, making later steps cheap — the same idea a SQL
  planner uses when ordering joins. The author's term order may be preserved by default; if you
  implement reordering, it must be a transparent optimization that does not change results, and any
  probing of term frequency it requires should be cheap and clearly justified.
- **Parallel clauses (optional):** clauses are independent, so the per-clause evaluations may run
  concurrently and their results unioned at the end. This is the natural place for parallelism.
  Note that within a single `rg` invocation ripgrep already parallelizes internally, so do not
  expect large gains from also parallelizing the orchestration on small inputs; measure before
  committing to it.
- **DNF blow-up** is the known worst case (§6.2); document it rather than trying to defeat it.
- **Line-level seam (do not build in v1):** a future same-line mode would reuse the lexer, parser,
  and normalizer unchanged, and only swap the engine: a clause would compile to a single ripgrep
  invocation using look-around assertions (positive look-ahead per positive literal, negative
  look-ahead per negative literal) under ripgrep's alternate regex engine, matching within one line.
  Keep the engine behind an interface so this can be added as a second backend later.

---

## 12. Error handling & exit codes

- **Parse/lex errors** (empty query, dangling operator such as `cat AND`, leading operator,
  unbalanced parenthesis, unterminated quote, adjacency like `cat dog`): print a clear message to
  stderr naming the problem; exit with a **usage error** code (2).
- **Unknown CLI flag:** print a message hinting that the word can be searched literally by quoting it
  inside the query; exit 2.
- **`rg` not found or fails:** surface a clear error; propagate a nonzero exit.
- **Success with zero matching files:** exit **0** (an empty result is not an error).
- **Unsatisfiable query** (all clauses contradictory): informational message to stderr, exit 0.
- Warnings (e.g. positive-free clause) go to **stderr** and do not change the exit code.

---

## 13. Testing requirements

Provide automated tests. These encode correctness (§5); do not consider the tool done without them.

### 13.1 Unit tests
- **Lexer:** keyword case-insensitivity; barewords; single- and double-quoted terms; a quoted
  keyword is a term; unterminated quote errors; parentheses tokenize correctly.
- **Parser:** precedence (`a AND b OR c` ⇒ `(a AND b) OR c`); `NOT` binds tightest; parentheses
  override; n-ary flattening of `a AND b AND c`; adjacency, dangling operator, leading operator, and
  unbalanced parenthesis each error.
- **NNF:** each De Morgan rule; double-negation elimination; `NOT (A OR B)` ⇒ `(NOT A) AND (NOT B)`.
- **DNF:** distribution produces the expected clause count (e.g. `(a OR b) AND (c OR d)` ⇒ 4
  clauses); literal and clause deduplication; a `t AND NOT t` clause is dropped.

### 13.2 Golden `--explain` outputs
Assert the exact `--explain` output for at least these queries:
- `(cat AND dog) OR bird`
- `NOT (cat OR dog)`
- `NOT cage AND bird`
- `NOT NOT cat`
- `cat AND dog OR bird`
- `cat AND NOT cat` (unsatisfiable)
- `(a OR b) AND (c OR d)`
- `"AND" OR cat`

### 13.3 Integration tests against a fixture tree
- Create a temporary directory containing files with known contents.
- Run real queries and assert the **exact** set of returned paths for AND, OR, NOT, and nested
  combinations, checked against the §5 semantics computed by hand.
- Verify scope-flag consistency: e.g. a `NOT` query returns the universe minus matches under the
  same scope flags.
- Verify `--print0` output framing.
- **Tree renderer:** feed a known path list (including one given **out of sorted order**) and assert
  the exact box-drawing output from §10.4 (and that out-of-order input still renders sorted).
- **Large candidate set:** construct enough matching files to exercise the `ARG_MAX` batching path
  (§8.2) and confirm results are still correct.

---

## 14. Build order (suggested milestones)

Implement incrementally; each milestone should compile and be testable on its own.

1. **CLI skeleton:** argument parsing, flag classification (match vs scope), `--help`, exit codes.
2. **Front end:** lexer + parser + AST, plus an early `--explain` that prints the parsed (then
   normalized) form. Land the §13.1 lexer/parser tests.
3. **Normalization:** NNF + DNF + cleaning. Land the NNF/DNF unit tests and the §13.2 golden
   `--explain` outputs.
4. **Engine:** spawn `rg`, per-clause narrowing, clause union, default list output. Land the §13.3
   integration tests, including `ARG_MAX` batching and scope-flag consistency.
5. **Tree module:** trie build + ASCII render; wire up `--tree`; land the tree golden test.
6. **Hardening:** leading-dash guarding, all-negative warnings, `--print0`, error messages, final
   exit-code audit.
7. **Optional:** parallel clause evaluation; most-selective-first ordering. (Future, separate:
   native search via ripgrep's library crates; the line-level backend.)

---

## 15. Acceptance criteria (summary)

- Parses the full grammar with correct precedence and clear errors on malformed input.
- Normalizes via NNF then DNF; handles arbitrary nesting, compound `NOT`, double negation, and
  `NOT`-first clauses correctly; drops contradictions; deduplicates.
- Computes results matching the §5 set semantics, verified by integration tests.
- Applies match vs scope flags correctly so `NOT` and intersections are consistent.
- Handles large candidate sets (batching), leading-dash terms/paths, quoted keywords, and
  non-UTF-8 / newline-containing paths (byte-oriented, NUL-separated).
- Provides `--tree` (internal renderer, exact output as specified), `--print0`, and `--explain`.
- Ships with the unit, golden, and integration tests described in §13.
- Is a single self-contained binary whose only external runtime dependency is `rg`.
