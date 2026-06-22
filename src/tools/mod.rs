//! LLM-facing file tools backed by artifact primitives.
//!
//! The LLM receives no direct filesystem access. Instead it issues typed
//! [`FileToolRequest`] values that are interpreted by [`FileToolExecutor`].
//! Read operations are satisfied immediately from an [`ArtifactView`]; write
//! operations are accumulated as pending [`ArtifactUpdate`] changes and
//! committed later by the integration layer.

mod file;

pub use file::{FileToolExecutor, FileToolRequest, FileToolResponse, parse_tool_request};
