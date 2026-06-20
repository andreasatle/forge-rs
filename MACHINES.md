# Forge-rs State Machine Framework — Draft 1

## 0. Core Principle

Forge-rs is not organized around service classes that mutate internal fields.

It is organized around explicit state machines.

Every machine follows the same grammar:

transition(state, event) -> Transition {
    next_state,
    effects,
}

A machine does not perform I/O directly. It decides what should happen next.

All I/O is represented as effects.

State  = where the machine is now
Event  = what just happened
Effect = what must be performed outside the pure transition

The goal is to make illegal states unrepresentable and to make hidden control flow impossible.

⸻

## 1. Machine Hierarchy

The proposed runtime consists of these machines:

RunMachine
  SchedulerMachine
    NodeMachine
      PlannerMachine
        AttemptMachine
          TurnMachine
      WorkerMachine
        AttemptMachine
          ToolLoopMachine
      IntegrationMachine

A simpler view:

RunMachine
  SchedulerMachine
    NodeMachine
      AttemptMachine
        ToolLoopMachine
      IntegrationMachine

The deeper split between PlannerMachine and WorkerMachine may be useful because planning and work execution produce different outputs.

⸻

## 2. Shared Concepts

2.1 Transition

pub struct Transition<S, E> {
    pub next_state: S,
    pub effects: Vec<E>,
}

Each machine has its own state, event, and effect enums.

Example:

pub fn transition(
    state: AttemptState,
    event: AttemptEvent,
) -> Transition<AttemptState, AttemptEffect>

⸻

2.2 Effects

Effects are commands emitted by machines.

Effects are not executed inside transitions.

Examples:

CallProvider
ExecuteTool
CreateWorktree
RunTests
CommitWorktree
MergeWorktree
AppendTelemetry
PersistState
DispatchNode
ReturnResult

This is the wall between pure logic and side effects.

⸻

2.3 Events

Events are facts returned to a machine after an effect completes.

Examples:

ProviderReturned
ProviderFailed
ToolCompleted
ToolFailed
TestsPassed
TestsFailed
NodeCompleted
NodeFailed
IntegrationSucceeded
IntegrationFailed

Effects leave the machine. Events come back in.

⸻

2.4 IDs

Important IDs should be typed, not raw strings.

pub struct RunId(Uuid);
pub struct NodeId(Uuid);
pub struct RequestId(Uuid);
pub struct ArtifactId(String);
pub struct ToolName(String);
pub struct ModelProfile(String);

This prevents mixing unrelated identifiers.

⸻

## 3. RunMachine

Purpose

The RunMachine owns the lifecycle of one full Forge run.

It answers:

Has the program been initialized?
Has config been loaded?
Has the workspace been prepared?
Has the scheduler started?
Did the run complete?
Did the run fail before scheduling?

It does not know how nodes execute.

It does not know how tools work.

It only owns the outer runtime journey.

⸻

States

pub enum RunState {
    NotStarted,
    LoadingConfig {
        config_path: PathBuf,
    },
    InitializingWorkspace {
        config: ForgeConfig,
    },
    BuildingRuntime {
        config: ForgeConfig,
        workspace: WorkspaceSpec,
    },
    Scheduling {
        run_id: RunId,
        scheduler_state: SchedulerState,
    },
    Completed {
        run_id: RunId,
        final_state: SchedulerState,
    },
    Failed {
        run_id: Option<RunId>,
        failure: RunFailure,
    },
}

⸻

Events

pub enum RunEvent {
    StartRequested {
        config_path: PathBuf,
    },
    ConfigLoaded {
        config: ForgeConfig,
    },
    ConfigLoadFailed {
        error: ConfigError,
    },
    WorkspaceInitialized {
        workspace: WorkspaceSpec,
    },
    WorkspaceInitializationFailed {
        error: WorkspaceError,
    },
    RuntimeBuilt {
        run_id: RunId,
        initial_scheduler_state: SchedulerState,
    },
    RuntimeBuildFailed {
        error: RuntimeBuildError,
    },
    SchedulerCompleted {
        final_state: SchedulerState,
    },
    SchedulerFailed {
        failure: SchedulerFailure,
    },
}

⸻

Effects

pub enum RunEffect {
    LoadConfig {
        path: PathBuf,
    },
    InitializeWorkspace {
        config: ForgeConfig,
    },
    BuildRuntime {
        config: ForgeConfig,
        workspace: WorkspaceSpec,
    },
    StartScheduler {
        run_id: RunId,
        initial_state: SchedulerState,
    },
    PersistFinalState {
        run_id: RunId,
        state: SchedulerState,
    },
    AppendTelemetry {
        event: TelemetryEvent,
    },
    ReturnRunResult {
        result: RunResult,
    },
}

⸻

Invariants

The RunMachine is the only machine that owns startup and shutdown.

