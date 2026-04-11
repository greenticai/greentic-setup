//! JSON error envelope for all `/api/*` handlers.
//!
//! Every error response has the shape:
//! ```json
//! {
//!   "error": {
//!     "code": "machine.code",
//!     "key": "ui.error.i18n_key",
//!     "params": { "example": "value" },
//!     "fields": {
//!       "field_name": { "key": "ui.error.field_key", "params": {} }
//!     }
//!   }
//! }
//! ```
//!
//! `code` is a stable machine-readable identifier; `key` is the i18n lookup
//! key used by the SPA to display a localized message; `params` carries ICU
//! parameters. `fields` is only present on validation errors.

use std::collections::HashMap;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};

/// API error response with automatic JSON serialization and status code mapping.
#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    code: String,
    key: String,
    params: Value,
    fields: HashMap<String, FieldError>,
}

/// Per-field validation error (used inside `ApiError.fields`).
#[derive(Debug, Clone, Serialize)]
pub struct FieldError {
    pub key: String,
    pub params: Value,
}

impl FieldError {
    pub fn new(key: &str) -> Self {
        Self {
            key: key.into(),
            params: json!({}),
        }
    }

    pub fn with_params(mut self, params: Value) -> Self {
        self.params = params;
        self
    }
}

impl ApiError {
    fn new(status: StatusCode, code: &str, key: &str) -> Self {
        Self {
            status,
            code: code.into(),
            key: key.into(),
            params: json!({}),
            fields: HashMap::new(),
        }
    }

    pub fn validation(code: &str, key: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, key)
    }

    pub fn unauthorized(code: &str, key: &str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, key)
    }

    pub fn forbidden(code: &str, key: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, key)
    }

    pub fn not_found(code: &str, key: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, key)
    }

    pub fn conflict(code: &str, key: &str) -> Self {
        Self::new(StatusCode::CONFLICT, code, key)
    }

    pub fn internal(code: &str, key: &str) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, key)
    }

    pub fn new_too_many(code: &str, key: &str) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, code, key)
    }

    pub fn with_params(mut self, params: Value) -> Self {
        self.params = params;
        self
    }

    pub fn with_field(mut self, name: &str, err: FieldError) -> Self {
        self.fields.insert(name.into(), err);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Build the JSON body manually so `fields` is omitted when empty.
        let mut error_obj = serde_json::Map::new();
        error_obj.insert("code".into(), Value::String(self.code));
        error_obj.insert("key".into(), Value::String(self.key));
        error_obj.insert("params".into(), self.params);
        if !self.fields.is_empty() {
            let fields_obj: serde_json::Map<String, Value> = self
                .fields
                .into_iter()
                .map(|(k, v)| (k, serde_json::to_value(v).unwrap_or(json!({}))))
                .collect();
            error_obj.insert("fields".into(), Value::Object(fields_obj));
        }
        let body = json!({ "error": error_obj });
        (self.status, Json(body)).into_response()
    }
}
