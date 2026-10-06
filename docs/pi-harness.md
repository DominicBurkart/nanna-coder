# Pi as nanna's internal harness

Nanna runs every delegated agent on pi, inside a container. The agent loop and
in-house reasoning code go away; nanna keeps what it is good at: choosing an
agent definition, granting capabilities, enforcing scope, and integrating the
result.

## Use cases

1. **A calling agent (Claude) delegates to nanna.** `assign_task` selects an
   identity, nanna resolves it into a launch plan, runs pi in a container,
   integrates the resulting patch and returns it through the unchanged MCP
   `tasks/result` shape.
2. **one_track and nanna together, no agent calling nanna.** The same path,
   with the scheduler/bridge as the caller. Nothing in the seam depends on who
   submitted the task.

## The seam

```
AgentIdentity ──resolve──▶ ResolvedAgent ──HarnessAdapter──▶ LaunchPlan
 (TOML, scope)             (narrowed capabilities)           (files, argv, env,
                                                              mounts, network)
                                                                    │
                                                         IsolationPolicy::check
                                                                    │
                                                                 Executor
```

- `ResolvedAgent` holds only the capabilities the identity's tool patterns
  allow and whose effect class is within `max_effect`.
- `HarnessAdapter` is pure (no I/O). Pi is the first adapter; later harnesses
  implement the same trait. An adapter that cannot honour a definition returns
  `Unsupported` rather than dropping a constraint.
- `launch_plan` is the only way to obtain a plan: it runs `IsolationPolicy`
  on whatever the adapter produced, so isolation is enforced once, for every
  harness, not re-implemented per adapter.

## Isolation (hard requirement)

Agents act only through predefined capabilities. Pi has no permission system
and full access to whatever it runs on, so the guarantee is built around it:

| Layer | Mechanism |
| --- | --- |
| No ambient tools | `--no-builtin-tools --no-extensions --no-skills --no-prompt-templates --no-context-files --no-mcp --no-approve`; one static extension registers the granted capabilities and nothing else |
| No workspace in the agent container | the plan may mount only the broker socket; the repo is never visible to pi, so repo-borne `.pi/` config cannot influence it |
| Capabilities are brokered | each call goes over the socket to nanna, which runs the existing Rust `Tool` under identity scope, effect ceiling and incident holds |
| Network | only the model gateway endpoint, or none |
| No secrets | environment names are an exact-name allowlist (`HOME`, `LANG`, `PI_CODING_AGENT_DIR`, `NANNA_BROKER_SOCKET`, `NANNA_CAPABILITIES`) plus a secret-marker check; the model `apiKey` is a placeholder |
| Fixed hardening | privileges, Linux capabilities and devices are not expressible in a plan; the executor will fix them (non-root, read-only root, all caps dropped, no-new-privileges). **Not implemented yet.** |
| Fail closed | `Unsupported` for empty capability sets, empty model, or unrepresentable capability names; a plan that drops the agent's scope or limits is rejected |
| Verified lockdown | after start the executor compares the runtime's tool set to the plan (`verify_runtime_tools`) and aborts on any difference |

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
to the identity's `max_effect`, and `pi_plan_carries_scope_and_limits_for_enforcement`
and `middle_and_outer_loop_ceilings_grant_effectful_capabilities` cover this.

The broker is the single door. For each capability call it must, in order:

1. look the tool up in the registry scoped to the identity (the same registry
   that produced the grant, see below); unknown or out-of-grant names are
   refused;
