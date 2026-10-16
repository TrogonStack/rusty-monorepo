use serde::{Deserialize, Serialize};
use trogon_presence::OwnerId;

use crate::subjects::drain_subject;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrainRequest {
    instance: OwnerId,
}

impl DrainRequest {
    pub fn new(instance: OwnerId) -> Self {
        Self { instance }
    }

    pub fn instance(&self) -> OwnerId {
        self.instance
    }

    pub fn subject(&self) -> String {
        drain_subject(self.instance)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrainReply {
    pub instance: OwnerId,
    pub released_views: usize,
    pub released_writers: usize,
    pub already_draining: bool,
}
