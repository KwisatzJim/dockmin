use crate::{
    api::{AppState, Result, cookie, ensure},
    auth,
    db::{Host, Stack},
    settings::Runtime,
    ssh,
};
use axum::{
    Json,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn settings(State(s): State<Arc<AppState>>) -> Json<Runtime> {
    Json(s.prefs())
}
pub async fn save_settings(
    State(s): State<Arc<AppState>>,
    Json(prefs): Json<Runtime>,
) -> Result<Json<Value>> {
    let _gate = s.mutations.lock().await;
    prefs.validate()?;
    s.db.set_setting(
        "runtime",
        &serde_json::to_value(&prefs).map_err(anyhow::Error::from)?,
    )?;
    *s.runtime.write().unwrap() = prefs;
    s.slot_changed.notify_waiters();
    s.force_health.store(
        s.prefs().health_refresh_seconds != 0,
        std::sync::atomic::Ordering::SeqCst,
    );
    s.health_changed.notify_one();
    s.db.trim_jobs(s.prefs().history_limit)?;
    Ok(Json(json!({"settings":s.prefs()})))
}
pub async fn refresh_health(State(s): State<Arc<AppState>>) -> Json<Value> {
    s.force_health
        .store(true, std::sync::atomic::Ordering::SeqCst);
    s.health_changed.notify_one();
    Json(json!({"ok":true}))
}
#[derive(Deserialize)]
pub struct Password {
    pub current_password: String,
    pub new_password: String,
}
async fn verify_current(s: &Arc<AppState>, password: String) -> Result<()> {
    ensure(password.len() <= 1024, "Password is too long")?;
    let _gate = s.login_lock.try_lock().map_err(|_| {
        crate::api::ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Please wait before trying again".into(),
        )
    })?;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let hash = s.db.password()?.ok_or_else(crate::api::missing)?;
    let valid = tokio::task::spawn_blocking(move || auth::verify(&password, &hash))
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    if !valid {
        return Err(crate::api::ApiError(
            StatusCode::FORBIDDEN,
            "Current password is incorrect".into(),
        ));
    }
    Ok(())
}
pub async fn change_password(
    State(s): State<Arc<AppState>>,
    Json(body): Json<Password>,
) -> Result<Response> {
    ensure(
        (12..=1024).contains(&body.new_password.len()),
        "New password must be 12–1024 bytes",
    )?;
    verify_current(&s, body.current_password).await?;
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&body.new_password))
        .await
        .map_err(|e| anyhow::anyhow!(e))??;
    s.db.set_password(&hash)?;
    let (token, csrf) = s.db.new_session()?;
    let mut response = Json(json!({"csrf":csrf})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie(&s, &token, 43200).parse().unwrap(),
    );
    Ok(response)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub format: String,
    pub version: u32,
    pub hosts: Vec<Host>,
    pub stacks: Vec<Stack>,
    pub settings: Runtime,
    #[serde(default)]
    pub connections: Vec<crate::onboarding::NewHost>,
    #[serde(default)]
    pub automation: Option<crate::scheduler::Preferences>,
}
pub async fn export(State(s): State<Arc<AppState>>) -> Result<Response> {
    let body = Export {
        format: "dockmin".into(),
        version: 2,
        connections: s
            .db
            .records("ssh-host", "")?
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<_, _>>()
            .map_err(anyhow::Error::from)?,
        automation: Some(crate::scheduler::preferences(&s)?),
        hosts: s.db.hosts()?,
        stacks: s.db.stacks()?,
        settings: s.prefs(),
    };
    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        "attachment; filename=\"dockmin-config.json\""
            .parse()
            .unwrap(),
    );
    Ok(response)
}
pub fn validate_import(data: &Export) -> anyhow::Result<()> {
    anyhow::ensure!(
        data.format == "dockmin" && [1, 2].contains(&data.version),
        "Unsupported configuration format or version"
    );
    anyhow::ensure!(
        data.hosts.len() <= 100 && data.stacks.len() <= 500,
        "Import supports up to 100 hosts and 500 stacks"
    );
    data.settings.validate()?;
    let mut ids = std::collections::HashSet::new();
    let mut aliases = std::collections::HashSet::new();
    for host in &data.hosts {
        ssh::validate_host(host)?;
        anyhow::ensure!(
            !host.id.is_empty() && ids.insert(&host.id) && aliases.insert(&host.alias),
            "Duplicate or empty host identity in import"
        );
    }
    let mut stack_ids = std::collections::HashSet::new();
    for stack in &data.stacks {
        ssh::validate_stack(stack)?;
        anyhow::ensure!(
            !stack.id.is_empty() && stack_ids.insert(&stack.id) && ids.contains(&stack.host_id),
            "Invalid, duplicate, or orphaned stack identity"
        );
    }
    let mut connections = std::collections::HashSet::new();
    for connection in &data.connections {
        crate::onboarding::validate_connection(connection)?;
        anyhow::ensure!(
            aliases.contains(&connection.alias) && connections.insert(&connection.alias),
            "Invalid or duplicate imported connection"
        );
    }
    if let Some(schedule) = &data.automation {
        anyhow::ensure!(
            (15..=10080).contains(&schedule.interval_minutes)
                && schedule.stack_ids.iter().all(|id| stack_ids.contains(id)),
            "Invalid imported schedule"
        );
    }
    Ok(())
}
#[derive(Deserialize)]
pub struct Import {
    pub configuration: Value,
    pub confirm: String,
    #[serde(default)]
    pub apply_settings: bool,
}
pub async fn import(
    State(s): State<Arc<AppState>>,
    Json(body): Json<Import>,
) -> Result<Json<Value>> {
    ensure(
        body.confirm == "IMPORT CONFIGURATION",
        "Confirm IMPORT CONFIGURATION",
    )?;
    let mut configuration = normalize_import(body.configuration, s.prefs())?;
    validate_import(&configuration)?;
    let _gate = s.mutations.lock().await;
    let aliases = crate::onboarding::original_aliases(&s)?;
    configuration
        .connections
        .retain(|c| !aliases.contains(&c.alias));
    let (hosts, stacks) = s.db.import_config(&configuration, body.apply_settings)?;
    crate::onboarding::sync_managed(&s)?;
    if body.apply_settings {
        *s.runtime.write().unwrap() = configuration.settings;
        s.slot_changed.notify_waiters();
    }
    s.force_health.store(
        s.prefs().health_refresh_seconds != 0,
        std::sync::atomic::Ordering::SeqCst,
    );
    s.health_changed.notify_one();
    Ok(Json(json!({"hosts_added":hosts,"stacks_added":stacks})))
}
#[derive(Deserialize)]
pub struct Backup {
    pub password: String,
}
pub async fn backup(State(s): State<Arc<AppState>>, Json(body): Json<Backup>) -> Result<Response> {
    let _backup = s.backup_lock.try_lock().map_err(|_| {
        crate::api::ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "A backup is already running".into(),
        )
    })?;
    verify_current(&s, body.password).await?;
    let state = s.clone();
    let data = tokio::task::spawn_blocking(move || state.db.backup_bytes(&state.config.database))
        .await
        .map_err(|e| anyhow::anyhow!(e))??;
    let mut response = data.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/vnd.sqlite3".parse().unwrap(),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        "attachment; filename=\"dockmin-backup.sqlite\""
            .parse()
            .unwrap(),
    );
    Ok(response)
}

