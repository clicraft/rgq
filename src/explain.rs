//! `--explain`: render the normalized query and the execution plan without
//! running anything (spec §9). The output is stable and covered by golden tests
//! (spec §13.2), so its format must not drift casually.
//!
//! The plan mirrors what the engine will actually do per clause (spec §8.1): seed
//! from the first positive literal (or the whole file set if the clause has none),
//! narrow by the remaining positives, then exclude the negatives; finally union
//! the per-clause results.

use crate::ast::{render_term, Normalized};

/// Build the full `--explain` output for a parsed query and its normal form.
/// `parsed` is shown first so the reader sees how precedence bound the query.
pub fn explain(parsed: &crate::ast::Ast, normalized: &Normalized) -> String {
    let mut out = String::new();
    out.push_str(&format!("query: {parsed}\n"));

    let list = match normalized {
        Normalized::Unsatisfiable => {
            out.push_str("unsatisfiable: every clause is self-contradictory — matches no files\n");
            return out;
        }
        Normalized::Clauses(list) => list,
    };

    let n = list.clauses.len();
    out.push_str(&format!("normal form: {n} clause(s), unioned\n"));
    for (i, clause) in list.clauses.iter().enumerate() {
        out.push_str(&format!("  clause {}: {}\n", i + 1, clause));
    }

    out.push_str("execution plan:\n");
    for (i, clause) in list.clauses.iter().enumerate() {
        out.push_str(&format!("  clause {}:\n", i + 1));

        let positives: Vec<&[u8]> = clause
            .literals
            .iter()
            .filter(|l| !l.negated)
            .map(|l| l.term.as_slice())
            .collect();
        let negatives: Vec<&[u8]> = clause
            .literals
            .iter()
            .filter(|l| l.negated)
            .map(|l| l.term.as_slice())
            .collect();

        match positives.split_first() {
            Some((seed, rest)) => {
                out.push_str(&format!(
                    "    seed candidates from files matching: {}\n",
                    render_term(seed)
                ));
                for term in rest {
                    out.push_str(&format!(
                        "    narrow to files also matching: {}\n",
                        render_term(term)
                    ));
                }
            }
            None => {
                out.push_str(
                    "    seed candidates from all files in scope (no positive term — scans everything)\n",
                );
            }
        }
        for term in &negatives {
            out.push_str(&format!(
                "    narrow to files NOT matching: {}\n",
                render_term(term)
            ));
        }
    }
    out.push_str("  union the per-clause results\n");

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Clause, ClauseList, Literal};
    use crate::lexer::lex;
    use crate::normalize::normalize;
    use crate::parser::parse;

    fn explained(query: &str) -> String {
        let ast = parse(&lex(query).unwrap()).unwrap();
        let norm = normalize(&ast, usize::MAX).unwrap();
        explain(&ast, &norm)
    }

    #[test]
    fn unsatisfiable_is_a_single_line() {
        let text = explained("cat AND NOT cat");
        assert!(text.contains("unsatisfiable"));
        assert!(!text.contains("execution plan"));
    }

    #[test]
    fn positive_free_clause_seeds_from_universe() {
        let text = explained("NOT cat");
        assert!(text.contains("all files in scope"));
        assert!(text.contains("NOT matching: cat"));
    }

    #[test]
    fn seed_and_narrow_are_shown() {
        let text = explained("cat AND dog");
        assert!(text.contains("seed candidates from files matching: cat"));
        assert!(text.contains("narrow to files also matching: dog"));
        assert!(text.ends_with("union the per-clause results\n"));
    }

    #[test]
    fn clause_display_uses_normalized_form() {
        // Directly exercise the clause-rendering path independent of the parser.
        let list = ClauseList {
            clauses: vec![Clause {
                literals: vec![
                    Literal::positive(b"bird".to_vec()),
                    Literal::negative(b"cage".to_vec()),
                ],
            }],
        };
        let ast = parse(&lex("bird AND NOT cage").unwrap()).unwrap();
        let text = explain(&ast, &Normalized::Clauses(list));
        assert!(text.contains("clause 1: bird AND NOT cage"));
    }
}