2. enforce `scope.paths` / `read_paths` and protected paths on file arguments;
3. pass the **action gate** (the auditor's per-action review) before
   execution; there is no unaudited path, and a refusal is returned to the
   agent as a tool error and surfaced to the human like any other gap;
4. for `Repository` and above: check the human-availability **window**
   and take the coordination **lease**, holding it for the call;
5. check the **production hold** for `Production`;
6. execute, and record the call for the task result and audit log.

Spawning goes through the existing spawn gate: a launch plan is only built
from an identity the auditor has allowed, and the executor refuses a plan that
was not built from one. The broker and executor are not written yet; this list
is their contract and is reviewed before code.

`ToolRegistry::scoped_for(identity)` landed with #669 and also withholds
tools that cannot honour path scope (`run_command` for path-restricted
identities). `ScopedCapabilities` can only be built from a registry that was
scoped with it (an unscoped registry is refused), and
`ResolvedAgent::resolve` grants exactly that set and refuses capabilities
scoped to a different identity. There is no second filter, so the grant cannot
drift from what the broker's registry enforces. Residual: the registry's scope
is identified by identity name, so the production caller must build the
registry and the `ResolvedAgent` from the same loaded identity (to be wired when
`TaskRunner` is switched).

## Runtime handshake

A runtime that silently ignores a lockdown flag fails open, and
`IsolationPolicy` cannot see argv semantics. After start, the executor must
query the runtime's registered tools (pi RPC) and abort the run unless
`verify_runtime_tools(plan, reported)` succeeds. The comparison function exists and is tested; it has no caller yet. **Lockdown is not verified until the executor calls it against real pi** (`NANNA_PI_BIN` integration test, a blocker for merging the executor).

## Limits and scope are carried, not yet enforced

`LaunchPlan.limits` and `LaunchPlan.scope` must equal the agent's
(`IsolationPolicy` rejects a plan that differs). This catches an adapter that forgets them; `PiAdapter` copies them, so for pi the check is a guard for future adapters. **Nothing enforces scope, turn count or deadline yet.** The executor entry point must take an enforcement value (broker handle plus deadline) as a required argument so a run cannot start without it. Pi has no iteration cap, so
the executor counts turns against `max_iterations` and enforces
`max_wall_clock_secs`; the broker enforces `scope`. `max_concurrent` is the
scheduler's.

## Hardening status

Implemented: mount, network, capability, env-allowlist, file-path, image,
scope and limits checks in `IsolationPolicy`; fail-closed `Unsupported`;
handshake comparison.

Not implemented yet, all executor work: non-root user, read-only root
filesystem, dropped Linux capabilities, `no-new-privileges`, and network
enforcement (the plan states the allowed endpoint; nothing enforces it until
the executor runs the container on an internal network). `container::NetworkPolicy::for_ceiling` (landed with #669) governs the dev
container where broker-run tools execute and is unchanged. The agent container
is separate: its plan is always exactly the model gateway at every ceiling
(tested), so there is one rule per container and nothing ceiling-derived to
drift. This module's type is `PlanNetwork` to avoid colliding with it.

## Status of this change

Landed here: `harness_adapter` (types, `IsolationPolicy`, `PiAdapter`,
handshake), tested and doctested.

Not yet done (in order):

1. Broker per the contract above. Depends on #669 (scoped registry), #677
   (spawn gate) and #706 (action gate).
2. Executor trait plus a container executor (hardening above) with a fake for
   tests; RPC event parsing (`agent_settled`, tool events) into the existing
   `TaskResult`/`FailureDiagnostics`, keeping `tasks/result` JSON unchanged for
   the one_track bridge fixtures. Blocked on the real-pi integration test below.
3. Packaging: pi as a pinned flake input (`github:earendil-works/pi`) with a
   lock hash and `npm install --ignore-scripts`, built into the agent image
   only. Upstream changed organisations and ships breaking renames in patch
   releases, so upgrades are deliberate and tested. The dev-container flake
   gains no pi dependency.
4. `harness` key in `[identity]`, default `pi`, invariant under repo-local
   override (equality required in `narrows`). Deferred until #669/#677 land
   and the identity owner agrees.
5. Switch `TaskRunner` to the adapter path.

## One harness, and deleting the old one

Pi is the only harness once the executor lands. The in-house `AgentLoop` path
is not kept as a second option. Order of removal, each its own commit:

1. `TaskRunner` uses the adapter (step 5 above).
2. `eval/runner.rs` and the swebench eval, which depend on `AgentLoop`, move to
   the executor or are rewritten against the MCP surface.
3. `AgentLoop`, `agent/rag.rs`-style in-loop helpers and the unused in-loop
   tool plumbing are deleted. `ToolRegistry` and the `Tool` impls stay: the
   broker runs them.

No tracking issues have been filed yet; filing them is for the repository
owner to approve. `ARCHITECTURE.md` links here; the SDLC docs PR (#713) should
reference it when this stack merges.

## Unverified assumptions (verify before step 2 ships)

Pi was not run while writing this; facts come from its docs. Check by running
it in the agent image:

- exact flag spellings (`--no-builtin-tools`, `--no-extensions` combined with
  `--extension`, `--no-mcp`, `--no-approve`, `--offline`);
- the extension API used by `pi_extension.ts` (`registerTool`, result shape,
  whether raw JSON Schema is accepted for `parameters`);
- `SYSTEM.md` and `models.json` locations under `PI_CODING_AGENT_DIR`;
- RPC event schema and exit codes.

An integration test gated on `NANNA_PI_BIN` should assert, against real pi,
that no tool other than the granted ones is callable.
