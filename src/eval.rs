//! Evaluation of a parsed expression over a FHIR JSON resource.
//!
//! `FHIRPath` works on collections: every step takes the collection the steps
//! before it produced and yields another. A member step reaches into every
//! object in the collection and flattens the arrays it finds, which is how
//! `Patient.name.given` names every given name across every name. An item
//! that came from the resource remembers where, as a JSON pointer, so a write
//! can replace it in place; an item a function computed — a count, an
//! existence — has no place and no pointer.

use crate::expression::{Comparison, Expression, Literal, Step};
use crate::lexer::error;
use contract::ContractError;
use serde_json::Value;
use std::borrow::Cow;

/// One item of a collection: a value, and where in the resource it sits when
/// it sits anywhere.
#[derive(Debug)]
pub struct Item<'a> {
    /// The value; borrowed from the resource, or owned when computed.
    pub value: Cow<'a, Value>,
    /// The JSON pointer to the value in the resource; `None` when computed.
    pub pointer: Option<String>,
}

impl<'a> Item<'a> {
    fn at(value: &'a Value, pointer: String) -> Self {
        Self {
            value: Cow::Borrowed(value),
            pointer: Some(pointer),
        }
    }

    fn computed(value: Value) -> Self {
        Self {
            value: Cow::Owned(value),
            pointer: None,
        }
    }
}

/// Evaluate `expression` against `resource`, yielding the resulting collection.
///
/// # Errors
/// The expression names a resource type and the resource's `resourceType` is
/// another, or absent.
pub fn evaluate<'a>(
    expression: &Expression,
    resource: &'a Value,
) -> Result<Vec<Item<'a>>, ContractError> {
    if let Some(expected) = &expression.resource_type {
        let actual = resource.get("resourceType").and_then(Value::as_str);
        if actual != Some(expected.as_str()) {
            return Err(error(format!(
                "the expression is for {expected}, the resource is {}",
                actual.unwrap_or("of no type")
            )));
        }
    }
    let mut collection = vec![Item::at(resource, String::new())];
    for step in &expression.steps {
        collection = apply(step, collection);
    }
    Ok(match &expression.comparison {
        None => collection,
        Some((comparison, literal)) => compare(&collection, *comparison, literal)
            .map(|outcome| Item::computed(Value::Bool(outcome)))
            .into_iter()
            .collect(),
    })
}

fn apply<'a>(step: &Step, collection: Vec<Item<'a>>) -> Vec<Item<'a>> {
    match step {
        Step::Member(name) | Step::Select(name) => collection
            .iter()
            .flat_map(|item| member(item, name))
            .collect(),
        Step::Index(index) => collection.into_iter().nth(*index).into_iter().collect(),
        Step::First => collection.into_iter().take(1).collect(),
        Step::Last => collection.into_iter().last().into_iter().collect(),
        Step::Count => vec![Item::computed(Value::from(collection.len()))],
        Step::Exists => vec![Item::computed(Value::Bool(!collection.is_empty()))],
        Step::Empty => vec![Item::computed(Value::Bool(collection.is_empty()))],
        Step::Where {
            member: name,
            comparison,
            literal,
        } => collection
            .into_iter()
            .filter(|item| compare(&member(item, name), *comparison, literal) == Some(true))
            .collect(),
    }
}

/// The member `name` of `item`, its elements when it is an array. A computed
/// item is a scalar and has no members; so has a scalar from the resource.
fn member<'a>(item: &Item<'a>, name: &str) -> Vec<Item<'a>> {
    let Cow::Borrowed(Value::Object(members)) = &item.value else {
        return Vec::new();
    };
    let pointer = member_pointer(item.pointer.as_deref().unwrap_or_default(), name);
    match members.get(name) {
        Some(Value::Array(elements)) => elements
            .iter()
            .enumerate()
            .map(|(index, element)| Item::at(element, format!("{pointer}/{index}")))
            .collect(),
        Some(found) => vec![Item::at(found, pointer)],
        None => Vec::new(),
    }
}

/// The JSON pointer of member `name` under `parent`, RFC 6901 escaping applied.
#[must_use]
pub fn member_pointer(parent: &str, name: &str) -> String {
    format!("{parent}/{}", name.replace('~', "~0").replace('/', "~1"))
}