The SchedulerMachine cannot load config.

The SchedulerMachine cannot initialize the workspace.

The RunMachine cannot inspect individual node internals.

⸻

## 4. SchedulerMachine

Purpose

The SchedulerMachine owns the DAG.

It answers:

Which nodes exist?
Which nodes are ready?
Which nodes are running?
Which nodes are terminal?
Can more nodes be dispatched?
Is the whole DAG completed?
Is the whole DAG failed?

It does not know how an agent works.

It does not run LLMs.

It does not merge work.

It only coordinates node execution.

⸻

States

pub enum SchedulerState {
    Empty,
    Ready {
        dag: Dag,
        limits: SchedulerLimits,
    },
    Dispatching {
        dag: Dag,
        ready_nodes: Vec<NodeId>,
        running_nodes: Vec<NodeId>,
        limits: SchedulerLimits,
    },
    AwaitingNodes {
        dag: Dag,
        running_nodes: Vec<NodeId>,
        limits: SchedulerLimits,
    },
    ApplyingNodeOutcome {
        dag: Dag,
        node_id: NodeId,
        outcome: NodeOutcome,
        limits: SchedulerLimits,
    },
    Completed {
        dag: Dag,
    },
    Failed {
        dag: Dag,
        failure: SchedulerFailure,
    },
}

Alternative: Ready, Dispatching, and AwaitingNodes may be collapsed into one Active state.

pub enum SchedulerState {
    Empty,
    Active { dag: Dag, running: HashSet<NodeId>, limits: SchedulerLimits },
    Completed { dag: Dag },
    Failed { dag: Dag, failure: SchedulerFailure },
}

The explicit version is easier to reason about at first.

⸻

Events

pub enum SchedulerEvent {
    Initialized {
        dag: Dag,
        limits: SchedulerLimits,
    },
    Tick,
    NodeDispatched {
        node_id: NodeId,
    },
    NodeCompleted {
        node_id: NodeId,
        outcome: NodeOutcome,
    },
    NodeFailed {
        node_id: NodeId,
        failure: NodeFailure,
    },
    ChildNodesCreated {
        parent_id: NodeId,
        children: Vec<NodeSpec>,
    },
    NodeIntegrated {
        node_id: NodeId,
    },
    NodeIntegrationFailed {
        node_id: NodeId,
        failure: IntegrationFailure,
    },
    BudgetExceeded {
        reason: BudgetFailure,
    },
}

⸻

Effects

pub enum SchedulerEffect {
    DispatchNode {
        node_id: NodeId,
        request: AgentRequest,
    },
    StartIntegration {
        node_id: NodeId,
        output: WorkOutput,
    },
    ExpandPlan {
        parent_id: NodeId,
        decision: PlanDecision,
    },
    PersistSchedulerState {
        state: SchedulerStateSnapshot,
    },
    AppendTelemetry {
        event: TelemetryEvent,
    },
    CompleteRun {
        dag: Dag,
    },
    FailRun {
        failure: SchedulerFailure,
    },
}

⸻

Invariants

A node can be dispatched only if all dependencies are integrated.

A node can be integrated only after successful terminal work output.

A planner node can create child nodes but cannot mutate artifact state.

A worker node can produce work but cannot directly mutate main artifact state.

Only IntegrationMachine can make work real.

The scheduler does not call providers or tools.

⸻

## 5. NodeMachine

Purpose

The NodeMachine owns the lifecycle of a single DAG node.

The scheduler sees many nodes.

The NodeMachine sees one node.

It answers:

Is this node pending?
Has it been dispatched?
Is it running?
Did it complete?
Did it request decomposition?
Did it fail?
Was its work integrated?
Should it be retried?

This prevents per-node lifecycle rules from leaking into the SchedulerMachine.

⸻

States

pub enum NodeState {
    Pending {
        request: AgentRequest,
        dependencies: Vec<NodeId>,
    },
    Dispatchable {
        request: AgentRequest,
    },
    Running {
        request: AgentRequest,
        attempt_state: AttemptState,
    },
    Completed {
        request: AgentRequest,
        response: AgentResponse,
    },
    DecompositionRequested {
        request: AgentRequest,
        decision: PlanDecision,
    },
    AwaitingIntegration {
        request: AgentRequest,
        output: WorkOutput,
    },
    Integrated {
        request: AgentRequest,
        integration: IntegrationResult,
    },
    RetryQueued {
        request: AgentRequest,
        reason: RetryReason,
        attempt: u32,
    },
    Failed {
        request: AgentRequest,
        failure: NodeFailure,
    },
}

⸻

Events

