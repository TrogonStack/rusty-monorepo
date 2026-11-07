use std::sync::Arc;

use crate::current_actor::CurrentActor;

#[derive(Clone, Debug, PartialEq)]
pub enum Delegation {
    Direct,
    Delegated(Arc<CurrentActor>),
}
