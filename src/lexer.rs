//! The tokens of a `FHIRPath` expression.
//!
//! `FHIRPath` quotes strings with single quotes, and a number is an integer or a
//! decimal by whether it carries a point. Identifiers cover both member names
//! and function names; the parser tells them apart by the parenthesis that
//! follows a function.

use codec::char_reader::CharReader;
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

/// Split `expression` into tokens. Whitespace is any Unicode whitespace, and
/// an identifier may hold any letter.
///
/// # Errors
/// A character that begins no token, an unterminated string, or a number that
/// does not parse.
pub fn tokenize(expression: &str) -> Result<Vec<Token>, ContractError> {
    let mut reader = CharReader::new(expression);
    let mut tokens = Vec::new();
    while let Some(first) = reader.peek() {
        if reader.skip_whitespace() {
            continue;
        }
        let token = match first {
            '.' => single(&mut reader, Token::Dot),
            '[' => single(&mut reader, Token::OpenBracket),
            ']' => single(&mut reader, Token::CloseBracket),
            '(' => single(&mut reader, Token::OpenParen),
            ')' => single(&mut reader, Token::CloseParen),
            '=' => single(&mut reader, Token::Equal),
            '!' if reader.eat_str("!=") => Token::NotEqual,
            '\'' => text(&mut reader)?,
            character if character.is_ascii_digit() => number(&mut reader)?,
            character if character.is_alphabetic() || character == '_' => {
                let name = reader.take_while(|next| next.is_alphanumeric() || next == '_');
                Token::Identifier(name.to_string())
            }
            other => {
                return Err(error(format!(
                    "unexpected {other:?} at {} in {expression:?}",
                    reader.offset()
                )));
            }
        };
        tokens.push(token);
    }
    Ok(tokens)
}

fn single(reader: &mut CharReader<'_>, token: Token) -> Token {
    reader.bump();
    token
}

fn text(reader: &mut CharReader<'_>) -> Result<Token, ContractError> {
    let start = reader.offset();
    reader.bump();
    let body = reader.take_while(|character| character != '\'');
    if !reader.eat('\'') {
        return Err(error(format!(
            "unterminated string in {:?}",
            reader.since(start)
        )));
    }
    Ok(Token::Text(body.to_string()))
}

/// Digits and points; a trailing point belongs to the next step.
fn number(reader: &mut CharReader<'_>) -> Result<Token, ContractError> {
    let digits = reader
        .peek_while(|character| character.is_ascii_digit() || character == '.')
        .trim_end_matches('.');
    reader.eat_str(digits);
    let not_a_number = || error(format!("{digits:?} is not a number"));
    Ok(if digits.contains('.') {
        Token::Decimal(digits.parse().map_err(|_| not_a_number())?)
    } else {
        Token::Integer(digits.parse().map_err(|_| not_a_number())?)
    })
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

    #[test]
    fn multibyte_whitespace_and_letters_lex_without_panic() {
        let tokens =
            tokenize("Patient\u{a0}.\u{3000}naam\u{2003}=\u{a0}'Zoë 名前'").expect("lexes");
        assert_eq!(
            tokens,
            vec![
                Token::Identifier("Patient".into()),
                Token::Dot,
                Token::Identifier("naam".into()),
                Token::Equal,
                Token::Text("Zoë 名前".into()),
            ]
        );
        assert_eq!(
            tokenize("Straße.名前").expect("lexes"),
            vec![
                Token::Identifier("Straße".into()),
                Token::Dot,
                Token::Identifier("名前".into()),
            ]
        );
        let stray = tokenize("a\u{a0}€").expect_err("refused");
        assert!(stray.message.contains("'€' at 3"), "{}", stray.message);
        assert!(tokenize("a = 'öpen\u{a0}").is_err());
    }
}
