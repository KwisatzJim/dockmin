use crate::{
    api::AppState,
    db::{Host, Job, Stack, now},
    ssh,
};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use uuid::Uuid;
const OUTPUT_LIMIT: usize = 128 * 1024;

pub async fn enqueue(
    state: Arc<AppState>,
    host: Host,
    stack: Option<Stack>,
    action: String,
) -> anyhow::Result<Job> {
    let remote = if action == "update" {
        crate::recovery::update_script(
            &state,
            stack
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Update requires a stack"))?,
            true,
        )?
    } else if action == "prune-containers" && stack.is_none() {
        // Resolve retained checkpoints when this job runs, after earlier updates finish.
        String::new()
    } else {
        ssh::remote_command(stack.as_ref(), &action)?
    };
    enqueue_script(state, host, stack, action, remote).await
}
pub async fn enqueue_script(
    state: Arc<AppState>,
    host: Host,
    stack: Option<Stack>,
    action: String,
    remote: String,
) -> anyhow::Result<Job> {
    let lock = {
        let mut locks = state.host_locks.lock().unwrap();
        locks
            .entry(host.id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let job = Job {
        id: Uuid::new_v4().to_string(),
        host_id: host.id.clone(),
        stack_id: stack.as_ref().map(|s| s.id.clone()),
        action,
        state: "queued".into(),
        created: now(),
        finished: None,
        output: String::new(),
    };
    // Bound queued work as well as running processes.
    anyhow::ensure!(
        state
            .db
            .jobs()?
            .iter()
            .filter(|j| j.finished.is_none())
            .count()
            < 64,
        "Job queue is full"
    );
    state.db.create_job(&job)?;
    let id = job.id.clone();
    let action = job.action.clone();
    tokio::spawn(async move {
        let _host = lock.lock().await;
        let _permit = state.acquire_slot().await;
        if let Err(e) = run(&state, &id, &host, &remote, stack.as_ref(), &action).await {
            eprintln!("Operation {id}: {e}");
            let existing = state
                .db
                .job(&id)
                .ok()
                .flatten()
                .map(|j| j.output)
                .unwrap_or_default();
            let _ = state.db.update_job(
                &id,
                "interrupted",
                &format!("{existing}\nExecution interrupted; refresh status before retrying. {e}"),
                true,
            );
        }
        let _ = crate::scheduler::job_finished(&state, &id);
        if action == "refresh"
            && state
                .db
                .job(&id)
                .ok()
                .flatten()
                .is_some_and(|j| j.state != "succeeded")
        {
            if let Some(stack) = stack.as_ref() {
                let _=state.db.set_status(&stack.id,&serde_json::json!({"services":[],"checked":now(),"error":"Status refresh did not complete. Inspect operation output and test the host connection."}));
            }
        }
        if matches!(
            action.as_str(),
            "start" | "stop" | "restart" | "update" | "deploy" | "rollback" | "restore" | "backup"
        ) {
            if let Some(stack) = stack.as_ref() {
                let followups: &[&str] = if action == "update" {
                    &["refresh", "check-updates"]
                } else {
                    &["refresh"]
                };
                for &followup in followups {
                    let next = Job {
                        id: Uuid::new_v4().to_string(),
                        host_id: host.id.clone(),
                        stack_id: Some(stack.id.clone()),
                        action: followup.into(),
                        state: "queued".into(),
                        created: now(),
                        finished: None,
                        output: String::new(),
                    };
                    if state.db.create_job(&next).is_ok() {
                        let command =
                            ssh::remote_command(Some(stack), followup).expect("fixed action");
                        if let Err(e) =
                            run(&state, &next.id, &host, &command, Some(stack), followup).await
                        {
                            let _ =
                                state
                                    .db
                                    .update_job(&next.id, "interrupted", &e.to_string(), true);
                        }
                        let _ = crate::scheduler::job_finished(&state, &next.id);
                    }
                }
            }
        }
        let _ = state.db.trim_jobs(state.prefs().history_limit);
    });
    Ok(job)
}
async fn run(
    state: &AppState,
    id: &str,
    host: &Host,
    remote: &str,
    stack: Option<&Stack>,
    action: &str,
) -> anyhow::Result<()> {
    state.db.update_job(id, "running", "", false)?;
    let protected_cleanup = if action == "prune-containers" {
        Some(crate::containers::prune_script(
            &state.db.records("container-checkpoint", &host.id)?,
        )?)
    } else {
        None
    };
    let remote = protected_cleanup.as_deref().unwrap_or(remote);
    let mut child = match ssh::stdin_command(&state.config, host).spawn() {
        Ok(c) => c,
        Err(e) => {
            state
                .db
                .update_job(id, "failed", &format!("Could not launch SSH: {e}"), true)?;
            return Ok(());
        }
    };
    let mut input = child.stdin.take().unwrap();
    let script = remote.to_string();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let _ = input.write_all(script.as_bytes()).await;
    });
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut out_open = true;
    let mut err_open = true;
    let mut a = [0u8; 4096];
    let mut b = [0u8; 4096];
    let mut raw = Vec::new();
    let mut stdout_raw = Vec::new();
    let mut truncated = false;
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(
        state.prefs().command_timeout_seconds,
    ));
    tokio::pin!(deadline);
    while out_open || err_open {
        let data = tokio::select! {
            _=&mut deadline=>{let _=child.kill().await;let mut output=String::from_utf8_lossy(&raw).into_owned();output.push_str("\nTimeout. Remote outcome may be unknown; refresh before retrying.");state.db.update_job(id,"interrupted",&output,true)?;return Ok(())},
            n=stdout.read(&mut a),if out_open=>{let n=n?;if n==0{out_open=false} let room=OUTPUT_LIMIT.saturating_sub(stdout_raw.len());stdout_raw.extend_from_slice(&a[..n.min(room)]);persist_records(state,host,stack,action,&stdout_raw)?;a[..n].to_vec()},
            n=stderr.read(&mut b),if err_open=>{let n=n?;if n==0{err_open=false} b[..n].to_vec()},
        };
        let room = OUTPUT_LIMIT.saturating_sub(raw.len());
        let take = data.len().min(room);
        raw.extend_from_slice(&data[..take]);
        if take < data.len() {
            truncated = true;
        }
        if !data.is_empty() {
            state
                .db
                .update_job(id, "running", &String::from_utf8_lossy(&raw), false)?;
        }
    }
    let exit = tokio::select! {
        status=child.wait()=>status?,
        _=&mut deadline=>{let _=child.kill().await;state.db.update_job(id,"interrupted","Timeout waiting for SSH; remote outcome may be unknown.",true)?;return Ok(())}
    };
    let mut output = String::from_utf8_lossy(&raw).into_owned();
    persist_records(state, host, stack, action, &stdout_raw)?;
    if action == "check-updates" && exit.success() {
        output = crate::images::summarize(&String::from_utf8_lossy(&stdout_raw));
    }
    if truncated {
        output.push_str("\n[Output truncated at 128 KiB]");
    }
    let status = if exit.success() {
        "succeeded"
    } else if exit.code() == Some(255) {
        output.push_str(
            "\nSSH failed. Remote outcome may be unknown; verify connectivity and refresh status.",
        );
        "interrupted"
    } else {
        "failed"
    };
    if action == "refresh" {
        if let Some(stack) = stack {
            let value = if status == "succeeded" {
                match ssh::parse_status(&String::from_utf8_lossy(&stdout_raw)) {
                    Ok(services) => serde_json::json!({"services":services,"checked":now()}),
                    Err(e) => {
                        serde_json::json!({"services":[],"checked":now(),"error":format!("Could not parse Compose status: {e}")})
                    }
                }
            } else {
                serde_json::json!({"services":[],"checked":now(),"error":"Status refresh failed. Inspect operation output and test the host connection."})
            };
            state.db.set_status(&stack.id, &value)?;
        }
    }
    state.db.update_job(id, status, &output, true)?;
    Ok(())
}

