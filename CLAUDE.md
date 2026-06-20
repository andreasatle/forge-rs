# CLAUDE.md

## Project

forge-rs is a Rust implementation of Forge built around explicit state machines.

The core architecture is:

state + event -> next_state + effects

Business logic belongs in pure transition functions. Side effects belong in effect handlers.

## Architectural rules

* Prefer enums with payloads over flags, nullable fields, or string states.
* Make illegal states unrepresentable.
* Keep transitions pure.
* Do not perform I/O inside transition functions.
* Emit effects for all external actions.
* Use exhaustive match statements.
* Avoid compatibility layers unless explicitly requested.
* Avoid fallback behavior that preserves old architecture.
* Avoid hidden mutation and global state.
* Prefer owned data in public types: String, PathBuf, Vec<T>, HashMap<K, V>.
* Avoid lifetime parameters in public Forge types unless clearly necessary.

## Machine pattern

Each machine should normally have:

state.rs
event.rs
effect.rs
transition.rs
mod.rs

Example:

pub fn transition(
    state: AttemptState,
    event: AttemptEvent,
) -> Transition<AttemptState, AttemptEffect>

Transitions return effects; they do not execute them.

## Initial machine set

The intended machine hierarchy is:

RunMachine
  SchedulerMachine
    NodeMachine
      AttemptMachine
        ToolLoopMachine
      IntegrationMachine

Optional later machines:

ProviderMachine
ToolMachine
TelemetryMachine
ConfigMachine

Do not introduce optional machines until the simpler effect-handler boundary becomes insufficient.

## Implementation order

Start with pure core logic.

Preferred order:

1. common Transition type and IDs
2. AttemptMachine
3. ToolLoopMachine
4. NodeMachine
5. SchedulerMachine
6. IntegrationMachine
7. RunMachine
8. effect handlers
9. providers, tools, workspace/git

Do not begin with HTTP providers, CLI, git, or tool execution.

## Rust style

* Use small modules.
* Use clear public names.
* Avoid clever abstractions.
* Prefer explicit structs and enums.
* Clone at machine boundaries when it keeps ownership simple.
* Optimize later.
* Keep tests close to transition logic.

## Testing

Every machine transition should have focused tests.

Test:

* allowed transitions
* forbidden transitions
* emitted effects
* terminal states
* exhausted/retry paths
* invariant preservation

Tests should not require real providers, git, network, or filesystem unless testing effect handlers.

## Non-goals

Do not port the Python code mechanically.

The Python Forge repo is reference material, not the authority.

The authority is the Rust state-machine architecture.