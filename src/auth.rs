use crate::api::AppState;
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!(e.to_string()))
}
pub fn verify(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).ok().is_some_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}
pub fn token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|c| c.trim().strip_prefix("dockmin_session="))
}
pub fn origin_valid(state: &AppState, headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(header::ORIGIN)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|o| state.config.origins.iter().any(|v| v == o))
}
pub async fn guard(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let csrf = token(req.headers()).and_then(|t| state.db.session(t).ok().flatten());
    let Some(csrf) = csrf else {
        return (StatusCode::UNAUTHORIZED, "Please sign in").into_response();
    };
    if req.method() != axum::http::Method::GET && req.method() != axum::http::Method::HEAD {
        if !origin_valid(&state, req.headers())
            || req
                .headers()
                .get("x-csrf-token")
                .and_then(|h| h.to_str().ok())
                != Some(&csrf)
        {
            return (StatusCode::FORBIDDEN, "Invalid origin or CSRF token").into_response();
        }
    }
    next.run(req).await
}
