//! Minimal JSON Schema validation for provider setup contracts.
//!
//! This intentionally supports only the subset used by this crate's setup
//! schemas. It avoids adding a heavy dependency while still making doctor use
//! the published schema files as the first validation layer.

use std::collections::BTreeSet;

use serde_json::Value;

pub fn validate(value: &Value, schema: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    validate_at(value, schema, schema, "$", &mut errors);
    errors
}

fn validate_at(value: &Value, schema: &Value, root: &Value, path: &str, errors: &mut Vec<String>) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        match resolve_ref(root, reference) {
            Some(resolved) => validate_at(value, resolved, root, path, errors),
            None => errors.push(format!("{path}: unresolved schema ref {reference}")),
        }
        return;
    }

    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        let valid = one_of.iter().any(|candidate| {
            let mut candidate_errors = Vec::new();
            validate_at(value, candidate, root, path, &mut candidate_errors);
            candidate_errors.is_empty()
        });
        if !valid {
            errors.push(format!("{path}: value does not match any oneOf schema"));
        }
        return;
    }

    if let Some(expected) = schema.get("const")
        && value != expected
    {
        errors.push(format!("{path}: expected const {expected}"));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && !values.iter().any(|candidate| candidate == value)
    {
        errors.push(format!("{path}: value is not in enum"));
    }
    if let Some(schema_type) = schema.get("type").and_then(Value::as_str)
        && !type_matches(value, schema_type)
    {
        errors.push(format!("{path}: expected {schema_type}"));
        return;
    }

    match value {
        Value::Object(object) => {
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for key in required.iter().filter_map(Value::as_str) {
                    if !object.contains_key(key) {
                        errors.push(format!("{path}: missing required property {key}"));
                    }
                }
            }
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (key, property_schema) in properties {
                    if let Some(child) = object.get(key) {
                        validate_at(
                            child,
                            property_schema,
                            root,
                            &format!("{path}.{key}"),
                            errors,
                        );
                    }
                }
            }
        }
        Value::Array(items) => {
            if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64)
                && items.len() < min_items as usize
            {
                errors.push(format!("{path}: expected at least {min_items} items"));
            }
            if schema
                .get("uniqueItems")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let mut seen = BTreeSet::new();
                for item in items {
                    if !seen.insert(item.to_string()) {
                        errors.push(format!("{path}: array items must be unique"));
                        break;
                    }
                }
            }
            if let Some(item_schema) = schema.get("items") {
                for (idx, item) in items.iter().enumerate() {
                    validate_at(item, item_schema, root, &format!("{path}[{idx}]"), errors);
                }
            }
        }
        Value::String(text) => {
            if let Some(min_length) = schema.get("minLength").and_then(Value::as_u64)
                && text.len() < min_length as usize
            {
                errors.push(format!("{path}: expected string length >= {min_length}"));
            }
        }
        Value::Number(number) => {
            if let Some(minimum) = schema.get("minimum").and_then(Value::as_i64)
                && number.as_i64().is_some_and(|actual| actual < minimum)
            {
                errors.push(format!("{path}: expected number >= {minimum}"));
            }
        }
        _ => {}
    }
}

fn resolve_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    root.pointer(pointer)
}

fn type_matches(value: &Value, schema_type: &str) -> bool {
    match schema_type {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_required_types_enum_and_refs() {
        let schema = json!({
            "type": "object",
            "required": ["items"],
            "properties": {
                "items": {
                    "type": "array",
                    "minItems": 1,
                    "items": {"$ref": "#/$defs/item"}
                }
            },
            "$defs": {
                "item": {
                    "type": "object",
                    "required": ["kind"],
                    "properties": {
                        "kind": {"enum": ["a", "b"]}
                    }
                }
            }
        });

        assert!(validate(&json!({"items": [{"kind": "a"}]}), &schema).is_empty());
        let errors = validate(&json!({"items": [{"kind": "c"}]}), &schema);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("enum"));
    }
}
