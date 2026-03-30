use crate::auth::{AuthError, TokenStore};
use crate::mcp::NannaMcpServer;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use serde_json::Value;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

/// Extract bearer token from the Authorization header.
fn extract_bearer_token(req: &Request<Body>) -> Result<&str, AuthError> {
    let header = req
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .ok_or(AuthError::MissingToken)?;

    let value = header.to_str().map_err(|_| AuthError::InvalidToken)?;

    value
        .strip_prefix("Bearer ")
        .ok_or(AuthError::InvalidToken)
}

/// JSON-RPC error response body.
fn json_rpc_error(code: i32, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": code, "message": message }
    })
}

/// Build an HTTP response with a given status and JSON body.
fn json_response(status: StatusCode, body: &Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap_or_default()))
        .expect("failed to build response")
}

async fn handle_http_request(
    req: Request<Body>,
    server: Arc<NannaMcpServer>,
    token_store: Arc<TokenStore>,
) -> Result<Response<Body>, Infallible> {
    // Only accept POST
    if req.method() != Method::POST {
        let body = json_rpc_error(-32600, "Only POST is accepted");
        return Ok(json_response(StatusCode::METHOD_NOT_ALLOWED, &body));
    }

    // Authenticate
    match extract_bearer_token(&req) {
        Ok(token) => {
            if let Err(e) = token_store.validate(token) {
                let (status, msg) = match e {
                    AuthError::ExpiredToken => (StatusCode::UNAUTHORIZED, "expired token"),
                    AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "invalid token"),
                    _ => (StatusCode::UNAUTHORIZED, "authentication failed"),
                };
                let body = json_rpc_error(-32000, msg);
                return Ok(json_response(status, &body));
            }
        }
        Err(_) => {
            let body = json_rpc_error(-32000, "missing authorization header");
            return Ok(json_response(StatusCode::UNAUTHORIZED, &body));
        }
    }

    // Read body
    let body_bytes = match hyper::body::to_bytes(req.into_body()).await {
        Ok(bytes) => bytes,
        Err(_) => {
            let body = json_rpc_error(-32700, "failed to read request body");
            return Ok(json_response(StatusCode::BAD_REQUEST, &body));
        }
    };

    // Parse JSON-RPC request
    let rpc_request: super::JsonRpcRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            let body = json_rpc_error(-32700, &format!("Parse error: {}", e));
            return Ok(json_response(StatusCode::BAD_REQUEST, &body));
        }
    };

    // Delegate to existing handler
    let response = server.handle_request(rpc_request).await;
    let response_body =
        serde_json::to_value(&response).unwrap_or_else(|_| json_rpc_error(-32603, "internal error"));

    Ok(json_response(StatusCode::OK, &response_body))
}

/// Start the HTTP JSON-RPC server with bearer-token authentication.
pub async fn run_http(
    server: Arc<NannaMcpServer>,
    token_store: Arc<TokenStore>,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    let make_svc = make_service_fn(move |_conn| {
        let server = Arc::clone(&server);
        let token_store = Arc::clone(&token_store);
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                handle_http_request(req, Arc::clone(&server), Arc::clone(&token_store))
            }))
        }
    });

    let server = Server::bind(&addr).serve(make_svc);

    server.await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::AUTHORIZATION;
    use std::time::Duration;

    fn make_request_with_auth(token: &str) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri("/")
            .header(AUTHORIZATION, format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap()
    }

    fn make_request_without_auth() -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri("/")
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn test_extract_bearer_token() {
        let req = make_request_with_auth("my_secret_token");
        let token = extract_bearer_token(&req).unwrap();
        assert_eq!(token, "my_secret_token");
    }

    #[test]
    fn test_missing_auth_header_rejected() {
        let req = make_request_without_auth();
        let result = extract_bearer_token(&req);
        assert!(result.is_err());
        match result.unwrap_err() {
            AuthError::MissingToken => {}
            other => panic!("Expected MissingToken, got: {:?}", other),
        }
    }

    #[test]
    fn test_wrong_token_rejected() {
        let store = TokenStore::new(Duration::from_secs(3600));
        let result = store.validate("definitely_not_the_right_token");
        assert!(result.is_err());
        match result.unwrap_err() {
            AuthError::InvalidToken => {}
            other => panic!("Expected InvalidToken, got: {:?}", other),
        }
    }
}