pub enum NodeEvent {
    DependenciesSatisfied,
    DispatchRequested,
    AttemptStarted {
        attempt_state: AttemptState,
    },
    AttemptCompleted {
        response: AgentResponse,
    },
    AttemptFailed {
        failure: AttemptFailure,
    },
    DecompositionAccepted {
        decision: PlanDecision,
    },
    WorkOutputAccepted {
        output: WorkOutput,
    },
    IntegrationStarted,
    IntegrationSucceeded {
        result: IntegrationResult,
    },
    IntegrationFailed {
        failure: IntegrationFailure,
    },
    RetryAllowed {
        reason: RetryReason,
    },
    RetryDenied {
        reason: RetryDeniedReason,
    },
}

⸻

Effects

pub enum NodeEffect {
    StartAttempt {
        request: AgentRequest,
    },
    RequestPlanExpansion {
        parent: NodeId,
        decision: PlanDecision,
    },
    RequestIntegration {
        node_id: NodeId,
        output: WorkOutput,
    },
    QueueRetry {
        node_id: NodeId,
        request: AgentRequest,
        reason: RetryReason,
    },
    MarkNodeCompleted {
        node_id: NodeId,
        response: AgentResponse,
    },
    MarkNodeFailed {
        node_id: NodeId,
        failure: NodeFailure,
    },
    AppendTelemetry {
        event: TelemetryEvent,
    },
}

⸻

Invariants

A node cannot be both running and integrated.

A planner node cannot enter AwaitingIntegration.

A worker node cannot enter DecompositionRequested unless decomposition is allowed for failed or oversized work.

A failed node is terminal unless the scheduler explicitly creates a retry node.

Node retries are represented as new states or new nodes, not by mutating old terminal history.

⸻

## 6. PlannerMachine

Purpose

The PlannerMachine owns planner-specific agent execution.

It answers:

Should this objective be executed directly?
Should it be split?
What child tasks exist?
What dependencies exist between child tasks?
Is the plan valid?

The planner does not write artifacts.

The planner produces decisions.

⸻

States

pub enum PlannerState {
    NotStarted {
        request: AgentRequest,
    },
    Attempting {
        request: AgentRequest,
        attempt_state: AttemptState,
    },
    ValidatingDecision {
        request: AgentRequest,
        decision: PlanDecision,
    },
    Completed {
        decision: PlanDecision,
    },
    Failed {
        failure: PlannerFailure,
    },
}

⸻

Events

pub enum PlannerEvent {
    Start,
    AttemptCompleted {
        response: AgentResponse,
    },
    AttemptFailed {
        failure: AttemptFailure,
    },
    DecisionParsed {
        decision: PlanDecision,
    },
    DecisionInvalid {
        failure: PlanValidationFailure,
    },
    DecisionValid,
}

⸻

Effects

pub enum PlannerEffect {
    StartAttempt {
        request: AgentRequest,
        output_kind: OutputKind,
    },
    ValidatePlanDecision {
        request: AgentRequest,
        decision: PlanDecision,
    },
    ReturnPlanDecision {
        decision: PlanDecision,
    },
    ReturnPlannerFailure {
        failure: PlannerFailure,
    },
    AppendTelemetry {
        event: TelemetryEvent,
    },
}

⸻

PlanDecision

pub enum PlanDecision {
    Work {
        task: WorkTaskIntent,
    },
    SplitGraph {
        nodes: Vec<PlanNodeSpec>,
    },
}

⸻

Invariants

Planner output is intent, not runtime routing.

The planner does not choose adapters if the framework can resolve them.

The planner does not choose language plugins unless that is explicitly part of the contract.

Plan expansion belongs to the scheduler side, not to the planner.

⸻

## 7. WorkerMachine

Purpose

The WorkerMachine owns worker-specific agent execution.

It answers:

What artifact is this worker assigned to?
What worktree is it allowed to modify?
What tools are available?
Was work actually produced?
Was output acceptable?

The worker does not merge to main.

The worker does not directly update scheduler state.

It produces a WorkOutput plus worktree changes.

⸻

States

pub enum WorkerState {
    NotStarted {
        request: AgentRequest,
    },
    PreparingWorktree {
        request: AgentRequest,
    },
    Attempting {
        request: AgentRequest,
        worktree: WorktreeSpec,
        attempt_state: AttemptState,
    },
    ValidatingWorkOutput {
        request: AgentRequest,
        worktree: WorktreeSpec,
        output: WorkOutput,
    },
    Completed {
        output: WorkOutput,
        worktree: WorktreeSpec,
    },
    Failed {
        failure: WorkerFailure,
    },
}

⸻

Events

pub enum WorkerEvent {
    Start,
    WorktreeCreated {
        worktree: WorktreeSpec,
    },
    WorktreeCreationFailed {
        failure: WorkspaceError,
    },
    AttemptCompleted {
        response: AgentResponse,
    },
    AttemptFailed {
        failure: AttemptFailure,
    },
    WorkOutputParsed {
        output: WorkOutput,
    },
    WorkOutputInvalid {
        failure: WorkOutputValidationFailure,
    },
    WorktreeHasChanges,
    WorktreeHasNoChanges,
}

