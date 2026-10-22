use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use tokio::sync::broadcast;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, MissedTickBehavior};

use crate::config::HeartbeatInterval;
use crate::constants::{
    HEARTBEAT_DEGRADED_AFTER_ROUNDS, HEARTBEAT_JITTER_DIVISOR, HEARTBEAT_PUBLISH_TIMEOUT, PRESENCE_EVENT_BUFFER,
};
use crate::holder::HolderId;
use crate::kv_key::KvKey;
use crate::phx_ref::StoredRef;
use crate::position::LifetimeId;
use crate::shard::ShardCount;
use crate::store::KvWriter;
use crate::tracker::{HolderBusy, Tracker, WeakTracker};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresenceEvent {
    Retracked {
        kv_key: KvKey,
        old_lifetime: LifetimeId,
        old_ref: StoredRef,
        new_lifetime: LifetimeId,
        new_ref: StoredRef,
    },
    Degraded {
        since: SystemTime,
    },
    Restored,
    Shutdown {
        holder: HolderId,
    },
}

#[derive(Clone)]
pub(crate) struct Heartbeat {
    shared: Arc<Shared>,
}

struct Shared {
    writer: KvWriter,
    interval: HeartbeatInterval,
    events: broadcast::Sender<PresenceEvent>,
    trackers: Mutex<HashMap<HolderId, WeakTracker>>,
    lifecycle: Mutex<Lifecycle>,
}

enum Lifecycle {
    Idle,
    Running(JoinHandle<()>),
    Stopped,
}

impl Heartbeat {
    pub(crate) fn new(writer: KvWriter, interval: HeartbeatInterval) -> Self {
        let (events, _) = broadcast::channel(PRESENCE_EVENT_BUFFER);
        Self {
            shared: Arc::new(Shared {
                writer,
                interval,
                events,
                trackers: Mutex::new(HashMap::new()),
                lifecycle: Mutex::new(Lifecycle::Idle),
            }),
        }
    }

    pub(crate) fn writer(&self) -> &KvWriter {
        &self.shared.writer
    }

    pub(crate) fn interval(&self) -> Duration {
        self.shared.interval.get()
    }

    pub(crate) fn tracker_for(&self, holder: HolderId, shards: ShardCount) -> Result<Tracker, HolderBusy> {
        let mut trackers = lock(&self.shared.trackers);
        if let Some(weak) = trackers.get(&holder) {
            return weak.upgrade().ok_or(HolderBusy::new(holder));
        }
        let tracker = Tracker::spawn(holder, shards, self.clone());
        trackers.insert(holder, tracker.downgrade());
        Ok(tracker)
    }

    pub(crate) fn release(&self, holder: &HolderId) {
        let mut trackers = lock(&self.shared.trackers);
        if trackers.get(holder).is_some_and(|weak| weak.upgrade().is_none()) {
            trackers.remove(holder);
        }
    }

    pub(crate) fn emit(&self, event: PresenceEvent) {
        let _ = self.shared.events.send(event);
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<PresenceEvent> {
        self.shared.events.subscribe()
    }

    pub(crate) fn ensure_started(&self) {
        let mut lifecycle = lock(&self.shared.lifecycle);
        if matches!(*lifecycle, Lifecycle::Idle) {
            *lifecycle = Lifecycle::Running(tokio::spawn(self.clone().run()));
        }
    }

    pub(crate) fn stop(&self) {
        if let Lifecycle::Running(task) = std::mem::replace(&mut *lock(&self.shared.lifecycle), Lifecycle::Stopped) {
            task.abort();
        }
    }

    pub(crate) fn trackers(&self) -> Vec<Tracker> {
        lock(&self.shared.trackers)
            .values()
            .filter_map(WeakTracker::upgrade)
            .collect()
    }

    async fn run(self) {
        let period = self.shared.interval.get();
        let mut ticker = tokio::time::interval_at(Instant::now() + period, period);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut health = Health::default();
        loop {
            ticker.tick().await;
            tokio::time::sleep(jitter(period)).await;
            let failed = self.round(period.min(HEARTBEAT_PUBLISH_TIMEOUT)).await;
            if let Some(event) = health.observe(failed) {
                self.emit(event);
            }
        }
    }

    async fn round(&self, publish_timeout: Duration) -> bool {
        let mut beats = JoinSet::new();
        for tracker in self.trackers() {
            beats.spawn(async move { tracker.beat(publish_timeout).await });
        }
        let mut failed = false;
        while let Some(joined) = beats.join_next().await {
            match joined {
                Ok(Ok(report)) => failed |= report.failed,
                Ok(Err(_closed)) => {}
                Err(_) => failed = true,
            }
        }
        failed
    }
}

#[derive(Default)]
struct Health {
    failed_rounds: u32,
    first_failure: Option<SystemTime>,
    degraded: bool,
}

impl Health {
    fn observe(&mut self, failed: bool) -> Option<PresenceEvent> {
        if !failed {
            self.failed_rounds = 0;
            self.first_failure = None;
            return std::mem::take(&mut self.degraded).then_some(PresenceEvent::Restored);
        }
        self.failed_rounds += 1;
        let since = *self.first_failure.get_or_insert_with(SystemTime::now);
        if self.degraded || self.failed_rounds < HEARTBEAT_DEGRADED_AFTER_ROUNDS {
            return None;
        }
        self.degraded = true;
        Some(PresenceEvent::Degraded { since })
    }
}

pub(crate) fn jitter(period: Duration) -> Duration {
    let span = u64::try_from((period / HEARTBEAT_JITTER_DIVISOR).as_nanos()).unwrap_or(u64::MAX);
    match getrandom::u64() {
        Ok(random) if span > 0 => Duration::from_nanos(random % span),
        _ => Duration::ZERO,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_degrades_after_three_failed_rounds_and_restores_once() {
        let mut health = Health::default();
        assert_eq!(health.observe(false), None);
        assert_eq!(health.observe(true), None);
        assert_eq!(health.observe(true), None);
        assert!(matches!(health.observe(true), Some(PresenceEvent::Degraded { .. })));
        assert_eq!(health.observe(true), None);
        assert_eq!(health.observe(false), Some(PresenceEvent::Restored));
        assert_eq!(health.observe(false), None);
    }

    #[test]
    fn health_resets_the_streak_on_success() {
        let mut health = Health::default();
        health.observe(true);
        health.observe(true);
        assert_eq!(health.observe(false), None);
        assert_eq!(health.observe(true), None);
        assert_eq!(health.observe(true), None);
        assert!(matches!(health.observe(true), Some(PresenceEvent::Degraded { .. })));
    }

    #[test]
    fn jitter_stays_within_a_tenth_of_the_period() {
        let period = Duration::from_millis(400);
        for _ in 0..1000 {
            assert!(jitter(period) < period / HEARTBEAT_JITTER_DIVISOR);
        }
    }
}
