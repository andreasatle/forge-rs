//! Stateless services.
//!
//! Services transform input data into output data without owning long-lived
//! machine state.
//!
//! Examples:
//! - prompt rendering
//! - config loading
//! - response parsing
//! - plan validation
//! - graph validation
//!
//! If a component has durable state and transitions over time, it belongs under
//! `machines/`, not `services/`.
