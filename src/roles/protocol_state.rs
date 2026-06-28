//! Protocol state tracking for a single role invocation.

pub(super) const MAX_PROTOCOL_RETRIES: usize = 2;
pub(super) const MAX_TOOL_STEPS: usize = 5;
pub(super) const MAX_READ_ONLY_TOOL_STEPS: usize = 2;

/// Encapsulates all mutable protocol counters and pressure flags for one role invocation.
pub(super) struct ProtocolState {
    protocol_attempt: usize,
    tool_steps: usize,
    read_only_tool_steps: usize,
    decision_pressure_active: bool,
    final_response_only: bool,
    seen_tool_fingerprints: std::collections::HashSet<String>,
    repeated_observation_coercion_active: bool,
    read_file_executed: bool,
    read_file_attempted: usize,
    is_work_producer: bool,
    is_read_only_reviewer: bool,
    requires_read_enforcement: bool,
}

impl ProtocolState {
    pub(super) fn new(
        is_work_producer: bool,
        is_read_only_reviewer: bool,
        requires_read_enforcement: bool,
    ) -> Self {
        Self {
            protocol_attempt: 1,
            tool_steps: 0,
            read_only_tool_steps: 0,
            decision_pressure_active: false,
            final_response_only: false,
            seen_tool_fingerprints: std::collections::HashSet::new(),
            repeated_observation_coercion_active: false,
            read_file_executed: false,
            read_file_attempted: 0,
            is_work_producer,
            is_read_only_reviewer,
            requires_read_enforcement,
        }
    }

    pub(super) fn allow_tool_call(&self) -> bool {
        !self.final_response_only
    }

    pub(super) fn allow_model_call(&self) -> bool {
        self.protocol_attempt <= MAX_PROTOCOL_RETRIES
    }

    pub(super) fn record_tool_call(&mut self) {
        self.tool_steps += 1;
    }

    /// Records a `read_file` attempt via the executor.
    ///
    /// Must only be called from inside the `Some(exec)` branch so that the count
    /// matches the original invariant: `read_file_attempted` only counts
    /// executor-backed calls.
    pub(super) fn record_read_file_attempt(&mut self) {
        self.read_file_attempted += 1;
    }

    pub(super) fn tool_loop_limit_reached(&self) -> bool {
        self.tool_steps > MAX_TOOL_STEPS
    }

    /// Records the result of a completed tool call and updates all pressure flags.
    ///
    /// `fingerprint` is `"{tool_name}\n{observation}"`. `mutation_recorded` is
    /// `true` when the tool produced `FileToolResponse::UpdateRecorded`.
    /// `read_file_succeeded` is `true` when a `read_file` returned `FileContents`.
    ///
    /// Also resets `protocol_attempt` to 1 since a successful tool step restarts
    /// the protocol-retry counter for the next model call.
    pub(super) fn record_tool_result(
        &mut self,
        fingerprint: String,
        mutation_recorded: bool,
        read_file_succeeded: bool,
    ) {
        if read_file_succeeded {
            self.read_file_executed = true;
        }
        if self.is_work_producer && mutation_recorded {
            self.enter_completion_pressure();
        }
        if self.is_read_only_reviewer {
            self.read_only_tool_steps += 1;
            if self.read_only_tool_steps >= MAX_READ_ONLY_TOOL_STEPS {
                self.enter_decision_pressure();
            }
        }
        if !self.seen_tool_fingerprints.insert(fingerprint) && !self.final_response_only {
            self.repeated_observation_coercion_active = true;
            self.final_response_only = true;
        }
        self.protocol_attempt = 1;
    }

    fn enter_completion_pressure(&mut self) {
        self.final_response_only = true;
    }

    fn enter_decision_pressure(&mut self) {
        self.final_response_only = true;
        self.decision_pressure_active = true;
    }

    pub(super) fn record_protocol_failure(&mut self) {
        self.protocol_attempt += 1;
    }

    pub(super) fn is_decision_pressure_active(&self) -> bool {
        self.decision_pressure_active
    }

    pub(super) fn is_repeated_observation_coercion_active(&self) -> bool {
        self.repeated_observation_coercion_active
    }

    pub(super) fn read_file_attempted(&self) -> usize {
        self.read_file_attempted
    }

