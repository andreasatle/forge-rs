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
SchedulerMachine
 ↓                           ↓
[RunNode effect]        [IntegrateWork effect]
 ↓                           ↓
DeliberationMachine      Validator
 ↓                           ↓
RoleRunner               Integration (bare git, force-with-lease)
 ↓
Provider (cheap tier or strong tier)
```

Artifact history is orthogonal:

```text
Artifact = bare Git repository (branch-specific)
```

Telemetry is orthogonal:

```text
Telemetry = timestamped run directory + machine event traces
```

## Configuration

Forge is configured through a `forge.yaml` file:

```yaml
objective: "Write a short haiku about Rust state machines."
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  base_url: "http://localhost:8080"
  n_predict: 512
  timeout_seconds: 120          # optional; default 120
  strong_base_url: "http://localhost:8081"  # optional; fallback to base_url
  strong_n_predict: 1024        # optional; fallback to n_predict
  strong_timeout_seconds: 180   # optional; fallback to timeout_seconds
telemetry:
  directory: "runs"
validation:                     # optional
  commands:
    - cargo fmt --check
    - cargo test
  timeout_seconds: 120          # optional; default 120 per command
```

Relative paths in `artifact.repo_path` and `telemetry.directory` are resolved against the directory containing `forge.yaml`, not the working directory.

## CLI

```text
cargo run -- run     forge.yaml   — continue from current artifact history
cargo run -- show    forge.yaml   — display current files from the artifact
cargo run -- history forge.yaml   — display commit history
cargo run -- reset   forge.yaml   — delete artifact history and create a fresh Initial commit
```

### Example session

```
cargo run -- reset forge.yaml
cargo run -- run forge.yaml
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

The scheduler owns the run graph and decides which node may advance.

Responsibilities:

- Graph execution and dependency ordering
- Sequential node dispatch
- Recovery classification and bounded recovery growth
- Graph and protocol validation

States:

- `Running` — validate the graph and dispatch the first ready node.
- `Waiting` — one node is executing or its work is integrating.
- `Complete` — all graph activity reached a terminal status.
- `Failed` — the run cannot continue; graph and failure reason are retained.

Events: `Start`, `NodeReturned`, `IntegrationReturned`.

Effects: `RunNode`, `IntegrateWork`, `ReturnComplete`, `ReturnFailed`.

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
- `Rejected` — role completed but rejected the content. Triggers a revision loop for the Referee (if revisions remain), terminal failure for Producer and Critic.
- `Failed` — role could not execute. Always terminal; never enters the revision loop.

The role layer handles protocol retries when a provider response cannot be parsed as valid JSON.

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

### WorkspaceFileOps

Safe mutation primitives operating inside a `Workspace`.

Operations:

- `list_files`
- `read_file`
- `write_file`
- `replace_text`
- `delete_file`

Path containment is enforced on all operations. Symlink safety is enforced on top of lexical validation: all operations canonicalize the resolved path and verify it remains inside the workspace root. Broken symlinks and symlinks whose targets point outside the workspace are rejected.

`replace_text` fails if the target string is absent or appears more than once.

### ArtifactUpdate

Represents intended changes as an ordered list of `FileChange` variants:

- `Write { path, content }` — create or overwrite a file.
- `Replace { path, old, new }` — replace a unique substring.
- `Delete { path }` — remove a file.

Changes are applied in order. The first error stops application.

### Integration

Commits workspace changes into the artifact's bare repository using a CAS-safe push.

Protocol:

1. **CAS pre-check** — read the branch tip before staging. If it differs from the workspace base commit, return `Conflict` immediately without touching the repository.
2. **Stage and commit** — `git add --all` and `git commit` inside the workspace.
3. **Force-with-lease push** — push using `--force-with-lease=refs/heads/<branch>:<base>`. On failure the branch tip is re-read; if it has advanced, the error is reclassified as `Conflict` to distinguish a CAS race from an unrelated push error.

## Node execution

Nodes are executed through the `NodeRunner` trait.

```text
ArtifactView
  ↓
NodeRunner
  ↓
ArtifactUpdate
```

The `SchedulerHandler` connects the scheduler to the runner. For each `RunNode` effect it:

1. Builds an `ArtifactView` from the current `Artifact` snapshot.
2. Passes the view to the runner via `NodeRunRequest`.
3. If the runner returns `WorkAccepted` with an `ArtifactUpdate`, runs the configured validator against a temporary workspace.
4. If validation passes, applies the update and calls `integrate`, advancing the artifact.
5. If validation fails, the node is marked failed and the artifact is not advanced.

### DeliberatingNodeRunner

Runs a node by driving a `DeliberationMachine` backed by a real `ProviderClient`.

When the request carries an `ArtifactView`, a brief context block — file listing and `README.md` if present — is prepended to the deliberation objective so the Producer has file context without any workspace mutation.

Mapping from deliberation output to `NodeRunResult`:

- Plan node: `PlanAccepted` with one child work node whose objective is the Producer content.
- Work node: `WorkAccepted` with the Producer content as summary and an `ArtifactUpdate` writing `output.txt`.
- Deliberation failure: `Failed` with `RecoveryAction::Terminal`.

## Tool system

Each role invocation drives a bounded tool loop backed by a `FileToolExecutor`.

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

Write operations issued by Critic or Referee receive an error observation and no update is recorded.

### Read-after-write overlay

`FileToolExecutor` maintains an in-memory overlay of pending writes and deletes. Reads consult the overlay before falling back to the committed artifact view. This means:

- A file written in one tool call is immediately visible to a subsequent `read_file` in the same session.
- A file deleted in one tool call is hidden from subsequent reads.
- The overlay is local to one executor instance and is discarded after the role completes.

The overlay does not mutate the artifact. Only the `ArtifactUpdate` produced at the end of the role loop is eligible for integration.

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

Role responses use structured JSON output hints (`output_schema: Some(Json)`). The LlamaCppProvider passes the prompt unchanged; it does not activate the llama.cpp grammar or JSON-schema constraint mode.

### Model tier routing

The runtime builds two provider stacks from a single `ProviderConfig`:

- **Cheap tier** — uses `base_url`, `n_predict`, and `timeout_seconds`.
- **Strong tier** — uses `strong_base_url` (fallback `base_url`), `strong_n_predict` (fallback `n_predict`), and `strong_timeout_seconds` (fallback `timeout_seconds`).

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
  "provider": "http://localhost:8080"
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
- **No llama.cpp grammar support.** The `output_schema: Json` hint is carried in `ProviderRequest` but the LlamaCppProvider does not activate the grammar or JSON-schema constraint endpoint parameter.
- **No durable resume.** A run interrupted mid-way cannot be resumed; the artifact is consistent (commits are atomic), but the run itself must be restarted from the beginning.
- **No prompt-window management.** The prompt grows as tool calls accumulate within a role invocation. There is no token counting, context pruning, or truncation strategy.
- **No automatic conflict retry.** When `IntegrationError::Conflict` is returned by `integrate`, the node is marked failed. The scheduler may apply a recovery action (Retry, ElevateModel, Split), but there is no dedicated conflict-retry path that replays the tool loop against the updated artifact.

## Testing

```
cargo fmt --check
cargo test
cargo run -- run forge.yaml
```

Tests cover machine transitions, emitted effects, integration gating, recovery behavior, graph validation, protocol violations, bounded growth, terminal states, invariant preservation, artifact data-plane operations, validation commands, manifest lifecycle, model tier routing, and CAS/force-with-lease semantics. Tests do not require real providers, network access, or persistent filesystem state beyond temporary directories.

Run `cargo test` to see the current test count.
