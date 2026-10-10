use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use crate::constants::{BARRIER_WAITERS_PER_PROCESS, BARRIER_WAITERS_PER_TOPIC, BARRIER_WAIT_MAX};
use crate::kv_key::EntryKey;
use crate::operation::UnixMillis;
use crate::position::StreamGeneration;
use crate::revision::EntryRevision;
use crate::topic::Topic;

use super::engine::{Slot, WatchCore};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReadBarrier {
    generation: StreamGeneration,
    entry: EntryKey,
    target: EntryRevision,
    expires: UnixMillis,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BarrierError {
    #[error("barrier for revision {target} expired before the view reached it")]
    Expired { target: EntryRevision },
    #[error("barrier names generation {expected} but the stream is at generation {current}")]
    GenerationChanged {
        expected: StreamGeneration,
        current: StreamGeneration,
    },
    #[error("view is rebuilding and is not ready")]
    NotReady,
    #[error("view passed revision {target} without evidence for the entry")]
    Unavailable { target: EntryRevision },
    #[error("too many barrier waiters")]
    Overloaded,
    #[error("barrier names topic {barrier} but the view serves {view}")]
    WrongTopic { barrier: Topic, view: Topic },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Satisfied,
    Pending,
    Unavailable,
}

impl ReadBarrier {
    pub fn new(generation: StreamGeneration, entry: EntryKey, target: EntryRevision, expires: UnixMillis) -> Self {
        Self {
            generation,
            entry,
            target,
            expires,
        }
    }

    pub fn generation(&self) -> StreamGeneration {
        self.generation
    }

    pub fn entry(&self) -> &EntryKey {
        &self.entry
    }

    pub fn target(&self) -> EntryRevision {
        self.target
    }

    pub fn expires(&self) -> UnixMillis {
        self.expires
    }

    pub fn expired(&self) -> BarrierError {
        BarrierError::Expired { target: self.target }
    }

    pub fn wait_budget(&self, now: UnixMillis) -> Result<Duration, BarrierError> {
        let left = self.expires.get().saturating_sub(now.get());
        if left == 0 {
            return Err(self.expired());
        }
        Ok(BARRIER_WAIT_MAX.min(Duration::from_millis(left)))
    }

    pub fn admit(&self, generation: StreamGeneration, topic: &Topic) -> Result<(), BarrierError> {
        if self.generation != generation {
            return Err(BarrierError::GenerationChanged {
                expected: self.generation,
                current: generation,
            });
        }
        if self.entry.topic() != topic {
            return Err(BarrierError::WrongTopic {
                barrier: self.entry.topic().clone(),
                view: topic.clone(),
            });
        }
        Ok(())
    }

    pub fn verdict(&self, core: &WatchCore) -> Verdict {
        let slot = Slot::new(self.entry.key().clone(), *self.entry.holder());
        let reached = |revision: Option<EntryRevision>| revision.is_some_and(|revision| revision >= self.target);
        if reached(core.entry_revision(&slot)) || reached(core.retirement_revision(&slot)) {
            Verdict::Satisfied
        } else if reached(core.revision()) {
            Verdict::Unavailable
        } else {
            Verdict::Pending
        }
    }

    pub fn unavailable(&self) -> BarrierError {
        BarrierError::Unavailable { target: self.target }
    }
}

#[derive(Debug, Default)]
struct Occupancy {
    topics: HashMap<Topic, usize>,
    total: usize,
}

#[derive(Debug, Clone, Default)]
pub struct BarrierWaiters {
    occupancy: Arc<Mutex<Occupancy>>,
}

#[derive(Debug)]
pub struct WaiterPermit {
    occupancy: Arc<Mutex<Occupancy>>,
    topic: Topic,
}

static PROCESS_WAITERS: LazyLock<BarrierWaiters> = LazyLock::new(BarrierWaiters::default);

impl BarrierWaiters {
    pub fn process() -> Self {
        PROCESS_WAITERS.clone()
    }

    pub fn acquire(&self, topic: &Topic) -> Result<WaiterPermit, BarrierError> {
        let mut occupancy = self.occupancy.lock().unwrap_or_else(PoisonError::into_inner);
        let waiting = occupancy.topics.get(topic).copied().unwrap_or_default();
        if waiting >= BARRIER_WAITERS_PER_TOPIC || occupancy.total >= BARRIER_WAITERS_PER_PROCESS {
            return Err(BarrierError::Overloaded);
        }
        occupancy.topics.insert(topic.clone(), waiting + 1);
        occupancy.total += 1;
        Ok(WaiterPermit {
            occupancy: Arc::clone(&self.occupancy),
            topic: topic.clone(),
        })
    }

    pub fn waiting(&self) -> usize {
        self.occupancy.lock().unwrap_or_else(PoisonError::into_inner).total
    }
}

impl Drop for WaiterPermit {
    fn drop(&mut self) {
        let mut occupancy = self.occupancy.lock().unwrap_or_else(PoisonError::into_inner);
        occupancy.total = occupancy.total.saturating_sub(1);
        if let Some(waiting) = occupancy.topics.get_mut(&self.topic) {
            *waiting = waiting.saturating_sub(1);
            if *waiting == 0 {
                occupancy.topics.remove(&self.topic);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn refuses_the_thirty_third_waiter_on_a_topic_and_the_hundred_twenty_ninth_overall() -> TestResult {
        let waiters = BarrierWaiters::default();
        let room: Topic = "room:a".parse()?;
        let mut held = Vec::new();
        for _ in 0..BARRIER_WAITERS_PER_TOPIC {
            held.push(waiters.acquire(&room)?);
        }
        assert_eq!(waiters.acquire(&room).err(), Some(BarrierError::Overloaded));
        for index in 0..BARRIER_WAITERS_PER_PROCESS - BARRIER_WAITERS_PER_TOPIC {
            held.push(waiters.acquire(&format!("room:{index}").parse()?)?);
        }
        assert_eq!(
            waiters.acquire(&"room:last".parse()?).err(),
            Some(BarrierError::Overloaded)
        );
        drop(held);
        assert_eq!(waiters.waiting(), 0);
        assert!(waiters.acquire(&room).is_ok());
        Ok(())
    }

    #[test]
    fn wait_budget_is_capped_and_expires() -> TestResult {
        let entry = EntryKey::new("room:a".parse()?, "ana".parse()?, "q3V9hX0bS2mWf1ZkR8aT1A".parse()?);
        let target = EntryRevision::from(5);
        let barrier = ReadBarrier::new(StreamGeneration::from([1; 16]), entry, target, UnixMillis::from(10_000));
        assert_eq!(barrier.wait_budget(UnixMillis::from(1_000)), Ok(BARRIER_WAIT_MAX));
        assert_eq!(
            barrier.wait_budget(UnixMillis::from(9_800)),
            Ok(Duration::from_millis(200))
        );
        assert_eq!(
            barrier.wait_budget(UnixMillis::from(10_000)),
            Err(BarrierError::Expired { target })
        );
        Ok(())
    }
}
