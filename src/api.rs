use crate::{
    auth,
    config::Config,
    db::{Database, Host, Stack, now},
    jobs, ssh,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    middleware,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex},
};

pub struct AppState {
    pub config: Config,
    pub db: Database,
    pub host_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub runtime: std::sync::RwLock<crate::settings::Runtime>,
    pub running: std::sync::atomic::AtomicUsize,
    pub slot_changed: tokio::sync::Notify,
    pub health_changed: tokio::sync::Notify,
    pub shutdown_changed: tokio::sync::Notify,
    pub force_health: std::sync::atomic::AtomicBool,
    pub stopping: std::sync::atomic::AtomicBool,
    pub backup_lock: tokio::sync::Mutex<()>,
    pub terminal_slots: Arc<tokio::sync::Semaphore>,
    pub download_slots: Arc<tokio::sync::Semaphore>,
    pub log_slots: Arc<tokio::sync::Semaphore>,
    pub(crate) mutations: tokio::sync::Mutex<()>,
    pub(crate) login_lock: tokio::sync::Mutex<()>,
}
impl AppState {
    pub fn new(config: Config, db: Database) -> anyhow::Result<Self> {
        crate::onboarding::clean_bootstrap_files(&config)?;
        let runtime = db
            .get_setting("runtime")?
            .map(|v| serde_json::from_value(v))
            .transpose()?
            .unwrap_or_else(|| crate::settings::Runtime::defaults(&config));
        runtime.validate()?;
        let state = Self {
            runtime: std::sync::RwLock::new(runtime),
            running: std::sync::atomic::AtomicUsize::new(0),
            slot_changed: tokio::sync::Notify::new(),
            health_changed: tokio::sync::Notify::new(),
            shutdown_changed: tokio::sync::Notify::new(),
            force_health: std::sync::atomic::AtomicBool::new(false),
            stopping: std::sync::atomic::AtomicBool::new(false),
            backup_lock: tokio::sync::Mutex::new(()),
            terminal_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            download_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            log_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            config,
            db,
            host_locks: Mutex::new(HashMap::new()),
            mutations: tokio::sync::Mutex::new(()),
            login_lock: tokio::sync::Mutex::new(()),
        };
        crate::onboarding::sync_managed(&state)?;
        Ok(state)
    }
    pub fn host_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.host_locks
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
}
pub(crate) struct ApiError(pub(crate) StatusCode, pub(crate) String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        eprintln!("API error: {e}");
        Self(StatusCode::BAD_REQUEST, e.to_string())
    }
}
pub(crate) type Result<T> = std::result::Result<T, ApiError>;
pub(crate) fn ensure(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(ApiError(StatusCode::BAD_REQUEST, message.into()))
    }
}
pub(crate) fn missing() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "Not found".into())
}
pub fn router(state: Arc<AppState>) -> Router {
    let protected = Router::new()
        .route("/api/session", get(session))
        .route("/api/logout", post(logout))
        .route("/api/overview", get(overview))
        .route(
            "/api/settings",
            get(crate::data::settings).post(crate::data::save_settings),
        )
        .route(
            "/api/automation",
            get(crate::scheduler::get).post(crate::scheduler::save),
        )
        .route("/api/ntfy", get(crate::ntfy::get).post(crate::ntfy::save))
        .route("/api/ntfy/test", post(crate::ntfy::test))
        .route("/api/notifications/read", post(crate::scheduler::read))
        .route("/api/password", post(crate::data::change_password))
        .route("/api/health/refresh", post(crate::data::refresh_health))
        .route("/api/config/export", get(crate::data::export))
        .route(
            "/api/config/import",
            post(crate::data::import).layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .route("/api/backups", post(crate::data::backup))
        .route("/api/stacks/{id}/logs", get(crate::logs::stream))
        .route("/api/ssh/aliases", get(crate::onboarding::aliases))
        .route(
            "/api/hosts/{id}/discover",
            post(crate::onboarding::discover),
        )
        .route("/api/hosts/{id}/identity", get(crate::onboarding::identity))
        .route("/api/hosts/{id}/keys", post(crate::onboarding::scan_keys))
        .route("/api/hosts/{id}/trust", post(crate::onboarding::trust_key))
        .route(
            "/api/hosts/{id}/containers",
            get(crate::containers::list)
                .post(crate::containers::create)
                .layer(DefaultBodyLimit::max(256 * 1024)),
        )
        .route(
            "/api/hosts/{id}/container-checkpoints",
            get(crate::containers::checkpoints),
        )
        .route(
            "/api/hosts/{id}/containers/{cid}",
            get(crate::containers::inspect),
        )
        .route(
            "/api/hosts/{id}/containers/{cid}/actions",
            post(crate::containers::action),
        )
        .route(
            "/api/hosts/{id}/containers/{cid}/logs",
            get(crate::containers::stream),
        )
        .route(
            "/api/hosts/{id}/containers/{cid}/terminal-ticket",
            post(crate::terminal::ticket),
        )
        .route(
            "/api/hosts/{id}/containers/{cid}/terminal",
            get(crate::terminal::connect),
        )
        .route("/api/hosts/new", post(crate::onboarding::create_host))
        .route(
            "/api/hosts/{id}/connection",
            get(crate::onboarding::connection).post(crate::onboarding::save_connection),
        )
        .route(
            "/api/hosts/{id}/bootstrap",
            post(crate::onboarding::bootstrap).layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route("/api/hosts", post(save_host))
        .route("/api/hosts/{id}", delete(delete_host))
        .route(
            "/api/stacks/create",
            post(crate::compose_files::create).layer(DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route(
            "/api/stacks/{id}/compose",
            get(crate::compose_files::get)
                .post(crate::compose_files::save)
                .layer(DefaultBodyLimit::max(4 * 1024 * 1024)),
        )
        .route(
            "/api/stacks/{id}/compose/preview",
            post(crate::compose_files::preview).layer(DefaultBodyLimit::max(4 * 1024 * 1024)),
        )
        .route("/api/stacks/{id}/checkpoints", get(crate::recovery::list))
        .route(
            "/api/stacks/{id}/deployment-policy",
            post(crate::recovery::save_policy),
        )
        .route("/api/stacks/{id}/deploy", post(crate::recovery::deploy))
        .route("/api/stacks/{id}/rollback", post(crate::recovery::rollback))
        .route("/api/stacks/{id}/data", get(crate::stack_data::mounts))
        .route(
            "/api/stacks/{id}/data-backups",
            get(crate::stack_data::list).post(crate::stack_data::backup),
        )
        .route("/api/stacks/{id}/restore", post(crate::stack_data::restore))
        .route(
            "/api/stacks/{id}/data-backups/{bid}/download",
            get(crate::stack_data::download),
        )
        .route(
            "/api/stacks/{id}/data-backups/import",
            post(crate::stack_data::import).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/stacks", post(save_stack))
        .route("/api/stacks/{id}", delete(delete_stack))
        .route("/api/hosts/{id}/actions", post(host_action))
        .route("/api/stacks/{id}/actions", post(stack_action))
        .route("/api/bulk", post(bulk))
        .route("/api/jobs/{id}", get(job))
        .route("/api/jobs/{id}/events", get(events))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::guard));
    Router::new()
        .merge(protected)
        .route("/api/login", post(login))
        .route(
            "/api/health",
            get(|| async { Json(json!({"status":"ok","version":env!("CARGO_PKG_VERSION")})) }),
        )
        .fallback_service(tower_http::services::ServeDir::new(
            &state.config.static_dir,
        ))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(headers))
        .with_state(state)
}
async fn headers(req: axum::extract::Request, next: middleware::Next) -> Response {
    let mut r = next.run(req).await;
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    ] {
        r.headers_mut().insert(
            axum::http::HeaderName::from_static(name),
            axum::http::HeaderValue::from_static(value),
        );
    }
    r
}
pub(crate) fn cookie(state: &AppState, token: &str, age: u32) -> String {
    format!(
        "dockmin_session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={age}{}",
        if state.config.secure_cookie {
            "; Secure"
        } else {
            ""
        }
    )
}
#[derive(Deserialize)]
struct Login {
    password: String,
}
async fn login(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Login>,
) -> Result<Response> {
    if !auth::origin_valid(&s, &headers) {
        return Err(ApiError(StatusCode::FORBIDDEN, "Origin not allowed".into()));
    }
    if body.password.len() > 1024 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Password too long".into(),
        ));
    }
    // Global serialization and delay prevent concurrent expensive password checks.
    let Ok(_gate) = s.login_lock.try_lock() else {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Please wait before signing in again".into(),
        ));
    };
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let hash = s.db.password()?.ok_or_else(missing)?;
    let valid = tokio::task::spawn_blocking(move || auth::verify(&body.password, &hash))
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    if !valid {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Incorrect password".into(),
        ));
    }
    let (token, csrf) = s.db.new_session()?;
    let mut response = Json(json!({"csrf":csrf})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie(&s, &token, 43200).parse().unwrap(),
    );
    Ok(response)
}
async fn session(State(s): State<Arc<AppState>>, headers: HeaderMap) -> Result<Json<Value>> {
    Ok(Json(
        json!({"csrf":s.db.session(auth::token(&headers).unwrap_or_default())?.ok_or_else(missing)?}),
    ))
}
async fn logout(State(s): State<Arc<AppState>>, headers: HeaderMap) -> Result<Response> {
    s.db.logout(auth::token(&headers).unwrap_or_default())?;
    let mut r = Json(json!({"ok":true})).into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, cookie(&s, "", 0).parse().unwrap());
    Ok(r)
}
async fn overview(State(s): State<Arc<AppState>>) -> Result<Json<Value>> {
    let all = s.db.jobs()?;
    let statuses = s.db.statuses()?;
    let stacks = s.db.stacks()?;
    let updates: std::collections::HashMap<_, _> = stacks
        .iter()
        .map(|stack| (stack.id.clone(), crate::updates::for_stack(&all, &stack.id)))
        .collect();
    let summaries:Vec<_>=all.iter().map(|j|json!({"id":j.id,"host_id":j.host_id,"stack_id":j.stack_id,"action":j.action,"state":j.state,"created":j.created,"finished":j.finished,"result":if j.action=="check-updates" && j.finished.is_some(){Some(j.output.chars().take(2000).collect::<String>())}else{None}})).collect();
    Ok(Json(
        json!({"hosts":s.db.hosts()?,"stacks":stacks,"jobs":summaries,"statuses":statuses,"updates":updates,"server_time":now(),"settings":s.prefs(),"host_health":s.db.health()?,"notifications":s.db.notifications()?,"automation":crate::scheduler::preferences(&s)?}),
    ))
}
fn busy(s: &AppState, host: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !s.host_locks
            .lock()
            .unwrap()
            .get(host)
            .is_some_and(|lock| lock.try_lock().is_err()),
        "Host has a running operation; wait before editing its configuration"
    );
    anyhow::ensure!(
        !s.db
            .jobs()?
            .iter()
            .any(|j| j.host_id == host && j.finished.is_none()),
        "Host has queued or running operations; wait before editing its configuration"
    );
    Ok(())
}
async fn save_host(State(s): State<Arc<AppState>>, Json(mut h): Json<Host>) -> Result<Json<Host>> {
    let _gate = s.mutations.lock().await;
    ssh::validate_host(&h)?;
    if h.id.is_empty() {
        h.id = uuid::Uuid::new_v4().to_string()
    } else {
        if !s.db.hosts()?.iter().any(|v| v.id == h.id) {
            return Err(missing());
        }
        busy(&s, &h.id)?;
    }
    s.db.save_host(&h)?;
    Ok(Json(h))
}
async fn delete_host(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let _gate = s.mutations.lock().await;
    busy(&s, &id)?;
    s.db.delete_host(&id)?;
    crate::onboarding::removed_host(&s, &id)?;
    Ok(Json(json!({"ok":true})))
}
async fn save_stack(
    State(s): State<Arc<AppState>>,
    Json(mut stack): Json<Stack>,
) -> Result<Json<Stack>> {
    let _gate = s.mutations.lock().await;
    ssh::validate_stack(&stack)?;
    if !s.db.hosts()?.iter().any(|h| h.id == stack.host_id) {
        return Err(missing());
    }
    busy(&s, &stack.host_id)?;
    if stack.id.is_empty() {
        stack.id = uuid::Uuid::new_v4().to_string()
    } else {
        let old =
            s.db.stacks()?
                .into_iter()
                .find(|v| v.id == stack.id)
                .ok_or_else(missing)?;
        busy(&s, &old.host_id)?;
    }
    s.db.save_stack(&stack)?;
    Ok(Json(stack))
}
async fn delete_stack(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let _gate = s.mutations.lock().await;
    let old =
        s.db.stacks()?
            .into_iter()
            .find(|v| v.id == id)
            .ok_or_else(missing)?;
    busy(&s, &old.host_id)?;
    s.db.delete_stack(&id)?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
struct Action {
    action: String,
    #[serde(default)]
    confirm: String,
}
fn confirm(body: &Action, name: &str) -> anyhow::Result<()> {
    if matches!(
        body.action.as_str(),
        "stop" | "restart" | "update" | "prune-images" | "prune-containers" | "prune-build"
    ) {
        anyhow::ensure!(
            body.confirm == name,
            "Confirm the exact target name before this operation"
        );
    }
    Ok(())
}
async fn host_action(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Action>,
) -> Result<Json<crate::db::Job>> {
    let _gate = s.mutations.lock().await;
    let host =
        s.db.hosts()?
            .into_iter()
            .find(|h| h.id == id)
            .ok_or_else(missing)?;
    confirm(&body, &host.name)?;
    Ok(Json(
        jobs::enqueue(s.clone(), host, None, body.action).await?,
    ))
}
async fn stack_action(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Action>,
) -> Result<Json<crate::db::Job>> {
    let _gate = s.mutations.lock().await;
    let stack =
        s.db.stacks()?
            .into_iter()
            .find(|v| v.id == id)
            .ok_or_else(missing)?;
    confirm(&body, &stack.name)?;
    let host =
        s.db.hosts()?
            .into_iter()
            .find(|h| h.id == stack.host_id)
            .ok_or_else(missing)?;
    Ok(Json(
        jobs::enqueue(s.clone(), host, Some(stack), body.action).await?,
    ))
}
#[derive(Deserialize)]
struct Bulk {
    #[serde(default)]
    only_available: bool,
    stack_ids: Vec<String>,
    action: String,
    #[serde(default)]
    confirm: String,
}
async fn bulk(State(s): State<Arc<AppState>>, Json(body): Json<Bulk>) -> Result<Json<Value>> {
    let _gate = s.mutations.lock().await;
    ensure(
        matches!(body.action.as_str(), "refresh" | "update" | "check-updates"),
        "Bulk supports refresh, update, or check-updates",
    )?;
    ensure(
        !body.stack_ids.is_empty() && body.stack_ids.len() <= 32,
        "Select 1–32 stacks",
    )?;
    if body.action == "update" {
        ensure(body.confirm == "UPDATE SELECTED", "Confirm UPDATE SELECTED")?;
    }
    let stacks = s.db.stacks()?;
    let hosts = s.db.hosts()?;
    let recent = s.db.jobs()?;
    let mut targets = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for id in &body.stack_ids {
        ensure(ids.insert(id), "Duplicate stack target")?;
        if body.only_available {
            ensure(
                body.action == "update"
                    && crate::updates::for_stack(&recent, id).state == "available",
                "Update results changed. Check for updates again before updating.",
            )?;
        }
        let stack = stacks
            .iter()
            .find(|v| &v.id == id)
            .ok_or_else(missing)?
            .clone();
        let host = hosts
            .iter()
            .find(|h| h.id == stack.host_id)
            .ok_or_else(missing)?
            .clone();
        targets.push((host, stack));
    }
    ensure(
        s.db.jobs()?.iter().filter(|j| j.finished.is_none()).count() + targets.len() <= 64,
        "Job queue is full",
    )?;
    let mut result = Vec::new();
    for (host, stack) in targets {
        result.push(jobs::enqueue(s.clone(), host, Some(stack), body.action.clone()).await?);
    }
    Ok(Json(json!({"jobs":result})))
}
async fn job(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<crate::db::Job>> {
    Ok(Json(s.db.job(&id)?.ok_or_else(missing)?))
}
async fn events(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>>> {
    s.db.job(&id)?.ok_or_else(missing)?;
    let token = auth::token(&headers).unwrap_or_default().to_string();
    let stream = async_stream::stream! {
        let mut previous=String::new();
        loop {
            if s.db.session(&token).ok().flatten().is_none(){break;}
            match s.db.job(&id) {
                Ok(Some(job))=>{let done=job.finished.is_some();let data=serde_json::to_string(&job).unwrap_or_default();if data!=previous{yield Ok(Event::default().event("job").data(data.clone()));previous=data;}if done{break;}},
                _=>break,
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
