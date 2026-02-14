# MVP Roadmap

Concrete task breakdown for reaching a working agent that can receive a prompt, call tools via an LLM, and return a result.

## Current State

- 128 tests passing across workspace
- Model provider abstraction exists but `OllamaProvider::chat()` uses the generate (completion) API — no tool-call support
- Agent control loop (`AgentLoop`) compiles but every method is a stub/no-op
- Tool trait and `ToolRegistry` exist and work
- Entity/container/monitoring infrastructure is in place
- Tool-call handling code in `main.rs` (lines 92–158) is dead code because the model never returns tool calls

## Milestone 1: Model Provider Tool-Call Support

### Task 1.1 — Switch OllamaProvider from generate API to chat API

**File**: `model/src/ollama.rs` (lines 130–168)

`OllamaProvider::chat()` currently concatenates all messages into a single string and calls the generate (completion) endpoint. This makes tool calling impossible because the generate API doesn't support tools.

- Replace the generate call with `ollama_rs`'s chat API (`ChatMessageRequest`)
- Map `ChatRequest.tools` into Ollama's tool format
- Parse tool calls from the chat response into `ChatResponse.choices[].message.tool_calls`
- Map `FinishReason` correctly: `Stop` when no tool calls, `ToolCalls` when tools are requested
- Update existing tests; add tests for tool-call round-trip with a mocked response

### Task 1.2 — Upgrade reqwest to fix RUSTSEC-2025-0134

**Tracking**: issue #40

- Bump reqwest (or its transitive dependency) to a patched version
- Verify `cargo audit` passes clean

## Milestone 2: Agent Loop Implementation

### Task 2.1 — Inject real dependencies into AgentLoop

**File**: `harness/src/agent/mod.rs` (lines 91–98)

`AgentLoop` currently holds only `state`, `config`, and `iterations`. It needs:

- A `Box<dyn ModelProvider>` for LLM calls
- A `ToolRegistry` for executing tool calls
- A `Vec<ChatMessage>` for conversation history
- A system prompt (from `AgentContext.task_description` or similar)

Update `AgentLoop::new()` and `AgentConfig` accordingly.

### Task 2.2 — Implement `plan()`

**File**: `harness/src/agent/mod.rs` (line 181)

- Build a `ChatRequest` from conversation history + system prompt
- Include available tools from `ToolRegistry`
- Call `model_provider.chat(request).await`
- Append the assistant's response to conversation history

### Task 2.3 — Implement `perform()`

**File**: `harness/src/agent/mod.rs` (line 206)

- Extract `tool_calls` from the last assistant message
- For each tool call, look up the tool in `ToolRegistry` and execute it
- Append each tool result as a `ChatMessage` with role `Tool`
- Handle tool execution errors gracefully (append error message, don't panic)

### Task 2.4 — Implement `check_task_complete()`

**File**: `harness/src/agent/mod.rs` (line 187)

- Return `true` when the last assistant message has `FinishReason::Stop` **and** no tool calls
- Return `false` otherwise (model wants to call more tools)

### Task 2.5 — Simplify state machine for MVP

The current state machine has 7 states (Planning, CheckingCompletion, Deciding, Querying, Performing, Completed, Error). For MVP, the core loop is:

1. **Planning** → call model
2. **Performing** → execute tool calls (if any)
3. **CheckingCompletion** → done if no tool calls remain

Skip `Deciding` and `Querying` states (they depend on RAG which is post-MVP). Transition directly from CheckingCompletion to Planning when not complete.

## Milestone 3: End-to-End Integration

### Task 3.1 — Add `agent` CLI subcommand

**File**: `harness/src/main.rs`

- Add a subcommand (e.g. `nanna-coder agent --model <model> --prompt <prompt>`)
- Wire it to create an `OllamaProvider`, `ToolRegistry` with built-in tools, and `AgentLoop`
- Print final result to stdout

### Task 3.2 — Integration test with mock provider

- Create a mock `ModelProvider` that returns a scripted sequence: tool call → tool call → stop
- Assert that `AgentLoop::run()` executes the expected tools in order and terminates

### Task 3.3 — E2E test with containerized Ollama

- Use the existing container infrastructure to spin up Ollama
- Send a simple tool-using prompt and verify the full round-trip
- Gate behind a feature flag or `#[ignore]` for CI speed

## Milestone 4: Context Entity (issue #26)

### Task 4.1 — Implement ContextEntity fields

**File**: `harness/src/entities/context/types.rs`

Add fields to track a completed agent run:

- `task_description: String`
- `conversation: Vec<ChatMessage>`
- `tool_calls_made: Vec<ToolCallRecord>`
- `result_summary: String`
- `model_used: String`

### Task 4.2 — Store ContextEntity on agent completion

- After `AgentLoop::run()` completes, build a `ContextEntity` from the run data
- Persist it via the entity store

## Post-MVP

Items deferred to post-MVP (not ordered):

- RAG module for codebase querying (enables Deciding/Querying states)
- Decision module with cost/risk evaluation
- TestEntity (#24)
- EnvEntity (#25)
- TelemetryEntity (#27)
- AST entity for code-aware context
- Image builder improvements
- vLLM migration (#39)
- Project visualization (#47)
