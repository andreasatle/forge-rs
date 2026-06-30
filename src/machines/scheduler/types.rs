//! Payload vocabulary shared across scheduler events and effects.

/// The structured output of a work node that succeeded.
///
/// `WorkOutput` is minimal: a work node's only obligation is to report
/// what it did. The summary is stored on the node and is available in the
/// final `RunGraph` for audit and downstream context.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkOutput {
    /// A brief, human-readable description of what was accomplished.
    pub summary: String,
}
