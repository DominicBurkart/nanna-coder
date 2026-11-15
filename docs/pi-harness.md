# Pi as nanna's internal harness

**Status: planned.** The interface layer (`harness_adapter`) exists, and so do
the host-side `bind` and `broker` modules of slice 1, which nothing calls. No
socket server, executor or runner switch is written, nothing launches pi yet,
and `assign_task` still runs the in-house loop. This document describes the
target and what is and is not built. Tracking: #924 (merge criteria), #922
(eval runner and prompt paths), #923 (`tasks/result` wire shape); #921 (the
action-gate halt and widening the broker policy) is closed.

The target: nanna runs every delegated agent on pi, inside a container. The
agent loop and in-house reasoning code go away; nanna keeps what it is good at:
choosing an agent definition, granting capabilities, enforcing scope, and
integrating the result.

## Use cases

1. **A calling agent (Claude) delegates to nanna.** Target flow: `assign_task`
   selects an identity, the spawn gate allows it, nanna resolves it into a
   launch plan, runs pi in a container, integrates the resulting patch and
   returns it through the unchanged MCP `tasks/result` shape.
2. **one_track and nanna together, no agent calling nanna.** The same path,
   with the scheduler/bridge as the caller. Nothing in the seam depends on who
   submitted the task.

## The seam

```
Allowed (spawn gate) ──resolve──▶ ResolvedAgent ──HarnessAdapter──▶ draft LaunchPlan
 (identity bound to the audit)     (scoped capabilities)             (files, argv, env,
                                                                      broker socket, gateway)
                                                                           │
                                                          launch_plan: IsolationPolicy::check
                                                                           │
                                                                      CheckedPlan ──▶ Executor
```

- `ResolvedAgent` is built only from an `Allowed` proof (the identity the
  auditor saw, never a caller-supplied one), has private fields, and holds
  exactly the capabilities of the registry scoped to that identity. The broker
  socket path, model name and gateway endpoint are validated at resolve time.
- `HarnessAdapter` is pure (no I/O). Pi is the first adapter; later harnesses
  implement the same trait. An adapter that cannot honour a definition returns
  `Unsupported` rather than dropping a constraint.
- `launch_plan` is the only way to obtain a `CheckedPlan`, whose field is
  private to the module that defines `launch_plan`, so adapters cannot build
  one. Executors accept only `CheckedPlan`; isolation is enforced once, for
  every harness, not re-implemented per adapter. A `harness` selector type is
  added back with the `[identity] harness` key; it is not in the code until
  then.

## Isolation (hard requirement)

Agents act only through predefined capabilities. Pi has no permission system
and full access to whatever it runs on, so the guarantee is built around it:

| Layer | Mechanism |
| --- | --- |
| No ambient tools | `--no-builtin-tools --no-extensions --no-skills --no-prompt-templates --no-context-files --no-mcp --no-approve`; one static extension registers the granted capabilities and nothing else |
| No workspace in the agent container | the plan may mount only the broker socket; the repo is never visible to pi, so repo-borne `.pi/` config cannot influence it |
| Capabilities are brokered | each call goes over the socket to nanna, which runs the existing Rust `Tool` under identity scope, effect ceiling and incident holds |
| Network | the plan names exactly one endpoint, the model gateway. The host string is checked (no localhost, loopback, link-local, unspecified, multicast, `metadata.google.internal` or numeric-looking names), but private ranges (10/8, 192.168/16, 100.64/10) are accepted, names such as `foo.localhost` are accepted, and name checks cannot be complete. Real egress enforcement is the container network, not these checks. How the gateway is reached from inside the container (loopback there is the container itself) is not designed yet and returns with the executor. |
| No secrets | the environment is an exact (name, value) allowlist (`HOME`, `PI_CODING_AGENT_DIR`, `NANNA_BROKER_SOCKET`, `NANNA_CAPABILITIES`); there is no secret-name heuristic; the model `apiKey` is a placeholder |
| Fixed hardening | privileges, Linux capabilities and devices are not expressible in a plan; the executor will fix them (non-root, read-only root, all caps dropped, no-new-privileges). **Not implemented yet.** |
| Fail closed | `Unsupported` for empty capability sets or unrepresentable capability names; `ResolveError::InvalidModel`, `InvalidImage` and `InvalidBrokerSocket` for a bad model name, image reference or socket path; a plan that drops the agent's scope, limits or audited repository is rejected |
| Verified lockdown | **Planned.** After start the executor compares the runtime's reported tool set to the plan with `verify_runtime_tools` and aborts on any difference. `Broker::arm` calls it; nothing calls `arm` yet. |

