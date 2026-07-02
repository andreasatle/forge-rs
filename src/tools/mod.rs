//! LLM-facing file tools backed by artifact primitives.
//!
//! The LLM receives no direct filesystem access. Instead it issues typed
//! [`FileToolRequest`] values that are interpreted by [`FileToolExecutor`].
//! Read operations are satisfied immediately from an artifact view or a
//! WorkAttempt workspace. In artifact Work, writes mutate the workspace
//! directly.

mod file;

pub use file::{
    FileToolExecutor, FileToolPolicy, FileToolRequest, FileToolResponse, looks_like_tool_request,
    parse_tool_request,
};