⸻

Effects

pub enum WorkerEffect {
    CreateWorktree {
        artifact: ArtifactId,
        node_id: NodeId,
        base_sha: GitSha,
    },
    StartAttempt {
        request: AgentRequest,
        worktree: WorktreeSpec,
        tools: ToolSet,
    },
    InspectWorktree {
        worktree: WorktreeSpec,
    },
    ReturnWorkOutput {
        output: WorkOutput,
        worktree: WorktreeSpec,
    },
    ReturnWorkerFailure {
        failure: WorkerFailure,
    },
    AppendTelemetry {
        event: TelemetryEvent,
    },
}

⸻

Invariants

A worker may only write inside its assigned worktree.

The worker cannot merge.

The worker cannot modify main.

A completed worker must have either meaningful worktree changes or a valid already-done result.

Tool permissions are attached to the worktree and request, not globally available.

⸻

## 8. AttemptMachine

Purpose

The AttemptMachine owns the producer / critic / referee loop.

It answers:

Has the producer produced output?
Has the critic reviewed it?
Has the referee accepted it?
Should the producer revise?
Should the task decompose?
Are attempts exhausted?

This machine should not know whether the producer is a planner or worker.

It only knows:

request
producer output
critic finding
referee decision
revision history
attempt limits

⸻

States

pub enum AttemptState {
    NotStarted {
        request: AgentRequest,
        limits: AttemptLimits,
    },
    Producing {
        request: AgentRequest,
        attempt_no: u32,
        revisions: RevisionHistory,
    },
    Critiquing {
        request: AgentRequest,
        attempt_no: u32,
        output: ProducerOutput,
        revisions: RevisionHistory,
    },
    Refereeing {
        request: AgentRequest,
        attempt_no: u32,
        output: ProducerOutput,
        critic: CriticFinding,
        revisions: RevisionHistory,
    },
    RevisionQueued {
        request: AgentRequest,
        next_attempt_no: u32,
        revisions: RevisionHistory,
    },
    Accepted {
        output: ProducerOutput,
        review: ReviewRecord,
    },
    AlreadyDone {
        reason: AlreadyDoneReason,
    },
    DecomposeRequested {
        reason: DecomposeReason,
    },
    Rejected {
        failure: AttemptFailure,
    },
    Exhausted {
        failure: AttemptFailure,
        attempts_used: u32,
    },
}

⸻

Events

pub enum AttemptEvent {
    Start,
    ProducerCompleted {
        output: ProducerOutput,
    },
    ProducerFailed {
        failure: ProducerFailure,
    },
    ProducerReturnedEmptyOutput,
    CriticAccepted {
        finding: CriticFinding,
    },
    CriticRequestedRevision {
        finding: CriticFinding,
    },
    CriticRejected {
        finding: CriticFinding,
    },
    CriticRequestedDecomposition {
        finding: CriticFinding,
    },
    CriticFailed {
        failure: CriticFailure,
    },
    RefereeAccepted {
        decision: RefereeDecision,
    },
    RefereeRequestedRevision {
        decision: RefereeDecision,
    },
    RefereeRejected {
        decision: RefereeDecision,
    },
    RefereeMarkedAlreadyDone {
        decision: RefereeDecision,
    },
    RefereeRequestedDecomposition {
        decision: RefereeDecision,
    },
    RefereeFailed {
        failure: RefereeFailure,
    },
    MaxAttemptsReached,
}

⸻

Effects

pub enum AttemptEffect {
    CallProducer {
        request: AgentRequest,
        revisions: RevisionHistory,
    },
    CallCritic {
        request: AgentRequest,
        output: ProducerOutput,
    },
    CallReferee {
        request: AgentRequest,
        output: ProducerOutput,
        critic: CriticFinding,
    },
    AppendRevision {
        revision: RevisionRequest,
    },
    AppendTelemetry {
        event: TelemetryEvent,
    },
    ReturnAccepted {
        output: ProducerOutput,
        review: ReviewRecord,
    },
    ReturnAlreadyDone {
        reason: AlreadyDoneReason,
    },
    ReturnDecomposeRequested {
        reason: DecomposeReason,
    },
    ReturnFailed {
        failure: AttemptFailure,
    },
}

⸻

Invariants

The producer cannot revise without a structured revision request.

The critic does not decide final acceptance if a referee exists.

The referee must ground revision requests in the original contract.

An accepted attempt must carry the producer output.

An exhausted attempt must not carry accepted output.

Attempt counting is owned here and nowhere else.

⸻

## 9. TurnMachine

Purpose

The TurnMachine owns one provider call and response parsing.

It answers:

Was a provider call requested?
Did the provider return text?
Did the text parse as a tool call?
Did the text parse as a final response?
Was the protocol invalid?
Should repair be attempted?