`IsolationPolicy` encodes the rows that a plan can express. The property test
`plans_always_satisfy_isolation` and `narrower_identity_never_widens_the_plan`
pin the monotonicity guarantee: a narrower identity never produces a wider plan.

Even if the model is prompt-injected, it can only emit calls to granted
capabilities, and the broker is authoritative for every one.

## Effect boundary and the broker contract

Pi is only a reasoning loop. Every effect, at every class, happens in the
broker, never in the agent container. Middle and outer loop identities
(`pr-shepherd` at `ci`, `deployer` at `sandbox`, `incident-responder` at
`production`) receive their effectful tools through the broker exactly as an
inner-loop identity receives `write_file`; `ResolvedAgent` grants any class up
to the identity's `max_effect`. The tests `ceilings_grant_effectful_capabilities_up_to_the_ceiling`
and `resolve_carries_scope_limits_and_ceiling` cover the grant side.

The broker is the single door. For each capability call it **reimplements none
of these gates**; it calls `ToolRegistry::execute` on the identity's scoped
registry and maps the result. `ArmedBroker::handle` runs, in this order:

1. The halted check (a halted task serves nothing) and the served check (the
   capability is in the agent's grant and not above the `BrokerPolicy`
   ceiling). New, in the broker.
2. `ToolRegistry::execute`: look the tool up in the scoped registry (a name
   outside the scope is `ScopeDenied`, recorded); then the **production hold**
   for `Production` calls; then the **action gate** (the auditor's per-action
   review, #876) for calls of class `Repository` and above and for
   `run_command` only, and no other call is reviewed; then the tool.
3. Inside the tool: `scope.paths` / `read_paths` and protected paths.
   `read_file`, `write_file`, `list_directory` and `search` are built with
   `::scoped(root, scope)` and carry the identity's `PathScope`; `write_file`
   also carries `ProtectedPaths`; `execute` records `ScopeDenied` and
   `ProtectedPath` denials. The registry must be built with the path-scope-aware
   constructors, which `model_facing_registry` does;
   `create_tool_registry(root)` followed by `scoped_for` filters names but
   attaches no path scope. The registry and its tools enforce path scope; the
   broker adds nothing here.
4. The broker records the call (size and fingerprint of the arguments, never
   their content) in a bounded `AuditSink`, clips the output at
   `MAX_OUTPUT_BYTES`, and maps each `ToolError` onto a `ToolErrorKind` or a
   `Halt`.

Not built, and the reason the default `BrokerPolicy` serves nothing above
`Workspace`: the human-availability **window** and the coordination **lease**
for `Repository` and above. The runner's action subject carries no window,
pull request or environment today, so `Sandbox` and `Production` calls stop at
the missing lease context. There is also no registry hook to re-check the halt
flag between the action review and the tool (see the halt contract below), a
precondition for widening the policy (#921 is closed; #924 tracks the merge criteria).
The audit log is in-memory only (`RingAuditSink`); a durable sink is executor
work.

`ModelFacingLayers` (the escalation log for the production hold, the action
gate, the escalator and the task's action subject) is what `TaskRunner` builds
per task, and `model_facing_registry` is the one place that turns a workspace's
registry into the one a model may call: identity scope (or the read-only
unscoped registry), then the hold, then the gate, then the escalator. The type
is `pub(crate)` and stays so: `ResolvedAgent::bind_workspace` is `pub(crate)`
and takes `(&AgentIdentity, audited repo slug, &TaskWorkspace, layers)`, checks
the layers against the identity and workspace (task id, effect ceiling, path
scope, repository) and builds the bound registry through
`model_facing_registry`, the only constructor it uses. The layers themselves
(gate, log, escalator) are the runner's; a permissive gate cannot be detected
by `bind`, so a `TaskRunner` method that builds the layers and the bind in one
step, and that nobody else can call, is the only intended caller (a structural
test pins the callers of `bind_workspace`). The `TaskRunner` method is not in
slice 1.

Every model-facing registry that `TaskRunner` serves is built through
`model_facing_registry`. Three pre-existing paths build one without it, listed
and pinned by `tests/broker_bind_structural.rs` and tracked in #922:
`main.rs` `Commands::Agent`, the chat command and `eval/runner.rs`. They are
not made worse by the broker and are not fixed in slice 1.

Spawning goes through the existing spawn gate: a `ResolvedAgent` can only be
built from an `Allowed` proof, and a `CheckedPlan` only from a `ResolvedAgent`,
so a plan reaches an executor only through that chain (transitively; the
executor does not itself inspect the proof). The prompt and repository label
come from the audited request. Checking that the working repository on disk
names the audited repository, as `TaskManager::submit_spawn` does, is done by
`bind_workspace` (`names_repo` against the audited slug, then `check_repo`
against `scope.repos`). `bind_workspace` takes the identity and the audited
repository, not the `Allowed` proof, because the runner no longer holds the
proof when it runs (`submit_spawn` consumes it, restore re-audits and keeps the
identity); the broker fails closed on an identity the spawn gate did not bind
(`BoundIdentity::Unaudited`). The task prompt is carried by the `ResolvedAgent`
and cannot be re-checked from the identity.

### The halt contract

When the action gate escalates a call (or a production hold applies),
`ArmedBroker::handle` returns `BrokerOutcome::Halt`, sets the halted flag and
tells the runtime only that the task was halted. The executor, which does not
exist yet, must on the first `Halt`: stop writing to the runtime and kill the
container without waiting for pi to exit; fail the task with
`FailureDiagnostics.error_type` `HaltReason::error_type()` (`StateError`, the
label `AgentLoop` gives an escalated action, `agent/mod.rs`) and
`HaltReason::message()`; and surface `ArmedBroker::action_audit()` as the
result's `action_audit`. `ProductionHeld` is unreachable under the default
policy. A call admitted just before an escalation can still run its tool,
because the halted check precedes `execute`, which includes the review; that is
negligible under the default policy and a precondition for widening it.

`ToolRegistry::scoped_for(identity)` landed with #669 and also withholds
tools that cannot honour path scope (`run_command` for path-restricted
identities). `ScopedCapabilities::scope(base_registry, &identity)` calls it and
binds the result to the identity's name **and whole scope**; there is no public
constructor from an arbitrary registry. `ResolvedAgent::resolve` grants exactly
that set and refuses it unless name and scope both equal the identity being
resolved.

A name match alone is not sufficient: repo-local identities deliberately share
their global identity's name while narrowing it, so a grant built from the
global identity would otherwise pass for the narrower local one and widen it.
As a second, fail-closed layer, `resolve` also asserts that every granted
capability satisfies the identity's tool patterns and effect ceiling
(`GrantWiderThanIdentity`); it is an assertion, not a second filter. Regression
tests cover the same-name exploit, different `paths`/`read_paths`, and a
deliberately widened grant.

## Runtime handshake

A runtime that silently ignores a lockdown flag fails open, and
`IsolationPolicy` cannot see argv semantics. After start, the executor must
query the runtime's registered tools (pi RPC) and abort the run unless
`verify_runtime_tools(plan, reported)` succeeds. The comparison function exists and is tested; `Broker::arm` calls it, and nothing calls `arm` yet. **Lockdown is not verified until the executor calls it against real pi** (`NANNA_PI_BIN` integration test, a blocker for merging the executor).

## Limits and scope are carried, not yet enforced

`LaunchPlan.limits` and `LaunchPlan.scope` must equal the agent's
(`IsolationPolicy` rejects a plan that differs). This catches an adapter that forgets them; `PiAdapter` copies them, so for pi the check is a guard for future adapters. **Nothing enforces scope, turn count or deadline yet.** The executor entry point must take an enforcement value (broker handle plus deadline) as a required argument so a run cannot start without it. Pi has no iteration cap, so
the executor counts turns against `max_iterations` and enforces
`max_wall_clock_secs`; the registry and its tools enforce `scope`. `max_concurrent` is the
scheduler's.

## Hardening status

Implemented: broker-socket, gateway, capability, env (exact name and value),
file-path, image, scope, limits and audited-repository checks in
`IsolationPolicy`; image, model-name and broker-socket-path validation in
`resolve`; `Allowed`-bound prompt and repository; fail-closed `Unsupported`;
handshake comparison (called by `Broker::arm`, which nothing calls yet).

Not implemented yet, all executor work: non-root user, read-only root
filesystem, dropped Linux capabilities, `no-new-privileges`, and network
enforcement (the plan states the allowed endpoint; nothing enforces it until
the executor runs the container on an internal network). `container::NetworkPolicy::for_ceiling` (landed with #669) governs the dev
container where broker-run tools execute and is unchanged. The agent container
is separate: its plan is always exactly the model gateway at every ceiling
(tested), so there is one rule per container and nothing ceiling-derived to
drift. The plan carries a single `gateway` endpoint, so there is no
"no network" case to model.

## Status of this change

Landed: `harness_adapter` (types, `IsolationPolicy`, `PiAdapter`, handshake
comparison), and the corrective hardening of it: private-field `ResolvedAgent`
built from an `Allowed` spawn proof, with the prompt (the audited subtask) and
repository label taken from the audit; `CheckedPlan`; a stricter `Endpoint`;
image, model-name and broker-socket validation; an exact env allowlist; one
broker socket and one gateway; and removal of unused API (`HarnessKind`,
`PlanNetwork`, `Mount`, the operator-configured endpoint constructor and
`withhold_above`, which return with the executor).

Merge rule: the next merge to main makes pi runnable end to end for at least one
inner-loop identity, behind these isolation checks, with the real-pi test
passing. The broker, executor and `TaskRunner` switch may be developed as
stacked draft PRs, but they merge as a unit that works; no further unwired
slice lands on its own.

Slice 1 (this change, unwired): `harness_adapter::bind` and
`harness_adapter::broker` as described above, with `LaunchPlan.agent`,
`LaunchPlan.model` and `ScopeGrant.tools` so a broker can be matched to its
plan. `isolation.rs` and `checked.rs` are protected paths and are untouched; the
broker checks agent and model itself in `Broker::arm`, so `IsolationPolicy` does
not duplicate it (an `IsolationViolation::AgentChanged`/`ModelChanged` would be a
defence in depth, a separate change that needs the protected-path reviewer).

Not yet done (in order):

1. The socket server that reads broker lines from the container and calls
   `ArmedBroker::handle_line`, and the `TaskRunner` method that builds the
   layers and the bound registry in one step (requires
   `BoundIdentity::Audited`; keep the `BoundIdentity` through task start, which
   `run` flattens today).
2. Executor trait plus a container executor (hardening above) with a fake for
   tests; RPC event parsing (`agent_settled`, tool events) into the existing
   `TaskResult`/`FailureDiagnostics`, keeping `tasks/result` JSON unchanged for
   the one_track bridge fixtures (golden-JSON test); lease release on every exit
   path. Blocked on the real-pi integration test below.
3. Packaging: pi 1.2.0 as a pinned flake input (`github:earendil-works/pi`) with
   a lock hash and `npm install --ignore-scripts`, built into the agent image
   only; CI gets pi from that same pinned input. Upstream changed organisations
   and ships breaking renames in patch releases, so upgrades are deliberate and
   tested. The dev-container flake gains no pi dependency.
4. `harness` key in `[identity]`, default `pi`, invariant under repo-local
   override (equality required in `narrows`). Needs the identity owner's
   agreement.
5. Switch `TaskRunner` to the adapter path, then delete the loop.

## Broker socket path is a placeholder

`resolve` accepts a broker socket only under `/run/nanna/` or
`/run/user/<digits>/nanna/` with a restricted character set and normalised
components. Those prefixes are placeholders: nothing on main uses them (nanna
state derives from `NANNA_CONFIG_DIR` and `XDG_CONFIG_HOME/nanna`), `/run/nanna`
needs root and conflicts with rootless podman, and `/run/user/<digits>/` is not
checked against the current uid. The executor will allocate the socket (or
derive it from `XDG_RUNTIME_DIR`); the validator is kept as a shape check, not
as proof of ownership.

## Carried to the broker and executor slice

Known gaps, tracked here and not yet fixed:

- The executor must confirm the broker socket is one it created and owns, not
  just a validated path.
- `resolve(&Allowed)` does not compose with the runner as it is today:
  `submit_spawn` consumes `Allowed` with `into_parts()` and the queued task
  keeps only `Some(identity)`, so a runner executing later has no `Allowed`. The
  executor slice must resolve at dispatch time and carry the `ResolvedAgent` (or
  an equivalent audited launch spec) through the queue: one audit is one launch
  (a by-value consume), and a caller-supplied identity is never accepted.
- `resolve` does not assert that the audited repository is within
  `scope.repos`, and the audited repository may be a path; `bind_workspace`
  runs `names_repo` and `check_repo` against the workspace's source repository.
- The executor must detect all three pi failure signals (prompt `success:
  false`, a final `stopReason: "error"`, the deadline) and map each to a
  `FailureDiagnostics.error_type`, with the golden-JSON test; see "Real-pi
  findings".
- Carry the parent task id for audit-log correlation. The worktree path and
  branch are executor inputs obtained only through `bind_workspace`.
- Windows and leases, and a durable audit sink, remain broker work. The
  per-action gate is on main (#876) and is installed through
  `model_facing_registry`. Path scope and protected paths are already enforced
  inside the scoped file tools, and `ToolRegistry::execute` already does lookup,
  the production hold and denial recording; the broker does not reimplement
  them.

- `Broker::arm` matches a `CheckedPlan` against the broker's agent, audited
  repository, model, image, limits, scope, capability set and broker socket
  (`LaunchPlan.agent` and `.model` carry the first and third), so a plan for
  another agent cannot arm it.
- `ScopedCapabilities::scope` accepts any registry and does not check path
  scoping; `bind_workspace` builds the registry itself through
  `model_facing_registry` and asserts it offers exactly the agent's
  capabilities and effect classes.
- Model-name charset and length cap beyond the current option/whitespace/
  control-character checks.
- The extension's reply cap counts UTF-16 units, not bytes; a `null` reply needs
  a clean error; U+2028/U+2029 handling against pi's framing is undecided (the
  executor must use strict LF framing).
- The TypeScript extension has no tests of its own, so none of its guards
  (timeout, reply cap, the `!ok` throw) is pinned. It must be exercised by the
  `NANNA_PI_BIN` real-pi test.
- argv, stdin and file contents are pinned per adapter by that adapter's tests,
  not by `IsolationPolicy`; a custom adapter can pass the policy with an
  arbitrary command.

## One harness, and deleting the old one

Pi is the only harness once the executor lands. The in-house `AgentLoop` path
is not kept as a second option. Order of removal, each its own commit:

1. `TaskRunner` uses the adapter (step 5 above).
2. `eval/runner.rs` and the swebench eval, which depend on `AgentLoop`, move to
   the executor or are rewritten against the MCP surface.
3. `AgentLoop`, `agent/rag.rs`-style in-loop helpers and the unused in-loop
   tool plumbing are deleted. `ToolRegistry` and the `Tool` impls stay: the
   broker runs them.

Tracking issues: #924 (merge criteria and cross-reviewer gaps), #922 (eval
runner and prompt paths), #923 (`tasks/result` wire shape); #921 is closed.
`ARCHITECTURE.md` links here.

## Real-pi findings (pi 1.2.0)

First reported by the reviewer's verification pass (pi 1.2.0 installed with
`--ignore-scripts` in a throwaway directory and run on the host; that evidence
is not in this repository), then reproduced and extended inside a container:
`node:24-slim` (digest in `docs/pi-spike/evidence/01-image-digest.txt`), pi installed
with `npm install --ignore-scripts --save-exact @earendil-works/pi-coding-agent@1.2.0`,
`--network none`, driven over RPC with the adapter's exact flags and files
against a mock gateway and broker. The scripts, raw evidence and checksums are in
`docs/pi-spike/`. 1.2.0 is the npm `latest` dist-tag, so the pin stays.

Confirmed:

- Every flag in `PiAdapter`'s argv exists with that spelling, and an unknown
  flag is a hard error, so a misspelt lockdown flag fails closed at start.
- With those flags the model request carried only the granted tool. The
  extension API (`registerTool`), raw JSON Schema `parameters`, `models.json`,
  `SYSTEM.md`, `--provider`/`--model`, `--offline` against a local gateway and
  an empty `HOME` all work.
- Planted project `.pi/*`, `AGENTS.md` and `mcp.json` were inert.
- `--no-approve` is load-bearing: with `--approve`, a project `.pi/SYSTEM.md`
  replaced the nanna prompt. `--no-extensions` also stops pi auto-loading
  `$PI_CODING_AGENT_DIR/extensions/*.ts`.

Further findings from the container run:

- pi does **not** reject an unknown model name: it warns (`Using custom model
  id`), returns `success: true` and exits 0. Model names must be validated by
  nanna (`resolve` does).
- A failing gateway (HTTP 500) still gives `success: true` and exit 0. The
  failure appears only as an assistant message with `stopReason: "error"` in
  `agent_end`, after pi has retried three times.
- With a read-only agent directory the prompt returns `success: false`
  (`EROFS`), and pi then does **not** exit.

Constraints these findings put on the executor (none implemented):

- There are three failure signals, and the executor must handle each: (a) a
  prompt response with `success: false` means abort, kill the container and do
  not wait for exit; (b) a final assistant message in `agent_end` with
  `stopReason: "error"` is a task failure, because `success: true` and exit 0
  hide a dead gateway; (c) the wall-clock deadline. Each must map to an existing
  `FailureDiagnostics.error_type`, and the mapping is covered by the golden-JSON
  test that keeps `tasks/result` unchanged.

- There is no `get_tools` RPC command. The registered tool list appears only in
  the first system `message_start` event (`toolsAdded`), emitted after a prompt
  is sent. `verify_runtime_tools` must read that event, and **the broker must
  refuse every call until the executor has checked it**, otherwise the model
  already holds the tools before the handshake runs.
- pi can exit 0 when the prompt failed (`success: false` in the prompt
  response). The executor must check `success`, not the exit code.
- pi writes `auth.json` and `models-store.json` into `PI_CODING_AGENT_DIR`, so
  that directory must be writable; plan files cannot go on a read-only mount.
- The RPC `bash` command runs shell on the host side of the container. Only the
  executor may ever write to RPC stdin, and the framing is strict LF-delimited
  JSON (do not use a reader that splits on U+2028/U+2029).

The extension asset (`pi_extension.rs`, installed in the container as
`nanna-capabilities.ts`) sets UTF-8 decoding, rejects on error, close and
timeout, caps the reply size, and throws when the broker reports `ok: false`.
It has no tests of its own, so its timeout, reply cap, `!ok` throw, socket
default, newline handling and `close`/`error` handlers are all unpinned. The
real-pi test must cover them. (A reviewer-run TypeScript mutation pass is not
committed in this repository, so no mutant count is claimed here.)

## Still unverified

These stay unverified, and labelled as such, until a test proves them:

- Running as a non-root user, with a read-only root filesystem and with all
  Linux capabilities dropped (the spike ran as the container's default,
  rootless-mapped root).
- The extension's timeout, oversize-reply and close handlers against real pi.
- U+2028/U+2029 in RPC framing (the executor must use strict LF framing).
- A real model, and how the container reaches the real model gateway.
- RPC event field schemas beyond the fields used above.

The real-pi test (`NANNA_PI_BIN`) must assert against real pi that no tool other
than the granted ones is reachable. It must run in CI and must **fail**, not
skip, when pi is absent. The CI job installs pi by exact version (1.2.0), in a
container, with `--ignore-scripts`.
