# Test Topology

**Scope:** evals measure agent performance (outcomes); tests verify harness correctness (deterministic components); benches optimize harness resources; QA is done manually in the dev env by an agent.

```mermaid
flowchart TD
    U["Unit tests\nharness/src/**"] --> NX["cargo nextest run --workspace --lib --all-features"]
    I["Integration tests\nharness/tests/"] --> NXI["cargo nextest run --workspace --test '*' --all-features"]
    S["Shell tests\ntests/"] --> SH["./tests/run-all-tests.sh"]
    E["Eval framework\nharness/src/eval/"] --> NX
    NX & NXI & SH --> CI[".github/workflows/ci.yml"]
```

## Principles

- Sorted preferred strategies (note: you'll need an ensemble, use complementary approaches): realistic happy-path integration tests, happy-path doctests on interfaces, aneas/kani invariant validation, proptests, unit tests (edge cases), unhappy-path integration tests (error legibility), edge-case doctests. 
- Design interfaces which can use the highest standards for validation (example: a de/serialization path needs to be compatible with both integration tests and formal methods validation, an external API surface needs to be expressible with both doctests and proptests). 
- Surface failures as early as possible: compile-time > CI > runtime. Make control flow boring and obvious by validating invariants statically first.
- Untested code is brittle. Harden it, don't bend it. Never merge `|| true`, silent failures, fallback parameters that shadow bad or dead code, or any other graceful-degradation pattern.

## CI gate

Codecov patch coverage must be **100%** on every PR. A PR that drops patch coverage below 100% is blocked. See [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

## Running tests

All commands require the Nix devShell:

```bash
nix develop

# unit
cargo nextest run --workspace --lib --all-features

# integration (some need containers / Ollama)
cargo nextest run --workspace --test '*' --all-features

# shell-based security & integration
./tests/run-all-tests.sh

# lint + format
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check

# supply-chain
cargo deny check
```

## Where tests live

| Category | Path | Runner |
|---|---|---|
| Unit (Rust `#[test]`) | [`harness/src/`](harness/src/) | nextest `--lib` |
| Entity test fixtures | [`harness/src/entities/test/`](harness/src/entities/test/) | nextest `--lib` |
| Eval cases | [`harness/src/eval/`](harness/src/eval/) | nextest `--lib` |
| Integration (Rust) | [`harness/tests/`](harness/tests/) | nextest `--test` |
| Full-stack fixture monorepo | [`tests/fixtures/fullstack/`](tests/fixtures/fullstack/) | own workspace; see below |
| Model integration | [`model/tests/`](model/tests/) | nextest `--test` |
| Shell security | [`tests/security/`](tests/security/) | bash |
| Shell integration | [`tests/integration/`](tests/integration/) | bash |
| Shell helpers | [`tests/lib/test-helpers.sh`](tests/lib/test-helpers.sh) | sourced |
| CI matrix | [`.github/workflows/ci.yml`](.github/workflows/ci.yml) | GitHub Actions |

## Full-stack fixture monorepo

[`tests/fixtures/fullstack/`](tests/fixtures/fullstack/) is a small, deterministic full-stack Rust monorepo (actix-web `api/`, dioxus `ui/` built with trunk, `shared/` wire types, one sqlx/Postgres migration, a `Containerfile`, a `CHECKS` endpoint manifest and a `.nanna/deploy.toml`). It is its own cargo workspace, listed under `exclude` in the root `Cargo.toml`, so the main workspace, clippy and coverage never see it. See its [README](tests/fixtures/fullstack/README.md) for the endpoint table and env hooks.

| Layer | How it uses the fixture |
|---|---|
| Harness integration (`harness/tests/fullstack_fixture.rs`) | Host-side, no container: runs `onboarding::detect::scan_project` and `capabilities::detect_capabilities` against the fixture and asserts its shape (workspace with `api`/`shared`/`ui`, stack dependencies, `CHECKS`, `.nanna/deploy.toml`, exactly one migration). Runs in ordinary CI. |
| Fixture unit tests (`cargo test --workspace` inside the fixture) | Cover the health handler, the greeting handler, the `FIXTURE_BREAK_ROUTE=1` hook (greeting `500`, health still `200`), the static-asset fallback and the `ui` render states. `FIXTURE_FAIL_TEST=1` makes exactly one test fail, giving inner-loop test tooling a deterministic red case. |
| Inner-loop QA (container, later issues) | Build the `Containerfile`, start the image, then hit each path in `CHECKS`; with `FIXTURE_BREAK_ROUTE=1` the `/api/v1/greeting` check must fail while `/health/v1` passes. Browser tests assert on the `#greeting` element rendered by `ui`. |
| Profile detection and provisioning (`harness/src/onboarding/fullstack.rs`, `harness/tests/fullstack_fixture.rs`) | Host-side: `FullStackRust::detect` names `api` as the actix binary, `ui` as the trunk frontend, `shared`, the sqlx/Postgres migration and `/health/v1`; the generated flake is asserted to carry `trunk`, `wasm-bindgen-cli`, `sqlx-cli`, `postgresql` and the `wasm32-unknown-unknown` target; `trunk_build` and `sqlx_migrate` are the only signal-gated capabilities. |
| Postgres sidecar (`harness/tests/sidecar_integration.rs`, `#[ignore]`) | Container: starts a per-task `SidecarSet` with `PostgresSidecar`, attaches a dev container through `TaskWorkspace::create_with_container_and_sidecars` and runs `psql "$DATABASE_URL"` from inside it; two tasks get distinct databases; the fixture's generated flake is built with `nix build` and the profile tools are checked on `PATH`. Run with `cargo test -p harness --test sidecar_integration -- --ignored --test-threads=1`. |
| Running the app (`harness/tests/apprun_integration.rs`, `#[ignore]`) | Container: a stand-in dev image (official Rust image, wasm target, trunk, fixture dependencies pre-built) hosts a `TaskWorkspace` on a copy of the fixture; `app_start` builds `ui` with trunk and `api` with cargo, starts `api` on a per-task port and waits for `/health/v1`; `curl` inside the container fetches the index page (with the wasm bundle) and `/api/v1/greeting`; `app_logs` shows the startup line; a second `app_start` returns the same instance; `app_stop` kills it and is idempotent; `cleanup` stops a restarted app and releases its port. Run with `cargo test -p harness --test apprun_integration -- --ignored --test-threads=1`. |
| Local QA (`harness/tests/qa_integration.rs`, `#[ignore]`) | Container: the apprun stand-in dev image plus headless chromium hosts a `TaskWorkspace`; `qa_endpoints`/`qa_browser` before `app_start` return a typed error; after it, `qa_endpoints` passes every `CHECKS` path, and with `FIXTURE_BREAK_ROUTE=1` the greeting check fails with its `500` response snippet while health stays green; `qa_browser` mounts the fixture UI in headless Chromium over the DevTools protocol, asserts the rendered greeting, saves a screenshot and reports the browser's own `GET /favicon.ico` 404 as a console error. Run with `cargo test -p harness --test qa_integration -- --ignored --test-threads=1`. |
| Deployment executor and evals | Read `.nanna/deploy.toml` (a fake `container-registry+serverless` target on a reserved `.invalid` registry) to plan rollouts without touching a real registry. |

Build it locally with:

```bash
cd tests/fixtures/fullstack
cargo build --workspace && cargo test --workspace
(cd ui && trunk build)
```

## Identity fixture catalog

[`harness/tests/fixtures/identities/`](harness/tests/fixtures/identities/) is the fixture identity catalog the identity/RBAC and auditor test suites read through `IdentityCatalog::load` and `IdentityCatalog::with_repo_overrides`:

| Path | Contents |
|---|---|
| `identities/global/*.toml` | One card per dev loop: `auditor` (inner, inert — `max_effect = "none"`, no tools), `rust-implementer` (inner, `max_effect = "repository"`), `pr-shepherd` (middle, `max_effect = "ci"`), `deployer` (outer, `max_effect = "sandbox"`), `incident-responder` (outer, `max_effect = "production"`, scoped to `rollout_rollback`/`rollout_roll_forward_pr`/`read_logs`) |
| `identities/global/prompts/*.md` | The `system_prompt` files the above cards reference by path |
| `identities/repo/.nanna/agents/rust-implementer.toml` | A repo-local override that narrows `rust-implementer` to `paths = ["api/**"]` and `max_effect = "workspace"`, exercising `AgentIdentity::narrows` |

Use it directly with `IdentityCatalog::load("harness/tests/fixtures/identities/global")` (optionally `.with_repo_overrides("harness/tests/fixtures/identities/repo")`) rather than inventing ad hoc TOML in a new test; it already covers one card per loop plus an inert auditor and a narrowing repo override.

## Fake deploy adapter and health/shadow sources

The rollout executor's dependencies are all traits, each with an in-memory fake used across the rollout, leases and eval test suites:

| Trait | Fake | Notes |
|---|---|---|
| `TargetAdapter` | `FakeAdapter` | Starts serving `current_image` at 100% from `slot-0`; records every call as an `AdapterCall` (`calls()`); `fail(op)` / `succeed(op)` script a specific operation to error, for testing failure handling mid-rollout. |
| `HealthSource` | `FakeHealthSource` | `healthy(endpoints)` always reports `2xx`; `push(sample)` queues one scripted sample for the next poll, `push_after(n, sample)` queues `n` healthy polls then a breach, letting a test control exactly which bake poll trips a threshold. |
| `ShadowSource` | `FakeShadowSource` | `agreeing(pairs)` answers with `pairs` identical comparison samples, for shadow-deploy steps that must see no divergence. |

`harness::rollout::fake_executor(log, windows, current_image, endpoints)` wires all three together with an `InMemoryLeaseStore` and a `SimulatedClock` and returns `(RolloutExecutor, Arc<FakeAdapter>, Arc<FakeHealthSource>, Arc<FakeShadowSource>, Arc<SimulatedClock>)`, which is the fixture most rollout tests build on rather than constructing a `RolloutExecutor` by hand. `harness::rollout::run_simulated(executor, clock, id)` drives a rollout past every `Parked { until, .. }` state by advancing the returned clock to `until` and re-running, collecting each intermediate record — the way to run a whole gated rollout to completion in a test without a real sleep. The full-stack fixture's own `.nanna/deploy.toml` (a fake `container-registry+serverless` target on a reserved `.invalid` registry) is what deployment-executor and eval tests plan rollouts from, and — notably — is also the only kind of target the harness CLI's `deploy run`/`deploy roll-forward` subcommands can run against today: they require an explicit `--fake` flag and build a `fake_rollout_executor` (`harness/src/main.rs`); without `--fake` they refuse outright, since the CLI does not wire in the real `ServerlessAdapter` behind the `serverless-adapter` cargo feature at all.

## Simulated time

Two independent simulated-time patterns are used, depending on what's under test:

- **`harness::leases::Clock`** (`SystemClock` / `SimulatedClock`) is for anything that *sleeps and retries*: lease contention (`wait_for`/`Backoff`), the rollout executor's bake polling, and escalation dedupe windows (`Escalator`) all take a `Clock` so a test can call `clock.sleep(...)` and have it resolve instantly while still recording the requested duration (`SimulatedClock::sleeps()`). Clones of one `SimulatedClock` share the same instant.
- **Explicit `now: DateTime<Utc>` parameters** are for anything that only *evaluates* a point in time rather than waiting: `WindowSet::is_open`/`next_open`, `scheduler::parked_until`, and `SlotState`/`HybridPolicy::next` all take `now` directly, so tests pass whatever instant they need without a shared clock object at all.

Pick whichever pattern matches the API you're extending: a component that already takes `now` as a parameter should keep doing so rather than gaining a `Clock` dependency, and vice versa.

## Adding a new test

1. **Rust unit test** -- add `#[cfg(test)] mod tests` in the relevant `harness/src/` module.
2. **Rust integration test** -- add a file under [`harness/tests/`](harness/tests/).
3. **Shell test** -- create a script in `tests/security/` or `tests/integration/`, use helpers from [`tests/lib/test-helpers.sh`](tests/lib/test-helpers.sh), and register it in [`tests/run-all-tests.sh`](tests/run-all-tests.sh).
