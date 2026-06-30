//! Parser: token stream → AST, enforcing precedence (spec §4.2).
//!
//! Recursive descent over the grammar
//!
//! ```text
//! query    = or_expr ;
//! or_expr  = and_expr , { "OR"  , and_expr } ;
//! and_expr = not_expr , { "AND" , not_expr } ;
//! not_expr = "NOT" , not_expr | atom ;
//! atom     = "(" , or_expr , ")" | TERM ;
//! ```
//!
//! so `NOT` binds tightest, then `AND`, then `OR`. There is **no implicit AND**:
//! two adjacent terms (`cat dog`) are a parse error, detected as tokens left over
//! after a complete parse. AND/OR are built as binary nodes; the normalizer
//! flattens them later. A recursion-depth limit guards against a stack overflow
//! on pathological nesting like `((((…))))`.

use crate::ast::Ast;
use crate::lexer::Token;

/// Maximum nesting depth (parentheses / `NOT` chains) before we bail out cleanly
/// instead of risking a stack overflow.
const MAX_DEPTH: usize = 256;

/// A parse error. Maps to a usage error (exit 2, spec §12).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("empty query: provide a boolean expression, e.g. 'cat AND dog'")]
    Empty,
    #[error("expected a term or '(', but found {0}")]
    ExpectedAtom(String),
    #[error("unexpected {0} after a complete expression — there is no implicit AND, so write the operator explicitly (e.g. 'a AND b')")]
    TrailingInput(String),
    #[error("unbalanced parentheses: missing ')'")]
    MissingCloseParen,
    #[error("empty parentheses '()' have nothing to match")]
    EmptyParens,
    #[error("query nested too deeply (limit {0})")]
    TooDeep(usize),
}

