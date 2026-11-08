# Per-identity dev containers (design for #1012)

Status: design only. No code in this change. Refs #1012; relates to #1027, #1028, #733, #1031, #1032.

The goal: each inchworm SDLC journey step (`rust-implementer`, `pr-shepherd`, `reviewer`, `docs-writer`, `verifier`) runs in a dedicated container image whose contents are derived from the identity card's capability set, so a tool exists only in the environment of the identities permitted to use it. The container model stays the only execution path; nothing here adds a non-container way to run a command.

## What exists on main today (6f38d544)

- A task opts into the container path only when the base checkout has both `flake.nix` and `.devcontainer/` (`TaskRunner::run`, `harness/src/task.rs`). Otherwise `TaskWorkspace::create` runs with no container and `run_command` is absent.
- The image comes from one flake output, `devContainerImage`, built by `image_builder::build_dev_container` (`nix build path:<repo>#devContainerImage`), loaded into podman/docker by `container::load_image_from_path`, and cached in `TaskRunner::image_cache` keyed by canonical repo path only. One image per repo, shared by every identity.
- `nix/containers.nix` builds nanna's own image with `nix2container.buildImage` from `devToolPaths` plus the Rust toolchain and cargo plugins. The onboarding flake template (`harness/src/onboarding/flake_template.rs`) generates the same shape for other repos, filling `devContainerPackages` from `harness/src/capabilities.rs` (`CARGO_CAPABILITIES`: tool id to `nix_package`).
- The network policy is derived from the card, not from the image: `network_policy_for` calls `NetworkPolicy::for_ceiling(identity.scope.max_effect)`; `None` and `workspace` get `--network=none`.
- Tools split in two classes. Host-side tools (`read_file`, `write_file`, `list_directory`, `search`, `git_*`, `github_*`, `ci_*`, sandbox tools) run in the harness process against the worktree and never need anything from the image. Container-executed tools (`run_command`, `cargo_build|test|check|bench|run`, `cargo_deny`, `cargo_audit`, `trunk_build`, `sqlx_migrate`, and with #1032 `run_checks`) run through `<runtime> exec` and need the binary in the image.
- Gap found while reading: `create_container_tool_registry_for_guarded` (the identity path used by `TaskWorkspace::build_tool_registry_for`) registers only `run_command` from the container class. The `cargo_*` tools are registered only by the non-identity `create_container_tool_registry_guarded`. So a card listing `cargo_*` today receives no cargo tools in a container. Tool-registry linkage (below) has to close this regardless of this design.
- Repo-local cards (`.nanna/agents/*.toml`) may only narrow the global card (`AgentIdentity::narrows`, `harness/src/identity/narrowing.rs`): ceilings at or below, lists as exact-string subsets, limits at or below. `identity.model`, `system_prompt`, `description`, `loop` are free.
- #733: `[limits]`, `model` and `system_prompt` are validated but not enforced at runtime; `scope.repos` is now checked by `scope::check_repo` in `TaskRunner::run`. This design adds nothing that depends on the still-unenforced fields.

## Decisions

### D1. The mapping lives in the identity card, as an optional `[environment]` table

```toml
[environment]
name = "rust-impl"
```

One key in v1: `name`, matching `[a-z0-9][a-z0-9-]{0,62}`. Absent table means "the repo default image", exactly today's behaviour.

Why the card and not a separate file:

- The card is already the single place where the capability set (tools, paths, ceiling) is declared and where the narrowing rule is enforced. A second file (`.nanna/environments.toml`) would create a card-to-environment sync problem and a second narrowing implementation.
- Protection comes for free. Global cards live in the host config directory that the dev container never mounts; repo-local cards live under `.nanna/**`, which is in the shipped protected set. An agent cannot change which environment it runs in.
- The card carries only a name. The definition of what the name means (image, provided tools) lives in the repo's Nix flake, next to the existing `devContainerImage`. The card does not restate capabilities, so there is no duplicate list to drift.

Rejected: agent-definition frontmatter (cards are TOML, no frontmatter exists); a `network` or `image` field in the card (the image is chosen by repo-owned, protected Nix, never by a string an agent-adjacent file supplies; network stays derived from the ceiling).

### D2. Definition format: named nix2container flake outputs plus a capability manifest

For an environment `<name>` the repo flake exposes, per system:

- `packages.<system>.devContainerImage-<name>`: a `nix2container` image, same shape as `devContainerImage`.
- `packages.<system>.devContainerManifest-<name>`: a tiny `writeText` JSON derivation that does not depend on the image, so reading it never builds the image:

```json
{
  "schema": 1,
  "name": "rust-impl",
  "provides": ["cargo_build", "cargo_check", "cargo_test", "cargo_bench", "cargo_run"],
  "extends": null
}
```

`provides` lists container-executed registry tool ids. Both the image `paths` and the manifest are generated by one Nix function from the same capability list (`mkEnvironment { name; capabilities = [ ... ]; extraPackages = [ ... ]; }`), using a capability-to-package table that mirrors `CARGO_CAPABILITIES`. A host-side test pins that table against `capabilities.rs` so the two cannot drift. `run_command` is not in `provides`: every image has a shell because the `Cmd` is `sleep infinity` from coreutils; whether a card may use it is the card's decision.

`image_builder` gains `build_environment(source, name)` and `read_manifest(source, name)` next to `build_dev_container`, using the same `nix build path:...#...` invocation with the validated name appended; the name charset forbids anything that could alter the flake reference.

Why not the alternatives:

- `devcontainer.json`: the harness never reads it today (`.devcontainer/` is only an opt-in marker), its `features` fetch from the network at build time, and it cannot express a provable capability list.
- Containerfile: there is no builder for it in `image-builder/`, builds are not reproducible, and `podman build` needs network. It could be added later as a second backend that must still supply a manifest; out of scope.
- Nix is already how every dev image is produced, cached (Cachix, `docs/binary-cache-strategy.md`) and checked (`devContainerCmdCheck`).

### D3. Fallback to the repo default

- No `[environment]` table: today's `devContainerImage`, no capability check. Unchanged behaviour for every existing card.
- `[environment]` declared and the repo has no container opt-in (`flake.nix` + `.devcontainer/`): fail closed with a distinct error, the same posture as `VerifierRequiresContainer` in #1032. The card promised a specific environment; running without a container would silently widen it.
- `[environment]` declared and the flake lacks `devContainerImage-<name>` or its manifest, or the manifest fails to parse or check: fail closed. A declared environment never falls back to the default image, because the default image carries every tool and so is wider than any named environment.

Open question 1 below covers whether global cards used across many repos need an opt-out.

### D4. Inheritance and narrowing

Environment names are resolved against the target repo's flake, so the narrowing check has two phases.

Phase A, pure, in `AgentIdentity::narrows` (no I/O, so the existing property tests stay valid):

| base `[environment]` | override `[environment]` | result |
|---|---|---|
| absent | absent | ok |
| `name = X` | absent | ok, the override inherits X |
| `name = X` | `name = X` | ok |
| absent | `name = X` | rejected: `WidensScope { field: "environment.name" }`; an override cannot introduce an environment where the base runs on the default, because the override then picks the image |
| `name = X` | `name = Y`, Y != X | deferred to phase B |

Phase B, at task start, once the repo flake is readable (same point as `scope::check_repo`, before any container exists): a different name Y is accepted only if `manifest(Y).provides` is a string-subset of `manifest(X).provides` and `manifest(Y).extends == Some(X)`. The `extends` declaration is the "declared child" proof: the flake author states the relation and the harness verifies the subset, mirroring the exact-string subset rule of `subset()` in `narrowing.rs`. Transitivity holds because subset and `extends` chains compose, and a proptest pins it alongside `narrowing_is_transitive`.

Recommendation: ship v1 with phase A only plus "different name is rejected" (same image or omitted), and add the `extends` relation as a separate step once a repo needs it. Phase B is specified here so the schema does not need to change later.

Trust note: the manifest is written by the repo owner, who is trusted for their own repo; the threat model is a model-driven agent, which cannot edit `flake.nix`, `nix/**`, `.devcontainer/**` or `.nanna/**` (all protected). A lying manifest only misleads its author; the card's scope, ceiling and registry filtering still bound the agent.

### D5. Tool-registry link, checked at load time

"Load time" means task start, before the container is created, because the manifest lives in the repo. A new module `harness/src/environment.rs` (pure functions, unit-testable without Nix or a runtime) provides:

- `EnvironmentManifest::parse(json) -> Result<_, EnvironmentError>`
- `resolve(card, manifest) -> Result<ResolvedEnvironment, EnvironmentError>`

Rules, applied only to the container-executed class (host-side tools are never checked against the image):

1. Expand each `scope.tools` pattern over the known container-class tool ids (the fixed `cargo_build|test|check|bench|run`, the `CARGO_CAPABILITIES` ids, `run_checks`).
2. A pattern that is an exact name (no `*`) naming a container-class tool must be in `provides`; otherwise `EnvironmentError::ToolNotProvided { tool, environment }`.
3. A glob pattern (`cargo_*`) must match at least one provided tool; otherwise `DeadPattern`. It does not require every cargo tool: the registry registers the intersection of what the card allows and what the image provides.
4. If the card allows `run_command` (a shell can reach every binary in the image), `provides` must be a subset of the card's expanded container tools; otherwise `ImageWiderThanCard { extra }`. This is the literal "tools exist only where permitted" rule. For a card without `run_command`, extras in the image are unreachable except through registry tools that the card already filters, so they only produce a log warning.
5. `ResolvedEnvironment` carries the provided set into registry construction: a container-class tool is registered iff the card allows it and the image provides it. This also fixes the gap noted above, since the identity path will register `cargo_*` from the resolved set.

Network is never an environment property. `network_policy_for` is unchanged, so an environment cannot widen the ceiling-derived policy, and `ContainerConfig`/`container::run_args` do not change: they already take the image reference as an argument.

### D6. Build caching and reuse across cards

- Cache key becomes `(canonical repo path, environment name)` in `image_cache` and `build_locks`; the default image keeps the key `(path, None)`. Concurrent tasks for two environments of one repo build in parallel; two tasks for the same environment still serialise on one lock.
- Sharing happens in Nix, not in the harness: environments built from the same `devToolPaths` and toolchain share store paths, and `nix2container` emits layers per store path, so five images that differ by a few packages share the large layers on disk, in the binary cache and in the runtime's layer store.
- The loaded image reference must include the image store hash (as the current cache effectively does through the loaded ref), so identical environments of two repos reuse one runtime image.
- Cold cost is up to five builds per repo, but each is a delta over a cached base. Existing limitation, unchanged: the in-process cache never invalidates when a flake changes during a daemon's lifetime.
- The manifest is read before the image is built, so an invalid environment fails in seconds without a Nix image build.

### D7. Interaction with `--network=none` (#1027) and the verifier (#1028)

- #1027 stages a vendor directory on the host and mounts it read-only at `/opt/nanna-offline/vendor` plus `/.cargo/config.toml`. Both are host mounts, independent of the image, so they compose with any environment.
- Required condition: vendoring must run only when the resolved environment provides a cargo capability. A `docs-writer` environment with no cargo must not trigger `cargo vendor`, because #1031 refuses the task on a stale or missing `Cargo.lock`, which would block a docs task for an unrelated reason. This is a small `workspace.rs` change after #1031 lands.
- Network stays derived from the ceiling: `verifier` and `docs-writer` (ceiling `workspace`) are `--network=none`; `rust-implementer`, `pr-shepherd` and `reviewer` (ceiling `repository` or above) keep the default network.
- Verifier tension: its single tool `run_checks` executes repo-declared commands (`.nanna/verify.toml`), so the card cannot enumerate which binaries those need. The `verify` environment therefore provides the toolchain the repo's checks use (rustfmt, clippy, cargo, nextest) and no git-push or GitHub tooling. A check that names a binary absent from the image fails with the existing `VerifierExecFailed`. Open question 3.

### D8. Scope boundary with the pi harness

`docs/pi-harness.md` describes a second container: the agent container in which pi reasons, with at most a broker socket mounted. Environments in this document select the dev container where broker-run and `run_command` tools execute. They do not affect the pi agent image (`InvalidImage` checks in `IsolationPolicy` are unchanged), and the broker keeps calling `ToolRegistry::execute` on a registry built through `model_facing_registry`, which will now be built from the resolved environment.

## The five cards

Cards live in the host config directory (`$NANNA_CONFIG_DIR/agents/`). `rust-implementer` and `pr-shepherd` follow the existing fixtures with an `[environment]` table added; `reviewer`, `docs-writer` and `verifier` do not exist as fixtures yet, so their scope values below are proposals for the user to adjust. Repo names, models and prompt paths are illustrative.

```toml
[identity]
name = "rust-implementer"
description = "Implements a scoped issue in a Rust workspace and opens a draft PR."
loop = "inner"
model = "gemma4:e4b"
system_prompt = "prompts/rust-implementer.md"

[scope]
repos = ["github.com/example/repo"]
paths = ["api/**", "shared/**"]
max_effect = "repository"
tools = ["read_file", "write_file", "search", "cargo_*", "git_*"]

[environment]
name = "rust-impl"

[limits]
max_iterations = 200
max_wall_clock_secs = 3600
max_concurrent = 4
```

`rust-impl` provides `cargo_build`, `cargo_check`, `cargo_test`, `cargo_bench`, `cargo_run`, plus rustfmt and clippy. No `cargo_deny`, `cargo_audit`, `trunk_build`, `sqlx_migrate`. No `run_command`, so the image-wider-than-card rule is only a warning here.

```toml
[identity]
name = "pr-shepherd"
description = "Keeps an open pull request green: reads CI results, fixes review findings, never pushes to main."
loop = "middle"
model = "gemma4:e4b"
system_prompt = "prompts/pr-shepherd.md"

[scope]
repos = ["github.com/example/repo"]
paths = ["api/**", "shared/**", "tests/**"]
read_paths = ["**"]
max_effect = "ci"
tools = ["read_file", "write_file", "search", "list_directory", "cargo_check", "cargo_test", "git_*", "github_pr_status", "git_commit", "git_push_branch", "github_pr_open", "github_pr_comments", "github_pr_promote", "github_pr_close", "github_issue_read", "github_issue_comment"]

[environment]
name = "rust-fix"

[limits]
max_iterations = 100
max_wall_clock_secs = 1800
max_concurrent = 2
```

`rust-fix` provides `cargo_check` and `cargo_test` only. The fixture's `cargo_*` is narrowed to the two tools the shepherd needs to reproduce CI failures locally; this is a deliberate edit of that card, and the `rust-fix` image has no build/bench/run tooling beyond what those two need.

```toml
[identity]
name = "reviewer"
description = "Reads a pull request diff and posts review findings; never edits the repository."
loop = "middle"
model = "gemma4:e4b"
system_prompt = "prompts/reviewer.md"

[scope]
repos = ["github.com/example/repo"]
paths = []
read_paths = ["**"]
max_effect = "repository"
tools = ["read_file", "search", "list_directory", "git_diff", "git_status", "github_pr_status", "github_pr_comments", "github_issue_read"]

[environment]
name = "review"

[limits]
max_iterations = 60
max_wall_clock_secs = 1200
max_concurrent = 4
```

`review` provides no container-executed tools at all: git and coreutils only. All of the reviewer's tools are host-side, so the container exists to satisfy the invariant and to guarantee the reviewer has no compiler or package manager even if `run_command` were ever added to the card by mistake (rule 4 would then reject it).

```toml
[identity]
name = "docs-writer"
description = "Edits documentation and runs the documentation linters."
loop = "inner"
model = "gemma4:e4b"
system_prompt = "prompts/docs-writer.md"

[scope]
repos = ["github.com/example/repo"]
paths = ["docs/**", "README.md"]
max_effect = "workspace"
tools = ["read_file", "write_file", "search", "list_directory", "git_diff", "git_status", "markdown_lint"]

[environment]
name = "docs"

[limits]
max_iterations = 80
max_wall_clock_secs = 1200
max_concurrent = 4
```

`docs` provides `markdown_lint` (a new container-class capability backed by a Nix package such as `markdownlint-cli2` or `typos`; adding it means a new `CARGO_CAPABILITIES`-style entry and tool, see migration step 6) and no cargo. The ceiling is `workspace`, so `--network=none`, and with no cargo in the environment there is no vendoring.

```toml
[identity]
name = "verifier"
description = "Runs the repository's declared checks in the dev container and reports structured pass/fail."
loop = "middle"
model = "none"
system_prompt = { inline = "Deterministic: no model decides the verdict." }

[scope]
repos = ["github.com/example/repo"]
paths = []
max_effect = "workspace"
tools = ["run_checks"]

[environment]
name = "verify"

[limits]
max_iterations = 1
max_wall_clock_secs = 1800
max_concurrent = 2
```

`verify` provides the toolchain the repo's checks invoke (rustfmt, clippy, cargo, nextest) and nothing that can push or call GitHub. The shape of the card (`run_checks` as the only tool, `workspace` ceiling, `paths = []`) follows the fixture in #1032; field values such as `model` and `loop` should be checked against that PR before the fixture lands.

A repo-local narrowing example for `rust-implementer` (valid under phase A):

```toml
[identity]
name = "rust-implementer"
description = "Implements a scoped issue in the api crate only."
loop = "inner"
model = "gemma4:e4b"
system_prompt = { inline = "Work only in api/." }

[scope]
repos = ["github.com/example/repo"]
paths = ["api/**"]
max_effect = "repository"
tools = ["read_file", "write_file", "search", "cargo_*"]

[limits]
max_iterations = 100
max_wall_clock_secs = 1800
max_concurrent = 2
```

It omits `[environment]` and so inherits `rust-impl`. Adding `[environment] name = "rust-impl-lite"` is rejected in v1 and accepted in phase B only if `rust-impl-lite` declares `extends = "rust-impl"` with a subset of its `provides`.

## Test plan

Test names are proposals. Contract tests are written first in each step.

Pure, no Nix or runtime (run everywhere, including the Windows CI leg):

- `identity::schema`: declared `[environment]` parses and round-trips through `to_toml_string`; absent table parses to `None`; invalid names (empty, uppercase, `/`, `..`, 64+ chars, `#`, whitespace, unknown key under `[environment]`) are `InvalidField { field: "environment.name" }`.
- `identity::narrowing` (declared, default, widening): the phase A table above row by row; proptests extending `narrowed_from`, `narrowing_is_reflexive`, `narrowing_is_transitive` and `widening_one_field_is_rejected_naming_it` with an `environment` case; a doctest on `narrows`.
- `environment` (valid, invalid): manifest parse accepts schema 1 and rejects unknown keys, wrong schema, duplicate or unsorted ids, and oversized input; `resolve` covers exact-name provided, exact-name missing (`ToolNotProvided`), glob with matches, dead glob, `run_command` plus extra capability (`ImageWiderThanCard`), no `run_command` plus extras (ok, warning), host-side tools never checked.
- Phase B: `extends` accepted only when the child is a string-subset; a child with an extra id, a cycle, or `extends` naming a third environment are each rejected; proptest that a generated child chain never provides more than its root.
- Registry: the identity container registry for `rust-impl` contains `cargo_check` and not `cargo_audit`; for `review` contains no container-class tool; pinning that a card with `cargo_*` now receives cargo tools (closing the gap above).
- Drift: the Nix capability table and `CARGO_CAPABILITIES` agree (a unit test reading the table from a checked-in JSON generated by the flake step).

Harness-level with stubs (no Nix, no runtime), reusing `get_or_build_image_using`'s injected build function:

- cache key: two environments of one repo build separately and reuse separately; the default key is unchanged; one lock per `(path, env)`.
- default: a card without `[environment]` takes the default builder path and never reads a manifest.
- invalid: declared environment with no container opt-in, missing output, unreadable manifest, failed check, each fails the task with a distinct `error_type` before any workspace is created.
- network: environment resolution leaves `network_policy_for` unchanged for every ceiling.
- vendoring: `docs` (no cargo) skips `offline_cargo`; `rust-impl` at `workspace` ceiling still vendors.

Runtime-gated (the existing `#[ignore]` pattern in `harness/tests/dev_container_integration.rs`; not required for merge, run on a host with Nix and podman): build two environments of the nanna flake, assert `cargo` resolves in `rust-impl` and `docs` has no `cargo`, and that `--network=none` is set for the `docs` container.

## Migration plan

Each step is one PR, red tests first. "Hot" marks files the user named as owned by other sessions. Nothing in this plan touches `container.rs` or `protected.rs`: `ContainerConfig` already takes the image as an argument, and every file that carries environment definitions or cards (`flake.nix`, `nix/**`, `.devcontainer/**`, `.nanna/**`) is already in the shipped protected set. The plan does not add or change any workflow or CI file.

1. Schema and phase A narrowing. Files: `harness/src/identity/schema.rs`, `harness/src/identity/narrowing.rs`, `harness/src/identity/mod.rs` (docs and re-export), `harness/tests/fixtures/identities/global/*.toml` (add `[environment]` to `rust-implementer` and `pr-shepherd`). Hot: none. Note that `harness/src/identity/**` is in nanna's own protected declaration, so this PR needs a human author or explicit sign-off for the protected path. Independent of every open PR.
2. Manifest and checker. Files: `harness/src/environment.rs` (new), `harness/src/lib.rs` (one `pub mod` line). Hot: none. Pure; can run in parallel with step 1 after the `AgentIdentity` field exists, or against a stub field.
3. Nix definitions and builder. Files: `nix/containers.nix` (add `mkEnvironment`, nanna's own environments), `flake.nix` (export `devContainerImage-*` and `devContainerManifest-*`), `harness/src/onboarding/flake_template.rs` (generate the five environments, with tests), `image-builder/src/lib.rs` (`build_environment`, `read_manifest`). Hot: none. `nix/**` and `flake.nix` are protected paths. Overlaps `flake_template.rs` with the #1027 follow-up that asks the template to produce the offline setup; sequence after or with it.
4. Runtime wiring and registry. Files: `harness/src/task.rs` (cache key, environment resolution before the container block, new `error_type` labels), `harness/src/workspace.rs` (`build_tool_registry_for` takes the resolved environment), `harness/src/tools.rs` (identity container registry registers `cargo_*` from the resolved set). Hot: `task.rs` and `workspace.rs`. Conflicts with #1031 (`task.rs` hunks near the container block and `create_with_container_vendoring` in `workspace.rs`), #1032 (`task.rs` verifier branch in `TaskRunner::run`, `verify/`) and #1022 (policy solo mode in `task.rs`). Recommended order: #1031, #1032, #1022 merge first, then this step rebases onto them. If the owners prefer the reverse, the minimal seam is a single `resolve_environment(identity, repo) -> Result<Option<ResolvedEnvironment>, _>` call added immediately before the `use_container` block.
5. Vendoring gate. Files: `harness/src/workspace.rs` (skip `offline_cargo` when the environment provides no cargo capability), `harness/src/offline_cargo.rs` if #1031 has merged. Hot: `workspace.rs`. Strictly after #1031.
6. New capabilities and the three new cards. Files: `harness/src/capabilities.rs` (`markdown_lint` entry), `harness/src/tools.rs` (its tool), fixtures for `reviewer` and `docs-writer`, `harness/tests/fixtures/identities/global/`. Hot: none beyond `tools.rs` (a protected file in nanna's own declaration, not currently in an open PR's diff).
7. Verifier environment. Files: `harness/src/verify/*` and the verifier fixture from #1032. Hot: whichever `task.rs` hunks #1032 owns. Strictly after #1032 merges, owned by its author.
8. Repo-local `extends` (phase B), only when needed. Files: `harness/src/environment.rs`, `harness/src/task.rs` at the resolution seam from step 4. Hot: `task.rs`, tiny.
9. Docs. Files: `ARCHITECTURE.md` section, `harness/src/identity/mod.rs` module docs; the configuration reference promoted out of this document. Hot: none.

Steps 1, 2, 3 and 6 can proceed in parallel with the open hot-file PRs. Only steps 4, 5, 7 and 8 must wait for them.

## Open questions for the user

1. Global cards across repos. A global card with `[environment]` forces every repo it runs against to define that environment, or the task fails closed. Is that acceptable (the onboarding template generates the five), or should a card be able to say `fallback = "repo-default"`? The doc recommends no fallback for a declared environment, because the default image is wider than any named one; a `fallback` key would be allowed only for cards without `run_command`.
2. Strictness of rule 4 (`ImageWiderThanCard` error for cards with `run_command`) versus a warning. Error matches the stated goal; warning is friendlier to existing images.
3. Verifier: is `verify` a fixed toolchain environment (this doc), or should `.nanna/verify.toml` declare the capabilities its checks need so the image can be minimal? The latter couples #1028 config to this schema.
4. Repo-local widening to a child environment (phase B): ship it in v1, or defer until a repo asks? This doc recommends deferring.
5. New capability ids for non-Rust tooling (`markdown_lint` here): do we want a capability registry beyond `CARGO_CAPABILITIES`, or keep these as repo-defined `run_command`-style checks?
6. The `reviewer`, `docs-writer` and `verifier` card values above are proposals; the user should confirm ceilings and tool lists.
