//! Telemetry event types.

/// A sourced telemetry observation.
pub struct TelemetryRecord {
    /// Component or conceptual machine that emitted the event.
    pub source: String,
    /// Optional sub-component within the source (e.g. `Producer`, `Critic`).
    pub subsource: Option<String>,
    /// The event emitted by the source.
    pub event: TelemetryEvent,
}

impl TelemetryRecord {
    /// Construct a telemetry record from a source and event.
    pub fn new(source: impl Into<String>, event: TelemetryEvent) -> Self {
        Self {
            source: source.into(),
            subsource: None,
            event,
        }
    }

    /// Construct a telemetry record with an explicit subsource.
    pub fn new_with_subsource(
        source: impl Into<String>,
        subsource: impl Into<String>,
        event: TelemetryEvent,
    ) -> Self {
        Self {
            source: source.into(),
            subsource: Some(subsource.into()),
            event,
        }
    }

    /// Render the source, optional subsource, and event payload as a plain-text file body.
    pub fn file_content(&self) -> String {
        match &self.subsource {
            Some(sub) => format!(
                "source: {}\nsubsource: {}\n{}",
                self.source,
                sub,
                self.event.file_content()
            ),
            None => format!("source: {}\n{}", self.source, self.event.file_content()),
        }
    }
}

/// A single observation recorded during a machine run.
///
/// All fields are plain strings. Machine-specific state, event, and effect
/// values are formatted with `{:#?}` before being stored here so that the
/// sink receives a self-contained, human-readable record with no type
/// dependencies.
pub enum TelemetryEvent {
    /// A machine started its run loop.
    MachineStarted {
        /// Short type name of the machine (e.g. `SchedulerHandler`).
        machine: String,
    },
    /// A state was observed before a transition was applied.
    StateEntered {
        /// Short type name of the machine.
        machine: String,
        /// Pretty-printed debug representation of the state.
        state: String,
    },
    /// An event was observed before a transition was applied.
    EventReceived {
        /// Short type name of the machine.
        machine: String,
        /// Pretty-printed debug representation of the event.
        event: String,
    },
    /// An effect was emitted by a transition.
    EffectEmitted {
        /// Short type name of the machine.
        machine: String,
        /// Pretty-printed debug representation of the effect.
        effect: String,
    },
    /// A role prompt was rendered and is about to be sent to a provider.
    RolePromptRendered {
        /// The complete rendered prompt.
        prompt: String,
        /// One-based protocol attempt number.
        attempt_count: usize,
    },
    /// A provider returned raw content to the role layer.
    ProviderResponseReceived {
        /// The provider's unparsed response.
        raw_response: String,
        /// One-based protocol attempt number.
        attempt_count: usize,
    },
    /// The role layer parsed a provider response successfully.
    ParseSucceeded {
        /// One-based protocol attempt number.
        attempt_count: usize,
    },
    /// The role layer could not parse or validate a provider response.
    ParseFailed {
        /// The provider's unparsed response.
        raw_response: String,
        /// The parse or schema validation error.
        parse_error: String,
        /// One-based protocol attempt number.
        attempt_count: usize,
    },
    /// The role layer is retrying after a protocol failure.
    ProtocolRetry {
        /// The parse error that caused the retry.
        parse_error: String,
        /// The next one-based protocol attempt number.
        attempt_count: usize,
    },
    /// A provider call completed successfully.
    ProviderCallSucceeded {
        /// Identifier of the provider that was called.
        provider: String,
    },
    /// A provider call failed.
    ProviderCallFailed {
        /// Identifier of the provider that was called.
        provider: String,
        /// Human-readable failure reason.
        reason: String,
    },
    /// An artifact commit was created.
    ArtifactCommitCreated {
        /// The SHA of the newly created commit.
        commit_sha: String,
    },
    /// A component encountered a non-recoverable failure.
    Failure {
        /// Name of the component that failed.
        component: String,
        /// Human-readable failure reason.
        reason: String,
    },
    /// Workspace validation started before artifact integration.
    ValidationStarted,
    /// Workspace validation passed; integration may proceed.
    ValidationPassed {
        /// Human-readable validation summary.
        summary: String,
    },
    /// Workspace validation failed; artifact commit was blocked.
    ValidationFailed {
        /// Human-readable validation summary.
        summary: String,
    },
    /// A file tool was requested by a role during execution.
    ToolRequested {
        /// The tool name (e.g. `read_file`, `write_file`).
        tool: String,
    },
    /// A file tool was executed and its response was recorded.
    ToolReturned {
        /// The tool name.
        tool: String,
        /// Rendered observation returned to the model.
        result: String,
    },
    /// The tool loop reached its hard step limit without a final role result.
    ToolLoopLimitReached,
    /// A plan node's provider output was parsed as a structured task graph.
    PlannerOutputParsed {
        /// Number of tasks in the parsed graph.
        task_count: usize,
        /// Total number of dependency edges across all tasks.
        dependency_count: usize,
    },
    /// A plan node's provider output could not be parsed as a structured task
    /// graph and fell back to a single work node containing the raw content.
    PlannerOutputFallback,
    /// A plan node's provider output parsed successfully but failed structural
    /// validation. The run is halted.
    PlannerOutputValidationFailed {
        /// Human-readable description of the first validation error found.
        reason: String,
    },
    /// A deliberation failure was classified and a recovery action was chosen.
    ///
    /// Emitted once per failure so that audit logs show why the scheduler
    /// retried, escalated, or halted without requiring readers to understand
    /// classification rules in source code.
    FailureClassified {
        /// The raw failure reason from deliberation.
        reason: String,
        /// The recovery action chosen: `"Retry"`, `"ElevateModel"`, or `"Terminal"`.
        recovery: String,
    },
    /// A scheduler checkpoint was written to disk after a progress event.
    CheckpointSaved {
        /// Total node count in the graph at save time.
        node_count: usize,
        /// Number of nodes in `Completed` status at save time.
        completed_count: usize,
    },
    /// A scheduler checkpoint was loaded from disk at resume time.
    CheckpointLoaded {
        /// Total node count in the restored graph.
        node_count: usize,
        /// Number of nodes already `Completed` in the restored graph.
        completed_count: usize,
    },
    /// A plan node was resolved deterministically without calling the LLM planner.
    ///
    /// Emitted when the objective names exactly one source file and the fast
    /// path produces a minimal PlanOutput directly.
    FastPlanUsed {
        /// Number of work tasks in the generated plan.
        task_count: usize,
    },
}