This machine is lower-level than AttemptMachine.

It does not know critic/referee logic.

⸻

States

pub enum TurnState {
    Ready {
        messages: Vec<Message>,
        response_schema: ResponseSchema,
        tools: ToolSet,
    },
    AwaitingProvider {
        messages: Vec<Message>,
        response_schema: ResponseSchema,
        tools: ToolSet,
    },
    Parsing {
        raw: String,
        response_schema: ResponseSchema,
        tools: ToolSet,
    },
    ParsedTool {
        tool_turn: ToolTurn,
    },
    ParsedFinal {
        final_turn: FinalTurn,
    },
    Repairing {
        messages: Vec<Message>,
        parse_error: ParseError,
        repair_attempt: u32,
    },
    Failed {
        failure: TurnFailure,
    },
}

⸻

Events

pub enum TurnEvent {
    Start,
    ProviderReturned {
        raw: String,
    },
    ProviderFailed {
        failure: ProviderFailure,
    },
    ParsedToolTurn {
        tool_turn: ToolTurn,
    },
    ParsedFinalTurn {
        final_turn: FinalTurn,
    },
    ParseFailed {
        error: ParseError,
    },
    RepairPromptBuilt {
        messages: Vec<Message>,
    },
    RepairLimitReached,
}

⸻

Effects

pub enum TurnEffect {
    CallProvider {
        messages: Vec<Message>,
    },
    ParseProviderResponse {
        raw: String,
        tools: ToolSet,
        response_schema: ResponseSchema,
    },
    BuildRepairPrompt {
        raw: String,
        error: ParseError,
    },
    AppendTelemetry {
        event: TelemetryEvent,
    },
    ReturnParsedTool {
        tool_turn: ToolTurn,
    },
    ReturnParsedFinal {
        final_turn: FinalTurn,
    },
    ReturnTurnFailure {
        failure: TurnFailure,
    },
}

⸻

Invariants

The TurnMachine never executes tools.

The TurnMachine never validates business meaning.

It only owns provider response protocol.

A parsed turn is either tool or final, never both.

⸻

## 10. ToolLoopMachine

Purpose

The ToolLoopMachine owns an agent conversation where tools may be called before final output.

It answers:

Are tools still allowed?
Did the model call a tool?
Was the tool call valid?
Did a mutating tool succeed?
Did verification pass?
Did verification stabilize?
Should final-only pressure be applied?
Did the model return final output?
Did the loop exceed limits?

This is where tool protocol and final-response pressure belong.

⸻

States

pub enum ToolLoopState {
    NotStarted {
        request: AgentRequest,
        tools: ToolSet,
        limits: ToolLoopLimits,
    },
    Working {
        messages: Vec<Message>,
        telemetry: ToolLoopTelemetry,
        iteration: u32,
        tools: ToolSet,
    },
    ExecutingTool {
        messages: Vec<Message>,
        telemetry: ToolLoopTelemetry,
        iteration: u32,
        tool_turn: ToolTurn,
    },
    AwaitingFinalVerified {
        messages: Vec<Message>,
        telemetry: ToolLoopTelemetry,
        iteration: u32,
        reason: FinalOnlyReason,
    },
    AwaitingFinalStable {
        messages: Vec<Message>,
        telemetry: ToolLoopTelemetry,
        iteration: u32,
        reason: FinalOnlyReason,
    },
    Completed {
        response: AgentResponse,
        telemetry: ToolLoopTelemetry,
    },
    Failed {
        failure: ToolLoopFailure,
        telemetry: ToolLoopTelemetry,
    },
}

⸻

Events

pub enum ToolLoopEvent {
    Start,
    TurnParsedTool {
        tool_turn: ToolTurn,
    },
    TurnParsedFinal {
        final_turn: FinalTurn,
    },
    TurnFailed {
        failure: TurnFailure,
    },
    ToolCompleted {
        response: ToolResponse,
    },
    ToolFailed {
        failure: ToolFailure,
    },
    MutatingToolSucceeded {
        tool_name: ToolName,
    },
    VerificationPassed {
        tool_name: ToolName,
        result: VerificationResult,
    },
    VerificationFailed {
        tool_name: ToolName,
        fingerprint: VerificationFingerprint,
    },
    VerificationStabilized {
        fingerprint: VerificationFingerprint,
        count: u32,
    },
    EmptyFinalOutput,
    IterationLimitReached,
}

⸻

Effects

pub enum ToolLoopEffect {
    StartTurn {
        messages: Vec<Message>,
        tools: ToolSet,
        response_schema: ResponseSchema,
    },
    ExecuteTool {
        tool_turn: ToolTurn,
    },
    RecordToolResult {
        response: ToolResponse,
    },
    InjectMessage {
        message: Message,
    },
    ApplyFinalOnlyPressure {
        reason: FinalOnlyReason,
    },
    AppendTelemetry {
        event: TelemetryEvent,
    },
    ReturnCompleted {
        response: AgentResponse,
    },
    ReturnFailed {
        failure: ToolLoopFailure,
    },
}

