#![forbid(unsafe_code)]

//! The `FHIRPath` path technology — a technology of `xmip-core-path`.
//!
//! [`FhirPathLanguage`] is the [`PathLanguage`] `fhirpath`: it compiles an
//! [`Expression`] once, and the compiled expression reads from a FHIR JSON
//! resource (the `fhir` contract's representation) and writes into a rewrite
//! of it (ADR-0013). Promote reads through it; demote writes through it; route
//! and process read. The JSON document itself — parsed once per Message,
//! bridged to a scalar, written back as a Stream, written at a pointer — is
//! the capability's `json`, shared with the other JSON languages (ADR-0044);
//! what is this technology's is the expression and the resource it must be.
//!
//! The language is the normative core of `FHIRPath` that promotion needs: a
//! leading resource type that must match `resourceType` or be omitted, member
//! navigation with collection semantics, `[n]`, `first()`, `last()`,
//! `count()`, `exists()`, `empty()`, `where(member = literal)`,
//! `select(member)` and a top-level `=` or `!=` against a literal. A read
//! yields the first item of the resulting collection as one scalar, nothing
//! when the collection is empty, and refuses an object or array rather than
//! stringify it, because a promoted property is one value, not a document.
//! A write replaces the first item the path addresses, or adds the member
//! when it is missing from an object the path reaches; a path that computes
//! — `count()`, `exists()`, `empty()`, a comparison — names no place and is
//! refused.

mod eval;
mod expression;
mod lexer;

pub use expression::{Comparison, Expression, Literal, Step};

use contract::ContractError;
use eval::{evaluate, member_pointer};
use lexer::error;
use path::{CompiledExpression, Content, PathLanguage, Rewriting, json};
use serde_json::Value;
use xcore::ScalarValue;

/// The language `fhirpath`.
pub struct FhirPathLanguage;

impl PathLanguage for FhirPathLanguage {
    fn language(&self) -> &'static str {
        "fhirpath"
    }

    fn compile(&self, expression: &str) -> Result<Box<dyn CompiledExpression>, ContractError> {
        Ok(Box::new(Compiled {
            parsed: Expression::parse(expression)?,
            expression: expression.to_string(),
        }))
    }
}

/// An expression compiled, with the text a refusal names.
struct Compiled {
    parsed: Expression,
    expression: String,
}

/// A JSON document is a resource when it is an object.
fn resource(document: &Value) -> Result<&Value, ContractError> {
    if document.is_object() {
        Ok(document)
    } else {
        Err(error("the Stream is JSON but not a resource"))
    }
}

impl Compiled {
    /// The pointer the expression addresses in `resource`: the first item when
    /// there is one, else the missing member of the first object the steps
    /// before it reach.
    fn place(&self, resource: &Value) -> Result<String, ContractError> {
        let path = &self.expression;
        let expression = &self.parsed;
        let computes = expression.comparison.is_some()
            || expression
                .steps
                .iter()
                .any(|step| !step.addresses_content());
        if computes {
            return Err(error(format!(
                "{path:?} computes a value; a write needs a place in the resource"
            )));
        }
        if let Some(first) = evaluate(expression, resource)?.first() {
            if first.value.is_object() || first.value.is_array() {
                return Err(error(format!("{path} is not a scalar")));
            }
            return first
                .pointer
                .clone()
                .ok_or_else(|| error(format!("{path:?} addresses nothing to replace")));
        }
        let nothing = || error(format!("{path:?} addresses nothing in the resource"));
        let Some((Step::Member(name), parents)) = expression.steps.split_last() else {
            return Err(nothing());
        };
        let parent = Expression {
            resource_type: expression.resource_type.clone(),
            steps: parents.to_vec(),
            comparison: None,
        };
        match evaluate(&parent, resource)?.first() {
            Some(object) if object.value.is_object() => Ok(member_pointer(
                object.pointer.as_deref().unwrap_or_default(),
                name,
            )),
            _ => Err(nothing()),
        }
    }
}

impl CompiledExpression for Compiled {
    fn read(&self, content: &Content<'_>) -> Result<Option<ScalarValue>, ContractError> {
        let document = content.form::<Value>()?;
        let items = evaluate(&self.parsed, resource(&document)?)?;
        items
            .first()
            .map(|item| {
                json::scalar(&item.value, &self.expression)
                    .map_err(|refused| error(refused.message))
            })
            .transpose()
    }

