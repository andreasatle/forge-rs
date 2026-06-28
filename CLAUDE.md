CLAUDE.md

Project

Forge-RS is a Rust implementation of Forge built around explicit state machines.

The core architecture is:

state + event -> next_state + effects

Transition functions are pure. Effect handlers perform I/O and orchestration.

The architecture should make illegal states difficult or impossible to represent.

⸻

Core Principles

* Prefer structural solutions over patches.
* Prefer typed data over parsing strings.
* Failure recovery is driven by typed failure kinds, never human-readable messages.
* Semantic validation occurs before Critic/Referee or integration.
* Remove compatibility layers unless explicitly required.
* Remove obsolete architecture instead of preserving it.
* Keep abstractions no broader than the runtime semantics.

⸻

State Machines

Business logic belongs in transition functions.

Transitions:

* are pure
* never perform I/O
* emit effects
* use exhaustive matching

Effect handlers:

* execute providers
* execute tools
* perform workspace operations
* perform integration
* persist checkpoints
* emit telemetry

⸻

Project Adapters

The framework owns orchestration.

Project adapters own project-specific behavior.

Adapters define:

* target representation (TargetView)
* semantic validation
* project-specific rendering
* future project-specific behavior

The framework must never assume that targets are source files.

⸻

Language Abstraction

Languages are configured through YAML.

Core code must not contain assumptions about:

* Python
* Rust
* uv
* pytest
* Cargo
* filename conventions

Language-specific behavior belongs in language specifications or project adapters.

⸻

Tooling

Tool permissions come from structured metadata.

Never infer permissions from prompt text.

Producer:

* receives committed/base state

Critic and Referee:

* receive staged Producer state

Target information is carried as structured metadata (target_files), not parsed from prompts.

⸻

Rust Style

* Small cohesive modules.
* Prefer explicit structs and enums.
* Avoid hidden mutation.
* Prefer owned public types.
* Avoid unnecessary lifetimes.
* Clone at machine boundaries when ownership becomes simpler.
* Optimize later.

⸻

Module Size

Production source files should normally remain below approximately 500 LOC.

If a file grows beyond this, extract cohesive modules before adding additional functionality.

Avoid arbitrary splits (helpers, misc, part2).

Extract concepts instead.

⸻

Testing

Every state machine should have focused transition tests.

Test:

* valid transitions
* invalid transitions
* emitted effects
* retries
* failure paths
* invariants

Effect handlers should be tested independently.

Providers, tools, Git, and the filesystem should be exercised only in effect-handler tests.

Every new test must state the invariant it protects.
If a new test differs only by parameters from an existing test, add a table case instead of a new test.

⸻

Non-goals

Do not mechanically port the Python implementation.

The Python repository is historical reference material only.

The Rust architecture is the source of truth.