#[derive(Deserialize)]
struct DockmanHost {
    id: String,
    ssh_alias: String,
    label: String,
    compose_dirs: Vec<DockmanDir>,
}
#[derive(Deserialize)]
struct DockmanDir {
    id: String,
    path: String,
    label: String,
}
pub fn normalize_import(value: Value, settings: Runtime) -> anyhow::Result<Export> {
    if !value.is_array() {
        return Ok(serde_json::from_value(value)?);
    }
    let original: Vec<DockmanHost> = serde_json::from_value(value)?;
    let mut hosts = Vec::new();
    let mut stacks = Vec::new();
    for host in original {
        for directory in host.compose_dirs {
            stacks.push(Stack {
                id: format!("{}:{}", host.id, directory.id),
                host_id: host.id.clone(),
                name: directory.label,
                directory: directory.path,
                project: String::new(),
                files: vec![],
            });
        }
        hosts.push(Host {
            id: host.id,
            name: host.label,
            alias: host.ssh_alias,
        });
    }
    Ok(Export {
        format: "dockmin".into(),
        version: 1,
        connections: vec![],
        automation: None,
        hosts,
        stacks,
        settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prefs() -> Runtime {
        Runtime {
            health_refresh_seconds: 0,
            browser_poll_seconds: 5,
            concurrency: 2,
            command_timeout_seconds: 10,
            history_limit: 100,
        }
    }
    #[test]
    fn dockman_import_preserves_alias_and_directory_options() {
        let value = json!([{"id":"office","ssh_alias":"office-t","label":"Office","compose_dirs":[{"id":"/srv/media","path":"/srv/media","label":"Media"}]}]);
        let data = normalize_import(value, prefs()).unwrap();
        validate_import(&data).unwrap();
        assert_eq!(data.hosts[0].alias, "office-t");
        assert_eq!(data.stacks[0].directory, "/srv/media");
        assert!(data.stacks[0].project.is_empty());
    }
    #[test]
    fn orphan_and_duplicate_imports_are_rejected_before_any_write() {
        let mut data = Export {
            format: "dockmin".into(),
            version: 1,
            connections: vec![],
            automation: None,
            hosts: vec![Host {
                id: "h".into(),
                name: "Office".into(),
                alias: "office".into(),
            }],
            stacks: vec![],
            settings: prefs(),
        };
        data.hosts.push(data.hosts[0].clone());
        assert!(validate_import(&data).is_err());
        data.hosts.pop();
        data.stacks.push(Stack {
            id: "s".into(),
            host_id: "missing".into(),
            name: "Media".into(),
            directory: "/srv/media".into(),
            project: String::new(),
            files: vec![],
        });
        assert!(validate_import(&data).is_err());
    }
    #[test]
    fn imports_merge_idempotently_and_backups_remove_session_bytes() {
        let folder =
            std::env::temp_dir().join(format!("dockmin-data-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("db.sqlite");
        let db = crate::db::Database::open(&path).unwrap();
        db.set_password("test-hash").unwrap();
        let (token, csrf) = db.new_session().unwrap();
        db.put_record("terminal-ticket", "ticket", "h", &json!({"session":token}))
            .unwrap();
        let data=normalize_import(json!([{"id":"office","ssh_alias":"office","label":"Office","compose_dirs":[{"id":"/srv/media","path":"/srv/media","label":"Media"}]}]),prefs()).unwrap();
        assert_eq!(db.import_config(&data, true).unwrap(), (1, 1));
        assert_eq!(db.import_config(&data, true).unwrap(), (0, 0));
        let bytes = db.backup_bytes(&path).unwrap();
        for secret in [&token, &csrf] {
            assert!(
                !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
                "Session secret survived in SQLite bytes"
            );
        }
        assert_eq!(db.session(&token).unwrap(), Some(csrf));
        let backup = folder.join("download.sqlite");
        std::fs::write(&backup, bytes).unwrap();
        let restored = crate::db::Database::open(&backup).unwrap();
        assert!(restored.session(&token).unwrap().is_none());
        assert_eq!(restored.hosts().unwrap().len(), 1);
        assert_eq!(restored.password().unwrap().unwrap(), "test-hash");
        drop(restored);
        drop(db);
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn imported_connections_and_selected_schedules_follow_remapped_ids() {
        let folder = std::env::temp_dir().join(format!("dockmin-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&folder).unwrap();
        let db = crate::db::Database::open(&folder.join("db.sqlite")).unwrap();
        let mut data=normalize_import(json!([{"id":"old-host","ssh_alias":"new-pc","label":"New PC","compose_dirs":[{"id":"media","path":"/srv/media","label":"Media"}]}]),prefs()).unwrap();
        data.connections = vec![crate::onboarding::NewHost {
            name: "New PC".into(),
            alias: "new-pc".into(),
            address: "192.168.1.9".into(),
            user: "jim".into(),
            port: 22,
        }];
        data.automation = Some(crate::scheduler::Preferences {
            enabled: true,
            interval_minutes: 60,
            stack_ids: vec![data.stacks[0].id.clone()],
        });
        validate_import(&data).unwrap();
        db.import_config(&data, true).unwrap();
        let host = db.hosts().unwrap().remove(0);
        let stack = db.stacks().unwrap().remove(0);
        assert_eq!(
            db.record("ssh-host", &host.id).unwrap().unwrap()["address"],
            "192.168.1.9"
        );
        assert_eq!(
            db.get_setting("automation").unwrap().unwrap()["stack_ids"][0],
            stack.id
        );
        assert_eq!(db.import_config(&data, true).unwrap(), (0, 0));
        drop(db);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
