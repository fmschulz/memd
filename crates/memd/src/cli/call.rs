use std::path::Path;

use serde_json::{json, Value};

use crate::error::{MemdError, Result};

pub(super) fn parse_call_arguments(json_arg: Option<&str>, input: Option<&Path>) -> Result<Value> {
    let value = if let Some(path) = input {
        serde_json::from_str(&std::fs::read_to_string(path)?)?
    } else if let Some(json_arg) = json_arg {
        serde_json::from_str(json_arg)?
    } else {
        json!({})
    };

    if value.is_object() || value.is_null() {
        Ok(value)
    } else {
        Err(MemdError::ValidationError(
            "call arguments must be a JSON object".to_string(),
        ))
    }
}
