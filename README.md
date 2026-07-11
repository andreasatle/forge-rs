# forge-rs

`forge-rs` is a Rust implementation of Forge organized around explicit state machines.

## Core idea

Every machine follows the same contract:

```text
state + event -> next state + effects
```

- **States** are explicit checkpoints.
- **Events** are facts that have already occurred.
- **Effects** are commands for work outside the machine.
- **Transitions** are pure functions that decide the next state and effects.

Business logic belongs in pure transition functions. Side effects belong in effect handlers.

## Architecture

```text
CLI
 ↓
ForgeRuntime
 ↓
RunSession
 ↓
SchedulerDriver + SchedulerHandler
 ↓
SchedulerMachine
 ↓                                      ↓
[RunNode effect]                   [IntegrateWork effect]
 ↓                                      ↓
DeliberatingNodeRunner             IntegrationService
 ↓                                      ↓
DeliberationMachine                Validator + ArtifactIntegrator
 ↓
ProviderRoleRunner
 ↓
Provider (cheap tier or strong tier)
```

Artifact history is orthogonal:

```text
Artifact = bare Git repository (branch-specific)
```

Telemetry is orthogonal:

```text
Telemetry = timestamped run directory + manifest + machine event traces + checkpoint
```

The codebase is organized around responsibility-bearing types rather than
large procedural modules. Pure state transitions live in machine types such as
`SchedulerMachine` and `DeliberationMachine`. Side effects are delegated to
handlers and services such as `SchedulerHandler`, `IntegrationService`,
`DeliberationHandler`, `WorkspaceFactory`, `ArtifactIntegrator`,
`ProjectRuntimeSetup`, `ResolvedProviderStack`, and the default trace parser /
grouper / renderer pipeline.

Modules expose small entrypoints and keep most implementation helpers private
or crate-private. Public callers generally interact with the runtime, machine,
artifact, provider, node-runner, validation, and telemetry traits/types exported
from each module's `mod.rs`.

## Runtime setup

`ForgeRuntime` is the CLI-facing entrypoint. `run` starts a fresh run; `resume`
finds one running manifest/checkpoint pair and re-enters the scheduler.

`RunSession` owns the already-resolved runtime inputs for a scheduler drive:
config, run identity, telemetry sink, and provider stack. It:

1. Loads or creates the artifact repository.
2. Builds `ProjectRuntimeSetup`.
3. Creates the `DeliberatingNodeRunner`.
4. Creates the `SchedulerHandler`.
5. Drives the scheduler with telemetry.
6. Prints and persists the final outcome.

`ProjectRuntimeSetup` centralizes project-derived wiring: role policy,
context-file names, required test-target derivation, validation plan, and the
validator. It loads a project adapter from the `adapter` YAML path, along with
every language plugin that adapter declares in its own `plugins:` list. A
node's target files pick the plugin that applies to it by file extension —
different worker nodes in the same run can be validated by different
language plugins.

`ResolvedProviderStack` resolves `ProviderConfig` into:

- run-manifest metadata,
- cheap and strong `RetryingProvider<LlamaCppProvider>` handles,
- per-tier token budgets,
- managed llama.cpp server processes kept alive for the run.

## Configuration

Forge is configured through a `forge.yaml` file:

```yaml
objective: "Write a short haiku about Rust state machines."
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "qwen2.5-coder-7b-instruct"
      n_predict: 512
  strong:                       # optional; fallback to cheap
    unmanaged:
      base_url: "http://localhost:8081"
      model: "qwen2.5-coder-14b-instruct"
      n_predict: 1024
  timeout_seconds: 120          # optional; default 120
  strong_timeout_seconds: 180   # optional; fallback to timeout_seconds
telemetry:
  directory: "runs"
adapter: adapters/coding.yaml  # required; path to a project adapter YAML file
validation:                     # optional
  commands:
    - cargo fmt --check
    - cargo test
  timeout_seconds: 120          # optional; default 120 per command
```

