#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Plan,
    Work,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModelTier {
    Cheap,
    Strong,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub objective: String,
    pub dependencies: Vec<NodeId>,
    pub status: NodeStatus,
    pub attempt: u32,
    pub model_tier: ModelTier,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunGraph {
    pub nodes: Vec<Node>,
    /// Monotonic counter used to mint fresh NodeIds without global state.
    /// Increment each time a node is inserted.
    pub next_id: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerState {
    NotStarted { graph: RunGraph },
    SelectingReady { graph: RunGraph },
    Dispatching { graph: RunGraph, ready: Vec<NodeId> },
    Waiting { graph: RunGraph, running: NodeId },
    Complete { graph: RunGraph },
    Failed { graph: RunGraph, reason: String },
}
