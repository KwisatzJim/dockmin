use crate::{
    api::{AppState, Result, missing},
    auth, ssh,
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        Sse,
        sse::{Event, KeepAlive},
    },
};
use std::{convert::Infallible, sync::Arc};
use tokio::io::AsyncReadExt;

pub async fn stream(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>>> {
    let stack =
        s.db.stacks()?
            .into_iter()
            .find(|v| v.id == id)
            .ok_or_else(missing)?;
    let host =
        s.db.hosts()?
            .into_iter()
            .find(|v| v.id == stack.host_id)
            .ok_or_else(missing)?;
    let remote = format!(
        "cd -- {} && {} logs --follow --no-color --tail 150",
        ssh::quote(&stack.directory),
        ssh::compose(&stack)
    );
    open(s, host, remote, headers).await
}
pub async fn open(
    s: Arc<AppState>,
    host: crate::db::Host,
    remote: String,
    headers: HeaderMap,
) -> Result<Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>>> {
    let permit = s.log_slots.clone().try_acquire_owned().map_err(|_| {
        crate::api::ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Four live log streams are already open; close one and try again".into(),
        )
    })?;
    let mut child = ssh::command(&s.config, &host, &remote)
        .spawn()
        .map_err(anyhow::Error::from)?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let token = auth::token(&headers).unwrap_or_default().to_string();
    let events = async_stream::stream! {
        let _permit=permit;
        let mut out_open=true;let mut err_open=true;let mut a=[0u8;4096];let mut b=[0u8;4096];
        let end=tokio::time::sleep(std::time::Duration::from_secs(3600));tokio::pin!(end);
        loop {
            if s.stopping.load(std::sync::atomic::Ordering::SeqCst) || s.db.session(&token).ok().flatten().is_none(){break;}
            if !out_open && !err_open{break;}
            let data=tokio::select! {
                _=&mut end=>{yield Ok(Event::default().event("end").data("Log stream ended after one hour; reconnect to continue."));break;},
                _=s.shutdown_changed.notified()=>{break;},
                _=tokio::time::sleep(std::time::Duration::from_secs(5))=>{continue;},
                result=stdout.read(&mut a),if out_open=>{match result {Ok(0)=>{out_open=false;String::new()},Ok(n)=>String::from_utf8_lossy(&a[..n]).into_owned(),Err(e)=>{yield Ok(Event::default().event("error").data(e.to_string()));break;}}},
                result=stderr.read(&mut b),if err_open=>{match result {Ok(0)=>{err_open=false;String::new()},Ok(n)=>String::from_utf8_lossy(&b[..n]).into_owned(),Err(e)=>{yield Ok(Event::default().event("error").data(e.to_string()));break;}}},
            };
            if !data.is_empty(){yield Ok(Event::default().event("log").data(data));}
        }
        let _=child.kill().await;
        yield Ok(Event::default().event("end").data("Log stream closed."));
    };
    // Dropping this stream (navigation/disconnect) drops the child and its
    // kill-on-drop SSH process; no long-lived host mutation lock is held.
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}
