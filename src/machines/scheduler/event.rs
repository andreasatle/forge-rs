use super::state::{NodeId, NodeKind};

#[derive(Clone, Debug, PartialEq)]
pub struct PlanOutput {
    pub children: Vec<NodeRequest>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkOutput {
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeRequest {
    pub kind: NodeKind,
    pub objective: String,
    pub dependencies: Vec<NodeId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeFailure {
    pub reason: String,
    pub recovery: RecoveryAction,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecoveryAction {
    Retry { message: String },
    Split { message: String },
    ElevateModel { message: String },
    Terminal { message: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeOutcome {
    PlanAccepted(PlanOutput),
    WorkAccepted(WorkOutput),
    Failed(NodeFailure),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerEvent {
    Start,
    NodeReturned {
        node_id: NodeId,
        outcome: NodeOutcome,
    },
}
