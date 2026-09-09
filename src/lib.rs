#![forbid(unsafe_code)]

//! The `FHIRPath` path technology — a technology of `xmip-core-path`.
//!
//! Three things, because a path language is nothing without content to address:
//! [`FhirPathEngine`], the [`PathEngine`] for the language `fhirpath`;
//! [`FhirStructure`], a [`StructureReader`] over a FHIR JSON resource (the
//! `fhir` contract's representation, `application/fhir+json`); and
//! [`FhirRewrite`], a [`StructureWriter`] that produces a new Stream with one
//! or more values replaced, as ADR-0013 asks of anything that changes content.
//! Promote reads through the first two; demote writes through the first and
//! third; route and process read.
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

use contract::{
    ContractDescriptor, ContractError, ContractId, StructureReader, StructureWriter,
    StructuredValue,
};
use eval::{evaluate, member_pointer};
use lexer::error;
use path::{Path, PathCost, PathEngine};
use serde_json::Value;
use stream::Stream;
use xcore::StreamId;

/// The `fhirpath` engine. The reader speaks `FHIRPath` already, so the engine
/// adds no traversal of its own.
pub struct FhirPathEngine;

impl PathEngine for FhirPathEngine {
    fn language(&self) -> &'static str {
        "fhirpath"
    }

    fn read(
        &self,
        reader: &dyn StructureReader,
        path: &Path,
    ) -> Result<Option<StructuredValue>, ContractError> {
        reader.read(&path.expression)
    }

    fn write(
        &self,
        writer: &mut dyn StructureWriter,
        path: &Path,
        value: StructuredValue,
    ) -> Result<(), ContractError> {
        writer.write(&path.expression, value)
    }

    /// A resource is parsed whole before any collection can be walked.
    fn cost(&self, _path: &Path) -> PathCost {
        PathCost::Materialized
    }
}

fn descriptor() -> ContractDescriptor {
    ContractDescriptor {
        id: ContractId("fhir".to_string()),
        version: "1".to_string(),
        representation: "application/fhir+json".to_string(),
    }
}

fn parse(stream: &Stream) -> Result<Value, ContractError> {
    let value: Value = serde_json::from_slice(stream.bytes())
        .map_err(|parse_error| error(format!("the Stream is not valid JSON: {parse_error}")))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(error("the Stream is JSON but not a resource"))
    }
}

/// The first item `path` yields from `resource`, as a scalar.
fn read_first(resource: &Value, path: &str) -> Result<Option<StructuredValue>, ContractError> {
    let expression = Expression::parse(path)?;
    let items = evaluate(&expression, resource)?;
    items
        .first()
        .map(|item| scalar(&item.value, path))
        .transpose()
}

/// A FHIR JSON resource, read by `FHIRPath`.
#[derive(Debug)]
pub struct FhirStructure {
    descriptor: ContractDescriptor,
    value: Value,
}

impl FhirStructure {
    /// Parse `stream` once; every read is an evaluation after that.
    ///
    /// # Errors
    /// The Stream is not JSON, or is JSON but not an object.
    pub fn parse(stream: &Stream) -> Result<Self, ContractError> {
        Ok(Self {
            descriptor: descriptor(),
            value: parse(stream)?,
        })
    }
}

impl StructureReader for FhirStructure {
    fn contract(&self) -> &ContractDescriptor {
        &self.descriptor
    }

    fn read(&self, path: &str) -> Result<Option<StructuredValue>, ContractError> {
        read_first(&self.value, path)
    }
}

/// A FHIR JSON resource being rewritten into a new Stream.
#[derive(Debug)]
pub struct FhirRewrite {
    descriptor: ContractDescriptor,
    id: StreamId,
    value: Value,
}

impl FhirRewrite {
    /// Start from `stream`; the Stream `finish` produces carries `id`.
    ///
    /// # Errors
    /// The Stream is not JSON, or is JSON but not an object.
    pub fn of(stream: &Stream, id: StreamId) -> Result<Self, ContractError> {
        Ok(Self {
            descriptor: descriptor(),
            id,
            value: parse(stream)?,
        })
    }

    /// The pointer `path` addresses: the first item when there is one, else
    /// the missing member of the first object the steps before it reach.
    fn place(&self, path: &str) -> Result<String, ContractError> {
        let expression = Expression::parse(path)?;
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
        if let Some(first) = evaluate(&expression, &self.value)?.first() {
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
        match evaluate(&parent, &self.value)?.first() {
            Some(object) if object.value.is_object() => Ok(member_pointer(
                object.pointer.as_deref().unwrap_or_default(),
                name,
            )),
            _ => Err(nothing()),
        }
    }
}

impl StructureWriter for FhirRewrite {
    fn contract(&self) -> &ContractDescriptor {
        &self.descriptor
    }

