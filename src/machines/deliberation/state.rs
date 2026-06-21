//! DeliberationMachine state types.
//!
//! The deliberation machine runs Producer → Critic → Referee before completing.
//! When the Referee rejects, the machine loops back to Producer with accumulated
//! feedback, up to `max_revisions` times. Final output is always the producer
//! content; critic and referee do not replace it.

/// Feedback recorded when the Referee rejects a producer draft.
#[derive(Clone, Debug, PartialEq)]
pub struct RevisionFeedback {
    /// The reason the Referee gave for rejecting the draft.
    pub reason: String,
}

/// The input submitted to the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliberationRequest {
    /// The objective the pipeline should address.
    pub objective: String,
    /// Maximum number of revision loops allowed.
    ///
    /// `0` means no revisions: the first Referee rejection fails immediately.
    pub max_revisions: usize,
}

/// The final output produced by the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliberationOutput {
    /// The accepted producer content.
    pub content: String,
}

/// Terminal result returned by `run_machine` for the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationTerminalOutput {
    /// The pipeline completed successfully.
    Complete(DeliberationOutput),
    /// The pipeline failed before producing accepted content.
    Failed {
        /// Human-readable description of why the pipeline failed.
        reason: String,
    },
}

/// The three roles that participate in the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationRole {
    /// Generates the initial content for the objective.
    Producer,
    /// Evaluates the producer's content and accepts or rejects it.
    Critic,
    /// Makes the final acceptance decision after the critic has weighed in.
    Referee,
}

/// The lifecycle of the deliberation machine.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationState {
    /// The machine has a request and is waiting for `Start`.
    Ready {
        /// The request that will be processed once the machine starts.
        request: DeliberationRequest,
    },

    /// A role has been dispatched; the machine is waiting for its result.
    Waiting {
        /// The original request, carried forward for later stages.
        request: DeliberationRequest,
        /// The role that was dispatched and has not yet responded.
        role: DeliberationRole,
        /// Content accepted by the Producer. `None` while waiting for Producer;
        /// `Some` while waiting for Critic or Referee.
        producer_content: Option<String>,
        /// Content accepted by the Critic. `None` until Critic completes;
        /// `Some` while waiting for Referee.
        critic_content: Option<String>,
        /// Number of revision loops completed so far.
        revision_count: usize,
        /// Feedback accumulated from each Referee rejection.
        feedback: Vec<RevisionFeedback>,
    },

    /// The pipeline finished successfully. Terminal state.
    Complete {
        /// The accepted output from the pipeline.
        output: DeliberationOutput,
    },

    /// The pipeline failed. Terminal state.
    Failed {
        /// Human-readable description of why the pipeline failed.
        reason: String,
    },
}
