//! The abstract syntax tree produced by the parser.
//!
//! Four node kinds (spec §4.3): a **term** carrying the raw search bytes, a
//! **NOT** wrapping one child, and binary **AND**/**OR**. The parser builds
//! AND/OR as binary nodes; flattening into n-ary conjunctions is the
//! normalizer's job (spec §4.2, lands in M3).
//!
//! Terms are stored as `Vec<u8>`, not `String`: a search term becomes an `rg`
//! pattern, and the whole pipeline is byte-oriented (spec §2.2). They originate
//! from the (UTF-8) command line, but we stop treating them as text here.

use std::fmt;

/// A node in the boolean-query AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ast {
    /// A search term (raw bytes).
    Term(Vec<u8>),
    /// Logical negation of one subexpression.
    Not(Box<Ast>),
    /// Logical conjunction of two subexpressions.
    And(Box<Ast>, Box<Ast>),
    /// Logical disjunction of two subexpressions.
    Or(Box<Ast>, Box<Ast>),
}

impl Ast {
    /// Convenience constructors, used by tests and (read) by the `Display` impl.
    #[cfg(test)]
    pub fn term(s: &str) -> Ast {
        Ast::Term(s.as_bytes().to_vec())
    }
    #[cfg(test)]
    pub fn not(a: Ast) -> Ast {
        Ast::Not(Box::new(a))
    }
    #[cfg(test)]
    pub fn and(a: Ast, b: Ast) -> Ast {
        Ast::And(Box::new(a), Box::new(b))
    }
    #[cfg(test)]
    pub fn or(a: Ast, b: Ast) -> Ast {
        Ast::Or(Box::new(a), Box::new(b))
    }
}

/// Readable, fully-disambiguated rendering for `--explain`. Compound children are
/// parenthesized so precedence and associativity are explicit; terms that contain
/// whitespace (i.e. came from a quoted string) are shown quoted.
impl fmt::Display for Ast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ast::Term(t) => write!(f, "{}", render_term(t)),
            Ast::Not(a) => write!(f, "NOT {}", parenthesized(a)),
            Ast::And(a, b) => write!(f, "{} AND {}", parenthesized(a), parenthesized(b)),
            Ast::Or(a, b) => write!(f, "{} OR {}", parenthesized(a), parenthesized(b)),
        }
    }
}

fn parenthesized(a: &Ast) -> String {
    match a {
        Ast::Term(_) => a.to_string(),
        _ => format!("({a})"),
    }
}

/// Render a term for human display: quoted if it contains whitespace (i.e. came
/// from a quoted string), otherwise as-is (lossily, for non-UTF-8 bytes). Shared
/// by the AST, clause, and `--explain` rendering.
pub fn render_term(t: &[u8]) -> String {
    let s = String::from_utf8_lossy(t);
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        format!("\"{s}\"")
    } else {
        s.into_owned()
    }
}

// ---- Normalized (DNF) representation ----------------------------------------
//
// After normalization (NNF then DNF, spec §6) every query has the same shape: a
// union of conjunctive clauses. A `Literal` is a term, possibly negated; a
// `Clause` is an AND of literals; a `ClauseList` is an OR of clauses. The engine
// only ever has to handle "a clause" and "a union of clauses".

/// A literal: a term, possibly negated. The atom of a DNF clause.
///
/// `Ord` is `(term, negated)` so literals sort by term with the positive form
/// before the negative — which makes deduplication and contradiction detection
/// within a clause straightforward.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Literal {
    pub term: Vec<u8>,
    pub negated: bool,
}

impl Literal {
    pub fn positive(term: Vec<u8>) -> Literal {
        Literal {
            term,
            negated: false,
        }
    }
    pub fn negative(term: Vec<u8>) -> Literal {
        Literal {
            term,
            negated: true,
        }
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negated {
            write!(f, "NOT {}", render_term(&self.term))
        } else {
            write!(f, "{}", render_term(&self.term))
        }
    }
}

/// A clause: an AND of literals. The engine evaluates one by progressive
/// narrowing (spec §8.1). Literals are kept sorted and deduplicated.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Clause {
    pub literals: Vec<Literal>,
}

impl fmt::Display for Clause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.literals.iter().map(ToString::to_string).collect();
        write!(f, "{}", parts.join(" AND "))
    }
}

/// The normalized query: an OR (union) of clauses — disjunctive normal form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClauseList {
    pub clauses: Vec<Clause>,
}

/// Result of normalizing a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Normalized {
    /// A satisfiable query: a non-empty union of clauses.
    Clauses(ClauseList),
    /// Every clause was self-contradictory (e.g. `t AND NOT t`); the query
    /// denotes the empty set (spec §6.3).
    Unsatisfiable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_parenthesizes_by_precedence() {
        // (a AND b) OR c
        let ast = Ast::or(Ast::and(Ast::term("a"), Ast::term("b")), Ast::term("c"));
        assert_eq!(ast.to_string(), "(a AND b) OR c");
    }

    #[test]
    fn display_not_of_compound_and_of_term() {
        assert_eq!(Ast::not(Ast::term("cat")).to_string(), "NOT cat");
        assert_eq!(
            Ast::not(Ast::or(Ast::term("a"), Ast::term("b"))).to_string(),
            "NOT (a OR b)"
        );
        assert_eq!(
            Ast::not(Ast::not(Ast::term("cat"))).to_string(),
            "NOT (NOT cat)"
        );
    }

    #[test]
    fn display_quotes_terms_with_whitespace() {
        assert_eq!(Ast::term("cat dog").to_string(), "\"cat dog\"");
        assert_eq!(Ast::term("cat").to_string(), "cat");
    }
}
