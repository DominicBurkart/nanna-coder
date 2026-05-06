use crate::auth::{AuthError, RateLimiter, TokenStore};
use crate::mcp::NannaMcpServer;
use hyper::body::HttpBody;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use serde_json::Value;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

/// Maximum accepted request body size. JSON-RPC frames are tiny; 1 MiB is a
/// generous cap that prevents an authenticated client from streaming an
/// unbounded body and OOMing the server.
pub(crate) const MAX_BODY_BYTES: u64 = 1 << 20; // 1 MiB

/// The WWW-Authenticate challenge returned on every 401 response.
/// RFC 7235 §4.1 requires this header whenever 401 is returned.
const WWW_AUTHENTICATE: &str = "Bearer realm=\"nanna-mcp\"";

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

/// Build a 401 Unauthorized response with the required WWW-Authenticate header
/// (RFC 7235 §4.1) and a JSON-RPC error body.
fn unauthorized_response(message: &str) -> Response<Body> {
    let body = json_rpc_error(-32000, message);
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("Content-Type", "application/json")
        .header("WWW-Authenticate", WWW_AUTHENTICATE)
        .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
        .expect("failed to build 401 response")
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
    if rate_limiter.check_rate_limit(&client_ip).is_err() {
        let body = json_rpc_error(-32000, "rate limited");
        return Ok(json_response(StatusCode::TOO_MANY_REQUESTS, &body));
    }

    // Only accept POST — reject *before* attempting auth so bad-method requests
    // don't consume auth-failure budget on the rate limiter, and so they are
    // still counted toward the unauthenticated-request budget below.
    if req.method() != Method::POST {
        // Bad-method requests from any IP count as a failure so a flood of
        // GET/PUTs from a single IP can't exhaust the server forever.
        rate_limiter.record_failure(&client_ip);
        let body = json_rpc_error(-32600, "Only POST is accepted");
        return Ok(json_response(StatusCode::METHOD_NOT_ALLOWED, &body));
    }

    // Authenticate
    match extract_bearer_token(&req) {
        Ok(token) => {
            if let Err(e) = token_store.validate(token) {
                rate_limiter.record_failure(&client_ip);
                let msg = match e {
                    AuthError::ExpiredToken => "expired token",
                    AuthError::InvalidToken => "invalid token",
                    _ => "authentication failed",
                };
                return Ok(unauthorized_response(msg));
            }
        }
        Err(_) => {
            rate_limiter.record_failure(&client_ip);
            return Ok(unauthorized_response("missing authorization header"));
        }
    }

    // Auth succeeded — clear any rate-limit state for this IP
    rate_limiter.record_success(&client_ip);

    // Enforce hard body-size cap before buffering. An authenticated client
    // could otherwise stream an arbitrarily large body and OOM the process.
    let mut body = req.into_body();
    if body.size_hint().upper().is_some_and(|u| u > MAX_BODY_BYTES) {
        let err = json_rpc_error(-32700, "request body too large");
        return Ok(json_response(StatusCode::PAYLOAD_TOO_LARGE, &err));
    }
    // Stream chunks so we can detect overflow when `size_hint` lies (chunked
    // encoding with no advertised length). Stop reading as soon as we exceed
    // the cap — we never buffer more than MAX_BODY_BYTES + one chunk.
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(_) => {
                let err = json_rpc_error(-32700, "failed to read request body");
                return Ok(json_response(StatusCode::BAD_REQUEST, &err));
            }
        };
        buf.extend_from_slice(&chunk);
        if buf.len() as u64 > MAX_BODY_BYTES {
            let err = json_rpc_error(-32700, "request body too large");
            return Ok(json_response(StatusCode::PAYLOAD_TOO_LARGE, &err));
        }
    }
    let body_bytes = buf;

    // Parse JSON-RPC request. Don't surface serde_json::Error detail to the
    // client — its formatting includes line/column offsets and partial-content
    // hints from the request bytes, which becomes an info-disclosure vector
    // once non-loopback / TLS support lands. Route detail to logs.
    let rpc_request: super::JsonRpcRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "JSON-RPC parse error");
            let body = json_rpc_error(-32700, "parse error");
            return Ok(json_response(StatusCode::BAD_REQUEST, &body));
        }
    };

    // Delegate to existing handler
    let response = server.handle_request(rpc_request).await;
    let response_body = serde_json::to_value(&response)
        .unwrap_or_else(|_| json_rpc_error(-32603, "internal error"));

    Ok(json_response(StatusCode::OK, &response_body))
}

/// Macro that constructs the per-connection hyper service. Factored into a
/// macro (rather than a function) to avoid spelling out the deeply-nested
/// `impl Service<...>` return type twice.
macro_rules! build_make_service {
    ($server:expr, $token_store:expr, $rate_limiter:expr) => {{
        let server = $server;
        let token_store = $token_store;
        let rate_limiter = $rate_limiter;
        make_service_fn(move |conn: &hyper::server::conn::AddrStream| {
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
        })
    }};
}

/// Start the HTTP JSON-RPC server with bearer-token authentication and rate
/// limiting, binding the supplied address.
pub async fn run_http(
    server: Arc<NannaMcpServer>,
    token_store: Arc<TokenStore>,
    rate_limiter: Arc<RateLimiter>,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let make_svc = build_make_service!(server, token_store, rate_limiter);
    Server::bind(&addr).serve(make_svc).await?;
    Ok(())
}

