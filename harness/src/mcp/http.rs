use crate::auth::{AuthError, RateLimiter, TokenStore};
use crate::mcp::NannaMcpServer;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use serde_json::Value;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

/// Extract bearer token from the Authorization header.
pub(crate) fn extract_bearer_token(req: &Request<Body>) -> Result<&str, AuthError> {
    let header = req
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .ok_or(AuthError::MissingToken)?;

    let value = header.to_str().map_err(|_| AuthError::InvalidToken)?;

    value.strip_prefix("Bearer ").ok_or(AuthError::InvalidToken)
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
    rate_limiter: Arc<RateLimiter>,
    remote_addr: SocketAddr,
) -> Result<Response<Body>, Infallible> {
    let client_ip = remote_addr.ip();

    // Check rate limit before doing any work
    if let Err(_e) = rate_limiter.check_rate_limit(&client_ip) {
        let body = json_rpc_error(-32000, "rate limited");
        return Ok(json_response(StatusCode::TOO_MANY_REQUESTS, &body));
    }

    // Only accept POST
    if req.method() != Method::POST {
        let body = json_rpc_error(-32600, "Only POST is accepted");
        return Ok(json_response(StatusCode::METHOD_NOT_ALLOWED, &body));
    }

    // Authenticate
    match extract_bearer_token(&req) {
        Ok(token) => {
            if let Err(e) = token_store.validate(token) {
                rate_limiter.record_failure(&client_ip);
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
            rate_limiter.record_failure(&client_ip);
            let body = json_rpc_error(-32000, "missing authorization header");
            return Ok(json_response(StatusCode::UNAUTHORIZED, &body));
        }
    }

    // Auth succeeded — clear any rate-limit state for this IP
    rate_limiter.record_success(&client_ip);

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
    let response_body = serde_json::to_value(&response)
        .unwrap_or_else(|_| json_rpc_error(-32603, "internal error"));

    Ok(json_response(StatusCode::OK, &response_body))
}

/// Start the HTTP JSON-RPC server with bearer-token authentication and rate limiting.
pub async fn run_http(
    server: Arc<NannaMcpServer>,
    token_store: Arc<TokenStore>,
    rate_limiter: Arc<RateLimiter>,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    let make_svc = make_service_fn(move |conn: &hyper::server::conn::AddrStream| {
        let server = Arc::clone(&server);
        let token_store = Arc::clone(&token_store);
        let rate_limiter = Arc::clone(&rate_limiter);
        let remote_addr = conn.remote_addr();
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                handle_http_request(
                    req,
                    Arc::clone(&server),
                    Arc::clone(&token_store),
                    Arc::clone(&rate_limiter),
                    remote_addr,
                )
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

    // -------------------------------------------------------------------------
    // HTTP roundtrip integration tests
    // Each test spins up a real hyper server on 127.0.0.1:0 and talks to it
    // via reqwest, verifying end-to-end auth, routing, and response shape.
    // -------------------------------------------------------------------------

    fn make_noop_server() -> Arc<NannaMcpServer> {
        use async_trait::async_trait;
        use crate::task::TaskManager;
        use model::provider::ModelResult;
        use model::types::{ChatRequest, ChatResponse, ModelInfo};

        struct NoopProvider;

        #[async_trait]
        impl model::provider::ModelProvider for NoopProvider {
            async fn chat(&self, _: ChatRequest) -> ModelResult<ChatResponse> {
                unimplemented!()
            }
            async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
                Ok(vec![])
            }
            async fn health_check(&self) -> ModelResult<()> {
                Ok(())
            }
            fn provider_name(&self) -> &'static str {
                "noop"
            }
        }

        Arc::new(NannaMcpServer::new(
            Arc::new(TaskManager::default()),
            Arc::new(NoopProvider),
            "test-model".to_string(),
            10,
        ))
    }

    /// Bind to an ephemeral port, spawn the server, return (addr, token_value).
    async fn spawn_test_server() -> (SocketAddr, String) {
        // Port 0 lets the OS assign a free port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener); // Briefly release so hyper can rebind.

        let token_store = Arc::new(TokenStore::new(Duration::from_secs(3600)));
        let token_value = token_store.token().as_str().to_string();
        let rate_limiter = Arc::new(RateLimiter::new(10, Duration::from_secs(300)));
        let mcp_server = make_noop_server();

        tokio::spawn(run_http(mcp_server, token_store, rate_limiter, addr));

        // Give the server a moment to start accepting connections.
        tokio::time::sleep(Duration::from_millis(50)).await;

        (addr, token_value)
    }

    /// Full roundtrip: valid token + initialize => 200 with protocolVersion and
    /// capabilities.tools, token value absent from response body.
    #[tokio::test]
    async fn test_http_roundtrip_initialize() {
        let (addr, token_value) = spawn_test_server().await;

        let client = reqwest::Client::new();
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });

        let response = client
            .post(format!("http://{}", addr))
            .header("Authorization", format!("Bearer {}", token_value))
            .json(&payload)
            .send()
            .await
            .expect("HTTP request failed");

        assert_eq!(response.status(), reqwest::StatusCode::OK);

        let resp_text = response.text().await.unwrap();

        assert!(
            !resp_text.contains(&token_value),
            "Token must not appear in HTTP response body. Got: {}",
            resp_text
        );

        let resp_json: serde_json::Value =
            serde_json::from_str(&resp_text).expect("response should be valid JSON");

        let result = resp_json.get("result").expect("missing 'result' field");
        assert!(
            result.get("protocolVersion").is_some(),
            "Expected protocolVersion in initialize result. Got: {}",
            resp_json
        );
        assert!(
            result["capabilities"]["tools"].is_object(),
            "Expected capabilities.tools to be an object. Got: {}",
            resp_json
        );
    }

    /// Missing Authorization header => 401.
    #[tokio::test]
    async fn test_http_401_on_missing_auth() {
        let (addr, _) = spawn_test_server().await;

        let client = reqwest::Client::new();
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });

        let response = client
            .post(format!("http://{}", addr))
            .json(&payload)
            .send()
            .await
            .expect("HTTP request failed");

        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    /// Wrong token => 401.
    #[tokio::test]
    async fn test_http_401_on_wrong_token() {
        let (addr, _) = spawn_test_server().await;

        let client = reqwest::Client::new();
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });

        let response = client
            .post(format!("http://{}", addr))
            .header("Authorization", "Bearer this-is-definitely-wrong")
            .json(&payload)
            .send()
            .await
            .expect("HTTP request failed");

        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
}
