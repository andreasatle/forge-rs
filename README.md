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
      parallel: 1                # optional; default 1 concurrent request
  strong:                       # optional; fallback to cheap
    unmanaged:
      base_url: "http://localhost:8081"
      model: "qwen2.5-coder-14b-instruct"
      n_predict: 1024
  timeout_seconds: 300          # optional; default 300
  strong_timeout_seconds: 180   # optional; fallback to timeout_seconds
telemetry:
  directory: "runs"
adapter: path/to/adapter.yaml  # required; path to a project adapter YAML file
dispatch_cap: 4                 # optional; default 4 — see Concurrency below
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
Built-in single-purpose adapters ship in this repo's `adapters/` directory:
`planner.yaml`, `implement.yaml`, `create_test.yaml`, `pass_tests.yaml`,
meant to be combined in a multi-team `teams:` config (see below). Copy and
modify any of them freely, or point at your own file.

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

A `forge.yaml` can run more than one team side by side via a top-level
`teams:` list. The top-level `adapter` field is still required (it is the
fallback wiring `DeliberatingNodeRunner` builds from at startup), but every
team-spawned node dispatches under its own team's `adapter`/`northstar`
instead — there is no top-level `northstar` field for `teams:` to replace.

```yaml
teams:
  - name: planner
    northstar: northstar.md
    adapter: adapters/planner.yaml
    kind: plan
    trigger: start
  - name: implement
    northstar: northstar.md
    adapter: adapters/implement.yaml
    kind: work
    trigger: after_teams(planner)
  - name: create_test
    northstar: northstar.md
    adapter: adapters/create_test.yaml
    kind: work
    trigger: after_teams(planner)
  - name: pass_tests
    northstar: northstar.md
    adapter: adapters/pass_tests.yaml
    kind: work
    trigger: after_teams(implement, create_test)
```

Each team has its own `name`, `northstar`, `adapter`, `kind`, and `trigger`.
`kind` (`plan` or `work`) says what the team's spawned nodes *are* — a `plan`
team decomposes an objective into tasks, a `work` team executes one. `trigger`
says *when it activates*: either `start` (runs from the beginning) or
`after_teams(team_a, team_b, ...)` (runs after every named team has produced a
node). These are deliberately two separate fields rather than one: `kind` is
the source of truth for what a team produces, `trigger` for when it runs, and
collapsing them would make it impossible to express a future team shape where
those two questions have different answers. Today's only valid pairing is
`kind: plan` with `trigger: start`, and `kind: work` with
`trigger: after_teams(...)`; a mismatch fails at config load. The built-in
`planner.yaml`, `implement.yaml`, `create_test.yaml`, and `pass_tests.yaml`
adapters are designed to be combined this way — a planner team fans out
tasks, and separate implement/create_test/pass_tests teams each own one
concern instead of one adapter owning all of them.

Teams are symmetric — the planner is just another team, not a special case
wired into the scheduler. Every team, including a `trigger: start` planner,
has the same shape: a name, a northstar, an adapter, a kind, and a trigger.
The graph's root node *is* the run's `Trigger::Start` team's own node —
`SchedulerMachine::initial_state` seeds it with that team's `team`/`adapter`/
`northstar` fields directly, rather than bootstrapping a second, blank-identity
node that competes with it. (A config with no `Trigger::Start` team keeps the
historical blank-identity root, since there is no team to seed it from.) This
means the root node's task-manifest rows are correctly attributed to that
team from the start, so an `after_teams(planner)` trigger keyed on its name
sees them immediately instead of waiting on a redundant second decomposition
pass.

At config load, Forge computes each team's **terminal** status from this
trigger graph: a team is terminal if no other team's `after_teams` names it
(erroring if the graph has a cycle). Terminal teams mark the point where a
task is considered fully done.

Planner tasks can declare `depends_on: [other_task_id, ...]` so that a task
is not spawned as a Work node until its dependencies have completed. A
dependency only counts as satisfied once *every* terminal team has recorded
a completion row for it — e.g. with the trigger graph above, a task depended
on for its implementation isn't considered done until both `implement` and
`pass_tests` (the terminal teams) have completed it, not just `implement`.

### Task manifest

