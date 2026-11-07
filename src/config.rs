use serde::Deserialize;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: String,
    pub origins: Vec<String>,
    pub database: PathBuf,
    pub static_dir: PathBuf,
    pub ssh_config: PathBuf,
    pub known_hosts: PathBuf,
    #[serde(default = "identity")]
    pub service_identity: PathBuf,
    #[serde(default = "yes")]
    pub secure_cookie: bool,
    #[serde(default = "parallel")]
    pub concurrency: usize,
    #[serde(default = "health")]
    pub health_refresh_seconds: u64,
    #[serde(default = "timeout")]
    pub command_timeout_seconds: u64,
}
fn identity() -> PathBuf {
    PathBuf::from("/etc/dockmin/id_ed25519")
}
fn yes() -> bool {
    true
}
fn parallel() -> usize {
    3
}
fn health() -> u64 {
    60
}
fn timeout() -> u64 {
    1800
}
impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let cfg: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        anyhow::ensure!(
            !cfg.origins.is_empty(),
            "Configure at least one exact allowed origin"
        );
        anyhow::ensure!(
            cfg.concurrency > 0 && cfg.concurrency <= 16,
            "Concurrency must be 1–16"
        );
        anyhow::ensure!(
            (10..=7200).contains(&cfg.command_timeout_seconds),
            "Command timeout must be 10–7200 seconds"
        );
        anyhow::ensure!(
            cfg.health_refresh_seconds == 0 || (15..=3600).contains(&cfg.health_refresh_seconds),
            "Health refresh must be off or 15–3600 seconds"
        );
        let mut origins = HashSet::new();
        for origin in &cfg.origins {
            let uri: axum::http::Uri = origin.parse()?;
            anyhow::ensure!(
                matches!(uri.scheme_str(), Some("http" | "https"))
                    && uri.authority().is_some()
                    && uri.path_and_query().is_none_or(|p| p.as_str() == "/")
                    && !origin.ends_with('/'),
                "Origins must be exact scheme://host[:port] values"
            );
            anyhow::ensure!(
                !cfg.secure_cookie || uri.scheme_str() == Some("https"),
                "secure_cookie requires HTTPS origins"
            );
            if !cfg.secure_cookie {
                let host = uri.host().unwrap_or_default();
                anyhow::ensure!(
                    matches!(host, "localhost" | "127.0.0.1" | "[::1]"),
                    "Insecure cookies are allowed only for loopback development origins"
                );
            }
            anyhow::ensure!(origins.insert(origin), "Duplicate origin");
        }
        Ok(cfg)
    }
}

pub fn ssh_file(cfg: &Config) -> PathBuf {
    let managed = cfg
        .database
        .parent()
        .unwrap_or(Path::new("."))
        .join("managed_ssh_config");
    if managed.is_file() {
        managed
    } else {
        cfg.ssh_config.clone()
    }
}

pub fn public_identity(cfg: &Config) -> PathBuf {
    let mut path = cfg.service_identity.as_os_str().to_os_string();
    path.push(".pub");
    PathBuf::from(path)
}

pub fn state_directory(cfg: &Config) -> &Path {
    cfg.database
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