impl TelemetryEvent {
    /// Returns a short kebab-case slug that identifies the variant.
    ///
    /// Used to build the filename component in [`FileTelemetry`](crate::telemetry::FileTelemetry).
    pub fn kind_slug(&self) -> &'static str {
        match self {
            TelemetryEvent::MachineStarted { .. } => "machine-started",
            TelemetryEvent::StateEntered { .. } => "state-entered",
            TelemetryEvent::EventReceived { .. } => "event-received",
            TelemetryEvent::EffectEmitted { .. } => "effect-emitted",
            TelemetryEvent::RolePromptRendered { .. } => "role-prompt-rendered",
            TelemetryEvent::ProviderResponseReceived { .. } => "provider-response-received",
            TelemetryEvent::ParseSucceeded { .. } => "parse-succeeded",
            TelemetryEvent::ParseFailed { .. } => "parse-failed",
            TelemetryEvent::ProtocolRetry { .. } => "protocol-retry",
            TelemetryEvent::ProviderCallSucceeded { .. } => "provider-call-succeeded",
            TelemetryEvent::ProviderCallFailed { .. } => "provider-call-failed",
            TelemetryEvent::ArtifactCommitCreated { .. } => "artifact-commit-created",
            TelemetryEvent::Failure { .. } => "failure",
            TelemetryEvent::ValidationStarted => "validation-started",
            TelemetryEvent::ValidationPassed { .. } => "validation-passed",
            TelemetryEvent::ValidationFailed { .. } => "validation-failed",
            TelemetryEvent::ToolRequested { .. } => "tool-requested",
            TelemetryEvent::ToolReturned { .. } => "tool-returned",
            TelemetryEvent::ToolLoopLimitReached => "tool-loop-limit-reached",
            TelemetryEvent::PlannerOutputParsed { .. } => "planner-output-parsed",
            TelemetryEvent::PlannerOutputFallback => "planner-output-fallback",
            TelemetryEvent::PlannerOutputValidationFailed { .. } => {
                "planner-output-validation-failed"
            }
            TelemetryEvent::FailureClassified { .. } => "failure-classified",
            TelemetryEvent::CheckpointSaved { .. } => "checkpoint-saved",
            TelemetryEvent::CheckpointLoaded { .. } => "checkpoint-loaded",
            TelemetryEvent::FastPlanUsed { .. } => "fast-plan-used",
        }
    }

    /// Renders the event as a plain-text file body.
    ///
    /// Format mirrors the Forge-Py inspection style: one `key: value` line per
    /// field, with multi-line values (state, event, effect) printed on the line
    /// following their key.
    pub fn file_content(&self) -> String {
        match self {
            TelemetryEvent::MachineStarted { machine } => {
                format!("kind: MachineStarted\nmachine: {machine}\n")
            }
            TelemetryEvent::StateEntered { machine, state } => {
                format!("kind: StateEntered\nmachine: {machine}\nstate:\n{state}\n")
            }
            TelemetryEvent::EventReceived { machine, event } => {
                format!("kind: EventReceived\nmachine: {machine}\nevent:\n{event}\n")
            }
            TelemetryEvent::EffectEmitted { machine, effect } => {
                format!("kind: EffectEmitted\nmachine: {machine}\neffect:\n{effect}\n")
            }
            TelemetryEvent::RolePromptRendered {
                prompt,
                attempt_count,
            } => format!(
                "kind: RolePromptRendered\nattempt_count: {attempt_count}\nprompt:\n{prompt}\n"
            ),
            TelemetryEvent::ProviderResponseReceived {
                raw_response,
                attempt_count,
            } => format!(
                "kind: ProviderResponseReceived\nattempt_count: {attempt_count}\nraw_response:\n{raw_response}\n"
            ),
            TelemetryEvent::ParseSucceeded { attempt_count } => {
                format!("kind: ParseSucceeded\nattempt_count: {attempt_count}\n")
            }
            TelemetryEvent::ParseFailed {
                raw_response,
                parse_error,
                attempt_count,
            } => format!(
                "kind: ParseFailed\nattempt_count: {attempt_count}\nparse_error: {parse_error}\nraw_response:\n{raw_response}\n"
            ),
            TelemetryEvent::ProtocolRetry {
                parse_error,
                attempt_count,
            } => format!(
                "kind: ProtocolRetry\nattempt_count: {attempt_count}\nparse_error: {parse_error}\n"
            ),
            TelemetryEvent::ProviderCallSucceeded { provider } => {
                format!("kind: ProviderCallSucceeded\nprovider: {provider}\n")
            }
            TelemetryEvent::ProviderCallFailed { provider, reason } => {
                format!("kind: ProviderCallFailed\nprovider: {provider}\nreason: {reason}\n")
            }
            TelemetryEvent::ArtifactCommitCreated { commit_sha } => {
                format!("kind: ArtifactCommitCreated\ncommit_sha: {commit_sha}\n")
            }
            TelemetryEvent::Failure { component, reason } => {
                format!("kind: Failure\ncomponent: {component}\nreason: {reason}\n")
            }
            TelemetryEvent::ValidationStarted => "kind: ValidationStarted\n".to_string(),
            TelemetryEvent::ValidationPassed { summary } => {
                format!("kind: ValidationPassed\nsummary: {summary}\n")
            }
            TelemetryEvent::ValidationFailed { summary } => {
                format!("kind: ValidationFailed\nsummary: {summary}\n")
            }
            TelemetryEvent::ToolRequested { tool } => {
                format!("kind: ToolRequested\ntool: {tool}\n")
            }
            TelemetryEvent::ToolReturned { tool, result } => {
                format!("kind: ToolReturned\ntool: {tool}\nresult: {result}\n")
            }
            TelemetryEvent::ToolLoopLimitReached => "kind: ToolLoopLimitReached\n".to_string(),
            TelemetryEvent::PlannerOutputParsed {
                task_count,
                dependency_count,
            } => format!(
                "kind: PlannerOutputParsed\ntask_count: {task_count}\ndependency_count: {dependency_count}\n"
            ),
            TelemetryEvent::PlannerOutputFallback => "kind: PlannerOutputFallback\n".to_string(),
            TelemetryEvent::PlannerOutputValidationFailed { reason } => {
                format!("kind: PlannerOutputValidationFailed\nreason: {reason}\n")
            }
            TelemetryEvent::FailureClassified { reason, recovery } => {
                format!("kind: FailureClassified\nreason: {reason}\nrecovery: {recovery}\n")
            }
            TelemetryEvent::CheckpointSaved {
                node_count,
                completed_count,
            } => format!(
                "kind: CheckpointSaved\nnode_count: {node_count}\ncompleted_count: {completed_count}\n"
            ),
            TelemetryEvent::CheckpointLoaded {
                node_count,
                completed_count,
            } => format!(
                "kind: CheckpointLoaded\nnode_count: {node_count}\ncompleted_count: {completed_count}\n"
            ),
            TelemetryEvent::FastPlanUsed { task_count } => {
                format!("kind: FastPlanUsed\ntask_count: {task_count}\n")
            }
        }
    }
}
