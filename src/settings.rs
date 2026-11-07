use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub health_refresh_seconds: u64,
    pub browser_poll_seconds: u64,
    pub concurrency: usize,
    pub command_timeout_seconds: u64,
    pub history_limit: usize,
}
impl Runtime {
    pub fn defaults(config: &crate::config::Config) -> Self {
        Self {
            health_refresh_seconds: config.health_refresh_seconds,
            browser_poll_seconds: 5,
            concurrency: config.concurrency,
            command_timeout_seconds: config.command_timeout_seconds,
            history_limit: 100,
        }
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.health_refresh_seconds == 0 || (15..=3600).contains(&self.health_refresh_seconds),
            "Health refresh must be off (0) or 15–3600 seconds"
        );
        anyhow::ensure!(
            (2..=60).contains(&self.browser_poll_seconds),
            "Browser polling must be 2–60 seconds"
        );
        anyhow::ensure!(
            (1..=16).contains(&self.concurrency),
            "Concurrency must be 1–16"
        );
        anyhow::ensure!(
            (10..=7200).contains(&self.command_timeout_seconds),
            "Command timeout must be 10–7200 seconds"
        );
        anyhow::ensure!(
            (20..=1000).contains(&self.history_limit),
            "History retention must be 20–1000 operations"
        );
        Ok(())
    }
}

pub struct Slot(pub std::sync::Arc<crate::api::AppState>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0
            .running
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.0.slot_changed.notify_waiters();
    }
}
impl crate::api::AppState {
    pub fn prefs(&self) -> Runtime {
        self.runtime.read().unwrap().clone()
    }
    pub fn try_slot(self: &std::sync::Arc<Self>) -> Option<Slot> {
        use std::sync::atomic::Ordering::SeqCst;
        loop {
            let current = self.running.load(SeqCst);
            if current >= self.prefs().concurrency {
                return None;
            }
            if self
                .running
                .compare_exchange(current, current + 1, SeqCst, SeqCst)
                .is_ok()
            {
                return Some(Slot(self.clone()));
            }
        }
    }
    pub async fn acquire_slot(self: &std::sync::Arc<Self>) -> Slot {
        loop {
            let changed = self.slot_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(slot) = self.try_slot() {
                return slot;
            }
            changed.await;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_intervals_and_limits_are_rejected() {
        let mut prefs = Runtime {
            health_refresh_seconds: 60,
            browser_poll_seconds: 5,
            concurrency: 3,
            command_timeout_seconds: 1800,
            history_limit: 100,
        };
        assert!(prefs.validate().is_ok());
        prefs.health_refresh_seconds = 1;
        assert!(prefs.validate().is_err());
        prefs.health_refresh_seconds = 0;
        assert!(prefs.validate().is_ok());
        prefs.concurrency = 0;
        assert!(prefs.validate().is_err());
    }
}
