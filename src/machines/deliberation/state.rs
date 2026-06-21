//! DeliberationMachine state types.
//!
//! The deliberation machine runs Producer → Critic → Referee before completing.
//! Final output is always the producer content; critic and referee do not replace it.

/// The input submitted to the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliberationRequest {
    /// The objective the pipeline should address.
    pub objective: String,
}

/// The final output produced by the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliberationOutput {
    /// The accepted producer content.
    pub content: String,
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