Every completed node — Plan or Work, any team — appends a row to
`.forge/tasks.json` inside the artifact, committed alongside that node's own
changes. This manifest is what `trigger`/`depends_on` evaluation reads; teams'
prompts never see it directly.

```json
{
  "schema_version": 1,
  "tasks": [
    {
      "id": "t1",
      "objective": "Implement fibonacci(n: int)",
      "commit": "e3f4a2b",
      "completed_at": "2026-07-09T19:30:00Z",
      "team": "implement",
      "name": "fibonacci",
      "depends_on": []
    }
  ]
}
```

`team` is `None` only for nodes with no team (the single-team, no-`teams:`
path, where no trigger evaluation depends on it). `name` and `depends_on` are
carried from the planner task that produced this id and are absent for rows
recorded when a `Work` node itself completes, since a `Work` node has no
`name`/`depends_on` of its own. `forge tasks forge.yaml` lists this manifest's
rows from the latest artifact commit.

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
        parallel: 4               # optional; default 1 — passed to llama-server's --parallel
```

Managed mode is explicit. If the configured endpoint is already reachable before
Forge starts `llama-server`, Forge refuses to attach to it.

### Concurrency

Forge can run more than one node at once. Two independent knobs control this,
and neither implies the other:

- **`dispatch_cap`** (top-level `forge.yaml` field, default 4) — the maximum
  number of nodes the scheduler may have `Running`/`Integrating` at once,
  regardless of provider. A node can be in flight — spawned, running local
  tool calls, or blocked waiting for a provider permit — without holding a
  provider slot at every instant, so `dispatch_cap` is deliberately sized
  above any one provider's own concurrency so that provider stays saturated
  whenever a permit frees up.
- **`parallel`** (per provider tier, under `unmanaged`/`managed.llama_cpp`,
  default 1) — how many concurrent completions that tier's server can safely
  serve. Forge sizes one `ResourceManager` (a `Mutex`+`Condvar` permit gate,
  `src/runtime/resource_manager.rs`) per tier from this value; every call
  through that tier — including each individual retry attempt, not just the
  first — blocks until it acquires a permit. For a managed llama.cpp server,
  `parallel` is also passed through as `llama-server --parallel`, so the
  config value and the server's own concurrency stay in lockstep. For
  `backend: ollama`, which is unmanaged-only, `parallel` only bounds forge's
  own client-side concurrency — it has no path to the server's real
  concurrency ceiling (`OLLAMA_NUM_PARALLEL`, set independently when the
  Ollama process starts), so a mismatch doesn't error, it just means excess
  requests queue server-side instead of running truly in parallel.

Dispatch is **opportunistic, not wave-gated**: as soon as any in-flight node
resolves and frees a slot below `dispatch_cap`, the scheduler re-scans and
dispatches immediately, rather than waiting for the rest of the current batch
to drain first. A fast node's freed slot gets backfilled right away even if a
slower sibling dispatched alongside it is still running.

Within that re-scan, ready nodes are chosen **depth-first**: `RunGraph::find_ready`
prefers the most-recently-inserted ready node, and a Plan node's children are
always appended right after it — so the scheduler drills all the way into a
branch it just expanded before returning to an older, shallower sibling
elsewhere in the graph. This trades fairness for depth: a large branch
discovered early can keep winning dispatch over an older sibling for a
while. That tradeoff is bounded, not eliminated — `MAX_PLAN_DEPTH` and
`MAX_GRAPH_NODES` cap how deep or how large any one branch can grow — but a
shallow, long-pending sibling has no dedicated fairness/aging tiebreak of its
own; it simply becomes eligible again as soon as nothing deeper is ready.

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
cargo run -- tasks   forge.yaml            — list tasks recorded in .forge/tasks.json
cargo run -- prompt-preview forge.yaml --node <plan|work> --role <producer|critic|referee> [--worker NAME]
                                          — render a static role-prompt template without calling a provider
cargo run -- vast    search --min-ram G --max-price P — list Vast.ai GPU offers, cheapest first
cargo run -- vast    rent    OFFER_ID    — rent a Vast.ai instance from an offer
cargo run -- vast    list                — list rented Vast.ai instances with SSH info
cargo run -- vast    destroy INSTANCE_ID — destroy a rented Vast.ai instance
```

