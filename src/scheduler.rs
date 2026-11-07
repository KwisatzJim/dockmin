use crate::{
    api::{AppState, Result},
    db::now,
    jobs,
};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub enabled: bool,
    pub interval_minutes: u64,
    pub stack_ids: Vec<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_minutes: 1440,
            stack_ids: vec![],
        }
    }
}
pub fn preferences(s: &AppState) -> anyhow::Result<Preferences> {
    Ok(s.db
        .get_setting("automation")?
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default())
}
pub async fn get(State(s): State<Arc<AppState>>) -> Result<Json<Value>> {
    Ok(Json(
        json!({"preferences":preferences(&s)?,"last_checks":s.db.records("scheduled","")?}),
    ))
}
pub async fn save(
    State(s): State<Arc<AppState>>,
    Json(body): Json<Preferences>,
) -> Result<Json<Value>> {
    let _gate = s.mutations.lock().await;
    crate::api::ensure(
        (15..=10080).contains(&body.interval_minutes),
        "Schedule must be 15 minutes–7 days",
    )?;
    let ids: std::collections::HashSet<_> = s.db.stacks()?.into_iter().map(|s| s.id).collect();
    let unique: std::collections::HashSet<_> = body.stack_ids.iter().collect();
    crate::api::ensure(
        unique.len() == body.stack_ids.len() && body.stack_ids.iter().all(|id| ids.contains(id)),
        "Invalid or duplicate scheduled stack",
    )?;
    s.db.set_setting(
        "automation",
        &serde_json::to_value(&body).map_err(anyhow::Error::from)?,
    )?;
    Ok(Json(json!({"preferences":body})))
}
pub async fn read(State(s): State<Arc<AppState>>) -> Result<Json<Value>> {
    s.db.read_notifications()?;
    Ok(Json(json!({"ok":true})))
}
pub fn start(s: Arc<AppState>) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        loop {
            if s.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            if let Err(e) = tick(&s, now()).await {
                eprintln!("Scheduled checks: {e}");
            }
            tokio::select! {_=tokio::time::sleep(std::time::Duration::from_secs(30))=>{},_=s.shutdown_changed.notified()=>{break;}}
        }
    });
}
pub async fn tick(s: &Arc<AppState>, time: i64) -> anyhow::Result<usize> {
    let _gate = s.mutations.lock().await;
    let prefs = preferences(s)?;
    if !prefs.enabled {
        return Ok(0);
    }
    let hosts = s.db.hosts()?;
    let mut count = 0;
    for stack in s.db.stacks()? {
        if !prefs.stack_ids.is_empty() && !prefs.stack_ids.contains(&stack.id) {
            continue;
        }
        let last =
            s.db.record("scheduled", &stack.id)?
                .and_then(|v| v["checked"].as_i64())
                .unwrap_or(0);
        if time < last.saturating_add((prefs.interval_minutes * 60) as i64) {
            continue;
        }
        if s.db.active_on_host(&stack.host_id)? {
            continue;
        }
        let Some(host) = hosts.iter().find(|h| h.id == stack.host_id) else {
            continue;
        };
        let job = jobs::enqueue(
            s.clone(),
            host.clone(),
            Some(stack.clone()),
            "check-updates".into(),
        )
        .await?;
        s.db.put_record(
            "scheduled",
            &stack.id,
            "",
            &json!({"stack_id":stack.id,"checked":time,"job_id":job.id}),
        )?;
        count += 1;
    }
    Ok(count)
}
pub fn job_finished(s: &AppState, id: &str) -> anyhow::Result<()> {
    let Some(job) = s.db.job(id)? else {
        return Ok(());
    };
    if job.finished.is_none() {
        return Ok(());
    }
    if job.action == "check-updates" {
        let Some(stack_id) = job.stack_id.as_deref() else {
            return Ok(());
        };
        let name =
            s.db.stacks()?
                .into_iter()
                .find(|s| s.id == stack_id)
                .map(|s| s.name)
                .unwrap_or_else(|| "Removed stack".into());
        let update = crate::updates::for_stack(std::slice::from_ref(&job), stack_id);
        let signal = match update.state {
            "available" => format!("available:{}", job.output),
            "failed" => "failed".into(),
            _ => String::new(),
        };
        s.db.notify_change(
            &format!("update-signal:{stack_id}"),
            &signal,
            &format!(
                "{} · {name}",
                if update.state == "failed" {
                    "Update check failed"
                } else {
                    "Updates available"
                }
            ),
            &job.output.chars().take(2000).collect::<String>(),
        )?;
    } else if matches!(job.state.as_str(), "failed" | "interrupted") {
        let host =
            s.db.hosts()?
                .into_iter()
                .find(|h| h.id == job.host_id)
                .map(|h| h.name)
                .unwrap_or_else(|| "Removed host".into());
        let stack = job.stack_id.as_deref().and_then(|sid| {
            s.db.stacks()
                .ok()?
                .into_iter()
                .find(|v| v.id == sid)
                .map(|v| v.name)
        });
        let target = stack.map(|name| format!("{host} / {name}")).unwrap_or(host);
        s.db.notify_change(
            &format!("operation:{id}"),
            &job.state,
            &format!("{target} · {} {}", job.action, job.state),
            "Inspect operation output before retrying; the remote result may be uncertain.",
        )?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_notifications_are_suppressed_until_state_changes() {
        let path =
            std::env::temp_dir().join(format!("dockmin-notify-{}.sqlite", uuid::Uuid::new_v4()));
        let db = crate::db::Database::open(&path).unwrap();
        db.notify_change("s", "available", "title", "body").unwrap();
        db.notify_change("s", "available", "title", "body").unwrap();
        assert_eq!(db.notifications().unwrap().len(), 1);
        db.notify_change("s", "", "", "").unwrap();
        db.notify_change("s", "available", "title", "body").unwrap();
        assert_eq!(db.notifications().unwrap().len(), 2);
        db.read_notifications().unwrap();
        assert!(
            db.notifications()
                .unwrap()
                .iter()
                .all(|n| n["read"] == true)
        );
        drop(db);
        let _ = std::fs::remove_file(path);
    }
    #[tokio::test]
    async fn due_checks_respect_user_jobs_and_do_not_repeat_after_restart() {
        let folder =
            std::env::temp_dir().join(format!("dockmin-scheduler-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&folder).unwrap();
        let config_file = folder.join("config.toml");
        let database = folder.join("db.sqlite");
        let ssh = folder.join("ssh_config");
        std::fs::write(&ssh, "Host fixture\n HostName 127.0.0.1\n Port 1\n").unwrap();
        std::fs::write(&config_file,format!("listen=\"127.0.0.1:19099\"\norigins=[\"http://127.0.0.1:19099\"]\ndatabase={:?}\nstatic_dir={:?}\nssh_config={:?}\nknown_hosts={:?}\nsecure_cookie=false\n",database,folder,ssh,folder.join("known_hosts"))).unwrap();
        let config = crate::config::Config::load(&config_file).unwrap();
        let db = crate::db::Database::open(&database).unwrap();
        let host = crate::db::Host {
            id: "h".into(),
            name: "fixture".into(),
            alias: "fixture".into(),
        };
        db.save_host(&host).unwrap();
        db.save_stack(&crate::db::Stack {
            id: "s".into(),
            host_id: "h".into(),
            name: "fixture".into(),
            directory: "/tmp".into(),
            project: "fixture".into(),
            files: vec![],
        })
        .unwrap();
        let user = crate::db::Job {
            id: "user".into(),
            host_id: "h".into(),
            stack_id: Some("s".into()),
            action: "refresh".into(),
            state: "queued".into(),
            created: 1,
            finished: None,
            output: String::new(),
        };
        db.create_job(&user).unwrap();
        let state = Arc::new(crate::api::AppState::new(config, db).unwrap());
        assert_eq!(tick(&state, 100000).await.unwrap(), 0);
        state.db.update_job("user", "succeeded", "", true).unwrap();
        assert_eq!(tick(&state, 100000).await.unwrap(), 1);
        assert_eq!(tick(&state, 100001).await.unwrap(), 0);
        let check = state.db.record("scheduled", "s").unwrap().unwrap();
        assert_eq!(check["checked"], 100000);
        state
            .db
            .set_setting(
                "automation",
                &json!({"enabled":false,"interval_minutes":15,"stack_ids":[]}),
            )
            .unwrap();
        assert_eq!(tick(&state, 200000).await.unwrap(), 0);
        // State remains persisted even if queued work is recovered as interrupted.
        state.db.recover().unwrap();
        state
            .db
            .set_setting(
                "automation",
                &json!({"enabled":true,"interval_minutes":1440,"stack_ids":[]}),
            )
            .unwrap();
        assert_eq!(tick(&state, 100001).await.unwrap(), 0);
        drop(state);
        let _ = std::fs::remove_dir_all(folder);
    }
}