    /// Replace the first scalar the expression addresses, or add the member
    /// when it reaches an object that lacks it. A path into nothing is
    /// refused: demote names a place, it does not invent structure.
    fn write(&self, rewriting: &mut Rewriting, value: ScalarValue) -> Result<(), ContractError> {
        let document = rewriting.form_mut::<Value>()?;
        let pointer = self.place(resource(document)?)?;
        json::set_at_pointer(document, &pointer, json::from_scalar(value)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use contract::fixture::stream;
    use xcore::StreamId;

    const PATIENT: &str = r#"{"resourceType":"Patient","id":"p1","active":true,
        "name":[{"use":"official","family":"Chalmers","given":["Peter","James"]},
                {"use":"usual","given":["Jim"]}],
        "identifier":[{"system":"urn:mrn","value":"12345"}],
        "deceasedBoolean":false,"multipleBirthInteger":2,"weight":72.5}"#;

    fn compiled(path: &str) -> Result<Box<dyn CompiledExpression>, ContractError> {
        FhirPathLanguage.compile(path)
    }

    fn read(content: &Content<'_>, path: &str) -> Result<Option<ScalarValue>, ContractError> {
        compiled(path)?.read(content)
    }

    fn write(
        rewriting: &mut Rewriting,
        path: &str,
        value: ScalarValue,
    ) -> Result<(), ContractError> {
        compiled(path)?.write(rewriting, value)
    }

    #[test]
    fn reads_the_first_item_as_a_scalar_and_refuses_structures() {
        let patient = stream(PATIENT);
        let content = Content::of(&patient);
        assert_eq!(
            read(&content, "Patient.name.given").expect("reads"),
            Some(ScalarValue::Text("Peter".into()))
        );
        assert_eq!(
            read(&content, "Patient.name.where(use = 'usual').given.first()").expect("reads"),
            Some(ScalarValue::Text("Jim".into()))
        );
        assert_eq!(
            read(&content, "identifier.where(system = 'urn:mrn').value").expect("reads"),
            Some(ScalarValue::Text("12345".into()))
        );
        assert_eq!(
            read(&content, "Patient.active").expect("reads"),
            Some(ScalarValue::Bool(true))
        );
        assert_eq!(
            read(&content, "multipleBirthInteger").expect("reads"),
            Some(ScalarValue::Integer(2))
        );
        assert_eq!(
            read(&content, "weight").expect("reads"),
            Some(ScalarValue::Decimal(72.5))
        );
        assert_eq!(
            read(&content, "name.given.count()").expect("reads"),
            Some(ScalarValue::Integer(3))
        );
        assert_eq!(
            read(&content, "name.exists()").expect("reads"),
            Some(ScalarValue::Bool(true))
        );
        assert_eq!(
            read(&content, "deceasedBoolean = false").expect("reads"),
            Some(ScalarValue::Bool(true))
        );
        assert_eq!(read(&content, "Patient.birthDate").expect("reads"), None);
        assert_eq!(read(&content, "Patient.name[5]").expect("reads"), None);
        let structure_read = read(&content, "Patient.name").expect_err("refused");
        assert_eq!(
            structure_read.message,
            "FHIRPath: Patient.name is not a scalar"
        );
        assert!(read(&content, "Observation.value").is_err());
        assert!(read(&content, "Patient.name.trim()").is_err());
    }

    #[test]
    fn rewrites_the_first_addressed_scalar_into_a_new_stream() {
        let mut rewriting = Rewriting::of(&stream(PATIENT), StreamId::new(2));
        let mut write = |path: &str, value: ScalarValue| write(&mut rewriting, path, value);
        write("Patient.name.given", ScalarValue::Text("Pete".into())).expect("replaces");
        write(
            "Patient.name.where(use = 'usual').given.last()",
            ScalarValue::Text("Jimmy".into()),
        )
        .expect("replaces");
        write("Patient.active", ScalarValue::Bool(false)).expect("replaces");
        write("Patient.birthDate", ScalarValue::Text("1974-12-25".into())).expect("adds");
        write(
            "Patient.name[1].family",
            ScalarValue::Text("Chalmers".into()),
        )
        .expect("adds");
        let out = rewriting.finish().expect("finishes");
        assert_eq!(out.id(), StreamId::new(2));
        assert_eq!(out.media_type(), Some("application/json"));
        let back = Content::of(&out);
        assert_eq!(
            read(&back, "name.given").expect("reads"),
            Some(ScalarValue::Text("Pete".into()))
        );
        assert_eq!(
            read(&back, "name[0].given[1]").expect("reads"),
            Some(ScalarValue::Text("James".into()))
        );
        assert_eq!(
            read(&back, "name[1].given").expect("reads"),
            Some(ScalarValue::Text("Jimmy".into()))
        );
        assert_eq!(
            read(&back, "active").expect("reads"),
            Some(ScalarValue::Bool(false))
        );
        assert_eq!(
            read(&back, "birthDate").expect("reads"),
            Some(ScalarValue::Text("1974-12-25".into()))
        );
        assert_eq!(
            read(&back, "name.where(use = 'usual').family").expect("reads"),
            Some(ScalarValue::Text("Chalmers".into()))
        );
    }

    #[test]
    fn a_write_that_computes_or_addresses_nothing_is_refused() {
        let mut rewriting = Rewriting::of(&stream(PATIENT), StreamId::new(3));
        let computes = write(
            &mut rewriting,
            "Patient.name.count()",
            ScalarValue::Integer(0),
        )
        .expect_err("refused");
        assert_eq!(
            computes.message,
            "FHIRPath: \"Patient.name.count()\" computes a value; a write needs a place in \
             the resource"
        );
        assert!(write(&mut rewriting, "active = true", ScalarValue::Bool(true)).is_err());
        assert!(write(&mut rewriting, "Patient.name", ScalarValue::Null).is_err());
        let nowhere = write(
            &mut rewriting,
            "Patient.contact.name.family",
            ScalarValue::Null,
        )
        .expect_err("refused");
        assert_eq!(
            nowhere.message,
            "FHIRPath: \"Patient.contact.name.family\" addresses nothing in the resource"
        );
        assert!(write(&mut rewriting, "Patient.name[9].family", ScalarValue::Null).is_err());
        assert!(write(&mut rewriting, "Patient.id", ScalarValue::Binary(vec![1])).is_err());
    }

    #[test]
    fn a_stream_that_is_not_a_resource_is_refused() {
        let broken = stream("{nope");
        assert!(read(&Content::of(&broken), "id").is_err());
        let list = stream("[1,2]");
        let refused = read(&Content::of(&list), "id").expect_err("refused");
        assert_eq!(
            refused.message,
            "FHIRPath: the Stream is JSON but not a resource"
        );
        let mut rewriting = Rewriting::of(&list, StreamId::new(1));
        assert!(write(&mut rewriting, "id", ScalarValue::Null).is_err());
    }
}