/// Parse a token stream into an [`Ast`].
pub fn parse(tokens: &[Token]) -> Result<Ast, ParseError> {
    if tokens.is_empty() {
        return Err(ParseError::Empty);
    }
    let mut p = Parser { tokens, pos: 0, depth: 0 };
    let ast = p.or_expr()?;
    if p.pos != tokens.len() {
        return Err(ParseError::TrailingInput(describe(p.peek())));
    }
    Ok(ast)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) {
        self.pos += 1;
    }

    fn descend(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err(ParseError::TooDeep(MAX_DEPTH))
        } else {
            Ok(())
        }
    }

    fn ascend(&mut self) {
        self.depth -= 1;
    }

    fn or_expr(&mut self) -> Result<Ast, ParseError> {
        let mut left = self.and_expr()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.bump();
            let right = self.and_expr()?;
            left = Ast::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Ast, ParseError> {
        let mut left = self.not_expr()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.bump();
            let right = self.not_expr()?;
            left = Ast::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn not_expr(&mut self) -> Result<Ast, ParseError> {
        if matches!(self.peek(), Some(Token::Not)) {
            self.bump();
            self.descend()?;
            let inner = self.not_expr()?;
            self.ascend();
            Ok(Ast::Not(Box::new(inner)))
        } else {
            self.atom()
        }
    }

    fn atom(&mut self) -> Result<Ast, ParseError> {
        match self.peek() {
            Some(Token::LParen) => {
                self.bump();
                if matches!(self.peek(), Some(Token::RParen)) {
                    return Err(ParseError::EmptyParens);
                }
                self.descend()?;
                let inner = self.or_expr()?;
                self.ascend();
                match self.peek() {
                    Some(Token::RParen) => {
                        self.bump();
                        Ok(inner)
                    }
                    _ => Err(ParseError::MissingCloseParen),
                }
            }
            Some(Token::Term(t)) => {
                let t = t.clone();
                self.bump();
                Ok(Ast::Term(t))
            }
            other => Err(ParseError::ExpectedAtom(describe(other))),
        }
    }
}

/// Human-readable description of a token (or end of input) for error messages.
fn describe(tok: Option<&Token>) -> String {
    match tok {
        None => "end of query".to_string(),
        Some(Token::LParen) => "'('".to_string(),
        Some(Token::RParen) => "')'".to_string(),
        Some(Token::And) => "operator 'AND'".to_string(),
        Some(Token::Or) => "operator 'OR'".to_string(),
        Some(Token::Not) => "operator 'NOT'".to_string(),
        Some(Token::Term(t)) => format!("term '{}'", String::from_utf8_lossy(t)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    /// Lex then parse, expecting success.
    fn ast(s: &str) -> Ast {
        parse(&lex(s).expect("lex")).expect("parse")
    }

    /// Lex then parse, expecting a parse error.
    fn err(s: &str) -> ParseError {
        parse(&lex(s).expect("lex")).expect_err("should fail to parse")
    }

    #[test]
    fn p1_and_binds_tighter_than_or() {
        // a AND b OR c  ==  (a AND b) OR c
        assert_eq!(
            ast("a AND b OR c"),
            Ast::or(Ast::and(Ast::term("a"), Ast::term("b")), Ast::term("c"))
        );
    }

    #[test]
    fn p2_and_binds_tighter_than_or_other_order() {
        assert_eq!(
            ast("a OR b AND c"),
            Ast::or(Ast::term("a"), Ast::and(Ast::term("b"), Ast::term("c")))
        );
    }

    #[test]
    fn p3_not_binds_tightest() {
        assert_eq!(
            ast("NOT a AND b"),
            Ast::and(Ast::not(Ast::term("a")), Ast::term("b"))
        );
    }

    #[test]
    fn p4_not_of_parenthesized_compound() {
        assert_eq!(
            ast("NOT (a OR b)"),
            Ast::not(Ast::or(Ast::term("a"), Ast::term("b")))
        );
    }

    #[test]
    fn p5_parens_override_precedence() {
        assert_eq!(
            ast("(a OR b) AND c"),
            Ast::and(Ast::or(Ast::term("a"), Ast::term("b")), Ast::term("c"))
        );
    }

    #[test]
    fn p6_and_chain_is_left_associative_binary() {
        // n-ary flattening is the normalizer's job (M3); the parser builds binary.
        assert_eq!(
            ast("a AND b AND c"),
            Ast::and(Ast::and(Ast::term("a"), Ast::term("b")), Ast::term("c"))
        );
    }

    #[test]
    fn p7_double_not_parses() {
        assert_eq!(ast("NOT NOT cat"), Ast::not(Ast::not(Ast::term("cat"))));
    }

    #[test]
    fn p8_deep_parens_within_limit() {
        assert_eq!(ast("((((a))))"), Ast::term("a"));
    }

    #[test]
    fn p9_adjacency_is_an_error() {
        assert!(matches!(err("cat dog"), ParseError::TrailingInput(_)));
    }

    #[test]
    fn p10_dangling_operator_at_end() {
        assert!(matches!(err("cat AND"), ParseError::ExpectedAtom(_)));
        assert!(matches!(err("cat OR"), ParseError::ExpectedAtom(_)));
    }

    #[test]
    fn p11_leading_operator() {
        assert!(matches!(err("AND cat"), ParseError::ExpectedAtom(_)));
        assert!(matches!(err("OR cat"), ParseError::ExpectedAtom(_)));
    }

    #[test]
    fn p12_double_operator() {
        assert!(matches!(err("a AND AND b"), ParseError::ExpectedAtom(_)));
    }

    #[test]
    fn p13_unbalanced_parens() {
        assert!(matches!(err("(cat"), ParseError::MissingCloseParen));
        assert!(matches!(err("cat)"), ParseError::TrailingInput(_)));
    }

    #[test]
    fn p14_empty_parens() {
        assert!(matches!(err("()"), ParseError::EmptyParens));
    }

    #[test]
    fn p15_dangling_not() {
        assert!(matches!(err("NOT"), ParseError::ExpectedAtom(_)));
        assert!(matches!(err("cat AND NOT"), ParseError::ExpectedAtom(_)));
    }

    #[test]
    fn p16_too_deeply_nested_errors_cleanly() {
        let deep = format!("{}a{}", "(".repeat(MAX_DEPTH + 5), ")".repeat(MAX_DEPTH + 5));
        assert!(matches!(err(&deep), ParseError::TooDeep(_)));
    }

    #[test]
    fn empty_token_stream_is_empty_query() {
        assert_eq!(parse(&[]), Err(ParseError::Empty));
    }
}
