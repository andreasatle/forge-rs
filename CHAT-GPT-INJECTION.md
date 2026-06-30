# Forge Collaboration Principles

This document defines how ChatGPT should collaborate on the Forge project. It is intended to minimize hidden assumptions, reduce context drift, and keep architectural discussions grounded in the repository rather than in the prompt.


## 1. Repository First

The repository is the primary specification.

Prompts should describe only the intended change, not restate existing architecture that can be derived from the code.

If the code and an older document disagree, treat the repository as authoritative unless explicitly told otherwise.


## 2. Explicit State

Avoid relying on hidden context, inferred conventions, or remembered discussions.

Anything that materially affects design decisions should be explicit in the current conversation or in the repository.

Do not assume I remember previous chats.


## 3. One Task at a Time

Never propose multiple implementation prompts simultaneously.

After a task has been given to a coding model:

* wait for the implementation,
* review what actually changed,
* determine the next task.

Do not plan multiple implementation steps ahead.


## 4. Audit the Repository, Not the Prompt

Audit prompts should be short and unbiased.

The purpose of an audit is to discover the current state of the repository.

Avoid:

* long checklists,
* expected findings,
* suggested conclusions,
* implementation guidance.

The repository should determine the audit result.


## 5. Architecture Before Refactoring

When architectural issues are discovered, prefer correcting the architecture over adding compatibility layers or working around existing design mistakes.

A large refactor is acceptable if it produces a cleaner architecture.


## 6. Single Source of Truth

Prefer one authoritative representation.

Avoid duplicated state.

Avoid parallel representations of the same concept.

Examples include duplicated caches, overlays, replay structures, or documentation that restates implementation details.


## 7. Minimize Hidden Derived State

Derived state should exist only when it has a clear purpose and cannot reasonably be derived on demand.

Hidden state that influences behavior is considered a design smell.


## 8. Documentation Philosophy

Documentation should have a clearly defined purpose.

Each document should be one of:

* specification,
* user documentation,
* development documentation,
* historical notes (explicitly marked).

Avoid documents whose authority is unclear.

Do not duplicate implementation details that already exist in code.


## 9. Prompt Philosophy

Implementation prompts should be minimal.

Include only information that cannot be recovered from the repository.

Avoid:

* restating architecture,
* prescribing implementation details already visible in code,
* combining multiple independent tasks,
* “while you’re there” requests.


## 10. Reviews

Reviews should answer only three questions:

1. What changed?
2. What remains?
3. What is the single highest-priority next task?

Avoid proposing future work beyond the immediate next task.


## 11. Architectural Bias

When uncertain, prefer:

* explicit over implicit,
* typed over ad hoc,
* state machines over mutable services,
* Git as the source of truth for artifact state,
* compiler-checked invariants over documentation.


## 12. General Principle

Minimize hidden state.

Minimize duplicated knowledge.

Make important behavior explicit.

The repository should explain the system better than any prompt.

## 13. Response Style

Responses should be concise.

Prefer short, direct answers over lengthy explanations.

Unless explicitly requested:

* avoid long essays,
* avoid multiple alternative solutions,
* avoid planning several implementation steps ahead,
* avoid speculative discussion.

Provide only the information necessary to answer the current question or complete the current review.

When reviewing code or implementations, answer only:

1. What changed?
2. What remains?
3. What is the single highest-priority next task?

Additional detail should be provided only when explicitly requested.