    /// Replace the first scalar `path` addresses, or add the member when the
    /// path reaches an object that lacks it. A path into nothing is refused:
    /// demote names a place, it does not invent structure.
    fn write(&mut self, path: &str, value: StructuredValue) -> Result<(), ContractError> {
        let pointer = self.place(path)?;
        let replacement = json(value)?;
        if let Some(existing) = self.value.pointer_mut(&pointer) {
            *existing = replacement;
            return Ok(());
        }
        let (parent, key) = pointer
            .rsplit_once('/')
            .ok_or_else(|| error(format!("{path:?} addresses nothing in the resource")))?;
        let key = key.replace("~1", "/").replace("~0", "~");
        match self.value.pointer_mut(parent) {
            Some(Value::Object(members)) => {
                members.insert(key, replacement);
                Ok(())
            }
            _ => Err(error(format!("{path:?} has no object to write into"))),
        }
    }

    fn finish(self: Box<Self>) -> Result<Stream, ContractError> {
        let bytes = serde_json::to_vec(&self.value).map_err(|serialise_error| {
            error(format!("cannot serialise JSON: {serialise_error}"))
        })?;
        Ok(Stream::new(
            self.id,
            bytes,
            Some(self.descriptor.representation),
        ))
    }
}

fn scalar(value: &Value, path: &str) -> Result<StructuredValue, ContractError> {
    Ok(match value {
        Value::Null => StructuredValue::Null,
        Value::Bool(flag) => StructuredValue::Bool(*flag),
        Value::Number(number) => match number.as_i64() {
            Some(integer) => StructuredValue::Integer(integer),
            None => StructuredValue::Decimal(number.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(text) => StructuredValue::Text(text.clone()),
        Value::Array(_) | Value::Object(_) => {
            return Err(error(format!("{path} is not a scalar")));
        }
    })
}

fn json(value: StructuredValue) -> Result<Value, ContractError> {
    Ok(match value {
        StructuredValue::Null => Value::Null,
        StructuredValue::Bool(flag) => Value::Bool(flag),
        StructuredValue::Integer(integer) => Value::from(integer),
        StructuredValue::Decimal(decimal) => {
            serde_json::Number::from_f64(decimal).map_or(Value::Null, Value::Number)
        }
        StructuredValue::Text(text) => Value::String(text),
        StructuredValue::Binary(_) => {
            return Err(error("binary has no JSON form here"));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(text: &str) -> Stream {
        Stream::new(StreamId::new(1), text.as_bytes().to_vec(), None)
    }

    const PATIENT: &str = r#"{"resourceType":"Patient","id":"p1","active":true,
        "name":[{"use":"official","family":"Chalmers","given":["Peter","James"]},
                {"use":"usual","given":["Jim"]}],
        "identifier":[{"system":"urn:mrn","value":"12345"}],
        "deceasedBoolean":false,"multipleBirthInteger":2,"weight":72.5}"#;

    fn read(
        structure: &FhirStructure,
        path: &str,
    ) -> Result<Option<StructuredValue>, ContractError> {
        FhirPathEngine.read(structure, &Path::new("fhirpath", path))
    }

    #[test]
    fn reads_the_first_item_as_a_scalar_and_refuses_structures() {
        let structure = FhirStructure::parse(&stream(PATIENT)).expect("parses");
        assert_eq!(
            read(&structure, "Patient.name.given").expect("reads"),
            Some(StructuredValue::Text("Peter".into()))
        );
        assert_eq!(
            read(
                &structure,
                "Patient.name.where(use = 'usual').given.first()"
            )
            .expect("reads"),
            Some(StructuredValue::Text("Jim".into()))
        );
        assert_eq!(
            read(&structure, "identifier.where(system = 'urn:mrn').value").expect("reads"),
            Some(StructuredValue::Text("12345".into()))
        );
        assert_eq!(
            read(&structure, "Patient.active").expect("reads"),
            Some(StructuredValue::Bool(true))
        );
        assert_eq!(
            read(&structure, "multipleBirthInteger").expect("reads"),
            Some(StructuredValue::Integer(2))
        );
        assert_eq!(
            read(&structure, "weight").expect("reads"),
            Some(StructuredValue::Decimal(72.5))
        );
        assert_eq!(
            read(&structure, "name.given.count()").expect("reads"),
            Some(StructuredValue::Integer(3))
        );
        assert_eq!(
            read(&structure, "name.exists()").expect("reads"),
            Some(StructuredValue::Bool(true))
        );
        assert_eq!(
            read(&structure, "deceasedBoolean = false").expect("reads"),
            Some(StructuredValue::Bool(true))
        );
        assert_eq!(read(&structure, "Patient.birthDate").expect("reads"), None);
        assert_eq!(read(&structure, "Patient.name[5]").expect("reads"), None);
        let structure_read = read(&structure, "Patient.name").expect_err("refused");
        assert_eq!(
            structure_read.message,
            "FHIRPath: Patient.name is not a scalar"
        );
        assert!(read(&structure, "Observation.value").is_err());
        assert!(read(&structure, "Patient.name.trim()").is_err());
        assert_eq!(
            FhirPathEngine.cost(&Path::new("fhirpath", "Patient.id")),
            PathCost::Materialized
        );
        assert_eq!(structure.contract().representation, "application/fhir+json");
    }

    #[test]
    fn rewrites_the_first_addressed_scalar_into_a_new_stream() {
        let mut rewrite = FhirRewrite::of(&stream(PATIENT), StreamId::new(2)).expect("parses");
        let engine = FhirPathEngine;
        let mut write = |path: &str, value: StructuredValue| {
            engine.write(&mut rewrite, &Path::new("fhirpath", path), value)
        };
        write("Patient.name.given", StructuredValue::Text("Pete".into())).expect("replaces");
        write(
            "Patient.name.where(use = 'usual').given.last()",
            StructuredValue::Text("Jimmy".into()),
        )
        .expect("replaces");
        write("Patient.active", StructuredValue::Bool(false)).expect("replaces");
        write(
            "Patient.birthDate",
            StructuredValue::Text("1974-12-25".into()),
        )
        .expect("adds");
        write(
            "Patient.name[1].family",
            StructuredValue::Text("Chalmers".into()),
        )
        .expect("adds");
        let out = Box::new(rewrite).finish().expect("finishes");
        assert_eq!(out.id(), StreamId::new(2));
        assert_eq!(out.media_type(), Some("application/fhir+json"));
        let back = FhirStructure::parse(&out).expect("parses");
        assert_eq!(
            back.read("name.given").expect("reads"),
            Some(StructuredValue::Text("Pete".into()))
        );
        assert_eq!(
            back.read("name[0].given[1]").expect("reads"),
            Some(StructuredValue::Text("James".into()))
        );
        assert_eq!(
            back.read("name[1].given").expect("reads"),
            Some(StructuredValue::Text("Jimmy".into()))
        );
        assert_eq!(
            back.read("active").expect("reads"),
            Some(StructuredValue::Bool(false))
        );
        assert_eq!(
            back.read("birthDate").expect("reads"),
            Some(StructuredValue::Text("1974-12-25".into()))
        );
        assert_eq!(
            back.read("name.where(use = 'usual').family")
                .expect("reads"),
            Some(StructuredValue::Text("Chalmers".into()))
        );
    }

    #[test]
    fn a_write_that_computes_or_addresses_nothing_is_refused() {
        let mut rewrite = FhirRewrite::of(&stream(PATIENT), StreamId::new(3)).expect("parses");
        let computes = rewrite
            .write("Patient.name.count()", StructuredValue::Integer(0))
            .expect_err("refused");
        assert_eq!(
            computes.message,
            "FHIRPath: \"Patient.name.count()\" computes a value; a write needs a place in \
             the resource"
        );
        assert!(
            rewrite
                .write("active = true", StructuredValue::Bool(true))
                .is_err()
        );
        assert!(
            rewrite
                .write("Patient.name", StructuredValue::Null)
                .is_err()
        );
        let nowhere = rewrite
            .write("Patient.contact.name.family", StructuredValue::Null)
            .expect_err("refused");
        assert_eq!(
            nowhere.message,
            "FHIRPath: \"Patient.contact.name.family\" addresses nothing in the resource"
        );
        assert!(
            rewrite
                .write("Patient.name[9].family", StructuredValue::Null)
                .is_err()
        );
        assert!(
            rewrite
                .write("Patient.id", StructuredValue::Binary(vec![1]))
                .is_err()
        );
    }

    #[test]
    fn a_stream_that_is_not_a_resource_is_refused_up_front() {
        assert!(FhirStructure::parse(&stream("{nope")).is_err());
        assert!(FhirRewrite::of(&stream("{nope"), StreamId::new(1)).is_err());
        let list = FhirStructure::parse(&stream("[1,2]")).expect_err("refused");
        assert_eq!(
            list.message,
            "FHIRPath: the Stream is JSON but not a resource"
        );
    }
}