/// Start the server from an already-bound `std::net::TcpListener`. Used by
/// tests to avoid the bind -> drop -> rebind race that caused intermittent CI
/// flake on parallel runs — the listener is handed directly to hyper so the
/// port is never released between allocation and serving.
pub async fn run_http_from_listener(
    server: Arc<NannaMcpServer>,
    token_store: Arc<TokenStore>,
    rate_limiter: Arc<RateLimiter>,
    listener: std::net::TcpListener,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let make_svc = build_make_service!(server, token_store, rate_limiter);
    Server::from_tcp(listener)?.serve(make_svc).await?;
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
    //
    // Gated on `cfg(unix)` because the HTTP transport is only fully supported
    // on Unix (read_token_file refuses to run on Windows — see auth.rs). The
    // spawn-based tests also exercise loopback TCP plus tokio::spawn on CI
    // runners where Windows has been observed to intermittently fail the
    // hyper handshake under parallel nextest load. The pure-logic tests
    // above (extract_bearer_token, missing/wrong header, Debug redaction)
    // still run on all platforms.
    // -------------------------------------------------------------------------

    #[cfg(unix)]
    fn make_noop_server() -> Arc<NannaMcpServer> {
        use crate::task::TaskManager;
        use async_trait::async_trait;
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
    ///
    /// Uses `Server::from_tcp` on an already-bound std listener so the port is
    /// never released between allocation and serving — this avoids the TOCTOU
    /// race in the original bind/drop/rebind approach, which produced CI flake
    /// under high test parallelism. The pre-startup sleep is no longer needed
    /// because the listener is already accepting connections before we spawn.
    #[cfg(unix)]
    async fn spawn_test_server() -> (SocketAddr, String) {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let addr = std_listener.local_addr().unwrap();

        let token_store = Arc::new(TokenStore::new(Duration::from_secs(3600)));
        let token_value = token_store.token().as_str().to_string();
        let rate_limiter = Arc::new(RateLimiter::new(10, Duration::from_secs(300)));
        let mcp_server = make_noop_server();

        tokio::spawn(run_http_from_listener(
            mcp_server,
            token_store,
            rate_limiter,
            std_listener,
        ));

        (addr, token_value)
    }

    /// Full roundtrip: valid token + initialize => 200 with protocolVersion and
    /// capabilities.tools, token value absent from response body.
    #[cfg(unix)]
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

    /// Missing Authorization header => 401 with WWW-Authenticate header, body
    /// must not contain the real token.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_http_401_on_missing_auth() {
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
            .json(&payload)
            .send()
            .await
            .expect("HTTP request failed");

        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert!(
            response.headers().contains_key("www-authenticate"),
            "401 must include WWW-Authenticate header"
        );

        let body = response.text().await.unwrap();
        assert!(
            !body.contains(&token_value),
            "401 response body must not contain the server token"
        );
    }

    /// Wrong token => 401 with WWW-Authenticate header, body must not contain
    /// either the real or submitted token.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_http_401_on_wrong_token() {
        let (addr, token_value) = spawn_test_server().await;
        let wrong_token = "this-is-definitely-wrong";

        let client = reqwest::Client::new();
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });

        let response = client
            .post(format!("http://{}", addr))
            .header("Authorization", format!("Bearer {}", wrong_token))
            .json(&payload)
            .send()
            .await
            .expect("HTTP request failed");

        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert!(
            response.headers().contains_key("www-authenticate"),
            "401 must include WWW-Authenticate header"
        );

        let body = response.text().await.unwrap();
        assert!(
            !body.contains(&token_value),
            "401 response body must not contain the real server token"
        );
        assert!(
            !body.contains(wrong_token),
            "401 response body must not contain the submitted wrong token"
        );
    }

    /// AuthToken Debug representation must never reveal the actual value.
    #[test]
    fn test_auth_token_debug_redacted() {
        use crate::auth::AuthToken;
        let secret = "super-secret-value-that-must-not-leak";
        let token = AuthToken::from_string_unchecked(secret.to_string());
        let debug = format!("{:?}", token);
        assert!(
            !debug.contains(secret),
            "AuthToken Debug must not contain the raw token"
        );
        assert!(debug.contains("REDACTED"));
    }

    /// An authenticated client posting a >1 MiB body must be rejected with 413
    /// so a malicious client with a valid token can't OOM the server.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_http_413_on_oversized_body() {
        let (addr, token_value) = spawn_test_server().await;

        // 2 MiB of zero bytes — valid UTF-8, invalid JSON, larger than the cap.
        let big = vec![b'0'; (2 << 20) as usize];

        let client = reqwest::Client::new();
        let response = client
            .post(format!("http://{}", addr))
            .header("Authorization", format!("Bearer {}", token_value))
            .header("Content-Type", "application/json")
            .body(big)
            .send()
            .await
            .expect("HTTP request failed");

        assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// Regression test: an unauthenticated oversized body must be rejected at
    /// 401 (auth) before reaching the body-size guard. The auth check happens
    /// before `record_success` and body buffering, so even a 4 MiB unauth'd
    /// request gets a quick UNAUTHORIZED without the server ever streaming the
    /// payload. Pin that ordering so a future refactor cannot quietly invert
    /// the check sequence.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_http_401_short_circuits_before_body_read() {
        let (addr, _) = spawn_test_server().await;

        // Larger than MAX_BODY_BYTES (1 MiB).
        let big = vec![b'0'; (4 << 20) as usize];

        let client = reqwest::Client::new();
        let response = client
            .post(format!("http://{}", addr))
            .header("Content-Type", "application/json")
            .body(big)
            .send()
            .await
            .expect("HTTP request failed");

        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "unauthenticated request must 401 before body-size enforcement"
        );
        assert!(
            response.headers().contains_key("www-authenticate"),
            "unauthenticated 401 must include WWW-Authenticate header"
        );
    }
}
