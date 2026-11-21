# Per-identity dev containers (design for #1012)

Status: design only. No code in this change. Refs #1012; relates to #1027,
#1028, #733, #1031, #1032.

The goal: each inchworm SDLC journey step (`rust-implementer`, `pr-shepherd`,
`reviewer`, `docs-writer`, `verifier`) is a subagent with an explicit
capability set and an explicit environment, and the environment is a
container. A capability exists only in the environment of the subagent
permitted to use it. The container model stays the only execution path;
nothing here adds a non-container way to run a command.

Vocabulary used below:

- Subagent definition: the identity card (TOML). It declares the capability
  set (`scope.tools`) and names the environment.
- Environment: a container image plus a small declaration of the capabilities
  that image implements. It is defined in the target repo.
- Capability: something a subagent can do, named by an id the harness treats
  as opaque. Examples: `cargo_test`, `check_docs`, `run_robot_test`.

## What exists on main today (6f38d544)

- A task opts into the container path only when the base checkout has both
  `flake.nix` and `.devcontainer/` (`TaskRunner::run`,
  `harness/src/task.rs`). Otherwise `TaskWorkspace::create` runs with no
  container and `run_command` is absent.
- The image comes from one flake output, `devContainerImage`, built by
  `image_builder::build_dev_container` (`nix build
  path:<repo>#devContainerImage`), loaded into podman/docker by
  `container::load_image_from_path`, and cached in `TaskRunner::image_cache`
  keyed by canonical repo path only. One image per repo, shared by every
  identity.
- `nix/containers.nix` builds nanna's own image with `nix2container.buildImage`
  from `devToolPaths` plus the Rust toolchain and cargo plugins. The
  onboarding flake template (`harness/src/onboarding/flake_template.rs`)
  generates the same shape for other repos, filling `devContainerPackages`
  from `harness/src/capabilities.rs` (`CARGO_CAPABILITIES`: tool id to
  `nix_package`).
- The network policy is derived from the card, not from the image:
  `network_policy_for` calls `NetworkPolicy::for_ceiling(identity.scope.
  max_effect)`; `None` and `workspace` get `--network=none`.