    pub(super) fn current_attempt(&self) -> usize {
        self.protocol_attempt
    }

    pub(super) fn reviewer_accepted_without_reading(&self) -> bool {
        self.requires_read_enforcement && !self.read_file_executed
    }

    pub(super) fn reviewer_accept_must_fail_immediately(&self) -> bool {
        !self.allow_tool_call() || !self.allow_model_call()
    }
}

#[cfg(test)]
impl ProtocolState {
    pub(super) fn read_file_executed(&self) -> bool {
        self.read_file_executed
    }
}

#[cfg(test)]
mod protocol_state_tests {
    use super::*;

    fn work_producer() -> ProtocolState {
        ProtocolState::new(true, false, false)
    }

    fn work_reviewer() -> ProtocolState {
        ProtocolState::new(false, true, true)
    }

    fn plain_producer() -> ProtocolState {
        ProtocolState::new(false, false, false)
    }

    #[test]
    fn tool_budget_exhaustion() {
        let mut proto = work_producer();
        for _ in 0..=MAX_TOOL_STEPS {
            proto.record_tool_call();
        }
        assert!(
            proto.tool_loop_limit_reached(),
            "tool_loop_limit_reached must be true after MAX_TOOL_STEPS+1 calls"
        );
        assert!(
            !proto.tool_loop_limit_reached() || {
                let mut p2 = work_producer();
                for _ in 0..MAX_TOOL_STEPS {
                    p2.record_tool_call();
                }
                !p2.tool_loop_limit_reached()
            },
            "tool_loop_limit_reached must be false before the limit is crossed"
        );
    }

    #[test]
    fn tool_budget_not_reached_before_limit() {
        let mut proto = work_producer();
        for _ in 0..MAX_TOOL_STEPS {
            proto.record_tool_call();
        }
        assert!(
            !proto.tool_loop_limit_reached(),
            "tool_loop_limit_reached must be false at exactly MAX_TOOL_STEPS calls"
        );
    }

    #[test]
    fn protocol_retry_budget_exhaustion() {
        let mut proto = plain_producer();
        assert!(proto.allow_model_call(), "model call allowed initially");
        for _ in 0..MAX_PROTOCOL_RETRIES {
            proto.record_protocol_failure();
        }
        assert!(
            !proto.allow_model_call(),
            "allow_model_call must be false after MAX_PROTOCOL_RETRIES failures"
        );
    }

    #[test]
    fn protocol_retry_budget_not_exhausted_before_limit() {
        let mut proto = plain_producer();
        for _ in 0..MAX_PROTOCOL_RETRIES - 1 {
            proto.record_protocol_failure();
        }
        assert!(
            proto.allow_model_call(),
            "allow_model_call must be true before MAX_PROTOCOL_RETRIES failures"
        );
    }

    #[test]
    fn completion_pressure_fires_after_write() {
        let mut proto = work_producer();
        assert!(proto.allow_tool_call(), "tools allowed initially");
        proto.record_tool_result("write_file\n{ok}".to_string(), true, false);
        assert!(
            !proto.allow_tool_call(),
            "tools must be blocked after a successful mutation (completion pressure)"
        );
    }

    #[test]
    fn completion_pressure_does_not_fire_for_reviewer() {
        let mut proto = work_reviewer();
        proto.record_tool_result("write_file\n{ok}".to_string(), true, false);
        assert!(
            proto.allow_tool_call(),
            "reviewer must not enter completion pressure after one step; got blocked early"
        );
    }

    #[test]
    fn decision_pressure_fires_after_read_budget() {
        let mut proto = work_reviewer();
        for i in 0..MAX_READ_ONLY_TOOL_STEPS {
            proto.record_tool_result(format!("read_file\nobs{i}"), false, true);
        }
        assert!(
            !proto.allow_tool_call(),
            "tools must be blocked after MAX_READ_ONLY_TOOL_STEPS (decision pressure)"
        );
        assert!(
            proto.is_decision_pressure_active(),
            "decision_pressure_active must be set"
        );
    }

    #[test]
    fn decision_pressure_not_active_before_budget() {
        let mut proto = work_reviewer();
        for i in 0..MAX_READ_ONLY_TOOL_STEPS - 1 {
            proto.record_tool_result(format!("read_file\nobs{i}"), false, true);
        }
        assert!(
            proto.allow_tool_call(),
            "tools must still be allowed before the read budget is exhausted"
        );
        assert!(
            !proto.is_decision_pressure_active(),
            "decision pressure must not be active before the budget is exhausted"
        );
    }

