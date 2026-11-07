use crate::api::{AppState, Result, ensure};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub enabled: bool,
    pub server: String,
    pub topic: String,
    pub token: String,
    pub generation: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    enabled: bool,
    server: String,
    topic: String,
    token: Option<String>,
}
fn preferences(s: &AppState) -> anyhow::Result<Preferences> {
    Ok(s.db
        .get_setting("ntfy")?
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default())
}
fn public(p: &Preferences) -> Value {
    json!({"enabled":p.enabled,"server":p.server,"topic":p.topic,"token_configured":!p.token.is_empty()})
}
fn validate(p: &Preferences) -> anyhow::Result<()> {
    if !p.enabled && p.server.is_empty() && p.topic.is_empty() {
        return Ok(());
    }
    let url = reqwest::Url::parse(&p.server)?;
    anyhow::ensure!(
        matches!(url.scheme(), "https" | "http")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Use an HTTP or HTTPS server URL without credentials, query, or fragment"
    );
    anyhow::ensure!(
        !p.topic.is_empty()
            && p.topic.len() <= 128
            && p.topic
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
        "Topic must contain 1–128 letters, digits, underscores, or hyphens"
    );
    anyhow::ensure!(
        p.token.len() <= 4096 && p.token.bytes().all(|c| c.is_ascii_graphic()),
        "Invalid access token"
    );
    Ok(())
}
pub async fn get(State(s): State<Arc<AppState>>) -> Result<Json<Value>> {
    let p = preferences(&s)?;
    Ok(Json(
        json!({"preferences":public(&p),"deliveries":s.db.records("ntfy-delivery",&p.generation)?}),
    ))
}
pub async fn save(State(s): State<Arc<AppState>>, Json(body): Json<Input>) -> Result<Json<Value>> {
    let _gate = s.mutations.lock().await;
    let previous = preferences(&s)?;
    let mut p = Preferences {
        enabled: body.enabled,
        server: body.server.trim().trim_end_matches('/').to_owned(),
        topic: body.topic.trim().to_owned(),
        token: body.token.unwrap_or(previous.token.clone()),
        generation: previous.generation.clone(),
    };
    validate(&p)?;
    // A changed destination starts with future alerts, avoiding replaying old inbox entries.
    if p.generation.is_empty()
        || p.server != previous.server
        || p.topic != previous.topic
        || p.enabled != previous.enabled
    {
        p.generation = uuid::Uuid::new_v4().to_string();
    }
    s.db.set_setting(
        "ntfy",
        &serde_json::to_value(&p).map_err(anyhow::Error::from)?,
    )?;
    Ok(Json(json!({"preferences":public(&p)})))
}
fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
async fn publish(
    client: &reqwest::Client,
    p: &Preferences,
    title: &str,
    body: &str,
) -> std::result::Result<(), String> {
    // JSON supports Unicode titles. Never forward command output or credentials to ntfy.
    let mut request = client
        .post(format!("{}/", p.server))
        .json(&json!({"topic":p.topic,"title":title,"message":body,"tags":["package"]}));
    if !p.token.is_empty() {
        request = request.bearer_auth(&p.token);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Could not reach ntfy (connection or timeout)".to_owned())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("ntfy returned HTTP {}", response.status().as_u16()))
    }
}
pub async fn test(State(s): State<Arc<AppState>>) -> Result<Json<Value>> {
    let _gate = s.mutations.lock().await;
    let p = preferences(&s)?;
    ensure(p.enabled, "Enable and save ntfy first")?;
    validate(&p)?;
    publish(
        &client()?,
        &p,
        "Dockmin test notification",
        "Dockmin notifications are connected.",
    )
    .await
    .map_err(|e| anyhow::anyhow!(e))?;
    Ok(Json(json!({"ok":true})))
}
pub fn start(s: Arc<AppState>) {
    tokio::spawn(async move {
        let Ok(client) = client() else {
            eprintln!("Could not initialize ntfy client");
            return;
        };
        loop {
            if s.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            if tick(&s, &client, crate::db::now()).await.is_err() {
                eprintln!("Could not process ntfy notification queue");
            }
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(15))=>{},_=s.shutdown_changed.notified()=>{break;}}
        }
    });
}
pub async fn tick(s: &AppState, client: &reqwest::Client, time: i64) -> anyhow::Result<usize> {
    let _gate = s.mutations.lock().await;
    let p = preferences(s)?;
    if !p.enabled {
        return Ok(0);
    }
    validate(&p)?;
    let mut delivered = 0;
    for mut item in
        s.db.records("ntfy-delivery", &p.generation)?
            .into_iter()
            .rev()
            .take(200)
    {
        if item["state"] == "sent" || item["next_attempt"].as_i64().unwrap_or(0) > time {
            continue;
        }
        let id = item["id"].as_str().unwrap_or_default().to_owned();
        let result = publish(
            client,
            &p,
            item["title"].as_str().unwrap_or("Dockmin"),
            "Open Dockmin’s Notifications inbox for details.",
        )
        .await;
        let attempts = item["attempts"].as_u64().unwrap_or(0).saturating_add(1);
        item["attempts"] = json!(attempts);
        item["updated"] = json!(time);
        match result {
            Ok(()) => {
                item["state"] = json!("sent");
                item["error"] = json!("");
                delivered += 1;
            }
            Err(e) => {
                item["state"] = json!("retrying");
                item["error"] = json!(e);
                item["next_attempt"] =
                    json!(time + (30_i64 * 2_i64.pow(attempts.min(7) as u32)).min(3600));
            }
        }
        s.db.put_record("ntfy-delivery", &id, &p.generation, &item)?;
        // One request per tick keeps outages and large queues from monopolizing administration.
        break;
    }
    Ok(delivered)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn delivery_retries_persist_and_settings_never_return_token() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let folder = std::env::temp_dir().join(format!("dockmin-ntfy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("config.toml");
        std::fs::write(&path,format!("listen=\"127.0.0.1:19099\"\norigins=[\"http://127.0.0.1:19099\"]\ndatabase={:?}\nstatic_dir={:?}\nssh_config={:?}\nknown_hosts={:?}\nsecure_cookie=false\n",folder.join("db.sqlite"),folder,folder.join("ssh_config"),folder.join("known_hosts"))).unwrap();
        std::fs::write(folder.join("ssh_config"), "").unwrap();
        let s = Arc::new(
            AppState::new(
                crate::config::Config::load(&path).unwrap(),
                crate::db::Database::open(&folder.join("db.sqlite")).unwrap(),
            )
            .unwrap(),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                    let count = count.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer tk_test_secret");
                        assert_eq!(body["topic"], "dockmin-test");
                        assert!(!body.to_string().contains("private-output"));
                        if count.fetch_add(1, Ordering::SeqCst) == 0 {
                            axum::http::StatusCode::SERVICE_UNAVAILABLE
                        } else {
                            axum::http::StatusCode::OK
                        }
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        s.db.notify_change("old", "alert", "Old alert", "").unwrap();
        let _ = save(
            State(s.clone()),
            Json(Input {
                enabled: true,
                server: format!("http://{address}"),
                topic: "dockmin-test".into(),
                token: Some("tk_test_secret".into()),
            }),
        )
        .await
        .map_err(|e| e.1)
        .unwrap();
        let response = get(State(s.clone())).await.map_err(|e| e.1).unwrap().0;
        assert_eq!(response["preferences"]["token_configured"], true);
        assert!(!response.to_string().contains("tk_test_secret"));
        s.db.notify_change("new", "alert", "New alert", "private-output")
            .unwrap();
        let p = preferences(&s).unwrap();
        assert_eq!(
            s.db.records("ntfy-delivery", &p.generation).unwrap().len(),
            1
        );
        let http = client().unwrap();
        assert_eq!(tick(&s, &http, 100).await.unwrap(), 0);
        assert_eq!(tick(&s, &http, 101).await.unwrap(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let queued = s.db.records("ntfy-delivery", &p.generation).unwrap();
        assert_eq!(queued[0]["state"], "retrying");
        // Reloading SQLite preserves the queue after a process restart.
        let reopened = crate::db::Database::open(&folder.join("db.sqlite")).unwrap();
        assert_eq!(
            reopened.records("ntfy-delivery", &p.generation).unwrap()[0]["attempts"],
            1
        );
        assert_eq!(tick(&s, &http, 200).await.unwrap(), 1);
        assert_eq!(tick(&s, &http, 300).await.unwrap(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let exported = crate::data::export(State(s.clone()))
            .await
            .map_err(|e| e.1)
            .unwrap();
        let bytes = http_body_util::BodyExt::collect(exported.into_body())
            .await
            .unwrap()
            .to_bytes();
        assert!(!String::from_utf8_lossy(&bytes).contains("tk_test_secret"));
        server.abort();
        drop(reopened);
        drop(s);
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn reject_credential_urls_and_invalid_topics() {
        let mut p = Preferences {
            enabled: true,
            server: "https://user:secret@example.com".into(),
            topic: "alerts".into(),
            ..Default::default()
        };
        assert!(validate(&p).is_err());
        p.server = "https://example.com".into();
        p.topic = "../other".into();
        assert!(validate(&p).is_err());
        p.topic = "alerts-test".into();
        assert!(validate(&p).is_ok());
    }
}
