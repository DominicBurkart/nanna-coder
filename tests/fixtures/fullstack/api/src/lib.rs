//! actix-web backend of the full-stack fixture.
//!
//! Routes:
//! - `GET /health/v1` returns [`shared::Health`].
//! - `GET /api/v1/greeting` returns [`shared::Greeting`], or `500` when the
//!   [`FixtureConfig::break_route`] hook is on.
//! - everything else is served from the built `ui/dist` directory.

use actix_files::Files;
use actix_web::{web, HttpResponse, Responder};
use shared::{Greeting, Health};
use std::path::PathBuf;

/// Environment variable that makes `/api/v1/greeting` return `500`.
pub const BREAK_ROUTE_ENV: &str = "FIXTURE_BREAK_ROUTE";
/// Environment variable that makes one unit test fail.
pub const FAIL_TEST_ENV: &str = "FIXTURE_FAIL_TEST";
/// Environment variable pointing at the directory trunk wrote the ui into.
pub const UI_DIST_ENV: &str = "FIXTURE_UI_DIST";
/// Environment variable holding the Postgres connection string.
pub const DATABASE_URL_ENV: &str = "DATABASE_URL";
/// Directory served at `/` when [`UI_DIST_ENV`] is unset.
pub const DEFAULT_UI_DIST: &str = "ui/dist";

/// Runtime configuration derived from the process environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureConfig {
    /// When `true`, `/api/v1/greeting` responds with `500`.
    pub break_route: bool,
    /// Directory served as static frontend assets.
    pub ui_dist: PathBuf,
}

impl Default for FixtureConfig {
    fn default() -> Self {
        Self {
            break_route: false,
            ui_dist: PathBuf::from(DEFAULT_UI_DIST),
        }
    }
}

/// Interprets the raw value of a `FIXTURE_*` hook variable.
///
/// Only the exact string `"1"` enables a hook, so `FIXTURE_BREAK_ROUTE=0` and
/// an unset variable are both "off".
pub fn hook_enabled(raw: Option<&str>) -> bool {
    raw == Some("1")
}

impl FixtureConfig {
    /// Builds a configuration from the current process environment.
    pub fn from_env() -> Self {
        Self {
            break_route: hook_enabled(std::env::var(BREAK_ROUTE_ENV).ok().as_deref()),
            ui_dist: std::env::var_os(UI_DIST_ENV)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_UI_DIST)),
        }
    }
}

/// `GET /health/v1`.
pub async fn health() -> impl Responder {
    HttpResponse::Ok().json(Health::ok())
}

/// `GET /api/v1/greeting`.
pub async fn greeting(config: web::Data<FixtureConfig>) -> HttpResponse {
    if config.break_route {
        return HttpResponse::InternalServerError().body("route broken by FIXTURE_BREAK_ROUTE=1");
    }
    HttpResponse::Ok().json(Greeting::default())
}

/// Registers every route of the fixture on `cfg`.
///
/// API routes are registered before the static file handler so that they take
/// precedence over any file of the same name in the dist directory.
pub fn configure(cfg: &mut web::ServiceConfig, config: FixtureConfig) {
    let ui_dist = config.ui_dist.clone();
    cfg.app_data(web::Data::new(config))
        .route("/health/v1", web::get().to(health))
        .route("/api/v1/greeting", web::get().to(greeting))
        .service(Files::new("/", ui_dist).index_file("index.html"));
}

/// Runs the bundled sqlx migrations against `database_url`.
pub async fn migrate(database_url: &str) -> Result<(), sqlx::Error> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(database_url)
        .await?;
    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .map_err(sqlx::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::{call_service, init_service, read_body_json, TestRequest};
    use actix_web::{body::to_bytes, http::StatusCode, App};
    use shared::DEFAULT_GREETING;

    fn config(break_route: bool) -> FixtureConfig {
        FixtureConfig {
            break_route,
            ui_dist: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ui/dist"),
        }
    }

    #[test]
    fn hook_enabled_only_for_literal_one() {
        assert!(hook_enabled(Some("1")));
        assert!(!hook_enabled(Some("0")));
        assert!(!hook_enabled(Some("true")));
        assert!(!hook_enabled(Some("")));
        assert!(!hook_enabled(None));
    }

    #[test]
    fn default_config_serves_ui_dist_with_route_intact() {
        let config = FixtureConfig::default();
        assert!(!config.break_route);
        assert_eq!(config.ui_dist, PathBuf::from(DEFAULT_UI_DIST));
    }

    #[actix_web::test]
    async fn health_returns_ok_json() {
        let app = init_service(App::new().configure(|c| configure(c, config(false)))).await;
        let req = TestRequest::get().uri("/health/v1").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Health = read_body_json(resp).await;
        assert_eq!(body, Health::ok());
    }

    #[actix_web::test]
    async fn greeting_returns_default_message() {
        let app = init_service(App::new().configure(|c| configure(c, config(false)))).await;
        let req = TestRequest::get().uri("/api/v1/greeting").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Greeting = read_body_json(resp).await;
        assert_eq!(body.message, DEFAULT_GREETING);
    }

    #[actix_web::test]
    async fn break_route_hook_makes_greeting_return_500() {
        let app = init_service(App::new().configure(|c| configure(c, config(true)))).await;
        let req = TestRequest::get().uri("/api/v1/greeting").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(resp.into_body()).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains(BREAK_ROUTE_ENV));
    }

    #[actix_web::test]
    async fn break_route_hook_leaves_health_untouched() {
        let app = init_service(App::new().configure(|c| configure(c, config(true)))).await;
        let req = TestRequest::get().uri("/health/v1").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn unknown_path_without_built_ui_is_not_found() {
        let mut config = config(false);
        config.ui_dist = PathBuf::from("/nonexistent/fixture/dist");
        let app = init_service(App::new().configure(|c| configure(c, config))).await;
        let req = TestRequest::get().uri("/missing.txt").to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn fail_test_hook_fails_this_test_when_set() {
        let raw = std::env::var(FAIL_TEST_ENV).ok();
        assert!(
            !hook_enabled(raw.as_deref()),
            "{FAIL_TEST_ENV}=1 forces this test to fail on purpose"
        );
    }
}