    #[test]
    fn repeated_identical_observation_triggers_coercion() {
        let mut proto = plain_producer();
        proto.record_tool_result("list_files\n{files:[]}".to_string(), false, false);
        assert!(
            proto.allow_tool_call(),
            "tools allowed after first observation"
        );
        assert!(
            !proto.is_repeated_observation_coercion_active(),
            "coercion must not be active after unique observation"
        );
        proto.record_tool_result("list_files\n{files:[]}".to_string(), false, false);
        assert!(
            !proto.allow_tool_call(),
            "tools must be blocked after repeated observation"
        );
        assert!(
            proto.is_repeated_observation_coercion_active(),
            "repeated_observation_coercion_active must be set"
        );
    }

    #[test]
    fn distinct_observations_do_not_trigger_coercion() {
        let mut proto = plain_producer();
        proto.record_tool_result("read_file\ncontent-a".to_string(), false, true);
        proto.record_tool_result("read_file\ncontent-b".to_string(), false, true);
        assert!(
            !proto.is_repeated_observation_coercion_active(),
            "distinct observations must not trigger coercion"
        );
    }

    #[test]
    fn failed_read_file_does_not_satisfy_evidence_requirement() {
        let mut proto = work_reviewer();
        proto.record_read_file_attempt();
        proto.record_tool_result(
            "read_file\n{ok:false,error:not found}".to_string(),
            false,
            false,
        );
        assert!(
            !proto.read_file_executed(),
            "read_file_executed must remain false when read_file failed"
        );
        assert!(
            proto.reviewer_accepted_without_reading(),
            "reviewer_accepted_without_reading must be true when no successful read occurred"
        );
    }

    #[test]
    fn successful_read_file_satisfies_evidence_requirement() {
        let mut proto = work_reviewer();
        proto.record_read_file_attempt();
        proto.record_tool_result(
            "read_file\n{ok:true,content:hello}".to_string(),
            false,
            true,
        );
        assert!(
            proto.read_file_executed(),
            "read_file_executed must be true after a successful read"
        );
        assert!(
            !proto.reviewer_accepted_without_reading(),
            "reviewer_accepted_without_reading must be false after a successful read"
        );
    }

    #[test]
    fn reviewer_accept_must_fail_immediately_when_tools_blocked() {
        let mut proto = work_reviewer();
        for i in 0..MAX_READ_ONLY_TOOL_STEPS {
            proto.record_tool_result(format!("read_file\nobs{i}"), false, false);
        }
        assert!(
            proto.reviewer_accept_must_fail_immediately(),
            "must fail immediately when tools are blocked"
        );
    }

    #[test]
    fn reviewer_accept_must_fail_immediately_when_retries_exhausted() {
        let mut proto = work_reviewer();
        for _ in 0..MAX_PROTOCOL_RETRIES {
            proto.record_protocol_failure();
        }
        assert!(
            proto.reviewer_accept_must_fail_immediately(),
            "must fail immediately when protocol retry budget is exhausted"
        );
    }

    #[test]
    fn reviewer_accept_must_not_fail_immediately_when_healthy() {
        let proto = work_reviewer();
        assert!(
            !proto.reviewer_accept_must_fail_immediately(),
            "must not fail immediately on a fresh reviewer state"
        );
    }

    #[test]
    fn requires_read_enforcement_false_skips_reviewer_check() {
        let proto = ProtocolState::new(false, true, false);
        assert!(
            !proto.reviewer_accepted_without_reading(),
            "reviewer_accepted_without_reading must be false when enforcement is disabled"
        );
    }

    #[test]
    fn protocol_attempt_resets_after_tool_result() {
        let mut proto = plain_producer();
        proto.record_protocol_failure();
        proto.record_protocol_failure();
        assert_eq!(proto.current_attempt(), 3);
        proto.record_tool_result("list_files\n{files:[]}".to_string(), false, false);
        assert_eq!(
            proto.current_attempt(),
            1,
            "protocol_attempt must reset to 1 after a successful tool step"
        );
    }
}
