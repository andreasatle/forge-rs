use super::state::NodeId;

#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerEvent {
    Start,
    NodeCompleted {
        node_id: NodeId,
    },
    NodeFailed {
        node_id: NodeId,
        reason: String,
    },
}