⸻

Invariants

Tools are executable only in Working or ExecutingTool.

Tools are rejected in final-only states.

Verification state is updated only through verification events.

Mutation bookkeeping is updated only through mutation events.

Final-only pressure is represented as state, not as an incidental prompt flag.

The loop cannot both execute a tool and return a final response in the same transition.

⸻

## 11. IntegrationMachine

Purpose

The IntegrationMachine owns the boundary where worker output becomes artifact state.

It answers:

Does the worktree contain changes?
Do tests pass?
Can the work be committed?
Can the branch be merged?
Should rollback happen?
Should the worktree be removed?

The worker produces work.

The IntegrationMachine decides whether the work becomes real.

⸻

States

pub enum IntegrationState {
    NotStarted {
        node_id: NodeId,
        artifact: ArtifactId,
        worktree: WorktreeSpec,
        output: WorkOutput,
    },
    InspectingWorktree {
        worktree: WorktreeSpec,
    },
    RunningTests {
        worktree: WorktreeSpec,
    },
    Committing {
        worktree: WorktreeSpec,
        test_result: TestResult,
    },
    Merging {
        worktree: WorktreeSpec,
        commit: GitSha,
    },
    RollingBack {
        worktree: WorktreeSpec,
        reason: IntegrationFailure,
    },
    CleaningUp {
        worktree: WorktreeSpec,
        result: IntegrationTerminal,
    },
    Integrated {
        result: IntegrationResult,
    },
    Failed {
        failure: IntegrationFailure,
    },
}

⸻

Events

pub enum IntegrationEvent {
    Start,
    WorktreeInspected {
        changes: WorktreeChanges,
    },
    WorktreeInspectionFailed {
        failure: WorkspaceError,
    },
    NoChangesFound,
    TestsPassed {
        result: TestResult,
    },
    TestsFailed {
        result: TestResult,
    },
    CommitSucceeded {
        commit: GitSha,
    },
    CommitFailed {
        failure: GitFailure,
    },
    MergeSucceeded {
        merge_commit: GitSha,
    },
    MergeFailed {
        failure: GitFailure,
    },
    RollbackSucceeded,
    RollbackFailed {
        failure: GitFailure,
    },
    CleanupSucceeded,
    CleanupFailed {
        failure: WorkspaceError,
    },
}

⸻

Effects

pub enum IntegrationEffect {
    InspectWorktree {
        worktree: WorktreeSpec,
    },
    RunTests {
        worktree: WorktreeSpec,
        command: TestCommand,
    },
    CommitWorktree {
        worktree: WorktreeSpec,
        message: String,
    },
    MergeWorktree {
        artifact: ArtifactId,
        worktree: WorktreeSpec,
        commit: GitSha,
    },
    RollbackMerge {
        artifact: ArtifactId,
        reason: IntegrationFailure,
    },
    RemoveWorktree {
        worktree: WorktreeSpec,
    },
    AppendTelemetry {
        event: TelemetryEvent,
    },
    ReturnIntegrated {
        result: IntegrationResult,
    },
    ReturnIntegrationFailed {
        failure: IntegrationFailure,
    },
}

⸻

Invariants

Only this machine mutates artifact main state.

No worker can merge directly.

Tests run before merge unless explicitly disabled by artifact policy.

Rollback is explicit state, not exception cleanup.

Cleanup happens after both success and failure when possible.

⸻

## 12. ProviderMachine

Purpose

The ProviderMachine owns a single LLM provider request.

It may be optional; provider calls can also be simple effects handled by an effect handler.

But a ProviderMachine becomes useful if retries, rate limits, streaming, and provider-specific failures become important.

⸻

States

pub enum ProviderState {
    Ready {
        provider: ProviderId,
    },
    Sending {
        provider: ProviderId,
        request: ProviderRequest,
    },
    Retrying {
        provider: ProviderId,
        request: ProviderRequest,
        attempt: u32,
        reason: ProviderFailure,
    },
    Completed {
        response: ProviderResponse,
    },
    Failed {
        failure: ProviderFailure,
    },
}

⸻

Events

pub enum ProviderEvent {
    SendRequested {
        request: ProviderRequest,
    },
    HttpSucceeded {
        body: String,
    },
    HttpFailed {
        failure: HttpFailure,
    },
    RateLimited {
        retry_after: Option<Duration>,
    },
    ResponseParsed {
        response: ProviderResponse,
    },
    ResponseParseFailed {
        failure: ProviderParseFailure,
    },
    RetryDelayElapsed,
    RetryLimitReached,
}

⸻

Effects

