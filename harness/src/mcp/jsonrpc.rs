//! Shared JSON-RPC 2.0 message types used by both the MCP server
//! ([`super`]) and the MCP Tasks client ([`super::client`]).
//!
//! The server deserializes [`JsonRpcRequest`] and serializes
//! [`JsonRpcResponse`]; the client does the reverse. Both derive
//! `Serialize` and `Deserialize` so a single set of types serves both
//! directions of the wire protocol.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC 2.0 request (or notification, when `id` is `None`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// Build a request with an id (expects a response).
    pub fn call(id: Value, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(id),
            method: method.into(),
            params: Some(params),
        }
    }

    /// Build a notification (no id, no response expected).
    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: method.into(),
            params: Some(params),
        }
    }
}

/// A JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Box<JsonRpcError>>,
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(Box::new(JsonRpcError {
                code,
                message,
                data: None,
            })),
        }
    }

    pub fn error_with_data(id: Option<Value>, code: i32, message: String, data: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(Box::new(JsonRpcError {
                code,
                message,
                data: Some(data),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_sets_id_and_params() {
        let req = JsonRpcRequest::call(serde_json::json!(1), "tasks/get", serde_json::json!({}));
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.id, Some(serde_json::json!(1)));
        assert_eq!(req.method, "tasks/get");
        assert!(req.params.is_some());
    }

    #[test]
    fn notification_has_no_id() {
        let req = JsonRpcRequest::notification("notifications/initialized", serde_json::json!({}));
        assert!(req.id.is_none());
        // A notification must not serialize an `id` field.
        let s = serde_json::to_string(&req).unwrap();
        assert!(!s.contains("\"id\""));
    }

    #[test]
    fn success_and_error_are_mutually_exclusive() {
        let ok = JsonRpcResponse::success(Some(serde_json::json!(2)), serde_json::json!({"a": 1}));
        assert!(ok.result.is_some());
        assert!(ok.error.is_none());

        let err = JsonRpcResponse::error(Some(serde_json::json!(3)), -32602, "bad".to_string());
        assert!(err.result.is_none());
        assert_eq!(err.error.unwrap().code, -32602);
    }

    #[test]
    fn error_data_is_omitted_when_absent_and_round_trips_when_present() {
        let plain = JsonRpcResponse::error(Some(serde_json::json!(1)), -32602, "bad".to_string());
        assert!(!serde_json::to_string(&plain).unwrap().contains("\"data\""));
        assert!(plain.error.unwrap().data.is_none());

        let with_data = JsonRpcResponse::error_with_data(
            Some(serde_json::json!(2)),
            -32602,
            "refused".to_string(),
            serde_json::json!({ "failure_kind": "spawn_refused" }),
        );
        let text = serde_json::to_string(&with_data).unwrap();
        let back: JsonRpcResponse = serde_json::from_str(&text).unwrap();
        let error = back.error.unwrap();
        assert_eq!(error.message, "refused");
        assert_eq!(error.data.unwrap()["failure_kind"], "spawn_refused");

        let legacy: JsonRpcResponse = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"old"}}"#,
        )
        .unwrap();
        assert!(legacy.error.unwrap().data.is_none());
    }

    #[test]
    fn response_roundtrips_through_json() {
        let ok = JsonRpcResponse::success(Some(serde_json::json!(2)), serde_json::json!({"a": 1}));
        let s = serde_json::to_string(&ok).unwrap();
        let back: JsonRpcResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, Some(serde_json::json!(2)));
        assert_eq!(back.result, Some(serde_json::json!({"a": 1})));
    }
}
