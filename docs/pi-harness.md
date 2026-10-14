# Pi as nanna's internal harness

**Status: planned.** Only the interface layer (`harness_adapter`) exists. No
broker, executor or runner switch is written, nothing launches pi yet, and
`assign_task` still runs the in-house loop. This document describes the target
and what is and is not built.

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
| Network | the plan names exactly one endpoint, the model gateway. The host string is checked (no localhost, loopback, link-local, metadata or numeric-looking names), but real egress enforcement is the container network, not name checks. |
| No secrets | the environment is an exact (name, value) allowlist (`HOME`, `PI_CODING_AGENT_DIR`, `NANNA_BROKER_SOCKET`, `NANNA_CAPABILITIES`); there is no secret-name heuristic; the model `apiKey` is a placeholder |
| Fixed hardening | privileges, Linux capabilities and devices are not expressible in a plan; the executor will fix them (non-root, read-only root, all caps dropped, no-new-privileges). **Not implemented yet.** |
| Fail closed | `Unsupported` for empty capability sets or unrepresentable capability names; `ResolveError::InvalidModel`, `InvalidImage` and `InvalidBrokerSocket` for a bad model name, image reference or socket path; a plan that drops the agent's scope, limits or audited repository is rejected |
| Verified lockdown | **Planned.** After start the executor compares the runtime's reported tool set to the plan with `verify_runtime_tools` and aborts on any difference. Nothing calls it yet. |

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

1. look the tool up in the registry scoped to the identity (built by the broker
   from the same identity and scope as the grant, see below); unknown or out-of-grant names are
   refused;
2. enforce `scope.paths` / `read_paths` and protected paths on file arguments;
3. pass the **action gate** (the auditor's per-action review) before
   execution; there is no unaudited path, and a refusal is returned to the
   agent as a tool error and surfaced to the human like any other gap;
4. for `Repository` and above: check the human-availability **window**
   and take the coordination **lease**, holding it for the call;
5. check the **production hold** for `Production`;
6. execute, and record the call for the task result and audit log.

Spawning goes through the existing spawn gate: a `ResolvedAgent` can only be
built from an `Allowed` proof, and a `CheckedPlan` only from a `ResolvedAgent`,
so a plan reaches an executor only through that chain (transitively; the
executor does not itself inspect the proof). The prompt and repository label
come from the audited request. Checking that the working repository on disk
names the audited repository, as `TaskManager::submit_spawn` does, is the
executor's job. The broker and executor are not written yet; this list is their
contract and is reviewed before code.

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
drift. The plan carries a single `gateway` endpoint, so there is no
"no network" case to model.

## Status of this change

Landed: `harness_adapter` (types, `IsolationPolicy`, `PiAdapter`, handshake
comparison), and the corrective hardening of it: private-field `ResolvedAgent`
built from an `Allowed` spawn proof, `CheckedPlan`, a stricter `Endpoint`, an
exact env allowlist, one broker socket and one gateway, and removal of unused
API (`HarnessKind`, `PlanNetwork`, `Mount`).

Merge rule: the next merge to main makes pi runnable end to end for at least one
inner-loop identity, behind these isolation checks, with the real-pi test
passing. The broker, executor and `TaskRunner` switch may be developed as
stacked draft PRs, but they merge as a unit that works; no further unwired
slice lands on its own.

Not yet done (in order):

1. Broker, per the contract above and the real-pi constraints (refuse every call
   until `toolsAdded` has been verified; capabilities above Workspace withheld
   until the per-action gate #706 is on main).
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

## Carried to the broker and executor slice

Known gaps, tracked here and not yet fixed:

- A `CheckedPlan` does not remember its agent or repository, so a plan for one
  agent could be handed to a broker built for another. The broker handle and
  plan must be matched at the executor entry point.
- `ScopedCapabilities::scope` accepts any registry and does not check path
  scoping; the contract (the registry must come from `create_*_registry_for`)
  must be documented and tested at the broker.
- Model-name charset and length cap beyond the current option/whitespace/
  control-character checks.
- The extension's reply cap counts UTF-16 units, not bytes; a `null` reply needs
  a clean error; U+2028/U+2029 handling against pi's framing is undecided (the
  executor must use strict LF framing).
- The TypeScript extension has no tests of its own: `TIMEOUT_MS`,
  `MAX_REPLY_BYTES` and the `!ok` throw survive mutation. It must be exercised
  by the `NANNA_PI_BIN` real-pi test.
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

No tracking issues have been filed yet; filing them is for the repository
owner to approve. `ARCHITECTURE.md` links here; the SDLC docs PR (#713) should
reference it when this stack merges.

## Real-pi findings (pi 1.2.0)

Reported by the reviewer's verification pass, which installed pi 1.2.0 with
`--ignore-scripts` in a throwaway directory and ran it on the host (evidence in
`/var/tmp/pi-verify`, not in this repository). Pin pi to exactly 1.2.0; earlier
text naming 1.0.3 is superseded.

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

Constraints these findings put on the executor (none implemented):

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
- The broker socket the executor mounts must be confirmed to be a socket the
  executor created and owns, not just a validated path.

The extension asset (`pi_extension.rs`, installed in the container as
`nanna-capabilities.ts`) sets UTF-8 decoding, rejects on error, close and
timeout, caps the reply size, and throws when the broker reports `ok: false`.

## Still unverified

- RPC event field schemas beyond `toolsAdded`, and exit codes.
- Behaviour under the container hardening flags (nothing runs a container yet).

A real-pi integration test (`NANNA_PI_BIN`) must assert against real pi that no
tool other than the granted ones is reachable, and it must **fail**, not skip,
when pi is absent in CI.