/// `FHIRPath` equality between a collection and one literal: empty says nothing,
/// a single item compares, and more than one item is never equal to one.
fn compare(collection: &[Item<'_>], comparison: Comparison, literal: &Literal) -> Option<bool> {
    let equal = match collection {
        [] => return None,
        [only] => equals(&only.value, literal),
        _ => false,
    };
    Some(match comparison {
        Comparison::Equal => equal,
        Comparison::NotEqual => !equal,
    })
}

fn equals(value: &Value, literal: &Literal) -> bool {
    match (value, literal) {
        (Value::String(text), Literal::Text(expected)) => text == expected,
        (Value::Bool(flag), Literal::Bool(expected)) => flag == expected,
        (Value::Number(number), Literal::Integer(expected)) => match number.as_i64() {
            Some(integer) => integer == *expected,
            // A whole number written with a point, `2.0 = 2`; the widening is
            // exact for anything a FHIR resource holds.
            #[allow(clippy::cast_precision_loss)]
            None => number.as_f64() == Some(*expected as f64),
        },
        (Value::Number(number), Literal::Decimal(expected)) => number.as_f64() == Some(*expected),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn patient() -> Value {
        json!({
            "resourceType": "Patient",
            "id": "p1",
            "active": true,
            "name": [
                {"use": "official", "family": "Chalmers", "given": ["Peter", "James"]},
                {"use": "usual", "given": ["Jim"]}
            ],
            "identifier": [{"system": "urn:mrn", "value": "12345"}],
            "multipleBirthInteger": 2
        })
    }

    fn values(expression: &str, resource: &Value) -> Vec<Value> {
        let parsed = Expression::parse(expression).expect("parses");
        evaluate(&parsed, resource)
            .expect("evaluates")
            .into_iter()
            .map(|item| item.value.into_owned())
            .collect()
    }

    #[test]
    fn a_member_through_arrays_flattens_and_remembers_where() {
        let resource = patient();
        assert_eq!(
            values("Patient.name.given", &resource),
            vec![json!("Peter"), json!("James"), json!("Jim")]
        );
        let parsed = Expression::parse("name.given").expect("parses");
        let items = evaluate(&parsed, &resource).expect("evaluates");
        assert_eq!(items[2].pointer.as_deref(), Some("/name/1/given/0"));
        assert_eq!(
            values("Patient.name.family", &resource),
            vec![json!("Chalmers")]
        );
        assert!(values("Patient.nowhere.deeper", &resource).is_empty());
    }

    #[test]
    fn index_first_last_where_and_select_narrow_the_collection() {
        let resource = patient();
        assert_eq!(values("name[1].given", &resource), vec![json!("Jim")]);
        assert_eq!(
            values("name.given.first()", &resource),
            vec![json!("Peter")]
        );
        assert_eq!(values("name.given.last()", &resource), vec![json!("Jim")]);
        assert_eq!(
            values("name.where(use = 'usual').given", &resource),
            vec![json!("Jim")]
        );
        assert_eq!(
            values("name.where(use != 'usual').select(family)", &resource),
            vec![json!("Chalmers")]
        );
        assert!(values("name.where(family = 'Nobody')", &resource).is_empty());
        assert!(values("name[7]", &resource).is_empty());
    }

    #[test]
    fn count_exists_empty_and_comparisons_compute_values_without_a_place() {
        let resource = patient();
        assert_eq!(values("name.given.count()", &resource), vec![json!(3)]);
        assert_eq!(values("name.exists()", &resource), vec![json!(true)]);
        assert_eq!(values("nowhere.empty()", &resource), vec![json!(true)]);
        assert_eq!(values("active = true", &resource), vec![json!(true)]);
        assert_eq!(
            values("name.given.count() = 3", &resource),
            vec![json!(true)]
        );
        assert_eq!(
            values("multipleBirthInteger != 2.0", &resource),
            vec![json!(false)]
        );
        assert_eq!(
            values("name.given = 'Peter'", &resource),
            vec![json!(false)]
        );
        assert!(values("nowhere = 'x'", &resource).is_empty());
        let parsed = Expression::parse("name.count()").expect("parses");
        let items = evaluate(&parsed, &resource).expect("evaluates");
        assert_eq!(items[0].pointer, None);
    }

    #[test]
    fn the_resource_type_must_agree_when_named() {
        let resource = patient();
        let parsed = Expression::parse("Observation.value").expect("parses");
        let refused = evaluate(&parsed, &resource).expect_err("refused");
        assert_eq!(
            refused.message,
            "FHIRPath: the expression is for Observation, the resource is Patient"
        );
        let untyped = Expression::parse("Patient.id").expect("parses");
        assert!(evaluate(&untyped, &json!({"id": "x"})).is_err());
    }

    #[test]
    fn member_pointers_escape_as_rfc_6901_asks() {
        assert_eq!(member_pointer("", "name"), "/name");
        assert_eq!(member_pointer("/a", "b/c~d"), "/a/b~1c~0d");
    }
}
