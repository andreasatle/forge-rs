#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProducerResponse {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CriticResponse {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RefereeResponse {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaskResult {
    pub task: Task,
    pub producer_response: ProducerResponse,
    pub critic_response: CriticResponse,
    pub referee_response: RefereeResponse,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DemoState {
    NotStarted { task: Task },

    PostProducer {
        task: Task,
        producer_response: ProducerResponse,
    },

    PostCritic {
        task: Task,
        producer_response: ProducerResponse,
        critic_response: CriticResponse,
    },

    PostReferee {
        result: TaskResult,
    },
}
