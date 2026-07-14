# CLAUDE.md

## Project

Forge-RS is a Rust implementation of Forge built around explicit state machines.

The core architecture is:
```state + event -> next_state + effects```

Transition functions are pure. Effect handlers perform I/O and orchestration.

The architecture should make illegal states difficult or impossible to represent.

---

## Core Principles

- Prefer structural solutions over patches.
- Single source of truth: never derive the same fact through two independent mechanisms.
- Prefer typed data over parsing strings.
- Failure recovery is driven by typed failure kinds, never human-readable messages.
- Semantic validation occurs before Critic/Referee or integration.
- Remove compatibility layers unless explicitly required.
- Remove obsolete architecture instead of preserving it.
- Keep abstractions no broader than the runtime semantics.

---

## Design Discipline

Never add special-case logic that bypasses the normal flow.
If a shortcut seems necessary, the architecture needs fixing, not a workaround.

Examples of violations that have been found and removed:
- **Fast-plan**: pattern-matched objectives to skip the LLM planner entirely
- **render_scoped_system**: conditionally modified prompts based on node structure
- **validate_no_recreate / validate_explicit_targets**: parsed objective strings to infer file permissions

Signs a change is wrong:
- It adds more lines than it removes
- It introduces a new early return before the main flow
- It parses strings to recover structured information
- It adds a flag or boolean to "just get it working"
- It bypasses the adapter, machine, or effect stack
- It encodes behavior that belongs in YAML as Rust code

When in doubt, remove the special case and fix the architecture.

---

## State Machines

Business logic belongs in transition functions.

Transitions:
- are pure
- never perform I/O
- emit effects
- use exhaustive matching

Effect handlers:
- execute providers
- execute tools
- perform workspace operations
- perform integration
- persist checkpoints
- emit telemetry

---

## Project Adapters

The framework owns orchestration.

Project adapters own project-specific behavior.

Adapters are defined entirely in YAML — no per-adapter Rust modules.

Adapters define:
- role prompts (planner, worker roles, critic, referee)
- worker roles and their validation plans
- context file names
- future project-specific behavior

The framework must never assume that targets are source files.

Adding a new adapter means adding a new YAML file — zero Rust changes required.

---

## Language Abstraction

Languages are configured through YAML plugin files.

Core code must not contain assumptions about:
- Python, Rust, or any other language
- uv, pytest, Cargo, or any build tool
- filename conventions

Language-specific behavior belongs in language plugin YAML files.

---

## Worker Roles

Worker roles are defined in the adapter YAML, not in Rust enums.

A worker role defines:
- a prompt
- a validation plan

The framework assigns roles to nodes based on target files and adapter rules.

Adding a new role means adding an entry to the adapter YAML — zero Rust changes required.

---

## Tooling

Tool permissions come from structured metadata.

Never infer permissions from prompt text.

Producer:
- receives committed/base state
- writes to the WorkAttempt workspace via file tools

Critic and Referee:
- receive Producer state from the WorkAttempt workspace
- read-only access

Target information is carried as structured metadata (`target_files`), not parsed from prompts.

---

## Rust Style

- Small cohesive modules.
- Prefer explicit structs and enums over free functions.
- Functions that operate on a struct's data belong as methods on that struct.
- Avoid hidden mutation.
- Prefer owned public types.
- Avoid unnecessary lifetimes.
- Clone at machine boundaries when ownership becomes simpler.
- Optimize later.

---

## Module Size

Production source files should normally remain below approximately 500 LOC.

If a file grows beyond this, extract cohesive modules before adding additional functionality.

Avoid arbitrary splits (helpers, misc, part2).

Extract concepts instead.

Test files are exempt from the 500 LOC limit.

---

## Testing

Every state machine should have focused transition tests.

Test:
- valid transitions
- invalid transitions
- emitted effects
- retries
- failure paths
- invariants

Effect handlers should be tested independently.

Providers, tools, Git, and the filesystem should be exercised only in effect-handler tests.

Every new test must state the invariant it protects.

If a new test differs only by parameters from an existing test, add a table case instead of a new test.

Do not add new inline tests to production files.

Add tests to existing test modules, or create behavior-oriented test modules when appropriate.

---

## Prompts for Claude Code

Implementation prompts must be minimal.

Include only what cannot be recovered from the repository.

Do not:
- Restate existing architecture
- Prescribe implementation details already visible in the code
- Combine independent tasks
- Add "while you're there" requests

One task at a time. Commit after each. Verify before the next.

Audit prompts must be short and unbiased.

Let the code determine findings — do not suggest expected conclusions or list what you expect to find.

---

## Non-goals

Do not mechanically port the Python implementation.

The Python repository is historical reference material only.

The Rust architecture is the source of truth.