Relative paths in `adapter`, `artifact.repo_path`, and `telemetry.directory`
are resolved against the directory containing `forge.yaml`, not the working
directory.

`adapter` is a path to the project adapter YAML file governing role prompt
policy. It is required; there is no default. A missing or invalid file — or
any language plugin it declares — fails `forge.yaml` loading immediately.
Built-in adapters ship in this repo's `adapters/` directory: `coding.yaml` is
a single-team adapter, and `planner.yaml`, `implement.yaml`,
`create_test.yaml`, `pass_tests.yaml` are single-purpose adapters meant to be
combined in a multi-team `teams:` config (see below). Copy and modify any of
them freely, or point at your own file.

An adapter declares which language plugins it supports in its own
`plugins:` list, e.g.:

```yaml
plugins:
  - ../plugins/python.yaml
  - ../plugins/rust.yaml
```

paths resolved relative to the adapter file's own directory. Each plugin
declares the file extensions it applies to (`extensions: [py]`); the
framework picks the plugin matching a node's target files to derive its
init commands, validation commands, and required test targets. Built-in
plugins (`python.yaml`, `rust.yaml`) ship in this repo's `plugins/`
directory. Declaring `plugins:` and an explicit `validation:` block are
mutually usable together — `validation:` acts as the fallback validator for
nodes whose target files match no configured plugin.

### Multi-team configs

A `forge.yaml` can run more than one team side by side instead of a single
`adapter`/`northstar` pair, via a top-level `teams:` list:

```yaml
teams:
  - name: planner
    northstar: northstar.md
    adapter: adapters/planner.yaml
    trigger: start
  - name: implement
    northstar: northstar.md
    adapter: adapters/implement.yaml
    trigger: after_each(planner)
  - name: create_test
    northstar: northstar.md
    adapter: adapters/create_test.yaml
    trigger: after_each(planner)
  - name: pass_tests
    northstar: northstar.md
    adapter: adapters/pass_tests.yaml
    trigger: after_each(implement, create_test)
```

Each team has its own `name`, `northstar`, and `adapter`, and activates
according to its `trigger`: either `start` (runs from the beginning) or
`after_each(team_a, team_b, ...)` (runs after every named team has produced a
node). The built-in `planner.yaml`, `implement.yaml`, `create_test.yaml`, and
`pass_tests.yaml` adapters are designed to be combined this way — a planner
team fans out tasks, and separate implement/create_test/pass_tests teams
each own one concern instead of one adapter owning all of them.

By default Forge connects to already-running provider servers. For llama.cpp,
Forge can instead own a local `llama-server` process:

```yaml
provider:
  cheap:
    managed:
      llama_cpp:
        command: "llama-server"
        model:
          path: "models/qwen2.5-coder-7b-instruct.gguf"
        host: "127.0.0.1"
        port: 8080
        context_size: 8192       # optional
        startup_timeout_seconds: 60 # optional; default 60
        n_predict: 512
```

Managed mode is explicit. If the configured endpoint is already reachable before
Forge starts `llama-server`, Forge refuses to attach to it.

## CLI

```text
cargo run -- start   forge.yaml            — start a run from current artifact history
cargo run -- start   forge.yaml --resume   — resume an interrupted running checkpoint
cargo run -- show    forge.yaml            — display current files from the artifact
cargo run -- history forge.yaml            — display commit history
cargo run -- reset   forge.yaml            — delete artifact history and create a fresh Initial commit
cargo run -- trace   forge.yaml            — show the latest run grouped by node/attempt
cargo run -- trace   forge.yaml --run ID   — trace a specific run
cargo run -- trace   forge.yaml --summary  — show the flat chronological trace
cargo run -- trace   forge.yaml --prompts  — show full role prompts
cargo run -- trace   forge.yaml --failures — show failure-related events
```

### Example session

```
cargo run -- reset forge.yaml
cargo run -- start forge.yaml
cargo run -- show forge.yaml
```

Example output:

```
Commit      : a0c3de5
Files:
output.txt
--- output.txt ---
Rust state machines spin,
Transitions shift with event triggers—
Code flows, precise.
```

## Machines

### SchedulerMachine

The scheduler owns the run graph and decides which node may advance. The pure
transition logic stays in `SchedulerMachine`; `SchedulerHandler` executes
effects, persists checkpoints after progress events, and delegates cohesive
side effects to smaller services.

Responsibilities:

- Graph execution and dependency ordering
- Sequential node dispatch
- Recovery classification and bounded recovery growth
- Graph and protocol validation
- Checkpointable progress through explicit states and events

States:

- `Active` — validate the graph and dispatch the first ready node.
- `Waiting` — one node is executing or its work is integrating.
- `Complete` — all graph activity reached a terminal status.
- `Failed` — the run cannot continue; graph and failure reason are retained.

Events: `Start`, `PlanAccepted`, `WorkAccepted`, `NodeFailed`,
`IntegrationSucceeded`, `IntegrationFailed`.

Effects: `RunNode`, `IntegrateWork`.

Node execution is sequential. The scheduler selects one pending node whose dependencies are all completed, marks it running, emits `RunNode`, and enters `Waiting`. It does not dispatch another node until the active one finishes.

Node lifecycle:

```text
Pending -> Running -> Integrating -> Completed
                   \-> Failed

Pending -> Cancelled
```

Only `Completed` satisfies a dependency. `Failed` and `Cancelled` nodes remain as history.

Recovery actions:

- `Retry` — append a replacement with the same objective and model tier.
- `ElevateModel` — append a replacement using the stronger model tier.
- `Split` — append a strong planning node that decomposes the failed work.
- `Terminal` — fail the run and cancel pending downstream dependents.

Recovery growth is bounded by `MAX_ATTEMPTS`, `MAX_GRAPH_NODES`, and `MAX_PLAN_DEPTH`.

`RecoveryApplicator` owns graph mutation for recoverable failures. It routes
`Retry`, `ElevateModel`, `Split`, and `Terminal`, marks the failed node,
creates replacement nodes when allowed, remaps pending dependencies, and turns
exhaustion/capacity/depth failures into terminal scheduler states.

### DeliberationMachine

The deliberation machine drives a three-role pipeline: Producer → Critic → Referee.

Responsibilities:

- Running the Producer role to generate content.
- Running the Critic role to review the content.
- Running the Referee role to accept or reject the reviewed content.
- Bounded revision loops when the Referee rejects.

The final output is always the Producer content. Critic and Referee do not replace it.

`RoleResult` distinguishes:

- `Accepted` — role completed successfully.
- `Rejected` — role completed but rejected the content. Producer rejection is terminal. Critic rejection is advisory and proceeds to the Referee. Referee rejection triggers a revision loop while budget remains, otherwise it terminates the node.
- `Failed` — role could not execute. Always terminal; never enters the revision loop.

The role layer handles protocol retries when a provider response cannot be parsed as valid JSON.

`DeliberationHandler` is the effect handler for the machine. It builds role
requests, constructs tool context and target views, validates structured plan
producer output, validates that artifact-producing Work actually changed the
workspace, and maps role results back into deliberation events.

`DeliberatingMachine` adapts `DeliberationMachine` plus `DeliberationHandler`
to the generic machine runner.

### Planner

Plan nodes produce structured planner JSON. `PlannerOutputProcessor` owns the
planner schema boundary: parsing provider content, validating task structure,
enforcing explicit target constraints, preventing accidental recreation of
existing files, deriving required test targets, and converting valid planner
output into scheduler `NodeRequest`s.

For simple plan objectives that explicitly name exactly one source file,
`DeliberatingNodeRunner` can use the processor's fast path to create a work
node, plus required test nodes when configured, without calling the provider.

## Artifact data plane

### Artifact

Represents committed truth. Backed by a bare Git repository.

Fields:

- `repo_path` — path to the bare repository.
- `branch` — logical branch name used for all reads and writes.
- `commit_sha` — exact commit pinning this version.

The artifact always resolves the configured `branch` by name, not `HEAD`. Bare repositories with non-default HEAD are handled correctly.

### ArtifactView

A read-only view of a specific commit. Does not require a working tree.

Operations:

- `list_files` — returns all file paths in the commit, sorted.
- `read_file` — reads a file's contents at the pinned commit.

Path containment is enforced: absolute paths and parent traversals are rejected.

### Workspace

A temporary mutable checkout cloned from an `Artifact`. Created for each work node integration. The directory is deleted automatically when the workspace is dropped.

`WorkspaceFactory` owns workspace creation. It creates detached worktrees for
temporary work attempts, clone/checkouts for caller-owned workspaces, and
centralizes sanitized Git command setup used by workspace lifecycle and
integration code.

### WorkspaceFileOps

Safe mutation primitives operating inside a `Workspace`.

Operations:

- `list_files`
- `read_file`
- `write_file` — create a new file or overwrite an existing file. This is the default tool when replacing most or all of a file.
- `replace_text` — make a small, localized edit after reading the file and identifying an exact, unique old string.
- `delete_file`

Path containment is enforced on all operations. Symlink safety is enforced on top of lexical validation: all operations canonicalize the resolved path and verify it remains inside the workspace root. Broken symlinks and symlinks whose targets point outside the workspace are rejected.

`replace_text` fails if the target string is absent or appears more than once. It matches exactly, so whitespace, indentation, or formatting differences cause it to fail. For whole-file rewrites, use `write_file` instead of retrying `replace_text`.

### Integration

`IntegrationService` is the scheduler-side artifact gate for Work nodes. It
creates and tracks pending `WorkAttempt` workspaces, records failed attempt
evidence, runs semantic and configured validation, and advances the current
artifact only after validation passes.

`ArtifactIntegrator` is the lower-level Git responsibility. It commits
workspace changes into the artifact's bare repository using a CAS-safe push.

Protocol:

1. **CAS pre-check** — read the branch tip before staging. If it differs from the workspace base commit, return `Conflict` immediately without touching the repository.
2. **Stage and commit** — `git add --all` and `git commit` inside the workspace.
3. **Force-with-lease push** — push using `--force-with-lease=refs/heads/<branch>:<base>`. On failure the branch tip is re-read; if it has advanced, the error is reclassified as `Conflict` to distinguish a CAS race from an unrelated push error.

Validation failures and integration failures do not advance the artifact. They
return scheduler integration-failure events with recovery actions.

## Node execution

Nodes are executed through the `NodeRunner` trait.

```text
ArtifactView
  ↓
NodeRunner
  ↓
WorkAttempt workspace
```

The `SchedulerHandler` connects the scheduler to the runner. For each `RunNode` effect it:

1. Builds an `ArtifactView` from the current `Artifact` snapshot.
2. Asks `IntegrationService` to create a `WorkAttempt` workspace for artifact-producing Work.
3. Passes the view and workspace to the runner via `NodeRunRequest`.
4. If the runner returns `WorkAccepted`, emits `IntegrateWork`.
5. During integration, `IntegrationService` runs validation and calls `ArtifactIntegrator`.
6. If validation or integration fails, the node is marked failed and the artifact is not advanced.

### DeliberatingNodeRunner

Runs a node by driving a `DeliberationMachine` backed by a real `ProviderClient`.

When the request carries an `ArtifactView`, configured context files and a file
listing are supplied as deliberation context so the Producer has artifact
context without any workspace mutation.

Mapping from deliberation output to `NodeRunResult`:

- Plan node: `PlanAccepted` with child work nodes from structured planner output.
- Work node: `WorkAccepted` with the Producer content as summary; artifact changes are already in the `WorkAttempt` workspace.
- Deliberation failure: `Failed` with `RecoveryAction::Terminal`.

## Tool system

