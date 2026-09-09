//! The shape of a `FHIRPath` expression and the parser that builds it.
//!
//! The normative core that promotion needs, and no more: a path of steps,
//! optionally under a resource type and optionally compared with a literal.
//! A resource type is told from a member by its case — FHIR names every
//! resource type in upper camel case and every element in lower camel case,
//! so `Patient.name` is the type and the member, never two members.

use crate::lexer::{Token, error, tokenize};
use contract::ContractError;

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
        let mut parser = Parser {
            tokens: &tokens,
            at: 0,
        };
        let expression = parser.expression()?;
        match parser.tokens.get(parser.at) {
            None => Ok(expression),
            Some(token) => Err(error(format!("unexpected {token:?} after the expression"))),
        }
    }
}

struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
}

impl Parser<'_> {
    fn expression(&mut self) -> Result<Expression, ContractError> {
        let (resource_type, mut steps) = match self.next() {
            Some(Token::Identifier(name)) if name.starts_with(char::is_uppercase) => {
                (Some(name.clone()), Vec::new())
            }
            Some(Token::Identifier(name)) => (None, vec![Step::Member(name.clone())]),
            other => return Err(error(format!("expected a name to start, found {other:?}"))),
        };
        loop {
            match self.peek() {
                Some(Token::Dot) => {
                    self.at += 1;
                    steps.push(self.step()?);
                }
                Some(Token::OpenBracket) => {
                    self.at += 1;
                    steps.push(Step::Index(self.index()?));
                }
                _ => break,
            }
        }
        let comparison = match self.peek() {
            Some(Token::Equal) => Some(Comparison::Equal),
            Some(Token::NotEqual) => Some(Comparison::NotEqual),
            _ => None,
        }
        .map(|comparison| {
            self.at += 1;
            self.literal().map(|literal| (comparison, literal))
        })
        .transpose()?;
        Ok(Expression {
            resource_type,
            steps,
            comparison,
        })
    }

    fn step(&mut self) -> Result<Step, ContractError> {
        let name = match self.next() {
            Some(Token::Identifier(name)) => name.clone(),
            other => return Err(error(format!("expected a step after '.', found {other:?}"))),
        };
        if self.peek() != Some(&Token::OpenParen) {
            return Ok(Step::Member(name));
        }
        self.at += 1;
        let step = match name.as_str() {
            "first" => Step::First,
            "last" => Step::Last,
            "count" => Step::Count,
            "exists" => Step::Exists,
            "empty" => Step::Empty,
            "where" => self.criterion()?,
            "select" => Step::Select(self.member()?),
            other => return Err(error(format!("{other}() is not a function this core has"))),
        };
        self.expect(&Token::CloseParen)?;
        Ok(step)
    }

    fn criterion(&mut self) -> Result<Step, ContractError> {
        let member = self.member()?;
        let comparison = match self.next() {
            Some(Token::Equal) => Comparison::Equal,
            Some(Token::NotEqual) => Comparison::NotEqual,
            other => return Err(error(format!("where() needs = or !=, found {other:?}"))),
        };
        let literal = self.literal()?;
        Ok(Step::Where {
            member,
            comparison,
            literal,
        })
    }

    fn member(&mut self) -> Result<String, ContractError> {
        match self.next() {
            Some(Token::Identifier(name)) => Ok(name.clone()),
            other => Err(error(format!("expected a member name, found {other:?}"))),
        }
    }

    fn index(&mut self) -> Result<usize, ContractError> {
        let index = match self.next() {
            Some(Token::Integer(index)) => {
                usize::try_from(*index).map_err(|_| error(format!("[{index}] is not an index")))?
            }
            other => return Err(error(format!("expected an index in [], found {other:?}"))),
        };
        self.expect(&Token::CloseBracket)?;
        Ok(index)
    }

    fn literal(&mut self) -> Result<Literal, ContractError> {
        match self.next() {
            Some(Token::Text(text)) => Ok(Literal::Text(text.clone())),
            Some(Token::Integer(integer)) => Ok(Literal::Integer(*integer)),
            Some(Token::Decimal(decimal)) => Ok(Literal::Decimal(*decimal)),
            Some(Token::Identifier(word)) if word == "true" => Ok(Literal::Bool(true)),
            Some(Token::Identifier(word)) if word == "false" => Ok(Literal::Bool(false)),
            other => Err(error(format!("expected a literal, found {other:?}"))),
        }
    }

    fn expect(&mut self, token: &Token) -> Result<(), ContractError> {
        match self.next() {
            Some(found) if found == token => Ok(()),
            other => Err(error(format!("expected {token:?}, found {other:?}"))),
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<&Token> {
        let token = self.tokens.get(self.at);
        self.at += 1;
        token
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
