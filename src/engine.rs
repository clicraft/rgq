//! Execution engine: evaluate the normalized clause list against `rg` and combine
//! the per-clause results into the final path set (spec §8).
//!
//! The final set is the **union over clauses** of the **per-clause set**. A clause
//! (an AND of literals) is evaluated by *progressive narrowing* (spec §8.1): seed
//! a candidate set, then shrink it one literal at a time so each step searches only
//! the survivors of the previous step.

use std::collections::BTreeSet;

use crate::ast::{Clause, ClauseList};
use crate::rg::{Rg, RgError};

/// The result of executing a query.
pub struct Outcome {
    /// Matching paths, ordered and deduplicated (spec §2.2).
    pub files: BTreeSet<Vec<u8>>,
    /// Non-fatal warnings to surface on stderr (e.g. positive-free clauses).
    pub warnings: Vec<String>,
}

const POSITIVE_FREE_WARNING: &str =
    "a clause with no positive term scans every file in scope and is expensive";

/// Execute `clauses`, returning the union of the per-clause path sets.
pub fn run(clauses: &ClauseList, rg: &Rg) -> Result<Outcome, RgError> {
    let mut files: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut warnings: Vec<String> = Vec::new();

    for clause in &clauses.clauses {
        let result = eval_clause(clause, rg, &mut warnings)?;
        files.extend(result);
    }

    warnings.dedup();
    Ok(Outcome { files, warnings })
}

/// Evaluate one clause by progressive narrowing (spec §8.1). Whether a literal is
/// positive or negative — not its position — decides how it is applied, which is
/// why `NOT a AND b` works: `b` seeds, `a` filters.
fn eval_clause(clause: &Clause, rg: &Rg, warnings: &mut Vec<String>) -> Result<Vec<Vec<u8>>, RgError> {
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

    // 1. Seed, then 2. apply the remaining positive literals.
    let mut candidates: Vec<Vec<u8>> = match positives.split_first() {
        Some((seed, rest)) => {
            let mut cands = rg.list_matching(seed, None)?;
            for term in rest {
                if cands.is_empty() {
                    break; // narrowed to ∅; stop (and never spawn rg with no paths)
                }
                cands = rg.list_matching(term, Some(&cands))?;
            }
            cands
        }
        None => {
            warnings.push(POSITIVE_FREE_WARNING.to_string());
            rg.list_files()?
        }
    };

    // 3. Apply the negative literals (set difference, one rg call each).
    for term in &negatives {
        if candidates.is_empty() {
            break;
        }
        candidates = rg.list_not_matching(term, &candidates)?;
    }

    Ok(candidates)
}
