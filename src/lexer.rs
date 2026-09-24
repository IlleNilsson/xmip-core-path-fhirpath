//! The tokens of a `FHIRPath` expression.
//!
//! `FHIRPath` quotes strings with single quotes, and a number is an integer or a
//! decimal by whether it carries a point. Identifiers cover both member names
//! and function names; the parser tells them apart by the parenthesis that
//! follows a function.

use contract::ContractError;

/// One token of an expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    /// A member name, a resource type, a function name or `true`/`false`.
    Identifier(String),
    /// A single-quoted string literal, quotes removed.
    Text(String),
    /// A whole number.
    Integer(i64),
    /// A number with a point.
    Decimal(f64),
    /// `.`
    Dot,
    /// `[`
    OpenBracket,
    /// `]`
    CloseBracket,
    /// `(`
    OpenParen,
    /// `)`
    CloseParen,
    /// `=`
    Equal,
    /// `!=`
    NotEqual,
}

/// Split `expression` into tokens.
///
/// # Errors
/// A character that begins no token, an unterminated string, or a number that
/// does not parse.
pub fn tokenize(expression: &str) -> Result<Vec<Token>, ContractError> {
    let mut tokens = Vec::new();
    let mut rest = expression;
    while let Some(first) = rest.chars().next() {
        let consumed = match first {
            character if character.is_whitespace() => 1,
            '.' => push(&mut tokens, Token::Dot),
            '[' => push(&mut tokens, Token::OpenBracket),
            ']' => push(&mut tokens, Token::CloseBracket),
            '(' => push(&mut tokens, Token::OpenParen),
            ')' => push(&mut tokens, Token::CloseParen),
            '=' => push(&mut tokens, Token::Equal),
            '!' if rest.starts_with("!=") => 1 + push(&mut tokens, Token::NotEqual),
            '\'' => text(rest, &mut tokens)?,
            character if character.is_ascii_digit() => number(rest, &mut tokens)?,
            character if character.is_alphabetic() || character == '_' => {
                identifier(rest, &mut tokens)
            }
            other => {
                return Err(error(format!(
                    "unexpected {other:?} at {} in {expression:?}",
                    expression.len() - rest.len()
                )));
            }
        };
        rest = &rest[consumed..];
    }
    Ok(tokens)
}

fn push(tokens: &mut Vec<Token>, token: Token) -> usize {
    tokens.push(token);
    1
}

fn text(rest: &str, tokens: &mut Vec<Token>) -> Result<usize, ContractError> {
    let body = &rest[1..];
    let end = body
        .find('\'')
        .ok_or_else(|| error(format!("unterminated string in {rest:?}")))?;
    tokens.push(Token::Text(body[..end].to_string()));
    Ok(end + 2)
}

fn number(rest: &str, tokens: &mut Vec<Token>) -> Result<usize, ContractError> {
    let end = rest
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(rest.len());
    let digits = rest[..end].trim_end_matches('.');
    let token = if digits.contains('.') {
        Token::Decimal(
            digits
                .parse()
                .map_err(|_| error(format!("{digits:?} is not a number")))?,
        )
    } else {
        Token::Integer(
            digits
                .parse()
                .map_err(|_| error(format!("{digits:?} is not a number")))?,
        )
    };
    tokens.push(token);
    Ok(digits.len())
}

fn identifier(rest: &str, tokens: &mut Vec<Token>) -> usize {
    let end = rest
        .find(|character: char| !character.is_alphanumeric() && character != '_')
        .unwrap_or(rest.len());
    tokens.push(Token::Identifier(rest[..end].to_string()));
    end
}

pub(crate) fn error(message: impl std::fmt::Display) -> ContractError {
    ContractError {
        message: format!("FHIRPath: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_every_kind() {
        let tokens = tokenize("Patient.name[0].where(use = 'official') != 2.5").expect("lexes");
        assert_eq!(
            tokens,
            vec![
                Token::Identifier("Patient".into()),
                Token::Dot,
                Token::Identifier("name".into()),
                Token::OpenBracket,
                Token::Integer(0),
                Token::CloseBracket,
                Token::Dot,
                Token::Identifier("where".into()),
                Token::OpenParen,
                Token::Identifier("use".into()),
                Token::Equal,
                Token::Text("official".into()),
                Token::CloseParen,
                Token::NotEqual,
                Token::Decimal(2.5),
            ]
        );
    }

    #[test]
    fn a_trailing_point_belongs_to_the_next_step_not_the_number() {
        let tokens = tokenize("x[1].y").expect("lexes");
        assert_eq!(tokens[2], Token::Integer(1));
        assert_eq!(tokens[4], Token::Dot);
    }

    #[test]
    fn refuses_stray_characters_and_open_strings() {
        let stray = tokenize("a # b").expect_err("refused");
        assert!(stray.message.contains("'#'"), "{}", stray.message);
        assert!(tokenize("a = 'open").is_err());
        assert!(tokenize("a ! b").is_err());
    }
}
