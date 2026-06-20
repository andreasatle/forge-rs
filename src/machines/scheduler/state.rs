#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

#[derive(Clone, Debug, PartialEq)]
pub enum NodeStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub dependencies: Vec<NodeId>,
    pub status: NodeStatus,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunGraph {
    pub nodes: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerState {
    NotStarted {
        graph: RunGraph,
    },
    SelectingReady {
        graph: RunGraph,
    },
    Dispatching {
        graph: RunGraph,
        ready: Vec<NodeId>,
    },
    Waiting {
        graph: RunGraph,
        running: NodeId,
    },
    Complete {
        graph: RunGraph,
    },
    Failed {
        graph: RunGraph,
        reason: String,
    },
}