- Tools split in two classes. Host-side tools (`read_file`, `write_file`,
  `list_directory`, `search`, `git_*`, `github_*`, `ci_*`, sandbox tools) run
  in the harness process against the worktree and never need anything from the
  image. Container-executed tools (`run_command`, `cargo_build|test|check|
  bench|run`, `cargo_deny`, `cargo_audit`, `trunk_build`, `sqlx_migrate`, and
  with #1032 `run_checks`) run through `<runtime> exec` and need the binary in
  the image.
- Gap found while reading: `create_container_tool_registry_for_guarded` (the
  identity path used by `TaskWorkspace::build_tool_registry_for`) registers
  only `run_command` from the container class. The `cargo_*` tools are
  registered only by the non-identity `create_container_tool_registry_guarded`.
  So a card listing `cargo_*` today receives no cargo tools in a container.
  Registry linkage (below) has to close this regardless of this design.
- Repo-local cards (`.nanna/agents/*.toml`) may only narrow the global card
  (`AgentIdentity::narrows`, `harness/src/identity/narrowing.rs`): ceilings at
  or below, lists as exact-string subsets, limits at or below.
  `identity.model`, `system_prompt`, `description`, `loop` are free.
- #733: `[limits]`, `model` and `system_prompt` are validated but not enforced
  at runtime; `scope.repos` is now checked by `scope::check_repo` in
  `TaskRunner::run`. This design adds nothing that depends on the
  still-unenforced fields.

## Decisions

D1 to D5 record the user's rulings, in the user's words, followed by what this
design does with them. D6 onward are design decisions that follow from them.

### D1. No repo-default fallback; installing nanna means declaring environments

Ruling: "no, we need to enforce that installing nanna means explicitly
declaring a dockerfile-like dev image that can be built locally."

- `[environment]` is required in every card. A card without it is a load
  error (`InvalidField`, missing `environment.name`), not a silent default.
- The implicit "repo default image" is removed entirely: no `devContainerImage`
  fallback, no `fallback` key, and the `flake.nix` plus `.devcontainer/`
  opt-in marker no longer decides whether a task gets a container. A task
  bound to an identity always runs in its declared environment, and if the
  environment is not declared in the repo or cannot be built locally, the task
  fails closed with a distinct error (the same posture as
  `VerifierRequiresContainer` in #1032).
- Consequence for global cards: a global card names an environment, and every
  repo it runs against must define that environment. The onboarding step that
  installs nanna in a repo generates the declarations; it is what "installing
  nanna" means here.
- Existing fixtures without `[environment]` (`harness/tests/fixtures/
  identities/**`) are updated in the schema step.

### D2. Card and environment declare capabilities together and must line up

Ruling: "no idea what wider images are in this context. if my guess to your
meaning is right: the images must be explicitly declared in the subagent def
so that the capabilities line up."

What "wider" meant: the earlier draft compared the tools an image provides
with the tools the card allows. An image is wider than the card when it
provides a capability the card does not allow (for example a card that may
only run tests, in an image that also has a deploy tool). The opposite
mismatch, an image that lacks a capability the card lists, was already an
error.

Reading applied here (flagged for confirmation): the card names the
environment and lists the capability set, and the two must be identical for
the container-executed class. A mismatch in either direction is a load-time
hard error: `CapabilityNotProvided` (card needs more than the image gives) and
`CapabilityNotPermitted` (image gives more than the card allows). There is no
warning tier. Host-side tools are never compared with the image.

### D3. The verifier is an ordinary subagent

Ruling: "verifiers are subagents and need an explicit container just like any
other."

The `verifier` card has its own `[environment]` and its own capability set,
exactly like the other four. There is no special fixed verifier toolchain and
no capability list declared by `.nanna/verify.toml`. The checks the verifier
may run are the capabilities its environment declares; if #1028 keeps a
`verify.toml`, it may only select among the verifier environment's
capabilities and cannot add one. This needs agreement with the owner of #1028
(see the confirmation list).

### D4. Child environments and `extends` are removed

Ruling: "no idea what a child environment is. Each subagent needs an explicit
capability set which corresponds with its environment, which is defined as a
container."

What it was: an earlier draft let a repo-local card pick a different
environment than the global card if that environment declared itself a
smaller version (a "child") of the global one via `extends`. That idea, the
`extends` key and the whole second narrowing phase (phase B) are removed. Each
subagent has exactly one environment, named by its card.

### D5. Capabilities are stack-agnostic, owned by subagent and environment

Ruling: "capabilities are stack-agnostic. a repo for a robot os could involve
the capability to run a physical test on actual robots. the interface of
capabilities and its implementation is internalized in the subagent definition
and its associated environment."

- There is no global capability registry for non-Rust tools and no hard-coded
  `markdown_lint` id. The harness treats capability ids as opaque strings.
- The interface lives in the card: `scope.tools` lists the capability ids the
  subagent may call, as it does for every tool today.
- The implementation lives in the environment declaration (D7): each
  capability the environment provides maps an id to the command the harness
  executes inside that environment's container.
- `CARGO_CAPABILITIES` and the `cargo_*` tools stay as shipped, built-in
  definitions for Rust repos. They are one stack's capabilities, not the
  model: the onboarding step for a Rust repo writes them into the environment
  declaration, and nothing in the schema or resolution code mentions Rust.
- Example: a robot-OS repo defines a `robot-lab` environment whose image holds
  the robot toolchain and whose declaration provides `run_robot_test` ("run the
  physical test suite on the attached robots", command
  `robot-test --suite hardware`). Only a subagent whose card lists
  `run_robot_test` and names `robot-lab` can ever call it. A physical test
  needs a network path to the robots, so such a card also needs a ceiling
  above `workspace`; the network policy is still derived from the ceiling,
  never from the environment.

### D6. The mapping lives in the card, as a required `[environment]` table

```toml
[environment]
name = "rust-impl"
```

One key in v1: `name`, matching `[a-z0-9][a-z0-9-]{0,62}`.

Why the card and not a separate file:

- The card is already the single place where the capability set (tools, paths,
  ceiling) is declared and where the narrowing rule is enforced. A second file
  would create a card-to-environment sync problem and a second narrowing
  implementation.
- Protection comes for free. Global cards live in the host config directory
  that the dev container never mounts; repo-local cards and environment
  declarations live under `.nanna/**`, which is in the shipped protected set.
  An agent cannot change which environment it runs in or what one provides.
- The card carries only a name. What the name means (how to build the image,
  which capabilities it implements) is in the repo, next to the other
  environments (D7).

Rejected: agent-definition frontmatter (cards are TOML, no frontmatter
exists); a `network` or `image` field in the card (the image is chosen by
repo-owned, protected files, never by a string an agent-adjacent file
supplies; network stays derived from the ceiling).

### D7. Definition format: one declaration per environment, two build backends

The user asked for a "dockerfile-like dev image that can be built locally",
while the earlier draft chose nix2container flake outputs. This design keeps
one declaration and lets it point at either, with the Containerfile as the
default:

```text
.nanna/environments/<name>/
  environment.toml   # the declaration: capabilities and build backend
  Containerfile      # present when the backend is a Containerfile
```

```toml
schema = 1
name = "rust-impl"

[build]
containerfile = "Containerfile"
# or, for repos that already have a Nix flake and want reproducible layers:
# flake_output = "devContainerImage-rust-impl"

[[capability]]
id = "cargo_check"
description = "Type-check the workspace."
exec = ["cargo", "check", "--workspace", "--offline"]

[[capability]]
id = "cargo_test"
description = "Run the workspace tests."
exec = ["cargo", "test", "--workspace", "--offline"]
```

Why the Containerfile is the default:

- It is what the user described and what a repo that is not a Nix project can
  write. Installing nanna must not require adopting Nix.
- `podman build` is a local build and the result runs in the same container
  runtime as today, so the container model is untouched. The build runs on the
  host as the harness's own step, not inside any agent container; it may use
  the network for base images and package installs, which the agent's
  container never gets unless its ceiling says so.
- The declaration, not the image, is the source of truth for the capability
  list (a Containerfile cannot prove what is installed). The repo owner, who is
  trusted for their own repo, writes it, and it is protected from the agent.

Why the Nix backend stays:

- Nanna's own repo and any repo already using the flake template get
  reproducible, cached, layer-sharing images (`docs/binary-cache-strategy.md`)
  with no new builder. `flake_output` reuses the existing `image_builder`
  path (`nix build path:<repo>#<output>`); the output name is validated to a
  fixed charset so it cannot alter the flake reference.
- The onboarding template can generate the capability list and the
  `environment.toml` from one Nix function, so the two cannot drift on that
  backend.

Rejected: `devcontainer.json` (the harness never reads it today, its
`features` fetch from the network at build time, and it cannot express a
capability list); a single Nix-only format (contradicts "dockerfile-like" and
makes Nix a prerequisite of installing nanna).

`run_command` is not a capability in the declaration: every image has a shell
because the container command is `sleep infinity` from coreutils; whether a
card may use it is the card's decision (see D8 for what that implies).

`image_builder` gains a Containerfile builder (`podman build` with a content
hash tag, so an unchanged Containerfile is not rebuilt) next to the existing
Nix one.

### D8. Load-time check: card and environment must line up

"Load time" means task start, before the container is created, because the
declaration lives in the repo. A new module `harness/src/environment.rs`
(pure functions, unit-testable without Nix or a runtime) provides:

- `EnvironmentDeclaration::parse(toml) -> Result<_, EnvironmentError>`
- `resolve(card, declaration) -> Result<ResolvedEnvironment, EnvironmentError>`

The rules apply to the container-executed class only: the capabilities named
in the declaration. `run_command` and `run_checks` are harness built-ins and
are decided by the card alone.

1. Expand each `scope.tools` pattern over the declared capability ids.
2. An exact name (no `*`) in the card that names a container-executed
   capability (one the harness knows is not host-side) must be provided by the
   environment, otherwise `CapabilityNotProvided { id, environment }`.
3. A glob pattern (`cargo_*`) must match at least one provided capability,
   otherwise `DeadPattern`.
4. Every capability the environment provides must be matched by some pattern
   in the card, otherwise `CapabilityNotPermitted { id, environment }`.
5. `ResolvedEnvironment` carries the provided set into registry construction:
   a container-class tool is registered iff the card allows it and the
   environment provides it, and the registered tool executes the declared
   `exec` in that environment's container. This also fixes the registry gap
   above.

Rules 2 and 4 together are the "must line up" ruling: for the container-
executed class the card's capability set and the environment's provided set
are equal. A card that also allows `run_command` can reach any binary in the
image, so for such a card the image contents are not bounded by the
declaration; this is recorded as a non-guarantee below and the shipped cards
omit `run_command`.

Network is never an environment property. `network_policy_for` is unchanged,
so an environment cannot widen the ceiling-derived policy, and
`ContainerConfig`/`container::run_args` do not change: they already take the
image reference as an argument.

### D9. Narrowing of repo-local cards

A repo-local card can only narrow, as today. For `[environment]` the rule is
pure and lives in `AgentIdentity::narrows` (no I/O, so the existing property
tests stay valid). An override either inherits the base card's environment or
repeats it unchanged:

| base `[environment]` | override `[environment]` | result |
|---|---|---|
| `name = X` | absent | ok, the override inherits X (the catalog copies the base environment into the override; see step 1) |
| `name = X` | `name = X` | ok, repeated unchanged |
| `name = X` | `name = Y`, Y != X | rejected: `WidensScope { field: "environment.name" }` |

Consequence of D8: an override that drops a container-class capability from
`scope.tools` while keeping X would no longer equal X's provided set, and
fails the load-time check. This is intended: the capability set and the
environment are one unit. A repo that wants a smaller capability set for the
same journey step declares a differently named subagent, with its own card and
environment, rather than narrowing the existing one. Overrides may still
narrow everything that is not container-executed (paths, ceiling, host-side
tools, limits).

### D10. Build caching and reuse across cards

- Cache key becomes `(canonical repo path, environment name)` in `image_cache`
  and `build_locks`. Concurrent tasks for two environments of one repo build
  in parallel; two tasks for the same environment serialise on one lock.
- The loaded image reference includes the image content hash (the Containerfile
  backend tags by hash of the Containerfile and its build context; the Nix
  backend by store hash), so identical environments of two repos reuse one
  runtime image.
- Sharing between environments happens in the build tools: Containerfiles that
  start from a shared base image reuse its layers in the runtime's layer
  store; the Nix backend shares store paths. Cold cost is up to five builds per
  repo, each a delta over a cached base. Existing limitation, unchanged: the
  in-process cache never invalidates when a declaration changes during a
  daemon's lifetime.
- The declaration is parsed and checked against the card before the image is
  built, so a mismatch fails in seconds without a build.

### D11. Interaction with `--network=none` (#1027) and the verifier (#1028)

- #1027 stages a vendor directory on the host and mounts it read-only at
  `/opt/nanna-offline/vendor` plus `/.cargo/config.toml`. Both are host
  mounts, independent of the image, so they compose with any environment.
- Required condition: vendoring must run only when the resolved environment
  provides a cargo capability. A `docs-writer` environment with no cargo must
  not trigger `cargo vendor`, because #1031 refuses the task on a stale or
  missing `Cargo.lock`, which would block a docs task for an unrelated
  reason. This is a small `workspace.rs` change after #1031 lands.
- Network stays derived from the ceiling: `verifier` and `docs-writer`
  (ceiling `workspace`) are `--network=none`; `rust-implementer`,
  `pr-shepherd` and `reviewer` (ceiling `repository` or above) keep the
  default network.
- The verifier needs nothing special: its environment declares the check
  capabilities it uses, and a check whose binary is missing from the image
  fails with the existing `VerifierExecFailed`.

### D12. Scope boundary with the pi harness

`docs/pi-harness.md` describes a second container: the agent container in
which pi reasons, with at most a broker socket mounted. Environments in this
document select the dev container where broker-run and container-executed
tools execute. They do not affect the pi agent image (`InvalidImage` checks in
`IsolationPolicy` are unchanged), and the broker keeps calling
`ToolRegistry::execute` on a registry built through `model_facing_registry`,
which will now be built from the resolved environment.

## The five cards

Cards live in the host config directory (`$NANNA_CONFIG_DIR/agents/`).
`rust-implementer` and `pr-shepherd` follow the existing fixtures with an
`[environment]` table added; `reviewer`, `docs-writer` and `verifier` do not
exist as fixtures yet, so their scope values below are proposals for the user
to adjust. Repo names, models, prompt paths, capability ids and commands are
illustrative. Each card is followed by the environment declaration it must
line up with, described by its capability list (the `[build]` table is as in
D7).

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
tools = [
  "read_file", "write_file", "search", "git_*",
  "cargo_build", "cargo_check", "cargo_test", "cargo_bench", "cargo_run",
]

[environment]
name = "rust-impl"

[limits]
max_iterations = 200
max_wall_clock_secs = 3600
max_concurrent = 4
```

`.nanna/environments/rust-impl/environment.toml` provides exactly
`cargo_build`, `cargo_check`, `cargo_test`, `cargo_bench`, `cargo_run`, and
its image carries the Rust toolchain with rustfmt and clippy. The fixture's
`cargo_*` glob is spelled out because rule 4 requires the two sets to be
equal; the card does not receive `cargo_deny`, `cargo_audit`, `trunk_build` or
`sqlx_migrate`, and has no `run_command`.

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
tools = [
  "read_file", "write_file", "search", "list_directory",
  "cargo_check", "cargo_test",
  "git_*", "github_pr_status", "git_commit", "git_push_branch",
  "github_pr_open", "github_pr_comments", "github_pr_promote",
  "github_pr_close", "github_issue_read", "github_issue_comment",
]

[environment]
name = "rust-fix"

[limits]
max_iterations = 100
max_wall_clock_secs = 1800
max_concurrent = 2
```

`rust-fix` provides `cargo_check` and `cargo_test` only. The fixture's
`cargo_*` is narrowed to the two tools the shepherd needs to reproduce CI
failures locally; this is a deliberate edit of that card.

```toml
[identity]
name = "reviewer"
description = "Reads a pull request diff and comments on it; never edits the repository."
loop = "middle"
model = "gemma4:e4b"
system_prompt = "prompts/reviewer.md"

[scope]
repos = ["github.com/example/repo"]
paths = []
read_paths = ["**"]
max_effect = "repository"
tools = [
  "read_file", "search", "list_directory", "git_diff", "git_status",
  "github_pr_status", "github_pr_comments", "github_issue_read",
  "github_issue_comment",
]

[environment]
name = "review"

[limits]
max_iterations = 60
max_wall_clock_secs = 1200
max_concurrent = 4
```

`review` declares no capabilities: a minimal image with git and coreutils. All
of the reviewer's tools are host-side, so the container exists to satisfy the
invariant and to guarantee the reviewer has no compiler or package manager. If
a capability were later added to the environment without the card, rule 4
would reject the load.

```toml
[identity]
name = "docs-writer"
description = "Edits documentation and runs the repository's documentation checks."
loop = "inner"
model = "gemma4:e4b"
system_prompt = "prompts/docs-writer.md"

[scope]
repos = ["github.com/example/repo"]
paths = ["docs/**", "README.md"]
max_effect = "workspace"
tools = [
  "read_file", "write_file", "search", "list_directory",
  "git_diff", "git_status", "check_docs",
]

[environment]
name = "docs"

[limits]
max_iterations = 80
max_wall_clock_secs = 1200
max_concurrent = 4
```

`docs` declares one capability, `check_docs`, whose `exec` is whatever the
repo uses to check its documentation (a Markdown linter, a spell checker); the
id and command are the repo's, not the harness's. No cargo, so no vendoring.
The ceiling is `workspace`, so `--network=none`.

```toml
[identity]
name = "verifier"
description = "Runs this repository's checks in its own environment and reports a structured pass or fail per check; no model decides the verdict."
loop = "inner"
model = "gemma4:e4b"
system_prompt = { inline = "Not used: the harness runs the checks itself and no model is consulted." }

[scope]
repos = ["github.com/example/repo"]
paths = []
max_effect = "workspace"
tools = ["run_checks", "check_format", "check_lint", "check_test"]

[environment]
name = "verify"

[limits]
max_iterations = 1
max_wall_clock_secs = 3600
max_concurrent = 4
```

`verify` is an ordinary environment with its own image and declares
`check_format`, `check_lint` and `check_test`, the checks this repo wants run.
It has no push or GitHub tooling because its image has none and its card
lists none. The card is the #1032 fixture
(`harness/tests/fixtures/identities/global/verifier.toml` on
`feat/1028-verifier`) plus the `[environment]` table and the explicit
capability ids; reconciling `run_checks` with these ids is a point for #1028's
owner.

A repo-local override for `rust-implementer` (valid under D9) either repeats
the environment unchanged, as here, or omits it and inherits `rust-impl`:

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
tools = [
  "read_file", "write_file", "search",
  "cargo_build", "cargo_check", "cargo_test", "cargo_bench", "cargo_run",
]

[environment]
name = "rust-impl"

[limits]
max_iterations = 100
max_wall_clock_secs = 1800
max_concurrent = 2
```

It narrows paths, host-side tools (no `git_*`) and limits, and repeats
`rust-impl`. Naming any other environment is rejected, and dropping a cargo
capability from `tools` fails the D8 check, so a smaller cargo set needs its
own subagent and environment.

## Worked example: the cargo dependency reviewer

Ruling, in the user's words: "offline cargo's install can happen when the
container is built against main. iff new deps are added, a dedicated subagent
can do a dep review to make sure that it's compliant with the repo's licenses
and that it passes a security review before the dependency is installed, in an
environment configured so that malicious code execution cannot lead to a
security incident or leak, and where computation and memory is bounded and
external network calls are limited to a small allowlist. This should be a
default/example nanna subagent specifically for interacting with cargo in rust
projects."

This section implements that ruling with the schema from D6 to D8. It is the
reference example of an environment built for hostile input, and of a subagent
whose capability set is deliberately tiny. Hosts, limits, capability ids and
commands are proposals. Three things here go beyond the schema above and are
marked as such: the `[limits]`, `[sandbox]` and `[network]` tables in
`environment.toml`, the verdict artifact, and the build gate. They are listed
under "Needs confirmation".

### Lifecycle: dependencies are installed at image build time

1. Installing. The environment image for a Rust repo (`rust-impl`, `rust-fix`,
   `verify`) is built against the lockfile on `main`. The Containerfile fetches
   (or vendors) every crate in `Cargo.lock` into the image's offline
   `CARGO_HOME` and sets `CARGO_NET_OFFLINE=true`. Build runs on the host as
   the harness's own step (D7), so it may use the network; the agent container
   never does. Nothing is installed at task time.
2. Changing. A change that adds or bumps a dependency (a diff to the
   `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]` or
   `[patch]`/`[replace]` tables, or to the set of `(name, version, checksum)`
   entries in `Cargo.lock`) first goes through the `cargo-dep-reviewer`
   subagent. The reviewer reads only the delta against the previously approved
   lockfile plus the deny policy, and writes a verdict (below).
3. Gating. Only an `approve` verdict for a dependency lets the image build
   install it. The build step refuses any lockfile entry whose checksum is not
   in the approved set. `deny` and `needs-human` leave the dependency
   uninstalled, so the image does not build with it and no task can compile
   against it.
4. Consequence for #1027 and #1031. #1027 stages a host `cargo vendor`
   directory at task start and mounts it at `/opt/nanna-offline/vendor`, and the
   work in #1031 refuses a task on a stale or missing `Cargo.lock`. If approved
   dependencies are already in the image, task-time vendoring is unnecessary
   for them, and the D11 vendoring gate (step 5) would have little left to
   gate. This design does not decide that. It is a question for the owners
   of issues 1027 and 1031: do they keep task-time vendoring for dependencies
   that have no approved verdict yet (which, with D1's no-fallback rule, would
   be a second install path that bypasses the reviewer), or retire it in
   favour of build-time installation? Until they answer, nothing in the
   migration plan removes their code.

Open point in step 2: the implementer cannot resolve a new lockfile entry
offline, so someone must produce the proposed `Cargo.lock` delta before the
reviewer sees it. Options are a harness pre-step that runs `cargo update -p`
with the same allowlisted egress, or the implementer submitting only a
`(name, version requirement)` request. Either way the reviewer's input is a
lockfile, never code the implementer wrote. See "Needs confirmation".

### The card

The card is `cargo-dep-reviewer`. It has no host-side file or git tools beyond
what the capabilities below expose, no `write_file`, no `git_*`, no `github_*`,
no `run_command`, and none of `cargo_build`, `cargo_check`, `cargo_test`,
`cargo_bench` or `cargo_run`. Nothing in its capability set can compile,
execute a build script, run a test, or run any dependency code.

```toml
[identity]
name = "cargo-dep-reviewer"
description = "Reviews added or bumped Rust dependencies for license compliance, advisories and suspicious build-time code, without executing any of it. Writes a verdict only."
loop = "inner"
model = "gemma4:e4b"
system_prompt = "prompts/cargo-dep-reviewer.md"

[scope]
repos = ["github.com/example/repo"]
paths = []
read_paths = ["Cargo.toml", "**/Cargo.toml", "Cargo.lock", "deny.toml"]
max_effect = "workspace"
tools = [
  "dep_inventory", "dep_lock_diff", "dep_name_check", "dep_fetch",
  "dep_read_source", "dep_build_script_scan", "dep_unsafe_scan",
  "dep_license_check", "dep_advisory_check", "write_verdict",
]

[environment]
name = "cargo-dep-review"

[limits]
max_iterations = 40
max_wall_clock_secs = 600
max_concurrent = 2
```

The ten capability ids are opaque to the harness (D5). Their meaning:

- `dep_inventory`: list every package in `Cargo.lock` with name, version,
  registry source and checksum, and flag any non-registry source (`git`,
  `path`, alternative registry). Parses the lockfile itself; does not call
  cargo.
- `dep_lock_diff`: compare the proposed lockfile with the previously approved
  one: added, removed and bumped packages, and the new transitive set each
  addition drags in.
- `dep_name_check`: for each new name, report edit distance and common
  confusions (`-`/`_`, transposition, homoglyphs) against the approved set and
  a bundled list of popular crate names, and the crate's age and publisher
  where the index records it.
- `dep_fetch`: download the `.crate` archives for the new packages through the
  allowlisted egress, verify each against the lockfile checksum, and unpack
  them into `/scratch/crates` with size and file-count caps. Unpacking never
  follows symlinks out of the target directory and never executes anything.
- `dep_read_source`: print a bounded range of a file from the unpacked crates.
  This is how the model reads `build.rs` and proc-macro sources.
- `dep_build_script_scan`: static scan (parse, no compile) of `build.rs` and
  of crates with `proc-macro = true`: process spawns, network use, filesystem
  access outside `OUT_DIR`, environment reads, `include!`/`include_bytes!` of
  paths outside the crate, linker and `cc` flags.
- `dep_unsafe_scan`: count and locate `unsafe` blocks and `extern` items per
  new crate (a parse, in the style of cargo-geiger without building).
- `dep_license_check`: run the repo's `deny.toml` license policy over the new
  packages.
- `dep_advisory_check`: check the new packages against the RustSec advisory
  database, including yanked versions and unmaintained notices.
- `write_verdict`: write the structured verdict to `/scratch/verdict.json`. It
  validates the shape and size and is the only way the subagent produces
  output.

The model's role is bounded on purpose. License, advisory, checksum and
source-kind outcomes come from deterministic capabilities and the harness
computes the combined verdict from them: the model can escalate a dependency
(for example to `needs-human` after reading a suspicious `build.rs`) but cannot
turn a deterministic `deny` into `approve`. Crate source is attacker-controlled
text; this rule is what keeps a prompt injection in a README or comment from
approving anything.

Verdict shape (one object per run, written once):

```json
{
  "schema": 1,
  "lockfile_sha256": "<hash of the proposed Cargo.lock>",
  "baseline_lockfile_sha256": "<hash of the approved Cargo.lock>",
  "dependencies": [
    {
      "name": "example-crate",
      "version": "1.2.3",
      "checksum": "<sha256 from Cargo.lock>",
      "verdict": "approve | deny | needs-human",
      "reasons": ["license MIT is allowed by deny.toml"],
      "evidence": [
        {"capability": "dep_license_check", "result": "pass"},
        {"capability": "dep_build_script_scan", "result": "no build.rs"}
      ]
    }
  ]
}
```

The harness validates this artifact (schema, size cap, string length caps,
every dependency in the lockfile delta present exactly once) before anyone
reads it, and treats a missing, truncated or invalid verdict as not approved.

### The environment

Declaration, at `.nanna/environments/cargo-dep-review/environment.toml`. The
`[[capability]]` entries are the implementation half of the ten ids above and
must equal the card's `scope.tools` (D8 rules 2 and 4). `[limits]`,
`[sandbox]`, `[network]` and `[[input]]` are new keys proposed for this
example.

```toml
schema = 1
name = "cargo-dep-review"

[build]
containerfile = "Containerfile"

[limits]
cpus = 1.0
memory_mb = 1024
pids = 128
wall_clock_secs = 600
tmpfs_mb = 256

[sandbox]
read_only_root = true
cap_drop = ["ALL"]
no_new_privileges = true
user = "10001:10001"
env = []

[network]
mode = "allowlist"
allow = [
  "index.crates.io:443",
  "static.crates.io:443",
  "github.com:443",
]

[[input]]
source = "staged-manifests"
target = "/input"
readonly = true

[[capability]]
id = "dep_inventory"
description = "List lockfile packages, checksums and source kinds."
exec = ["nanna-dep", "inventory", "--lock", "/input/Cargo.lock"]

[[capability]]
id = "dep_lock_diff"
description = "Diff the proposed lockfile against the approved one."
exec = ["nanna-dep", "lock-diff", "--new", "/input/Cargo.lock", "--base", "/input/approved/Cargo.lock"]

[[capability]]
id = "dep_name_check"
description = "Flag typosquat-like names among new packages."
exec = ["nanna-dep", "name-check", "--new", "/input/Cargo.lock", "--base", "/input/approved/Cargo.lock"]

[[capability]]
id = "dep_fetch"
description = "Download, checksum-verify and unpack new crates without executing them."
exec = ["nanna-dep", "fetch", "--lock", "/input/Cargo.lock", "--out", "/scratch/crates", "--max-crate-mb", "50", "--max-unpacked-mb", "150"]

[[capability]]
id = "dep_read_source"
description = "Print a bounded range of a file from an unpacked crate."
exec = ["nanna-dep", "read-source", "--root", "/scratch/crates"]

[[capability]]
id = "dep_build_script_scan"
description = "Statically scan build.rs and proc-macro crates."
exec = ["nanna-dep", "scan-build", "--root", "/scratch/crates"]

[[capability]]
id = "dep_unsafe_scan"
description = "List unsafe usage in new crates."
exec = ["nanna-dep", "scan-unsafe", "--root", "/scratch/crates"]

[[capability]]
id = "dep_license_check"
description = "Check new packages against the repo's deny.toml license policy."
exec = ["cargo", "deny", "--frozen", "--manifest-path", "/input/Cargo.toml", "--config", "/input/deny.toml", "check", "licenses"]

[[capability]]
id = "dep_advisory_check"
description = "Check new packages against the RustSec advisory database."
exec = ["nanna-dep", "advisories", "--lock", "/input/Cargo.lock", "--db", "/scratch/advisory-db"]

[[capability]]
id = "write_verdict"
description = "Validate and write the verdict artifact."
exec = ["nanna-dep", "write-verdict", "--out", "/scratch/verdict.json"]
```

Containerfile sketch (the `rust-impl` image of the same repo is built from
`main`'s lockfile with its own Containerfile and is not this image):

```dockerfile
FROM docker.io/library/rust:1-slim-bookworm@sha256:<pinned-digest> AS tools
RUN cargo install --locked --root /opt/tools \
      cargo-deny@<pinned> cargo-audit@<pinned>
COPY nanna-dep/ /src/nanna-dep/
RUN cargo install --locked --path /src/nanna-dep --root /opt/tools

FROM docker.io/library/debian:bookworm-slim@sha256:<pinned-digest>
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=tools /opt/tools/bin/ /usr/local/bin/
COPY rustc-info /usr/local/bin/rustc-info
RUN useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin reviewer
ENV CARGO_HOME=/scratch/cargo \
    CARGO_NET_OFFLINE=true \
    RUSTC=/usr/local/bin/rustc-info \
    HTTPS_PROXY=http://egress-proxy:3128
USER 10001:10001
WORKDIR /scratch
CMD ["sleep", "infinity"]
```

Notes on the sketch:

- `cargo-deny` and cargo need a `rustc` to answer `-vV` and `--print cfg`
  when they read a manifest. `RUSTC` points at `rustc-info`, a wrapper that
  answers only those two queries and exits non-zero for anything else, so the
  image contains no compiler that a capability could reach. This follows D8's
  caveat: the declaration bounds capabilities, not image contents, and this
  environment has no `run_command`.
- Tool versions and base images are pinned by digest so the reviewer itself is
  reproducible.

Hardening, one key to one container flag, applied by the harness when it
creates the container and not changeable by the agent:

- Rootless: the harness runs podman rootless; `user` makes the process a
  non-root user inside it.
- `read_only_root = true`: `--read-only`. The only writable place is the
  `/scratch` tmpfs.
- `cap_drop = ["ALL"]`: `--cap-drop=ALL`, no `--cap-add`.
- `no_new_privileges = true`: `--security-opt=no-new-privileges`.
- Default seccomp: no `--security-opt seccomp=` override, so the runtime's
  default profile applies. No `--privileged`, no devices, no extra namespaces.
- Mounts: `/input` read-only from a staged copy of the manifests, `Cargo.lock`,
  the approved lockfile and `deny.toml`. The harness stages only those files
  (with empty stub targets so cargo can parse the manifests), never the
  worktree, never `.git`, never `.cargo/config.toml`. `/scratch` is a tmpfs of
  `tmpfs_mb`. No bind mount is writable.
- No secrets: `env = []` means the container is created with an empty
  environment apart from the fixed values in the Containerfile. No `GH_TOKEN`,
  no registry token, no SSH agent socket, no credential helper, no
  `~/.cargo/credentials`. The harness does not forward its own environment.
- Bounds, concrete defaults: `cpus = 1.0` (`--cpus`), `memory_mb = 1024`
  (`--memory`, with `--memory-swap` equal so swap adds nothing), `pids = 128`
  (`--pids-limit`), `wall_clock_secs = 600` (the harness kills and removes the
  container at the deadline, independent of the card's own
  `max_wall_clock_secs`), `tmpfs_mb = 256` (`--tmpfs /scratch:size=...`).
  Because a tmpfs is charged to the container's memory cgroup, an oversized
  crate cannot exceed the memory bound by unpacking to scratch. `nanna-dep
  fetch` also enforces per-crate caps (50 MiB archive, 150 MiB unpacked, file
  count) so the usual outcome of an oversized crate is a `needs-human` verdict
  with the reason `too_large`; the cgroup limit is the backstop, and an
  out-of-memory kill or timeout is recorded as "not approved".

Network is off unless allowlisted, and the allowlist is enforced outside the
reviewer process:

- The harness creates a podman network with `--internal` (no route off the
  host) and attaches the reviewer container to it only. A second container, the
  egress proxy sidecar, is attached to that network and to a normal one. The
  reviewer has no DNS resolver of its own and reaches the world only through
  `HTTPS_PROXY`.
- The proxy accepts `CONNECT host:443` only, and only for the exact hosts in
  `allow`; everything else gets a refusal, and it logs every attempt (allowed
  and denied) to the harness. The proxy runs with the same hardening, belongs
  to the harness, and its configuration is generated from `[network] allow`,
  not from anything the reviewer controls.
- Hosts: `index.crates.io` (sparse index) and `static.crates.io` (crate
  downloads) for `dep_fetch`; `github.com` only to clone the public
  `rustsec/advisory-db` repository for `dep_advisory_check`. Advisory data can
  instead be a snapshot the harness fetches itself and mounts read-only, which
  removes `github.com` from the allowlist; that is the stricter variant.
- Alternative to the sidecar: rootless `pasta` outbound rules. They filter by
  address and port, not hostname, and the crates hosts sit behind a CDN with
  changing addresses, so the allowlist would be coarse. The proxy is preferred.
- Because the reviewer is in an internal network, a process that ignores
  `HTTPS_PROXY` (a direct `connect()`) has no route and fails. That is the
  property the network test checks.

What the allowlist does not guarantee: the proxy decides by host name, not by
URL path or content, so any allowed host can be used as a one-way channel by
anything that already runs inside the container (for example data encoded into
a request to `static.crates.io`). `github.com` is a broad host: the proxy
cannot tell `rustsec/advisory-db` from any other repository without terminating
TLS. A bug in the proxy or in podman's network isolation would break the
boundary. DNS lookups are done by the proxy for allowed names only.

### How the verdict gates the build

- The reviewer never installs anything. It has no write access to the repo,
  `CARGO_HOME` is on its own scratch tmpfs, and its only output is the verdict.
  The harness copies `/scratch/verdict.json` out after the container exits
  (`podman cp`), validates it, and stores it with the run record.
- Consumer: the harness's environment image build step. Before building any
  environment image that contains cargo dependencies, it compares the
  lockfile's `(name, version, checksum)` set with the repo's approved ledger.
  Every entry must be in the ledger or have an `approve` verdict whose
  `lockfile_sha256` matches the lockfile being built. A missing entry fails
  the build with a distinct error (for example `UnapprovedDependency`), in
  the same fail-closed posture as D1.
- On `deny`: the dependency is not installed and the build fails. The verdict
  and reasons are surfaced to whoever proposed the change (a PR comment posted
  by the harness, not by the reviewer, which has no GitHub tools). Reversing a
  `deny` takes a human edit to the repo's policy, not a re-run.
- On `needs-human`: the build also fails until a human or a repo policy decides.
  A human decision is an entry in the approved ledger committed through a normal
  reviewed change. A repo policy is, at most, a declarative rule such as "treat
  a `needs-human` that carries only the reason `unsafe_present` as approved",
  stored in protected `.nanna/**` (a human-owned file, per D6). The reviewer
  subagent never edits the ledger or the policy.
- The approved ledger (a lockfile-shaped file under `.nanna/`, protected like
  the environment declarations) is what turns "build against main" into "main
  contains only reviewed dependencies": merging a dependency change to main
  is, by construction, a human act that also records the approval.

### Threat model and known non-guarantees

The adversary is a dependency (or a dependency's transitive dependency) whose
published bytes are malicious, or a lockfile and manifest crafted to mislead
the tools reading them.

What the design relies on:

- Nothing is executed. No capability compiles, runs `build.rs`, expands a
  proc-macro, runs a test, or loads a dependency's code. The riskiest steps in
  Rust supply-chain attacks (`build.rs`, proc-macros, test code) are exactly
  the ones with no capability. The reviewer only reads bytes, so a malicious
  crate gets no code execution in any ordinary sense; the attack surface
  reduces to the parsers and tools that read it (the tar and gzip decoders,
  the TOML and Rust parsers, `cargo-deny`, `cargo-audit`).
- Even if one of those tools is exploited, the container has no secrets, no
  writable mounts besides tmpfs, no capabilities, bounded resources, and a
  network limited to the proxy allowlist, so the consequence is bounded to
  wasting the container's time or corrupting its own verdict.

Non-guarantees and residual risks:

- Container escape. A kernel or runtime vulnerability defeats rootless podman
  with default seccomp; the design reduces the likelihood but does not remove
  it. Running reviews on a dedicated host or VM is the stronger option and is
  not required here.
- A crafted verdict. A compromised tool can write any verdict. The harness
  limits the damage by computing deterministic outcomes (license, advisory,
  checksum, source kind) itself where possible and by validating shape, but
  cannot prove a reviewer's `approve` is honest. The human-merged ledger
  is the real backstop.
- Prompt injection. The model reads attacker-controlled text. Only the model's
  escalation power is open to it (see the card); it cannot override a
  deterministic `deny`.
- Malicious data in the lockfile or manifest: path or git sources, alternative
  registries, `[patch]`, `[source]` replacement, or checksums that point at a
  different crate than the one named. `dep_inventory` rejects non-registry
  sources by default, and the staged tree omits `.cargo/config.toml`, but a
  parser bug in cargo itself reading a hostile `Cargo.toml` remains possible.
- Advisory database poisoning. The database is fetched from a public repo over
  the allowlist. A compromised or stale database produces false negatives. The
  verdict records the database commit; the tool refuses a database older than a
  configured age (default 7 days); a pinned and signed snapshot is a stronger
  variant.
- Review is not proof of safety. A static scan of `build.rs` is heuristic, and
  a malicious runtime behaviour in a library the repo later executes is not
  caught by looking at its source for build-time patterns. License and advisory
  checks only see what is declared and known.
- Review/install TOCTOU. Between review and install the registry could serve
  different bytes for the same version. Mitigation: the lockfile pins every
  package by checksum; the reviewer fetches by that checksum and verifies the
  digest; the verdict records the checksum; the image build recomputes the
  checksum of every installed crate and installs only crates whose checksum
  equals an approved entry. Installed bytes therefore equal reviewed bytes, or
  the build fails.
- Allowlist limits (above): host-level only, a possible exfiltration channel
  for anything already running in the container, and `github.com` is broad.
- Image contents. For the Containerfile backend nothing proves the image holds
  only what the declaration says (the existing non-guarantee above). The
  `rustc-info` wrapper and the absence of `run_command` are what keep a
  compiler out of reach, not a check by the harness.

### Test plan for the example

Pure tests (no podman) run everywhere; the container tests use the existing
`#[ignore]` runtime-gated pattern. Each case has a fixture crate in
`harness/tests/fixtures/dep-review/`.

- Malicious `build.rs`: a crate whose `build.rs` writes a marker file to
  `/scratch`, touches the network and prints `cargo:rustc-env` lines. Expected:
  `dep_build_script_scan` reports process, network and filesystem use; the
  verdict is `deny` or `needs-human`; the marker file never exists, and the
  run's process list shows no `rustc` or `build-script-build` execution. This
  is the test that pins "executes nothing".
- Proc-macro crate that would run code at expansion: the same assertion.
- License violation: a crate under a licence the fixture `deny.toml` forbids.
  Expected: `deny` with the licence in `reasons`, regardless of what the model
  says.
- Advisory hit: a crate and version with a RustSec advisory (a fixture
  database snapshot for the pure test). Expected: `deny`; a yanked version
  likewise; an unmaintained notice yields `needs-human`.
- Typosquat-like name: `serde_jsn` against an approved `serde_json` and a
  homoglyph variant. Expected: `needs-human` with the closest legitimate name.
- Oversized crate: a generated archive over the per-crate cap (`needs-human`,
  reason `too_large`) and a decompression bomb that bypasses the cap and hits
  the memory limit. Expected: the container is killed by the cgroup, no verdict
  or a truncated one, recorded as not approved, host unaffected.
- Network outside the allowlist: from inside the container, a direct
  `connect()` to a non-allowlisted address and a proxied `CONNECT` to
  `example.com:443`. Expected: both fail, the proxy log has the denied entry,
  and allowed hosts still work. A `dep_fetch` of a crate whose index entry
  points at another host fails the same way.
- Hardening: container inspection shows read-only root, empty capability set,
  `no-new-privileges`, non-root uid, default seccomp, only the expected mounts
  (read-only `/input`, tmpfs `/scratch`), empty environment (no `GH_TOKEN`),
  and the configured cpu, memory and pids limits; writes outside `/scratch`
  fail.
- Wall clock: a stub tool that sleeps past `wall_clock_secs` is killed and the
  run is not approved.
- Schema and registry: the card and declaration resolve (D8) with equal sets;
  removing any one of the ten ids from either side fails with
  `CapabilityNotProvided` or `CapabilityNotPermitted`; the card's registry
  contains no cargo build, run or test tool.
- Verdict gate: a lockfile entry without an approved checksum fails the image
  build; a changed checksum for an approved version fails the build
  (TOCTOU); a verdict for a different `lockfile_sha256` is not accepted;
  `needs-human` blocks until a ledger entry exists.

## Test plan

Test names are proposals. Contract tests are written first in each step.

Pure, no Nix or runtime (run everywhere, including the Windows CI leg):

- `identity::schema`: declared `[environment]` parses and round-trips through
  `to_toml_string`; absent table is a load error; invalid names (empty,
  uppercase, `/`, `..`, 64+ chars, `#`, whitespace, unknown key under
  `[environment]`) are `InvalidField { field: "environment.name" }`.
- `identity::narrowing` (declared, inherited, widening): the D9 table row by
  row; proptests extending `narrowed_from`, `narrowing_is_reflexive`,
  `narrowing_is_transitive` and `widening_one_field_is_rejected_naming_it`
  with an `environment` case; a doctest on `narrows`.
- `environment` (valid, invalid): declaration parse accepts schema 1 and
  rejects unknown keys, wrong schema, duplicate ids, empty `exec`, both or
  neither of `containerfile`/`flake_output`, path escapes in `containerfile`,
  and oversized input; `resolve` covers equal sets (ok), card needs more
  (`CapabilityNotProvided`), image provides more (`CapabilityNotPermitted`),
  glob with matches, dead glob, empty declaration with a host-side-only card
  (ok), host-side tools never checked; a proptest that for any generated card
  and declaration `resolve` succeeds iff the container-class sets are equal.
- Registry: the identity container registry for `rust-impl` contains
  `cargo_check` and not `cargo_audit`; for `review` contains no
  container-class tool; a capability with a custom `exec` registers and
  executes that argv; pinning that a card with cargo tools now receives them
  (closing the gap above).
- Stack-agnostic: a fixture with a non-Rust capability (`run_robot_test`)
  resolves and registers with no Rust-specific code path.

Harness-level with stubs (no Nix, no runtime), reusing
`get_or_build_image_using`'s injected build function:

- cache key: two environments of one repo build separately and reuse
  separately; one lock per `(path, env)`.
- missing: a card without `[environment]`, an environment not declared in the
  repo, an unbuildable environment, and a mismatch each fail the task with a
  distinct `error_type` before any workspace is created; no path falls back
  to another image.
- network: environment resolution leaves `network_policy_for` unchanged for
  every ceiling.
- vendoring: `docs` (no cargo) skips `offline_cargo`; `rust-impl` at
  `workspace` ceiling still vendors.

Runtime-gated (the existing `#[ignore]` pattern in
`harness/tests/dev_container_integration.rs`; not required for merge, run on a
host with podman, and Nix for the flake backend): build two environments from
Containerfiles, assert `cargo` resolves in `rust-impl` and not in `docs`, and
that `--network=none` is set for the `docs` container.

## Migration plan

Each step is one PR, red tests first. "Hot" marks files the user named as
owned by other sessions. Nothing in this plan touches `container.rs` or
`protected.rs`: `ContainerConfig` already takes the image as an argument, and
every file that carries environment declarations or cards (`.nanna/**`,
`flake.nix`, `nix/**`) is already in the shipped protected set. The plan does
not add or change any workflow or CI file.

1. Schema and narrowing. Make `[environment]` required.
   `IdentityCatalog::with_repo_overrides` replaces the base entry with the
   override wholesale, so an override that omits `[environment]` would
   otherwise have none; the catalog must carry the base environment into the
   override, with the test "an override omitting `[environment]` runs in the
   base's environment". Files: `harness/src/identity/schema.rs`,
   `harness/src/identity/narrowing.rs`, `harness/src/identity/catalog.rs`,
   `harness/src/identity/mod.rs` (docs and re-export),
   `harness/tests/fixtures/identities/global/*.toml` (add `[environment]` to
   every fixture). Hot: none. `harness/src/identity/**` is in nanna's own
   protected declaration, so this PR needs a human author or explicit sign-off
   for the protected path. Independent of every open PR.
2. Declaration and checker. Files: `harness/src/environment.rs` (new),
   `harness/src/lib.rs` (one `pub mod` line). Hot: none. Pure; can run in
   parallel with step 1 after the `AgentIdentity` field exists, or against a
   stub field.
3. Builders and onboarding. Files: `image-builder/src/lib.rs` (Containerfile
   builder, named Nix output builder), `harness/src/onboarding/` (generate
   `.nanna/environments/<name>/` for the five cards, with tests; for the Nix
   backend, `flake_template.rs` emits the output and the declaration from one
   function). Hot: none. Overlaps `flake_template.rs` with the #1027 follow-up
   that asks the template to produce the offline setup; sequence after or with
   it.
4. Runtime wiring and registry. Files: `harness/src/task.rs` (cache key,
   environment resolution before the container block, remove the `flake.nix`
   plus `.devcontainer/` opt-in decision, new `error_type` labels),
   `harness/src/workspace.rs` (`build_tool_registry_for` takes the resolved
   environment), `harness/src/tools.rs` (identity container registry registers
   capabilities from the resolved set and runs their declared `exec`). Hot:
   `task.rs` and `workspace.rs`. Conflicts with #1031 (`task.rs` hunks near the
   container block and `create_with_container_vendoring` in `workspace.rs`),
   #1032 (`task.rs` verifier branch in `TaskRunner::run`, `verify/`) and #1022
   (policy solo mode in `task.rs`). Recommended order: #1031, #1032, #1022
   merge first, then this step rebases onto them. If the owners prefer the
   reverse, the minimal seam is a single `resolve_environment(identity, repo)
   -> Result<ResolvedEnvironment, _>` call added immediately before the
   `use_container` block.
5. Vendoring gate. Files: `harness/src/workspace.rs` (skip `offline_cargo` when
   the environment provides no cargo capability),
   `harness/src/offline_cargo.rs` if #1031 has merged. Hot: `workspace.rs`.
   Strictly after #1031.
6. Verifier environment. Files: `harness/src/verify/*` and the verifier
   fixture from #1032. Hot: whichever `task.rs` hunks #1032 owns. Strictly
   after #1032 merges, owned by its author.
7. Docs. Files: `ARCHITECTURE.md` section, `harness/src/identity/mod.rs`
   module docs; the configuration reference promoted out of this document.
   Hot: none.

8. Cargo dependency reviewer example (see the worked example above). Lands
   after the environment schema steps: it needs step 1 (card), step 2
   (declaration and checker, extended with the `[limits]`, `[sandbox]`,
   `[network]` and `[[input]]` keys), step 3 (Containerfile builder, onboarding
   for Rust repos) and step 4 (registry runs the declared `exec`). Files: a
   shipped example card and `cargo-dep-review` environment under
   `harness/src/onboarding/`, the `nanna-dep` tool crate, the verdict schema and
   validator, the egress proxy sidecar and network setup, and the
   approved-ledger check in the image build step in `image-builder/src/lib.rs`.
   Hot: whichever `task.rs` and `workspace.rs` hunks step 4 touches, and the
   vendoring question for the owners of 1027 and 1031. The vendoring change
   (retire or keep task-time vendoring) is not part of this step until they
   answer.

Steps 1, 2 and 3 can proceed in parallel with the open hot-file PRs. Only
steps 4, 5 and 6 must wait for them. Step 8 waits for steps 1 to 4.

## Known non-guarantees

- `[limits]`, `scope.repos` and the card's model and system prompt are
  validated at load time but not enforced at run time on main (#733). An
  `[environment]` table narrows what a task can run, but this design does not
  make those other fields binding, and it does not depend on them being so.
- A named environment limits which container tools exist. It does not make a
  card's `scope.paths` or `read_paths` binding for `run_command`; cards that
  restrict paths still do not receive `run_command`.
- The harness checks the declaration against the card, not the image against
  the declaration. For the Containerfile backend nothing proves the image
  contains only what the declaration says; the repo owner is trusted for that,
  and a card that allows `run_command` can reach any binary the image holds.
  The Nix backend narrows the gap by generating image and declaration from one
  list.

## Needs confirmation

1. Definition format (D7). Default to a Containerfile built locally with
   podman, with the existing nix2container flake output available behind the
   same `environment.toml`. Is Containerfile-first, Nix-optional what you meant
   by "dockerfile-like"?
2. Mismatch reading (D2). My reading of your answer on "wider" images: the
   card declares its environment and its capability set together, and any
   mismatch in either direction is a load-time hard error. Is that right, and
   in particular should an image that provides more than the card allows be an
   error rather than tolerated?
3. Consequence of D1 and D9. Every task bound to an identity now requires a
   declared, locally buildable environment, including repos that today run
   without a container, and a repo-local card cannot narrow a container-class
   capability set without becoming a separately named subagent. Acceptable?
4. Verifier (D3). `run_checks` and `.nanna/verify.toml` in #1028 must be
   reconciled so that checks are capabilities declared by the verifier's
   environment. Should #1028's owner be asked to adopt that, or should
   `verify.toml` remain a selector over the environment's capabilities?
5. The `reviewer`, `docs-writer` and `verifier` card values above are
   proposals; confirm ceilings and tool lists.
6. Schema keys for the dependency reviewer example. The `[limits]` (`cpus`,
   `memory_mb`, `pids`, `wall_clock_secs`, `tmpfs_mb`), `[sandbox]`
   (`read_only_root`, `cap_drop`, `no_new_privileges`, `user`, `env`),
   `[network]` (`mode`, `allow`) and `[[input]]` tables in `environment.toml`
   are new, and the default values (1 cpu, 1024 MB, 128 pids, 600 s, 256 MB
   scratch, 50/150 MB crate caps) are proposals. Is a declared network
   allowlist acceptable at all? D8 says network is derived from the card's
   ceiling and never an environment property, and the card here has ceiling
   `workspace`, which means `--network=none`. The example needs a narrow
   exception (an environment may only move from none to a named allowlist
   through the proxy, never to open network) or a new ceiling level. Which? And
   should the sandbox keys be a baseline every environment gets, with
   environments only able to tighten them?
7. Lifecycle change for #1027 and #1031. If approved dependencies are installed
   in the image at build time, task-time vendoring is unnecessary for them. Do
   the owners of #1027 and #1031 retire it, or keep it for dependencies without
   a verdict (which would be a second install path)? This document does not
   decide.
8. Who consumes the verdict. The design has the image build step consume it
   against a human-merged approved ledger under protected `.nanna/`, and a
   human (or a declarative repo policy) decide `needs-human`. Confirm the
   consumer, the ledger location, and whether any policy auto-approval is
   allowed at all.
9. How a branch that adds a dependency gets it. Either the implementer waits
   for the review and a derived image layer built from the approved delta, or
   the dependency change merges first and the next image build picks it up.
   Also who produces the proposed `Cargo.lock` delta offline.
10. Deterministic-first reviewer. The verdict is computed from deterministic
    checks and the model may only escalate. Confirm that the model must not be
    able to turn a `deny` into `approve`, and whether a model should be in the
    loop at all for the build-script review.
