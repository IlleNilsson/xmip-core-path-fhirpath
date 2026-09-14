//! The shape of a `FHIRPath` expression and the parser that builds it.
//!
//! The normative core that promotion needs, and no more: a path of steps,
//! optionally under a resource type and optionally compared with a literal.
//! A resource type is told from a member by its case — FHIR names every
//! resource type in upper camel case and every element in lower camel case,
//! so `Patient.name` is the type and the member, never two members.
//!
//! The cursor the parser walks its tokens with is the capability's, shared
//! with the predicate language (ADR-0044); the grammar below is `FHIRPath`'s.

use crate::lexer::{Token, error, tokenize};
use contract::ContractError;
use path::cursor::Cursor;

/// A literal on the right of `=` or `!=`.
#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    /// `'text'`
    Text(String),
    /// `42`
    Integer(i64),
    /// `4.2`
    Decimal(f64),
    /// `true` or `false`
    Bool(bool),
}

/// The two comparisons the core supports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Comparison {
    /// `=`
    Equal,
    /// `!=`
    NotEqual,
}

/// One step along a path, applied to the collection the steps before it made.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// `.name`: the member of every object in the collection, arrays flattened.
    Member(String),
    /// `[n]`: the nth item of the collection.
    Index(usize),
    /// `.first()`
    First,
    /// `.last()`
    Last,
    /// `.count()`
    Count,
    /// `.exists()`
    Exists,
    /// `.empty()`
    Empty,
    /// `.where(member = literal)`: the items whose member compares so.
    Where {
        /// The member of each item that is compared.
        member: String,
        /// `=` or `!=`.
        comparison: Comparison,
        /// What the member is compared with.
        literal: Literal,
    },
    /// `.select(member)`: the member of every item, flattened.
    Select(String),
}

impl Step {
    /// Whether the step names content — a place in the resource a write can
    /// replace — rather than computing a value that is nowhere in it.
    #[must_use]
    pub fn addresses_content(&self) -> bool {
        !matches!(self, Self::Count | Self::Exists | Self::Empty)
    }
}

/// A parsed expression.
#[derive(Clone, Debug, PartialEq)]
pub struct Expression {
    /// The resource type the expression is for, when it names one; the
    /// resource's `resourceType` must agree.
    pub resource_type: Option<String>,
    /// The steps, in order.
    pub steps: Vec<Step>,
    /// The comparison at the top level, when there is one.
    pub comparison: Option<(Comparison, Literal)>,
}

impl Expression {
    /// Parse `text`.
    ///
    /// # Errors
    /// The text does not lex, or is not `[Type][.step]*[[n]]* [(=|!=) literal]`
    /// with the steps this core supports.
    pub fn parse(text: &str) -> Result<Self, ContractError> {
        let tokens = tokenize(text)?;
        let mut cursor = Cursor::new("FHIRPath", &tokens);
        let parsed = expression(&mut cursor)?;
        match cursor.peek() {
            None => Ok(parsed),
            Some(token) => Err(error(format!("unexpected {token:?} after the expression"))),
        }
    }
}

type Tokens<'a> = Cursor<'a, Token>;

fn expression(cursor: &mut Tokens<'_>) -> Result<Expression, ContractError> {
    let (resource_type, mut steps) = match cursor.take() {
        Some(Token::Identifier(name)) if name.starts_with(char::is_uppercase) => {
            (Some(name.clone()), Vec::new())
        }
        Some(Token::Identifier(name)) => (None, vec![Step::Member(name.clone())]),
        other => return Err(error(format!("expected a name to start, found {other:?}"))),
    };
    loop {
        match cursor.peek() {
            Some(Token::Dot) => {
                cursor.advance(1);
                steps.push(step(cursor)?);
            }
            Some(Token::OpenBracket) => {
                cursor.advance(1);
                steps.push(Step::Index(index(cursor)?));
            }
            _ => break,
        }
    }
    let comparison = match cursor.peek() {
        Some(Token::Equal) => Some(Comparison::Equal),
        Some(Token::NotEqual) => Some(Comparison::NotEqual),
        _ => None,
    }
    .map(|comparison| {
        cursor.advance(1);
        literal(cursor).map(|literal| (comparison, literal))
    })
    .transpose()?;
    Ok(Expression {
        resource_type,
        steps,
        comparison,
    })
}