pub enum ProviderEffect {
    SendHttpRequest {
        request: ProviderRequest,
    },
    Sleep {
        duration: Duration,
    },
    ParseProviderResponse {
        body: String,
    },
    ReturnProviderResponse {
        response: ProviderResponse,
    },
    ReturnProviderFailure {
        failure: ProviderFailure,
    },
    AppendTelemetry {
        event: TelemetryEvent,
    },
}

⸻

Invariants

Provider-specific HTTP details do not leak into higher machines.

Higher machines receive either provider text or provider failure.

Rate limit handling is explicit.

⸻

## 13. ToolMachine

Purpose

The ToolMachine owns a single tool invocation.

This may also be optional if tools are simple effect handlers.

But a ToolMachine becomes useful if tool execution needs permission checks, argument validation, sandboxing, timeout, and structured failure.

⸻

States

pub enum ToolState {
    Ready {
        registry: ToolRegistry,
        permissions: ToolPermissions,
    },
    Validating {
        turn: ToolTurn,
    },
    Executing {
        invocation: ToolInvocation,
    },
    Completed {
        response: ToolResponse,
    },
    Failed {
        failure: ToolFailure,
    },
}

⸻

Events

pub enum ToolEvent {
    ToolRequested {
        turn: ToolTurn,
    },
    ToolFound {
        tool: ToolSpec,
    },
    ToolNotFound {
        name: ToolName,
    },
    ArgumentsValid {
        invocation: ToolInvocation,
    },
    ArgumentsInvalid {
        failure: ToolArgumentFailure,
    },
    PermissionGranted,
    PermissionDenied {
        failure: ToolPermissionFailure,
    },
    ExecutionSucceeded {
        response: ToolResponse,
    },
    ExecutionFailed {
        failure: ToolFailure,
    },
    Timeout,
}

⸻

Effects

pub enum ToolEffect {
    LookupTool {
        name: ToolName,
    },
    ValidateArguments {
        tool: ToolSpec,
        args: JsonValue,
    },
    CheckPermission {
        tool: ToolSpec,
        permissions: ToolPermissions,
    },
    ExecuteTool {
        invocation: ToolInvocation,
    },
    ReturnToolResponse {
        response: ToolResponse,
    },
    ReturnToolFailure {
        failure: ToolFailure,
    },
    AppendTelemetry {
        event: TelemetryEvent,
    },
}

⸻

Invariants

Unknown tools fail before execution.

Invalid arguments fail before execution.

Permission checks happen before execution.

Tool execution is scoped to a root or worktree.

⸻

## 14. ConfigMachine

Purpose

The ConfigMachine owns loading, parsing, resolving, and validating configuration.

It may be a simple parser at first.

But it becomes useful if model profiles, artifacts, adapters, language plugins, and defaults become complex.

⸻

States

pub enum ConfigState {
    NotLoaded,
    ReadingFile {
        path: PathBuf,
    },
    ParsingYaml {
        raw: String,
    },
    ResolvingDefaults {
        partial: RawConfig,
    },
    Validating {
        config: ForgeConfig,
    },
    Loaded {
        config: ForgeConfig,
    },
    Failed {
        failure: ConfigError,
    },
}

⸻

Events

pub enum ConfigEvent {
    LoadRequested {
        path: PathBuf,
    },
    FileRead {
        raw: String,
    },
    FileReadFailed {
        failure: IoFailure,
    },
    YamlParsed {
        raw_config: RawConfig,
    },
    YamlParseFailed {
        failure: ConfigParseFailure,
    },
    DefaultsResolved {
        config: ForgeConfig,
    },
    ValidationSucceeded,
    ValidationFailed {
        failure: ConfigValidationFailure,
    },
}

⸻

Effects

pub enum ConfigEffect {
    ReadFile {
        path: PathBuf,
    },
    ParseYaml {
        raw: String,
    },
    ResolveDefaults {
        raw_config: RawConfig,
    },
    ValidateConfig {
        config: ForgeConfig,
    },
    ReturnConfig {
        config: ForgeConfig,
    },
    ReturnConfigFailure {
        failure: ConfigError,
    },
}

⸻

## 15. TelemetryMachine

Purpose

Telemetry may start as simple append-only effects.

A TelemetryMachine is useful only if telemetry has buffering, flushing, formatting, or failure isolation.

The key principle:

Telemetry must never change core behavior.

⸻

States

pub enum TelemetryState {
    Disabled,
    Ready {
        sink: TelemetrySinkSpec,
    },
    Buffering {
        sink: TelemetrySinkSpec,
        buffer: Vec<TelemetryEvent>,
    },
    Flushing {
        sink: TelemetrySinkSpec,
        buffer: Vec<TelemetryEvent>,
    },
    FailedButIgnored {
        failure: TelemetryFailure,
    },
}

⸻

Events