fn persist_records(
    state: &AppState,
    host: &Host,
    stack: Option<&Stack>,
    action: &str,
    raw: &[u8],
) -> anyhow::Result<()> {
    for line in String::from_utf8_lossy(raw).lines() {
        let Some(record) = line.strip_prefix("DOCKMIN_RECORD ") else {
            continue;
        };
        let Some((kind, payload)) = record.split_once(' ') else {
            continue;
        };
        let allowed = match kind {
            "container-checkpoint" => action == "container-update",
            "checkpoint" => matches!(action, "update" | "deploy"),
            "data-backup" => matches!(action, "backup" | "restore"),
            _ => false,
        };
        if !allowed {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
            continue;
        };
        let Some(id) = value["id"].as_str() else {
            continue;
        };
        anyhow::ensure!(
            Uuid::parse_str(id).is_ok() && value["host_id"] == host.id,
            "Invalid checkpoint metadata from managed host"
        );
        let owner = if kind == "container-checkpoint" {
            host.id.as_str()
        } else {
            let stack = stack.ok_or_else(|| anyhow::anyhow!("Checkpoint has no stack"))?;
            anyhow::ensure!(
                value["stack_id"] == stack.id,
                "Checkpoint belongs to a different stack"
            );
            stack.id.as_str()
        };
        if state.db.record(kind, id)?.is_none() {
            state.db.put_record(kind, id, owner, &value)?;
        }
    }
    Ok(())
}
