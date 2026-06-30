//! Normalization: rewrite an arbitrary AST into disjunctive normal form (spec §6).
//!
//! Two passes then a clean-up:
//!
//! 1. **NNF** (§6.1) — push `NOT` to the leaves via De Morgan and double-negation
//!    elimination, so that afterwards a `NOT` can only wrap a bare term.
//! 2. **DNF** (§6.2) — distribute `AND` over `OR` so the whole query becomes an OR
//!    of clauses, each clause an AND of literals.
//! 3. **Cleaning** (§6.3) — dedup literals within a clause, drop self-contradictory
//!    clauses (`t AND NOT t`), dedup whole clauses; if every clause is dropped the
//!    query is unsatisfiable.
//!
//! This is what makes translation general: after DNF *every* query has the same
//! shape, so the engine only handles "a clause" and "a union of clauses". The
//! `proptest` at the bottom checks the rewrite preserves meaning for arbitrary
//! nesting, which is the actual correctness goal (spec §1).

use std::collections::HashSet;

use crate::ast::{Ast, Clause, ClauseList, Literal, Normalized};

/// Error from normalization. The only failure mode is the DNF clause count
/// blowing past the configured cap (spec §6.2 warns this can expand
/// combinatorially); we fail safe with a clear message rather than exhaust memory.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NormalizeError {
    #[error(
        "query expands to more than {max} clauses in disjunctive normal form; \
         simplify it or raise --max-clauses"
    )]
    TooManyClauses { max: usize },
}

/// Normalize `ast` to DNF, capping the clause count at `max_clauses`.
pub fn normalize(ast: &Ast, max_clauses: usize) -> Result<Normalized, NormalizeError> {
    let nnf = to_nnf(ast);
    let raw = to_dnf(&nnf, max_clauses)?;
    let cleaned = clean(raw);
    if cleaned.is_empty() {
        Ok(Normalized::Unsatisfiable)
    } else {
        Ok(Normalized::Clauses(ClauseList { clauses: cleaned }))
    }
}

// ---- NNF --------------------------------------------------------------------

/// Negation normal form: `NOT` only ever wraps a bare term.
fn to_nnf(ast: &Ast) -> Ast {
    match ast {
        Ast::Term(t) => Ast::Term(t.clone()),
        Ast::Not(inner) => nnf_negated(inner),
        Ast::And(a, b) => Ast::And(Box::new(to_nnf(a)), Box::new(to_nnf(b))),
        Ast::Or(a, b) => Ast::Or(Box::new(to_nnf(a)), Box::new(to_nnf(b))),
    }
}

/// NNF of `NOT ast`, applying De Morgan / double-negation on the way down.
fn nnf_negated(ast: &Ast) -> Ast {
    match ast {
        Ast::Term(t) => Ast::Not(Box::new(Ast::Term(t.clone()))),
        Ast::Not(inner) => to_nnf(inner), // NOT (NOT x) = x
        Ast::And(a, b) => Ast::Or(Box::new(nnf_negated(a)), Box::new(nnf_negated(b))),
        Ast::Or(a, b) => Ast::And(Box::new(nnf_negated(a)), Box::new(nnf_negated(b))),
    }
}

// ---- DNF --------------------------------------------------------------------

/// Distribute an NNF expression into a list of clauses (each a list of literals).
/// `NOT` is assumed to wrap only terms (the NNF invariant).
fn to_dnf(nnf: &Ast, max_clauses: usize) -> Result<Vec<Vec<Literal>>, NormalizeError> {
    match nnf {
        Ast::Term(t) => Ok(vec![vec![Literal::positive(t.clone())]]),
        Ast::Not(inner) => match &**inner {
            Ast::Term(t) => Ok(vec![vec![Literal::negative(t.clone())]]),
            _ => unreachable!("NNF guarantees NOT wraps only a term"),
        },
        Ast::Or(a, b) => {
            let mut clauses = to_dnf(a, max_clauses)?;
            clauses.extend(to_dnf(b, max_clauses)?);
            guard(clauses.len(), max_clauses)?;
            Ok(clauses)
        }
        Ast::And(a, b) => {
            let left = to_dnf(a, max_clauses)?;
            let right = to_dnf(b, max_clauses)?;
            // Distribute: (l1 OR l2) AND (r1 OR r2) -> l1r1 OR l1r2 OR l2r1 OR l2r2.
            let product = left.len().checked_mul(right.len()).unwrap_or(usize::MAX);
            guard(product, max_clauses)?;
            let mut out = Vec::with_capacity(product);
            for l in &left {
                for r in &right {
                    let mut clause = l.clone();
                    clause.extend(r.iter().cloned());
                    out.push(clause);
                }
            }
            Ok(out)
        }
    }
}

