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
| No secrets | env vars that look like credentials are rejected; the model `apiKey` is a placeholder |
| Fixed hardening | privileges, Linux capabilities and devices are not expressible in a plan; the executor fixes them (non-root, read-only root, all caps dropped, no-new-privileges) |
| Fail closed | `Unsupported` for empty capability sets, empty model, or unrepresentable capability names |

`IsolationPolicy` encodes the rows that a plan can express. The property test
`plans_always_satisfy_isolation` and `narrower_identity_never_widens_the_plan`
pin the monotonicity guarantee: a narrower identity never produces a wider plan.

Even if the model is prompt-injected, it can only emit calls to granted
capabilities, and the broker is authoritative for every one.

## Effect boundary

Pi performs only `Workspace`-class work, through broker-run tools. Effects at
`Repository` and above (push, PR, CI, deploy) stay in nanna's Rust layer and
are never delegated into the agent container.

## Status of this change

Landed here: `harness_adapter` (types, `IsolationPolicy`, `PiAdapter`),
tested, doctested, clippy-clean.

Not yet done (in order):

1. Broker: serve the capability socket from a task's `ToolRegistry`
   (JSONL `{tool,args}` → `{ok,output}`), reusing scope filtering and the
   production hold.
2. Executor trait plus a container executor (hardened flags above) with a fake
   for tests; RPC event parsing (`agent_settled`, tool events) into the
   existing `TaskResult`/`FailureDiagnostics`, keeping `tasks/result` JSON
   unchanged for the one_track bridge fixtures.
3. Nix packaging of pi (`@earendil-works/pi-coding-agent`, pinned exactly) into
   the agent image; the dev-container flake gains no pi dependency.
4. `harness` key in `[identity]`, default `pi`. A repo-local override must not
   change it: the narrowing rule treats `harness` as invariant (equality
   required), since it changes what executes the agent. Deferred until
   #669/#677 land, to avoid conflicts in `identity/`.
5. Switch `TaskRunner` to the adapter path, then remove `AgentLoop` and the
   in-house tool loop as separate commits, checking eval and rollout
   dependents.

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