pub enum TelemetryEventInput {
    AppendRequested {
        event: TelemetryEvent,
    },
    FlushRequested,
    WriteSucceeded,
    WriteFailed {
        failure: TelemetryFailure,
    },
}

⸻

Effects

pub enum TelemetryEffect {
    WriteEvent {
        event: TelemetryEvent,
    },
    Flush {
        events: Vec<TelemetryEvent>,
    },
    IgnoreTelemetryFailure {
        failure: TelemetryFailure,
    },
}

⸻

## 16. Suggested Crate Structure

forge-rs/
Cargo.toml
crates/
  forge-core/
    src/
      lib.rs
      ids.rs
      transition.rs
      failure.rs
      run/
        state.rs
        event.rs
        effect.rs
        transition.rs
      scheduler/
        dag.rs
        state.rs
        event.rs
        effect.rs
        transition.rs
      node/
        state.rs
        event.rs
        effect.rs
        transition.rs
      planner/
        state.rs
        event.rs
        effect.rs
        transition.rs
      worker/
        state.rs
        event.rs
        effect.rs
        transition.rs
      attempt/
        state.rs
        event.rs
        effect.rs
        transition.rs
        revision.rs
      turn/
        state.rs
        event.rs
        effect.rs
        transition.rs
        parser.rs
      tool_loop/
        state.rs
        event.rs
        effect.rs
        transition.rs
      integration/
        state.rs
        event.rs
        effect.rs
        transition.rs
      model/
        request.rs
        response.rs
        contract.rs
        output.rs
  forge-cli/
    src/
      main.rs
  forge-effects/
    src/
      provider.rs
      tool.rs
      workspace.rs
      git.rs
      telemetry.rs
  forge-provider/
    src/
      openai.rs
      anthropic.rs
      ollama.rs

At first, this can be simpler:

crates/
  forge-core/
  forge-cli/

Add more crates only when boundaries become stable.

⸻

## 17. Proposed Implementation Order

Step 1: Core transition trait and common types

Create:

Transition<S, E>
MachineFailure
RunId
NodeId
RequestId

No LLMs.

No HTTP.

No git.

⸻

Step 2: AttemptMachine

Why first?

Because it captures producer / critic / referee logic without requiring tools, git, or scheduling.

Implement:

AttemptState
AttemptEvent
AttemptEffect
transition()

Test all review paths.

⸻

Step 3: ToolLoopMachine

Implement the tool/final protocol.

Do not yet execute real tools.

Use fake effects and fake events in tests.

⸻

Step 4: NodeMachine

Wrap one node lifecycle around attempts and integration.

⸻

Step 5: SchedulerMachine

Implement DAG progress and dispatch rules.

Use fake NodeMachine outcomes.

⸻

Step 6: IntegrationMachine

Implement worktree/test/merge lifecycle.

Initially with fake git effects.

Later bind to real git.

⸻

Step 7: RunMachine

Wire config, workspace, scheduler, and final result.

⸻

Step 8: Effect handlers

Only after machines are stable:

Provider effect handler
Tool effect handler
Workspace/git effect handler
Telemetry handler

⸻

## 18. What This Framework Forbids

The design should forbid:

direct state mutation
hidden retries
implicit final-only pressure
tool execution outside tool states
scheduler calling providers directly
worker merging directly
planner choosing runtime-only details
telemetry changing behavior
fallback branches that preserve old architecture

⸻

## 19. Main Design Questions Still Open

19.1 Should PlannerMachine and WorkerMachine exist separately?

Option A:

NodeMachine -> AttemptMachine

and output type determines behavior.

Option B:

NodeMachine -> PlannerMachine / WorkerMachine -> AttemptMachine

Option B is clearer but creates more types.

⸻

19.2 Should ProviderMachine exist?

At first, probably no.

Provider calls can be effects.

Add ProviderMachine later if provider retries/rate limits become complex.

⸻

19.3 Should ToolMachine exist?

At first, probably no.

Tool execution can be an effect handler.

Add ToolMachine later if permissions, timeout, sandboxing, and argument validation become complex.

⸻

19.4 Should IntegrationMachine be under NodeMachine or SchedulerMachine?

I propose:

Scheduler starts integration.
IntegrationMachine owns integration.
Scheduler receives integration result.

NodeMachine may enter AwaitingIntegration, but IntegrationMachine owns actual artifact mutation.

⸻

19.5 Should retries be same node or new node?

I lean:

Attempt retries happen inside AttemptMachine.
Post-merge/profile retries become new NodeMachine history.

This keeps the original node terminal history immutable.

⸻

## 20. North Star

The north star is:

No important Forge behavior exists outside a named machine transition.
No side effect happens except through an emitted effect.
No impossible state can be represented by the type system.

The program should feel like a set of Rust enums and exhaustive matches, not like a set of mutable services.

The compiler should become the first architecture reviewer.