fn guard(count: usize, max: usize) -> Result<(), NormalizeError> {
    if count > max {
        Err(NormalizeError::TooManyClauses { max })
    } else {
        Ok(())
    }
}

// ---- Cleaning ---------------------------------------------------------------

fn clean(raw: Vec<Vec<Literal>>) -> Vec<Clause> {
    let mut clauses: Vec<Clause> = Vec::new();
    for mut literals in raw {
        literals.sort();
        literals.dedup();
        if is_contradictory(&literals) {
            continue; // a clause containing t and NOT t denotes ∅; drop it.
        }
        clauses.push(Clause { literals });
    }
    clauses.sort();
    clauses.dedup();
    clauses
}

/// A clause is contradictory if some term appears both positive and negated.
fn is_contradictory(literals: &[Literal]) -> bool {
    let positives: HashSet<&[u8]> = literals
        .iter()
        .filter(|l| !l.negated)
        .map(|l| l.term.as_slice())
        .collect();
    literals
        .iter()
        .any(|l| l.negated && positives.contains(l.term.as_slice()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- helpers ----

    fn ast(s: &str) -> Ast {
        crate::parser::parse(&crate::lexer::lex(s).unwrap()).unwrap()
    }

    /// Normalize with an effectively unlimited cap, returning the clause list.
    fn clauses(s: &str) -> Vec<Clause> {
        match normalize(&ast(s), usize::MAX).unwrap() {
            Normalized::Clauses(list) => list.clauses,
            Normalized::Unsatisfiable => vec![],
        }
    }

    fn pos(t: &str) -> Literal {
        Literal::positive(t.as_bytes().to_vec())
    }
    fn neg(t: &str) -> Literal {
        Literal::negative(t.as_bytes().to_vec())
    }
    fn clause(lits: &[Literal]) -> Clause {
        let mut literals = lits.to_vec();
        literals.sort();
        Clause { literals }
    }

    // ---- NNF (via the public normalize, observing the resulting literals) ----

    #[test]
    fn n1_de_morgan_over_and() {
        // NOT (a AND b) -> NOT a OR NOT b -> two single-literal clauses.
        assert_eq!(clauses("NOT (a AND b)"), vec![clause(&[neg("a")]), clause(&[neg("b")])]);
    }

    #[test]
    fn n2_de_morgan_over_or() {
        // NOT (a OR b) -> NOT a AND NOT b -> one clause of two negatives.
        assert_eq!(clauses("NOT (a OR b)"), vec![clause(&[neg("a"), neg("b")])]);
    }

    #[test]
    fn n3_double_negation() {
        assert_eq!(clauses("NOT NOT a"), vec![clause(&[pos("a")])]);
    }

    #[test]
    fn n4_triple_negation() {
        assert_eq!(clauses("NOT NOT NOT a"), vec![clause(&[neg("a")])]);
    }

    #[test]
    fn n6_nested_de_morgan_and_double_neg() {
        // NOT (a AND NOT b) -> NOT a OR b; clauses are sorted, so {NOT a} < {b}.
        assert_eq!(clauses("NOT (a AND NOT b)"), vec![clause(&[neg("a")]), clause(&[pos("b")])]);
    }

    // ---- DNF distribution + counts ----

    #[test]
    fn d1_distribution() {
        // a AND (b OR c) -> (a AND b) OR (a AND c)
        assert_eq!(
            clauses("a AND (b OR c)"),
            vec![clause(&[pos("a"), pos("b")]), clause(&[pos("a"), pos("c")])]
        );
    }

    #[test]
    fn d2_two_disjunctions_make_four_clauses() {
        assert_eq!(clauses("(a OR b) AND (c OR d)").len(), 4);
    }

    #[test]
    fn d3_three_disjunctions_make_eight_clauses() {
        assert_eq!(clauses("(a OR b) AND (c OR d) AND (e OR f)").len(), 8);
    }

    #[test]
    fn d4_literal_dedup_within_clause() {
        assert_eq!(clauses("a AND a"), vec![clause(&[pos("a")])]);
    }

    #[test]
    fn d5_whole_clause_dedup() {
        assert_eq!(clauses("a OR a"), vec![clause(&[pos("a")])]);
    }

    #[test]
    fn d6_contradiction_is_unsatisfiable() {
        assert_eq!(normalize(&ast("a AND NOT a"), usize::MAX).unwrap(), Normalized::Unsatisfiable);
    }

    #[test]
    fn d7_tautology_is_not_a_contradiction() {
        // a OR NOT a -> two clauses, both kept (neither is self-contradictory).
        assert_eq!(clauses("a OR NOT a"), vec![clause(&[pos("a")]), clause(&[neg("a")])]);
    }

    #[test]
    fn d8_end_to_end_nnf_then_dnf() {
        // NOT (a OR b) OR c -> (NOT a AND NOT b) OR c; sorted: {NOT a, NOT b} < {c}.
        assert_eq!(
            clauses("NOT (a OR b) OR c"),
            vec![clause(&[neg("a"), neg("b")]), clause(&[pos("c")])]
        );
    }

    #[test]
    fn clause_cap_is_enforced() {
        // (a OR b) AND (c OR d) = 4 clauses; cap at 3 must error, not OOM.
        assert_eq!(
            normalize(&ast("(a OR b) AND (c OR d)"), 3),
            Err(NormalizeError::TooManyClauses { max: 3 })
        );
    }

    // ---- the keystone: semantic equivalence over arbitrary nesting ----

    use proptest::prelude::*;
    use std::collections::HashMap;

    fn arb_ast() -> impl Strategy<Value = Ast> {
        let leaf = prop_oneof![
            Just(Ast::term("a")),
            Just(Ast::term("b")),
            Just(Ast::term("c")),
            Just(Ast::term("d")),
        ];
        leaf.prop_recursive(5, 48, 2, |inner| {
            prop_oneof![
                inner.clone().prop_map(Ast::not),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| Ast::and(a, b)),
                (inner.clone(), inner).prop_map(|(a, b)| Ast::or(a, b)),
            ]
        })
    }

    fn distinct_terms(ast: &Ast, out: &mut Vec<Vec<u8>>) {
        match ast {
            Ast::Term(t) => {
                if !out.contains(t) {
                    out.push(t.clone());
                }
            }
            Ast::Not(a) => distinct_terms(a, out),
            Ast::And(a, b) | Ast::Or(a, b) => {
                distinct_terms(a, out);
                distinct_terms(b, out);
            }
        }
    }

    fn eval_ast(ast: &Ast, env: &HashMap<Vec<u8>, bool>) -> bool {
        match ast {
            Ast::Term(t) => env[t],
            Ast::Not(a) => !eval_ast(a, env),
            Ast::And(a, b) => eval_ast(a, env) && eval_ast(b, env),
            Ast::Or(a, b) => eval_ast(a, env) || eval_ast(b, env),
        }
    }

    fn eval_norm(norm: &Normalized, env: &HashMap<Vec<u8>, bool>) -> bool {
        match norm {
            Normalized::Unsatisfiable => false,
            Normalized::Clauses(list) => list.clauses.iter().any(|c| {
                c.literals.iter().all(|l| {
                    let v = env[&l.term];
                    if l.negated {
                        !v
                    } else {
                        v
                    }
                })
            }),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]
        #[test]
        fn dnf_preserves_meaning_for_arbitrary_nesting(ast in arb_ast()) {
            let norm = normalize(&ast, 1_000_000).expect("depth-bounded ast cannot hit the cap");

            // Structural: DNF is a flat OR-of-ANDs; no clause is empty (an empty
            // clause would wrongly denote the universe).
            if let Normalized::Clauses(list) = &norm {
                for c in &list.clauses {
                    prop_assert!(!c.literals.is_empty());
                }
            }

            // Semantic: identical truth table over every assignment to its terms.
            let mut terms = Vec::new();
            distinct_terms(&ast, &mut terms);
            let k = terms.len();
            for mask in 0u32..(1u32 << k) {
                let env: HashMap<Vec<u8>, bool> = terms
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (t.clone(), (mask >> i) & 1 == 1))
                    .collect();
                prop_assert_eq!(
                    eval_ast(&ast, &env),
                    eval_norm(&norm, &env),
                    "truth-table mismatch for assignment mask {}",
                    mask
                );
            }
        }
    }
}
