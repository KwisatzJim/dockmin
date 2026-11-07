use crate::{
    api::AppState,
    db::{Host, now},
    ssh,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, atomic::Ordering};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Health {
    pub state: String,
    pub checked: i64,
    pub last_success: Option<i64>,
    pub failures: u32,
    pub next_check: i64,
    pub error: String,
}
pub fn backoff(interval: u64, failures: u32) -> u64 {
    interval
        .max(15)
        .saturating_mul(1u64 << failures.min(5))
        .min(3600)
}
pub fn start(state: Arc<AppState>) {
    tokio::spawn(async move {
        // Let startup and restored sessions finish before the first round.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        loop {
            if state.stopping.load(Ordering::SeqCst) {
                break;
            }
            let forced = state.force_health.swap(false, Ordering::SeqCst);
            let interval = state.prefs().health_refresh_seconds;
            if interval != 0 || forced {
                let health = state.db.health().unwrap_or_default();
                for host in state.db.hosts().unwrap_or_default() {
                    if state.stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    if !forced && health.get(&host.id).is_some_and(|h| h.next_check > now()) {
                        continue;
                    }
                    let state = state.clone();
                    tokio::spawn(async move {
                        if let Err(e) = refresh_host(&state, &host, interval.max(15)).await {
                            eprintln!("Health refresh {}: {e}", host.id);
                        }
                    });
                }
            }
            tokio::select! {
                _=tokio::time::sleep(std::time::Duration::from_secs(1))=>{},
                _=state.health_changed.notified()=>{},
                _=state.shutdown_changed.notified()=>{},
            }
        }
    });
}
async fn refresh_host(state: &Arc<AppState>, host: &Host, interval: u64) -> anyhow::Result<()> {
    let gate = state.mutations.lock().await;
    if state.db.active_on_host(&host.id)? {
        return Ok(());
    }
    let lock = state.host_lock(&host.id);
    let Ok(_host) = lock.try_lock_owned() else {
        return Ok(());
    };
    let Some(_slot) = state.try_slot() else {
        return Ok(());
    };
    drop(gate);
    let previous = state.db.health()?.remove(&host.id).unwrap_or_default();
    let mut health = Health {
        state: "checking".into(),
        checked: now(),
        last_success: previous.last_success,
        failures: previous.failures,
        next_check: now() + 30,
        error: String::new(),
    };
    state.db.set_health(&host.id, &health)?;
    let stacks: Vec<_> = state
        .db
        .stacks()?
        .into_iter()
        .filter(|s| s.host_id == host.id)
        .collect();
    let mut online = true;
    let mut attention = false;
    let targets: Vec<_> = if stacks.is_empty() {
        vec![None]
    } else {
        stacks.iter().map(Some).collect()
    };
    for stack in targets {
        if state.stopping.load(Ordering::SeqCst) {
            return Ok(());
        }
        if state.db.active_on_host(&host.id)? {
            break;
        }
        let remote = ssh::remote_command(stack, if stack.is_some() { "refresh" } else { "test" })?;
        let result = ssh::capture(ssh::command(&state.config, host, &remote), 30, 128 * 1024).await;
        match result {
            Ok(capture) => {
                if capture.code == Some(255) {
                    online = false;
                    health.error = capture.stderr.chars().take(300).collect();
                } else if !capture.success {
                    attention = true;
                    health.error = capture.stderr.chars().take(300).collect();
                }
                if let Some(stack) = stack {
                    let value = if capture.success {
                        match ssh::parse_status(&capture.stdout) {
                            Ok(services) => {
                                serde_json::json!({"services":services,"checked":now()})
                            }
                            Err(e) => {
                                attention = true;
                                serde_json::json!({"services":[],"checked":now(),"error":e.to_string()})
                            }
                        }
                    } else {
                        serde_json::json!({"services":[],"checked":now(),"error":"Host or Compose status unavailable; see host connectivity."})
                    };
                    state.db.set_status(&stack.id, &value)?;
                }
            }
            Err(e) => {
                online = false;
                health.error = e.to_string();
            }
        }
        if !online {
            break;
        }
    }
    health.checked = now();
    health.state = if !online {
        "offline"
    } else if attention {
        "attention"
    } else {
        "online"
    }
    .into();
    health.failures = if online {
        0
    } else {
        previous.failures.saturating_add(1)
    };
    if online && !attention {
        health.last_success = Some(now());
    }
    health.next_check = now()
        + if online {
            interval
        } else {
            backoff(interval, health.failures)
        } as i64;
    state.db.set_health(&host.id, &health)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn unreachable_hosts_back_off_with_a_bound() {
        assert_eq!(super::backoff(60, 0), 60);
        assert_eq!(super::backoff(60, 1), 120);
        assert_eq!(super::backoff(60, 3), 480);
        assert_eq!(super::backoff(3600, 10), 3600);
    }
}
