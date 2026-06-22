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
User
  ↓
Scheduler
  ↓
SchedulerHandler
  ↓
NodeRunner
  ↓
ArtifactView
  ↓
Deliberation
  ↓
Provider
  ↓
ArtifactUpdate
  ↓
WorkspaceFileOps
  ↓
Workspace
  ↓
Integration
  ↓
Artifact
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

Node execution is sequential. The scheduler selects one pending node whose dependencies are all completed, marks it running, emits `RunNode`, and enters `Waiting`. It does not dispatch another node until the active one finishes. This is a constraint of the current model, not the intended final scheduling model.

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

These two machines are independent state machines. They share no state and communicate only through the `NodeRunner` boundary.

## Artifact data plane

### Artifact

Represents committed truth. Backed by a bare Git repository.

Fields:

- `repo_path` — path to the bare repository.
- `branch` — logical branch name.
- `commit_sha` — exact commit pinning this version.

### ArtifactView

A read-only view of a specific commit. Does not require a working tree.

Operations:

- `list_files` — returns all file paths in the commit, sorted.
- `read_file` — reads a file's contents at the pinned commit.

Path containment is enforced: absolute paths and parent traversals are rejected.

### Workspace

A temporary mutable checkout cloned from an `Artifact`. Used to stage changes before integration.

### WorkspaceFileOps

Safe mutation primitives operating inside a `Workspace`.

Operations:

- `list_files`
- `read_file`
- `write_file`
- `replace_text`
- `delete_file`

Path containment is enforced on all operations.

`replace_text` fails if the target string is absent or appears more than once.

### ArtifactUpdate

Represents intended changes as an ordered list of `FileChange` variants:

- `Write { path, content }` — create or overwrite a file.
- `Replace { path, old, new }` — replace a unique substring.
- `Delete { path }` — remove a file.

Changes are applied in order. The first error stops application.

### Integration

Owns Git concerns only. Given an `Artifact` and a `Workspace`, it stages all changes, commits them to the bare repository, and returns a new `Artifact` pointing at the new commit. The branch is preserved.

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
3. If the runner returns `WorkAccepted` with an `ArtifactUpdate`, applies it through a `Workspace` and calls `integrate`, advancing the artifact.

### StaticNodeRunner

A minimal test runner. Returns fixed outcomes: plan nodes produce one child, work nodes accept, and nodes whose objective contains "fail" return a terminal failure.

### DeliberatingNodeRunner

Runs a node by driving a `DeliberationMachine` backed by a real `ProviderClient`.

When the request carries an `ArtifactView`, a brief context block — file listing and `README.md` if present — is prepended to the deliberation objective so the Producer has file context without any workspace mutation.

Mapping from deliberation output to `NodeRunResult`:

- Plan node: `PlanAccepted` with one child work node whose objective is the Producer content.
- Work node: `WorkAccepted` with the Producer content as summary and an `ArtifactUpdate` writing `output.txt`.
- Deliberation failure: `Failed` with `RecoveryAction::Terminal`.

## Providers

`ProviderClient` is the trait boundary for LLM calls. It takes a `ProviderRequest` (prompt string) and returns a `ProviderResponse` (content string) or a `ProviderError`.

Implemented providers:

- `OllamaProvider` — calls a local Ollama instance.
- `LlamaCppProvider` — calls a local llama.cpp server.
- `RetryingProvider` — wraps any provider and retries on retryable errors.

Role responses use structured JSON output.

## Testing

194 tests cover machine transitions, emitted effects, integration gating, recovery behavior, graph validation, protocol violations, bounded growth, terminal states, invariant preservation, and artifact data-plane operations. Tests do not require real providers, network access, or persistent filesystem state beyond temporary directories.
