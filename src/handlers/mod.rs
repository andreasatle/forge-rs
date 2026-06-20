//! Effect handlers.
//!
//! Handlers execute effects emitted by machines and turn external results back
//! into events. They are allowed to perform I/O.
//!
//! Examples:
//! - calling providers
//! - running tools
//! - running tests
//! - invoking git
//!
//! Handlers should not own machine state.
