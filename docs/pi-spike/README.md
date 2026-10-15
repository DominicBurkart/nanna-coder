# Real-pi spike (container), 2026-10-15

Evidence for the pi harness work, kept in the repository so it outlives `/var/tmp`. Raw logs: `logs/<scenario>/`; checksums in `SHA256SUMS`. The scripts were run from `/var/tmp/nanna-pi-spike` (they hard-code that path); `scripts/nanna-capabilities.ts` is a copy of the extension in `harness/src/harness_adapter/pi_extension.rs` as of this run and is not kept in sync.

## Setup (exact)
- Image: `docker.io/library/node:24-slim`, digest `sha256:ec857ea6bb60cb3d0fa4d97495590c5fe7d51be8572c307b55957eeb06569cf7`, Node v24.22.0, npm 11.19.1.
- Package: `@earendil-works/pi-coding-agent`, npm `latest` dist-tag = **1.2.0** (also `legacy-node20` = 0.74.2). Engines: node >=22.19.0. License MIT.
- Install (in the container): `npm install --ignore-scripts --save-exact @earendil-works/pi-coding-agent@1.2.0` (see `logs/01-install-command.txt`). `pi --version` printed `1.2.0`.
- Runs: `podman run --rm --network none`, pi driven over `--mode rpc` with the adapter's exact flags and files (generated from `harness_adapter` sources: the extension from `pi_extension.rs`, argv from the pinned `pi_argv_is_pinned_exactly`). A mock OpenAI-compatible gateway (`scripts/gateway.js`) and a mock broker (`scripts/broker.js`) ran inside the same container on loopback / a unix socket. Container ran as (rootless-mapped) root; non-root hardening is untested.
- Scripts: `scripts/{gateway.js,broker.js,run.js,prepare.sh,scenario.sh}`.

## Results (pi 1.2.0)
A. Baseline (adapter flags): the model received exactly one tool, `read_file`, in both requests. The system prompt was the plan's `SYSTEM.md` plus a `<cwd>` section. `toolsAdded` is present in the FIRST system `message_start` event. The prompt response was `{"success":true,"data":{"disposition":"started"}}`. The extension called the broker with `{"tool":"read_file","args":{"path":"README.md"}}` and the result reached the model. pi wrote `auth.json` and `models-store.json` into `PI_CODING_AGENT_DIR`. Run settled (`agent_settled`, `aborted:false`), exit 0.
B. Unknown flag: `Error: Unknown option: --definitely-not-a-flag`, exit 1 (hard error).
C. Hostile project files (`.pi/SYSTEM.md`, `AGENTS.md`, `CLAUDE.md`, `.pi/mcp.json` that would touch a file, `.pi/extensions/evil.ts`) with `--no-approve`: the model saw only `read_file`; none of the EVIL markers reached the system prompt; our prompt was intact; the MCP command did not run.
D. Same files with `--approve` instead: the project `.pi/SYSTEM.md` REPLACED our system prompt (our marker gone, EVIL marker present). `--no-approve` is load-bearing. (Tools stayed `read_file` only, because `--no-extensions` and `--no-mcp` still applied.)
E1. Unknown model name: pi does NOT reject it. It prints `Model "not-a-real-model" not found for provider "nanna". Using custom model id.`, the prompt response is still `success:true`, exit 0. Model names must be validated by nanna.
E2. Gateway returns HTTP 500: prompt response `success:true`; the failure appears only as an assistant message with `stopReason:"error"` and `errorMessage:"500: ..."` inside `agent_end`; pi auto-retried 3 times (4 gateway requests; `auto_retry_start`/`auto_retry_end` events), then `agent_settled` with `aborted:false`, exit 0. The executor must treat a final assistant `stopReason:"error"` as task failure; neither `success` nor the exit code shows it.
F. Read-only agent dir: prompt response `success:false`, `error:"Credential store read failed for nanna: EROFS ... /nanna/pi/auth.json"`, and pi did NOT exit (it hung until the driver's 30 s timeout SIGKILLed it). The executor must act on `success:false` (abort, then kill the container) and must not wait for exit.

## Implications for the executor (not implemented)
1. Read `toolsAdded` from the first system `message_start`, run `verify_runtime_tools`, and refuse broker calls until it passes.
2. Failure detection needs three signals: prompt response `success:false` (kill, do not wait for exit), assistant `stopReason:"error"` in the final `agent_end`, and a wall-clock deadline.
3. Validate the model name in nanna; pi accepts anything.
4. The agent dir must be writable.
5. The doc's pin to 1.2.0 is correct: it is the latest published.

## Not yet tested
- Non-root user, read-only root filesystem, dropped capabilities (hardening flags).
- The extension's timeout/oversize/close handlers against the real extension runtime (the 10 surviving TypeScript mutants).
- U+2028/U+2029 in RPC framing.
- A real model (a mock gateway only); the real gateway reachability from inside a container.