fn step(cursor: &mut Tokens<'_>) -> Result<Step, ContractError> {
    let name = match cursor.take() {
        Some(Token::Identifier(name)) => name.clone(),
        other => return Err(error(format!("expected a step after '.', found {other:?}"))),
    };
    if cursor.peek() != Some(&Token::OpenParen) {
        return Ok(Step::Member(name));
    }
    cursor.advance(1);
    let step = match name.as_str() {
        "first" => Step::First,
        "last" => Step::Last,
        "count" => Step::Count,
        "exists" => Step::Exists,
        "empty" => Step::Empty,
        "where" => criterion(cursor)?,
        "select" => Step::Select(member(cursor)?),
        other => return Err(error(format!("{other}() is not a function this core has"))),
    };
    cursor.expect(&Token::CloseParen)?;
    Ok(step)
}

fn criterion(cursor: &mut Tokens<'_>) -> Result<Step, ContractError> {
    let member = member(cursor)?;
    let comparison = match cursor.take() {
        Some(Token::Equal) => Comparison::Equal,
        Some(Token::NotEqual) => Comparison::NotEqual,
        other => return Err(error(format!("where() needs = or !=, found {other:?}"))),
    };
    let literal = literal(cursor)?;
    Ok(Step::Where {
        member,
        comparison,
        literal,
    })
}

fn member(cursor: &mut Tokens<'_>) -> Result<String, ContractError> {
    match cursor.take() {
        Some(Token::Identifier(name)) => Ok(name.clone()),
        other => Err(error(format!("expected a member name, found {other:?}"))),
    }
}

fn index(cursor: &mut Tokens<'_>) -> Result<usize, ContractError> {
    let index = match cursor.take() {
        Some(Token::Integer(index)) => {
            usize::try_from(*index).map_err(|_| error(format!("[{index}] is not an index")))?
        }
        other => return Err(error(format!("expected an index in [], found {other:?}"))),
    };
    cursor.expect(&Token::CloseBracket)?;
    Ok(index)
}

fn literal(cursor: &mut Tokens<'_>) -> Result<Literal, ContractError> {
    match cursor.take() {
        Some(Token::Text(text)) => Ok(Literal::Text(text.clone())),
        Some(Token::Integer(integer)) => Ok(Literal::Integer(*integer)),
        Some(Token::Decimal(decimal)) => Ok(Literal::Decimal(*decimal)),
        Some(Token::Identifier(word)) if word == "true" => Ok(Literal::Bool(true)),
        Some(Token::Identifier(word)) if word == "false" => Ok(Literal::Bool(false)),
        other => Err(error(format!("expected a literal, found {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typed_path_with_every_step_kind() {
        let expression = Expression::parse("Patient.name.where(use = 'official')[0].given.first()")
            .expect("parses");
        assert_eq!(expression.resource_type.as_deref(), Some("Patient"));
        assert_eq!(
            expression.steps,
            vec![
                Step::Member("name".into()),
                Step::Where {
                    member: "use".into(),
                    comparison: Comparison::Equal,
                    literal: Literal::Text("official".into()),
                },
                Step::Index(0),
                Step::Member("given".into()),
                Step::First,
            ]
        );
        assert_eq!(expression.comparison, None);
    }

    #[test]
    fn a_lower_case_start_is_a_member_and_a_comparison_may_follow() {
        let expression = Expression::parse("name.count() != 2").expect("parses");
        assert_eq!(expression.resource_type, None);
        assert_eq!(
            expression.steps,
            vec![Step::Member("name".into()), Step::Count]
        );
        assert_eq!(
            expression.comparison,
            Some((Comparison::NotEqual, Literal::Integer(2)))
        );
        let flag = Expression::parse("active = true").expect("parses");
        assert_eq!(
            flag.comparison,
            Some((Comparison::Equal, Literal::Bool(true)))
        );
    }

    #[test]
    fn refuses_what_the_core_does_not_have() {
        let unknown = Expression::parse("Patient.name.trim()").expect_err("refused");
        assert_eq!(
            unknown.message,
            "FHIRPath: trim() is not a function this core has"
        );
        assert!(Expression::parse("Patient.name[x]").is_err());
        assert!(Expression::parse("Patient.name = ").is_err());
        assert!(Expression::parse("Patient.name 'x'").is_err());
        assert!(Expression::parse("").is_err());
        assert!(Expression::parse("Patient.where(a < 1)").is_err());
        let unclosed = Expression::parse("Patient.name[0").expect_err("refused");
        assert_eq!(
            unclosed.message,
            "FHIRPath: expected CloseBracket, found None"
        );
    }

    #[test]
    fn only_computed_steps_fail_to_address_content() {
        assert!(Step::Member("a".into()).addresses_content());
        assert!(Step::First.addresses_content());
        assert!(!Step::Count.addresses_content());
        assert!(!Step::Exists.addresses_content());
        assert!(!Step::Empty.addresses_content());
    }
}