Each role invocation drives a bounded tool loop backed by a `FileToolExecutor`.
`RoleToolDispatcher` owns the mutable protocol state for that loop: current
prompt, accumulated observations, tool-call pressure, repeated-observation
coercion, telemetry, and final artifact-change detection.

`RoleResponseParser` owns role JSON parsing. It strips optional markdown code
fences, extracts the leading JSON object, rejects preamble text, rejects
framework placeholders, validates minimum meaningful content length, and maps
role-specific schemas to `RoleResult`.

### Tool loop

The loop runs up to `MAX_TOOL_STEPS` (5) tool calls per role invocation. Each iteration:

1. Call the provider.
2. If the response is a tool request JSON, execute the tool and append the observation to the prompt.
3. If the response is a role result JSON, return it.
4. If parsing fails, retry (up to `MAX_PROTOCOL_RETRIES` = 2 additional calls).

Exceeding the tool step limit is a protocol failure; the role returns `Failed`.

### Role permissions

```text
Producer  — read/write (list_files, read_file, write_file, replace_text, delete_file)
Critic    — read-only  (list_files, read_file)
Referee   — read-only  (list_files, read_file)
```

Write operations issued by Critic or Referee receive an error observation and do not mutate the workspace.

Producer write operations mutate the shared `WorkAttempt` workspace directly. Later reads in the same Work path, including reviewer reads, observe that workspace state.

## Validation

When a `validation` section is present in `forge.yaml`, Forge runs the configured commands inside a temporary workspace after the Producer completes but before committing to the artifact.

```yaml
validation:
  commands:
    - cargo fmt --check
    - cargo test
  timeout_seconds: 120
```

Behavior:

- Commands run in order via `sh -c` inside the workspace directory.
- Each command gets its own independent timeout budget (`timeout_seconds`, default 120).
- The first failing command stops the sequence; subsequent commands do not run.
- A timeout kills the child process and counts as a failure.
- Failed validation prevents integration; the artifact is not advanced.

When `validation` is absent, all changes pass automatically.

## Providers

`ProviderClient` is the trait boundary for LLM calls. It takes a `ProviderRequest` and returns a `ProviderResponse` or a `ProviderError`.

`ProviderRequest` carries:

- `prompt` — the complete prompt string.
- `max_tokens` — maximum tokens to generate.
- `output_schema` — optional structured output hint (`Json`).

`ProviderError` classifies failures as:

- `Retryable` — transient; a retry may succeed.
- `Terminal` — permanent; retrying will not help.
- `Timeout` — the provider did not respond within the configured deadline.

Implemented providers:

- `LlamaCppProvider` — calls a local llama-server `/completion` endpoint. HTTP timeout is enforced per-request.
- `OllamaProvider` — calls a local Ollama `/api/generate` endpoint (available but not wired into `ForgeRuntime` by default).
- `RetryingProvider` — wraps any provider and retries on `Retryable` errors.

`HttpProviderErrorClassifier` centralizes HTTP status and transport error
classification shared by the HTTP-backed providers.

Role responses use structured JSON output hints (`output_schema: Some(Json)`).
`LlamaCppProvider` maps that hint to a JSON-object GBNF grammar. It also accepts
provider requests carrying an explicit grammar.

### Model tier routing

The runtime builds two provider stacks from a single `ProviderConfig`:

- **Cheap tier** — uses `provider.cheap`, which is either `unmanaged` or `managed`.
- **Strong tier** — uses `provider.strong` when present, otherwise falls back to `provider.cheap`; `strong_timeout_seconds` falls back to `timeout_seconds`.

The scheduler's `ElevateModel` recovery action upgrades a retried node to the strong tier.

## Telemetry

Each run creates a timestamped directory under the configured `telemetry.directory`:

```text
runs/
  2026-06-23-14-31-42/
    manifest.json
    telemetry/
      000001--scheduler-machine--machine-started.txt
      000005--deliberation-machine--machine-started.txt
      000011--role-machine--producer--parse-failed.txt
      000012--role-machine--producer--protocol-retry.txt
  2026-06-23-15-00-01/
    manifest.json
    telemetry/
  latest -> 2026-06-23-15-00-01   (symlink on Unix)
```

The `latest` entry is updated atomically to point to the newest run after each new run directory is created.

Machine event trace files contain structured fields:

```
source: SchedulerMachine
kind: MachineStarted
machine: SchedulerMachine
```

Manifest finalization failures are logged to stderr and do not abort the run.

The default `trace` view is a pipeline:

- `DefaultTraceParser` reads telemetry files and assigns node/attempt context.
- `DefaultTraceGrouper` groups contextualized records into node and attempt summaries.
- `DefaultTraceRenderer` renders the node list and timeline.

`trace --summary` keeps the older flat chronological view. `trace --prompts`
and `trace --failures` print filtered full event bodies.

## Run manifest

Each run's `manifest.json` is created when the run starts and finalized when it ends.

Initial fields (written at startup):

```json
{
  "run_id": "2026-06-23-14-31-42",
  "started_at": "2026-06-23T14:31:42Z",
  "status": "running",
  "telemetry_dir": "telemetry",
  "artifact_repo": ".forge/artifacts/main.git",
  "objective": "Write a short haiku about Rust state machines.",
  "provider": "http://localhost:8080",
  "providers": {
    "cheap": {
      "base_url": "http://localhost:8080",
      "model": "qwen2.5-coder-7b-instruct",
      "n_predict": 512,
      "timeout_seconds": 120,
      "managed": false
    },
    "strong": {
      "base_url": "http://localhost:8081",
      "model": "qwen2.5-coder-14b-instruct",
      "n_predict": 1024,
      "timeout_seconds": 180,
      "managed": true,
      "managed_server": {
        "kind": "llama_cpp",
        "command": "llama-server",
        "port": 8081,
        "context_size": 8192,
        "startup_timeout_seconds": 60
      }
    }
  }
}
```

Final fields (merged at completion):

```json
{
  "completed_at": "2026-06-23T14-33-05Z",
  "duration_seconds": 83.2,
  "status": "succeeded",
  "final_commit": "a0c3de5b9f...",
  "failure_reason": null,
  "validation_passed": true
}
```

`validation_passed` semantics:

- `true` — validation ran and all commands passed.
- `false` — validation ran and at least one command failed or timed out.
- `null` — validation was never reached; the run failed before the integration gate (e.g., provider error, deliberation failure).

## Current Limitations

- **Single-artifact runtime.** Each run operates on one artifact repository. There is no multi-artifact graph.
- **No provider-native tools.** The LLM protocol uses plain JSON in the prompt; it does not use the provider's native function-calling or tool-use API.
- **Resume is checkpoint-based.** Resume replays any node that was active at interruption time. Completed work is preserved in Git, but in-flight provider/tool work is not resumed mid-call.
- **No prompt-window management.** The prompt grows as tool calls accumulate within a role invocation. There is no token counting, context pruning, or truncation strategy.
- **No automatic conflict retry.** When `IntegrationError::Conflict` is returned by `integrate`, the node is marked failed. The scheduler may apply a recovery action (Retry, ElevateModel, Split), but there is no dedicated conflict-retry path that replays the tool loop against the updated artifact.

## Testing

```
cargo build
cargo fmt --check
cargo test
cargo run -- start forge.yaml
cargo run -- start forge.yaml --resume
cargo run -- trace forge.yaml
cargo run --example scheduler_deliberation_demo
cargo run --example deliberation_demo
```

Tests cover machine transitions, emitted effects, integration gating, recovery behavior, graph validation, protocol violations, bounded growth, terminal states, invariant preservation, artifact data-plane operations, validation commands, manifest lifecycle, model tier routing, and CAS/force-with-lease semantics. Tests do not require real providers, network access, or persistent filesystem state beyond temporary directories.

Run `cargo test` to see the current test count.
