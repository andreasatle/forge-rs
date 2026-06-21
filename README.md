# forge-rs

`forge-rs` is a Rust implementation of Forge organized around explicit, durable state machines. The scheduler currently executes nodes sequentially while it expands a work graph, runs planning and work nodes, integrates accepted work, and applies bounded recovery. Scheduling will become asynchronous once the supporting execution and effect infrastructure is in place.

## Core idea

Every machine has a finite set of states, events, and effects:

```text
state + event -> next state + effects
```

- **States** are durable checkpoints.
- **Events** are facts that have already occurred.
- **Effects** are commands for work outside the machine.
- **Transitions** are pure functions that decide the next state and effects.

Recovery is part of this grammar, not an exception around it. Failures arrive as typed events and select an explicit recovery action. Restricting the possible states, events, and effects makes control flow inspectable, invalid combinations easier to reject, and behavior safer to evolve.

## Architecture

```text
RunRequest
    |
    v
SchedulerMachine
    |
    | RunNode effect
    v
Planner / Worker
    |
    | NodeReturned event
    v
SchedulerMachine
    |
    | IntegrateWork effect (for accepted work)
    v
Integration
    |
    | IntegrationReturned event
    v
SchedulerMachine
    |
    v
SchedulerOutput
```

The ownership boundary is deliberate:

| Component | Owns | Does not own |
| --- | --- | --- |
| Machine state | Durable graph and lifecycle data | I/O or external mutation |
| Transition function | Pure state advancement and effect selection | Effect execution |
| Effect handler | Side effects and conversion of results into events | Durable scheduler state |
| Generic runner | The event/transition/effect loop | Scheduler policy |

The runner advances one transition at a time. A transition may emit zero or one effect; there is no effect queue. An empty effect list produces another synthetic start tick, allowing pure bookkeeping to continue.

## Scheduler

The scheduler owns the run graph and decides which node may advance. It has four durable states:

- `Running`: validate and scan the graph, then dispatch the first ready node.
- `Waiting`: one node is running or its accepted work is integrating.
- `Complete`: all graph activity has reached a terminal status without halting the run.
- `Failed`: the run cannot continue; the graph and failure reason are retained.

It consumes three events:

- `Start`
- `NodeReturned`
- `IntegrationReturned`

It emits four effects:

- `RunNode`
- `IntegrateWork`
- `ReturnComplete`
- `ReturnFailed`

Node execution is currently sequential. In `Running`, the scheduler selects the first pending node whose dependencies are all completed, marks it running, emits `RunNode`, and enters `Waiting`. It does not dispatch another node until the active node either completes, enters recovery, or finishes integration. This is a constraint of the current runner and effect model, not the intended final scheduling model; execution will become asynchronous once the supporting infrastructure is in place.

```text
Pending -> Running -> Integrating -> Completed
                    \-> Failed

Pending -> Cancelled
```

`Failed` nodes remain as history. `Cancelled` is used for pending downstream work that can no longer run after a terminal failure. Only `Completed` satisfies a dependency; `Failed`, `Cancelled`, `Running`, and `Integrating` do not.

Plan nodes expand the graph with validated child requests. Work nodes return work that must pass through integration. The scheduler rejects duplicate graph IDs, unknown dependency or recovery-source references, invalid plan dependencies, node-kind/outcome mismatches, inconsistent waiting states, and deadlocked graphs by transitioning to `Failed`.

## Recovery

Node and integration failures carry one of four recovery actions:

- `Retry`: append a replacement with the same objective and model tier.
- `ElevateModel`: append a replacement using the stronger model tier.
- `Split`: append a strong planning node that decomposes the failed work.
- `Terminal`: fail the run and cancel pending downstream dependents.

Recovery never resurrects or removes the failed node. The failed node is marked `Failed`, its replacement is appended, and pending nodes that depended on the failed node are remapped to the replacement. This preserves the attempt history while allowing the graph to continue.

Recovery growth is bounded by three circuit breakers:

- `MAX_ATTEMPTS` limits repeated recovery for an objective.
- `MAX_GRAPH_NODES` limits total graph growth.
- `MAX_PLAN_DEPTH` limits recursive planning ancestry.

These limits turn otherwise unbounded retry, splitting, or recursive planning into explicit terminal failures. Bounded growth is necessary for the state graph to remain finite in practice and for malformed or unproductive recovery loops to stop deterministically.

## Integration

Accepted work is not completed work:

```text
WorkAccepted != Completed

WorkAccepted
    |
    v
Integrating
    |
    | IntegrateWork effect
    v
IntegrationReturned
    |
    +-- Succeeded -> Completed
    \-- Failed ----> Recovery or Failed
```

When a work node returns `WorkAccepted`, the scheduler marks it `Integrating` and emits `IntegrateWork`. Only a successful `IntegrationReturned` event marks the node `Completed` and stores its integration summary. Downstream nodes therefore wait for integration success, rather than acting on work that was produced but not accepted into the shared result.

## Invariants

- Node IDs are opaque, stable, and unique within a run. Their string representation must not be parsed for meaning.
- The graph is append-only. Nodes are not removed, and failed nodes are not reused.
- The current sequential scheduler permits only one active node, represented by its `Waiting` state.
- Only `Completed` nodes satisfy dependencies.
- Protocol violations transition the scheduler to `Failed` with a reason instead of panicking.
- Recovery preserves failed history and records how replacement nodes originated.
- Transitions are pure; handlers own side effects.

## Testing

The generic runner and scheduler machine are heavily unit-tested. The tests serve as executable specifications for allowed and forbidden transitions, emitted effects, integration gating, recovery behavior, graph validation, protocol violations, bounded growth, terminal states, and invariant preservation. They do not require real providers, tools, Git operations, network access, or filesystem effects.

## Current scope

Implemented:

- Serial execution
- Bounded recovery
- A distinct integration phase
- Typed scheduler output and recovery classification
- Graph and protocol validation

Deferred:

- Asynchronous execution and concurrent node dispatch
- Richer artifact handling
- Durable integration artifacts
- Provider, tool, and Git implementations
- Effect queues

## Philosophy

Restricting the space of possible states, events, and effects makes progress easier. The project favors explicit state machines and typed transitions over implicit behavior and string-driven logic.
