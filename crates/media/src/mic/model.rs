//! The mic state the service publishes.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mic {
    pub users: Vec<MicUser>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MicUser {
    pub pid: Option<u32>,
    pub label: Option<String>,
}
