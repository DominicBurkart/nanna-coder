# Full-stack fixture

A tiny, deterministic full-stack Rust monorepo used by harness tests and evals:
an actix-web backend (`api/`), a dioxus web frontend built with trunk (`ui/`),
a `shared/` crate holding the wire types, one sqlx/Postgres migration under
`migrations/`, a `Containerfile` that packages api + ui into one image, a
`CHECKS` endpoint manifest and a `.nanna/deploy.toml` describing a fake deploy
target. It is its own cargo workspace and is excluded from the repository's
root workspace.

## Endpoints

| Path | Response |
|---|---|
| `GET /health/v1` | `{"status":"ok"}` |
| `GET /api/v1/greeting` | `{"message":"Hello from the full-stack fixture"}` |
| `GET /` | the built `ui/dist` (rendered greeting inside `#greeting`) |

`CHECKS` lists these paths one per line for smoke tests.

`index.html` mounts the app into `<div id="main">`, dioxus 0.7's default
root: without it, `dioxus::launch` logs a fallback ("mounting to the body")
but never actually attaches the tree, so a real browser sees an empty
`<body>` (found running the `qa_browser` container test in a headless
Chromium; a plain `.wasm` substring check on `index.html`, as
`apprun_integration` does, does not catch this).

## Environment hooks

| Variable | Effect |
|---|---|
| `FIXTURE_BREAK_ROUTE=1` | `GET /api/v1/greeting` returns `500` (health stays `200`) |
| `FIXTURE_FAIL_TEST=1` | `api::tests::fail_test_hook_fails_this_test_when_set` fails |
| `DATABASE_URL` | when set, the sqlx migrations run at startup; when unset, the api runs without a database |
| `FIXTURE_UI_DIST` | directory served at `/` (default `ui/dist`) |
| `BIND_ADDR` | listen address (default `0.0.0.0:8080`) |

Only the literal value `1` enables a `FIXTURE_*` hook.

## Building

```bash
cargo build --workspace
cargo test --workspace
(cd ui && trunk build)
cargo run --bin api
```

`podman build -f Containerfile .` produces one image exposing port 8080.