`prompt-preview` always loads its adapter from the config's top-level `adapter`
field, never from `teams:` — for a multi-team config, it only reaches the
prompts of the team whose adapter happens to be the top-level one.

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
- Bounded-concurrency node dispatch, up to `dispatch_cap` in flight at once
- Recovery classification and bounded recovery growth
- Graph and protocol validation
- Checkpointable progress through explicit states and events

States:

- `Active` — validate the graph and dispatch into any free capacity below `dispatch_cap`.
- `Waiting` — `dispatch_cap` nodes are in flight (executing or integrating), or nothing is currently ready.
- `Complete` — all graph activity reached a terminal status.
- `Failed` — the run cannot continue; graph and failure reason are retained.

Events: `Start`, `PlanAccepted`, `WorkAccepted`, `NodeFailed`,
`IntegrationSucceeded`, `IntegrationFailed`, `PlannerTasksIntegrated`,
`PlannerTasksIntegrationFailed`.

Effects: `RunNode`, `IntegrateWork`.

Node dispatch is bounded-concurrent, not sequential: the scheduler may have
up to `dispatch_cap` nodes `Running`/`Integrating` at once (default 4), each
driven on its own thread by `SchedulerDriver`. See [Concurrency](#concurrency)
above for `dispatch_cap` vs. per-provider `parallel`, opportunistic
re-dispatch, and depth-first traversal order.

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
output into scheduler `NodeRequest`s. There is no pattern-matched shortcut
that skips the provider for simple-looking objectives — every Plan node goes
through the same Producer/Critic/Referee pipeline as any other node.

A `kind: "plan"` output that decomposes into fewer than two tasks never
actually split anything, regardless of wording, so `into_plan` treats it as a
no-op and short-circuits it into a terminal `kind: "task"` output instead of
recursing into another Plan node. Because any task in such a batch can end up
becoming that terminal row, `name` is required (and validated non-blank) on
`kind: "plan"` tasks the same way it already was on `kind: "task"` tasks —
`EmptyName` validation fires for any kind except `Work`.

Every Plan-node decomposition is judged against **MECE** — mutually exclusive
(no two sibling tasks or sub-objectives cover overlapping ground) and
collectively exhaustive (the siblings together cover everything the
decomposed objective asked for, with nothing dropped). This guidance lives
once, in the generic prompt layer's planner-only block
(`adapters/generic.yaml`, merged in only for Plan-node Producer/Critic/Referee
composition via `GenericPromptConfig::for_planner`), so every plan-capable
adapter inherits both halves without its own copy, and it never reaches a
Work node's rendered prompt.

### Role terminology

Three different concepts are all commonly called "role" in this codebase.
They compose, but they answer different questions:

- **Deliberation role** (`DeliberationRole`: `Producer`/`Critic`/`Referee`) —
  *which stage of the pipeline is running.* Every node, Plan or Work, goes
  through all three. This is a fixed, framework-level enum; it never varies
  by project or adapter.
- **`plugin_role`** (`WorkerRoleConfig::plugin_role`, e.g. `"tester"`,
  `"implementer"`) — *which worker specialization a Work node is.* Set on an
  adapter's `workers:` entries and matched against a language plugin's own
  `plugin_roles` list (`LanguageRoleConfig::plugin_role`) to pick that role's
  validation overrides and name-target rules. Optional when the adapter
  declares no language plugins at all (nothing to match against); required
  otherwise, enforced at config-load time.
- **`identity`** (`RolePromptConfig::identity`) — *the persona text inside one
  deliberation role's prompt*, e.g. the Producer's own description of who it
  is. Every deliberation role, and every worker role, has its own `identity`
  string, layered generic → adapter → worker-role at render time.

Concretely: a Work node dispatched under the `create_test` team runs all
three deliberation roles (Producer, Critic, Referee); its `plugin_role` is
`"tester"` (matched against `plugins/python.yaml`'s `tester` entry); and each
of those three deliberation roles' prompts carries its own `identity` text
describing that role specifically.

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
- `OllamaProvider` — calls a local Ollama `/api/generate` endpoint. Selected per unmanaged tier via `backend: ollama` (default `llama_cpp`); the two dialects are not wire-compatible, so `backend` picks which `ProviderClient` implementation talks to that tier's `base_url`.
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
