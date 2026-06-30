//! Lexer: turn the query string into a token stream (spec §4.1).
//!
//! Tokens are `(`, `)`, the case-insensitive keywords `AND`/`OR`/`NOT`, and
//! terms. A **bareword** runs up to the next whitespace or parenthesis; if it
//! equals a keyword (case-insensitively) it *is* that keyword. A **quoted string**
//! (single or double) is always a term, never a keyword — this is how a user
//! searches for the literal word `and`: `'"AND" OR cat'`.
//!
//! Quote rules pinned beyond the spec for robustness (the spec is silent):
//! * a quote opens a quoted term only at a token boundary (start, or after
//!   whitespace/paren); a quote appearing *inside* a bareword is a literal byte;
//! * the matching close-quote ends the term; there are no escape sequences (to
//!   include the other quote character, use the opposite quote style);
//! * an empty term (`""` / `''`) is an error — an empty pattern would match every
//!   line, hence every file.

/// A lexical token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    LParen,
    RParen,
    And,
    Or,
    Not,
    /// A search term (raw bytes).
    Term(Vec<u8>),
}

/// An error encountered while lexing. Maps to a usage error (exit 2, spec §12).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LexError {
    #[error("unterminated quoted string: no closing {0} found")]
    UnterminatedQuote(char),
    #[error("empty term: an empty search string would match every file — remove it, or quote real text")]
    EmptyTerm,
}

/// Tokenize `input`. Whitespace separates tokens and is otherwise insignificant,
/// so an empty or whitespace-only input yields an empty token stream (the parser
/// rejects that as an empty query).
pub fn lex(input: &str) -> Result<Vec<Token>, LexError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '\'' | '"' => {
                let quote = c;
                i += 1; // consume the opening quote
                let start = i;
                while i < chars.len() && chars[i] != quote {
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(LexError::UnterminatedQuote(quote));
                }
                let content: String = chars[start..i].iter().collect();
                i += 1; // consume the closing quote
                if content.is_empty() {
                    return Err(LexError::EmptyTerm);
                }
                tokens.push(Token::Term(content.into_bytes()));
            }
            _ => {
                // Bareword: run up to whitespace or a parenthesis. Quotes are not
                // special here — a quote mid-bareword is a literal byte.
                let start = i;
                while i < chars.len() {
                    let d = chars[i];
                    if d.is_whitespace() || d == '(' || d == ')' {
                        break;
                    }
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                match keyword(&word) {
                    Some(kw) => tokens.push(kw),
                    None => tokens.push(Token::Term(word.into_bytes())),
                }
            }
        }
    }

    Ok(tokens)
}

/// A bareword is a keyword iff it equals `and`/`or`/`not` ignoring ASCII case.
fn keyword(word: &str) -> Option<Token> {
    if word.eq_ignore_ascii_case("and") {
        Some(Token::And)
    } else if word.eq_ignore_ascii_case("or") {
        Some(Token::Or)
    } else if word.eq_ignore_ascii_case("not") {
        Some(Token::Not)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(s: &str) -> Token {
        Token::Term(s.as_bytes().to_vec())
    }

    fn lexed(s: &str) -> Vec<Token> {
        lex(s).expect("should lex")
    }

    #[test]
    fn l1_barewords_and_keyword() {
        assert_eq!(lexed("cat AND dog"), vec![term("cat"), Token::And, term("dog")]);
    }

    #[test]
    fn l2_keywords_are_case_insensitive() {
        assert_eq!(lexed("and And AND"), vec![Token::And, Token::And, Token::And]);
        assert_eq!(lexed("not OR nOt"), vec![Token::Not, Token::Or, Token::Not]);
    }

    #[test]
    fn l3_keyword_match_must_be_exact_word() {
        assert_eq!(lexed("andy ANDES nottingham"), vec![term("andy"), term("ANDES"), term("nottingham")]);
    }

    #[test]
    fn l4_quoted_keyword_is_a_term() {
        assert_eq!(lexed("\"AND\" OR cat"), vec![term("AND"), Token::Or, term("cat")]);
        assert_eq!(lexed("'NOT' cat"), vec![term("NOT"), term("cat")]);
    }

    #[test]
    fn l5_quoted_term_keeps_internal_whitespace() {
        assert_eq!(lexed("'cat dog'"), vec![term("cat dog")]);
    }

    #[test]
    fn l6_parens_tokenize() {
        assert_eq!(lexed("(cat)"), vec![Token::LParen, term("cat"), Token::RParen]);
    }

    #[test]
    fn l7_parens_break_barewords_without_whitespace() {
        assert_eq!(
            lexed("cat)AND(dog"),
            vec![term("cat"), Token::RParen, Token::And, Token::LParen, term("dog")]
        );
    }

    #[test]
    fn l8_inner_quotes_are_literal_inside_other_quote() {
        assert_eq!(lexed("'he said \"hi\"'"), vec![term("he said \"hi\"")]);
    }

    #[test]
    fn l9_quote_mid_bareword_is_a_literal_char() {
        assert_eq!(lexed("foo\"bar\""), vec![term("foo\"bar\"")]);
    }

    #[test]
    fn l10_regex_metachars_are_opaque_term_bytes() {
        assert_eq!(lexed("a.*b[0-9]+"), vec![term("a.*b[0-9]+")]);
    }

    #[test]
    fn l11_leading_dash_is_a_normal_term() {
        assert_eq!(lexed("-foo"), vec![term("-foo")]);
    }

    #[test]
    fn l12_non_ascii_passes_through() {
        assert_eq!(lexed("caté"), vec![term("caté")]);
    }

    #[test]
    fn l13_unterminated_quote_errors() {
        assert_eq!(lex("\"cat"), Err(LexError::UnterminatedQuote('"')));
        assert_eq!(lex("'cat AND dog"), Err(LexError::UnterminatedQuote('\'')));
    }

    #[test]
    fn l14_empty_quoted_term_errors() {
        assert_eq!(lex("\"\""), Err(LexError::EmptyTerm));
        assert_eq!(lex("''"), Err(LexError::EmptyTerm));
    }

    #[test]
    fn l15_whitespace_only_is_empty_stream() {
        assert_eq!(lexed("   \t  "), vec![]);
        assert_eq!(lexed(""), vec![]);
    }

    #[test]
    fn mixed_realistic_query() {
        assert_eq!(
            lexed("(cat OR feline) AND NOT 'kitten'"),
            vec![
                Token::LParen,
                term("cat"),
                Token::Or,
                term("feline"),
                Token::RParen,
                Token::And,
                Token::Not,
                term("kitten"),
            ]
        );
    }
}
