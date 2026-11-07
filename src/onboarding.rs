use crate::{
    api::{AppState, Result, ensure, missing},
    db::{Host, Stack},
    ssh,
};
use axum::{
    Json,
    extract::{Path, State},
};
use base64ct::{Base64, Base64Unpadded, Encoding};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path as FilePath, PathBuf},
    sync::Arc,
};
use tokio::process::Command;

fn host(s: &AppState, id: &str) -> Result<Host> {
    s.db.hosts()?
        .into_iter()
        .find(|h| h.id == id)
        .ok_or_else(missing)
}
fn read_aliases(
    path: &FilePath,
    depth: usize,
    values: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    anyhow::ensure!(depth < 8, "SSH Include nesting exceeds eight files");
    let text = std::fs::read_to_string(path)?;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some(words) = shlex::split(line) else {
            continue;
        };
        if words.len() < 2 {
            continue;
        }
        if words[0].eq_ignore_ascii_case("host") {
            for alias in &words[1..] {
                if ssh::validate_host(&Host {
                    id: String::new(),
                    name: alias.clone(),
                    alias: alias.clone(),
                })
                .is_ok()
                {
                    values.insert(alias.clone());
                }
            }
        } else if words[0].eq_ignore_ascii_case("include") {
            for pattern in &words[1..] {
                let pattern = if pattern.starts_with('/') {
                    PathBuf::from(pattern)
                } else {
                    let home = std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("/var/lib/dockmin"));
                    if pattern.starts_with("~/") {
                        home.join(pattern.trim_start_matches("~/"))
                    } else {
                        home.join(".ssh").join(pattern)
                    }
                };
                for entry in glob::glob(&pattern.to_string_lossy())?.flatten() {
                    read_aliases(&entry, depth + 1, values)?;
                }
            }
        }
    }
    Ok(())
}
pub async fn aliases(State(s): State<Arc<AppState>>) -> Result<Json<Value>> {
    let mut values = BTreeSet::new();
    read_aliases(&crate::config::ssh_file(&s.config), 0, &mut values)?;
    Ok(Json(json!({"aliases":values})))
}
struct Resolved {
    hostname: String,
    port: u16,
    user: String,
    identities: Vec<String>,
}
async fn resolve(s: &AppState, h: &Host) -> anyhow::Result<Resolved> {
    let mut cmd = Command::new("ssh");
    cmd.arg("-G")
        .arg("-F")
        .arg(crate::config::ssh_file(&s.config))
        .arg("--")
        .arg(&h.alias);
    let capture = ssh::capture(cmd, 10, 128 * 1024).await?;
    anyhow::ensure!(
        capture.success,
        "Cannot resolve SSH alias: {}",
        capture.stderr
    );
    let mut rows = BTreeMap::<String, Vec<String>>::new();
    for line in capture.stdout.lines() {
        if let Some((k, v)) = line.split_once(' ') {
            rows.entry(k.to_string())
                .or_default()
                .push(v.trim().to_string());
        }
    }
    let first = |key: &str| {
        rows.get(key)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    };
    for key in ["proxycommand", "proxyjump", "hostkeyalias"] {
        let value = first(key);
        anyhow::ensure!(
            value.is_empty() || value == "none",
            "Aliases using {key} require manual key enrollment"
        );
    }
    let hostname = first("hostname");
    anyhow::ensure!(
        !hostname.is_empty()
            && hostname.len() <= 253
            && !hostname.starts_with('-')
            && hostname
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_:".contains(&b)),
        "Invalid resolved SSH hostname"
    );
    let port = first("port").parse::<u16>()?;
    anyhow::ensure!(port > 0, "Invalid SSH port");
    Ok(Resolved {
        hostname,
        port,
        user: first("user"),
        identities: rows.remove("identityfile").unwrap_or_default(),
    })
}
pub async fn identity(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let h = host(&s, &id)?;
    let resolved = resolve(&s, &h).await?;
    let mut public_keys = vec![];
    for file in &resolved.identities {
        // Only read explicit absolute public-key files; never read private key bytes.
        if !file.starts_with('/') {
            continue;
        }
        if let Ok(raw) = std::fs::read_to_string(format!("{file}.pub")) {
            let words: Vec<_> = raw.split_whitespace().collect();
            if words.len() >= 2 && key_fingerprint(words[0], words[1]).is_ok() {
                public_keys.push(format!("restrict {} {} dockmin", words[0], words[1]));
            }
        }
    }
    Ok(Json(
        json!({"hostname":resolved.hostname,"port":resolved.port,"user":resolved.user,"public_keys":public_keys}),
    ))
}
pub async fn discover(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let h = host(&s, &id)?;
    let _lock = s.host_lock(&id).try_lock_owned().map_err(|_| {
        crate::api::ApiError(
            axum::http::StatusCode::CONFLICT,
            "Host is busy; try again after its operation finishes".into(),
        )
    })?;
    let _slot = s.try_slot().ok_or_else(|| {
        crate::api::ApiError(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "Service is busy; try discovery again shortly".into(),
        )
    })?;
    let remote = r#"ids=$(docker ps -aq) || exit; [ -z "$ids" ] || docker inspect --format '{"project":{{json (index .Config.Labels "com.docker.compose.project")}},"directory":{{json (index .Config.Labels "com.docker.compose.project.working_dir")}},"files":{{json (index .Config.Labels "com.docker.compose.project.config_files")}},"name":{{json .Name}}}' $ids"#;
    let output = ssh::capture(ssh::command(&s.config, &h, remote), 30, 128 * 1024).await?;
    ensure(
        output.success,
        &format!("Discovery failed: {}", output.stderr),
    )?;
    let (candidates, standalone) = parse_discovery(&id, &output.stdout)?;
    Ok(Json(json!({"stacks":candidates,"standalone":standalone})))
}
fn parse_discovery(host_id: &str, text: &str) -> anyhow::Result<(Vec<Stack>, Vec<String>)> {
    let mut groups = BTreeMap::new();
    let mut standalone = vec![];
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let row: Value = serde_json::from_str(line)?;
        let project = row["project"].as_str().unwrap_or_default();
        let directory = row["directory"].as_str().unwrap_or_default();
        if project.is_empty() || directory.is_empty() {
            standalone.push(
                row["name"]
                    .as_str()
                    .unwrap_or("Unknown container")
                    .trim_start_matches('/')
                    .to_string(),
            );
            continue;
        }
        let files = row["files"]
            .as_str()
            .unwrap_or_default()
            .split(',')
            .filter(|v| !v.is_empty())
            .map(String::from)
            .collect::<Vec<_>>();
        let candidate = Stack {
            id: String::new(),
            host_id: host_id.into(),
            name: project.into(),
            directory: directory.into(),
            project: project.into(),
            files,
        };
        ssh::validate_stack(&candidate)?;
        groups
            .entry(serde_json::to_string(&(
                project,
                directory,
                &candidate.files,
            ))?)
            .or_insert(candidate);
    }
    Ok((groups.into_values().collect(), standalone))
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Key {
    pub algorithm: String,
    pub key: String,
    pub fingerprint: String,
    pub trusted: bool,
}
fn key_fingerprint(algorithm: &str, key: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        [
            "ssh-ed25519",
            "ecdsa-sha2-nistp256",
            "ecdsa-sha2-nistp384",
            "ecdsa-sha2-nistp521",
            "ssh-rsa"
        ]
        .contains(&algorithm),
        "Unsupported host-key algorithm"
    );
    anyhow::ensure!(key.len() <= 8192, "Key is too large");
    let raw = Base64::decode_vec(key)?;
    anyhow::ensure!(raw.len() >= 4, "Invalid host key");
    let n = u32::from_be_bytes(raw[..4].try_into()?) as usize;
    anyhow::ensure!(
        raw.len() > 4 + n && raw.get(4..4 + n) == Some(algorithm.as_bytes()),
        "Key algorithm mismatch"
    );
    Ok(format!(
        "SHA256:{}",
        Base64Unpadded::encode_string(&Sha256::digest(raw))
    ))
}
fn lookup(resolved: &Resolved) -> String {
    if resolved.port == 22 {
        resolved.hostname.clone()
    } else {
        format!("[{}]:{}", resolved.hostname, resolved.port)
    }
}
async fn keys(s: &AppState, h: &Host) -> anyhow::Result<(Resolved, Vec<Key>, bool)> {
    let resolved = resolve(s, h).await?;
    let mut cmd = Command::new("ssh-keyscan");
    cmd.args(["-T", "5", "-p"])
        .arg(resolved.port.to_string())
        .args(["-t", "ed25519,ecdsa,rsa", "--"])
        .arg(&resolved.hostname);
    let scan = ssh::capture(cmd, 20, 128 * 1024).await?;
    anyhow::ensure!(scan.success, "Host-key scan failed: {}", scan.stderr);
    let known = if s.config.known_hosts.exists() {
        let mut cmd = Command::new("ssh-keygen");
        cmd.arg("-F")
            .arg(lookup(&resolved))
            .arg("-f")
            .arg(&s.config.known_hosts);
        ssh::capture(cmd, 10, 128 * 1024).await?.stdout
    } else {
        String::new()
    };
    let known_rows: Vec<_> = known
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let p: Vec<_> = l.split_whitespace().collect();
            if p.len() >= 3 {
                Some((p[1].to_string(), p[2].to_string()))
            } else {
                None
            }
        })
        .collect();
    let mut result = vec![];
    for line in scan.stdout.lines().filter(|l| !l.starts_with('#')) {
        let p: Vec<_> = line.split_whitespace().collect();
        if p.len() != 3 {
            continue;
        }
        let fingerprint = key_fingerprint(p[1], p[2])?;
        let trusted = known_rows.iter().any(|(a, k)| a == p[1] && k == p[2]);
        let key = Key {
            algorithm: p[1].into(),
            key: p[2].into(),
            fingerprint,
            trusted,
        };
        if !result.contains(&key) {
            result.push(key);
        }
    }
    anyhow::ensure!(!result.is_empty(), "No usable host keys returned");
    Ok((resolved, result, !known_rows.is_empty()))
}
pub async fn scan_keys(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let h = host(&s, &id)?;
    let (resolved, keys, has_trusted_keys) = keys(&s, &h).await?;
    Ok(Json(
        json!({"hostname":resolved.hostname,"port":resolved.port,"keys":keys,"has_trusted_keys":has_trusted_keys}),
    ))
}
#[derive(Deserialize)]
pub struct Trust {
    pub algorithm: String,
    pub key: String,
    pub fingerprint: String,
    pub confirm: String,
}
pub async fn trust_key(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Trust>,
) -> Result<Json<Value>> {
    let _gate = s.mutations.lock().await;
    let h = host(&s, &id)?;
    ensure(
        body.confirm == h.alias,
        "Confirm the SSH alias after independently verifying the fingerprint",
    )?;
    let (resolved, candidates, has_trusted) = keys(&s, &h).await?;
    let candidate = candidates
        .iter()
        .find(|k| {
            k.algorithm == body.algorithm && k.key == body.key && k.fingerprint == body.fingerprint
        })
        .ok_or_else(|| {
            crate::api::ApiError(
                axum::http::StatusCode::BAD_REQUEST,
                "Scanned key changed or fingerprint does not match; scan and verify again".into(),
            )
        })?;
    if candidate.trusted {
        return Ok(Json(json!({"trusted":true})));
    }
    ensure(
        !has_trusted,
        "This host already has trusted keys. Changed keys require manual verification and replacement on Pi2",
    )?;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = &s.config.known_hosts;
    let existing = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(anyhow::Error::from(e).into()),
    };
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("known_hosts must have a parent directory"))?;
    let temp = parent.join(format!(".known-hosts-{}", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(existing.as_bytes())?;
        writeln!(
            file,
            "\n{} {} {}",
            lookup(&resolved),
            candidate.algorithm,
            candidate.key
        )?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temp);
    result?;
    Ok(Json(json!({"trusted":true})))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compose_labels_group_containers_without_importing_standalone() {
        let row = r#"{"project":"media","directory":"/srv/media","files":"compose.yml,prod.yml","name":"/media-web-1"}"#;
        let text = format!("{row}\n{row}\n{{\"name\":\"/standalone\"}}");
        let (s, c) = parse_discovery("h", &text).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].files, vec!["compose.yml", "prod.yml"]);
        assert_eq!(c, vec!["standalone"]);
    }
    #[test]
    fn key_algorithm_must_match_wire_format() {
        assert!(
            key_fingerprint(
                "ssh-rsa",
                &Base64::encode_string(b"\0\0\0\x0bssh-ed25519fake")
            )
            .is_err()
        );
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewHost {
    pub name: String,
    pub alias: String,
    pub address: String,
    pub user: String,
    pub port: u16,
}
pub(crate) fn validate_connection(body: &NewHost) -> anyhow::Result<()> {
    ssh::validate_host(&Host {
        id: String::new(),
        name: body.name.clone(),
        alias: body.alias.clone(),
    })?;
    anyhow::ensure!(
        !body.address.is_empty()
            && body.address.len() <= 253
            && !body.address.starts_with('-')
            && body
                .address
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_:".contains(&b)),
        "Invalid hostname or IP address"
    );
    anyhow::ensure!(
        !body.user.is_empty()
            && body.user.len() <= 64
            && !body.user.starts_with('-')
            && body
                .user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "Invalid SSH user"
    );
    anyhow::ensure!(body.port > 0, "Invalid SSH port");
    Ok(())
}
fn config_quote(path: &FilePath) -> anyhow::Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("SSH paths must be UTF-8"))?;
    anyhow::ensure!(!text.chars().any(char::is_control), "Invalid SSH path");
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
fn write_managed(s: &AppState, entries: &[Value]) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = crate::config::state_directory(&s.config);
    let path = parent.join("managed_ssh_config");
    let temp = parent.join(format!(".managed-ssh-{}", uuid::Uuid::new_v4()));
    let mut text =
        String::from("# Managed by Dockmin. Change new-host connections in the web interface.\n");
    for value in entries {
        let connection: NewHost = serde_json::from_value(value.clone())?;
        validate_connection(&connection)?;
        text += &format!(
            "Host {}\n  HostName {}\n  User {}\n  Port {}\n  IdentityFile {}\n  IdentitiesOnly yes\n",
            connection.alias,
            connection.address,
            connection.user,
            connection.port,
            config_quote(&s.config.service_identity)?
        );
    }
    text += &format!("Host *\nInclude {}\n", config_quote(&s.config.ssh_config)?);
    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temp);
    result
}
pub async fn create_host(
    State(s): State<Arc<AppState>>,
    Json(body): Json<NewHost>,
) -> Result<Json<Value>> {
    validate_connection(&body)?;
    let _gate = s.mutations.lock().await;
    let mut aliases = BTreeSet::new();
    read_aliases(&crate::config::ssh_file(&s.config), 0, &mut aliases)?;
    ensure(
        !aliases.contains(&body.alias) && !s.db.hosts()?.iter().any(|h| h.alias == body.alias),
        "Alias already exists; register that existing SSH alias instead",
    )?;
    ensure(
        crate::config::public_identity(&s.config).is_file(),
        "Provision the service SSH identity on Pi2 first",
    )?;
    let h = Host {
        id: uuid::Uuid::new_v4().to_string(),
        name: body.name.clone(),
        alias: body.alias.clone(),
    };
    let value = serde_json::to_value(&body).map_err(anyhow::Error::from)?;
    let previous = s.db.records("ssh-host", "")?;
    let mut entries = previous.clone();
    entries.push(value.clone());
    write_managed(&s, &entries)?;
    if let Err(e) = s.db.add_managed_host(&h, &value) {
        let _ = write_managed(&s, &previous);
        return Err(e.into());
    }
    Ok(Json(json!({"host":h})))
}
pub async fn connection(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    host(&s, &id)?;
    Ok(Json(json!({"connection":s.db.record("ssh-host",&id)?})))
}
pub async fn save_connection(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<NewHost>,
) -> Result<Json<Value>> {
    validate_connection(&body)?;
    let _gate = s.mutations.lock().await;
    let h = host(&s, &id)?;
    ensure(
        h.alias == body.alias,
        "Keep the registered alias when editing its connection",
    )?;
    ensure(
        !s.db.active_on_host(&id)?,
        "Wait for this host’s operations to finish",
    )?;
    let previous = s.db.record("ssh-host", &id)?.ok_or_else(|| {
        crate::api::ApiError(
            axum::http::StatusCode::BAD_REQUEST,
            "This alias is administered in the original Pi2 SSH configuration".into(),
        )
    })?;
    let mut entries = s.db.records("ssh-host", "")?;
    let value = serde_json::to_value(&body).map_err(anyhow::Error::from)?;
    for item in &mut entries {
        if item["alias"] == h.alias {
            *item = value.clone();
        }
    }
    write_managed(&s, &entries)?;
    if let Err(e) = s.db.put_record("ssh-host", &id, "", &value) {
        for item in &mut entries {
            if item["alias"] == h.alias {
                *item = previous.clone();
            }
        }
        let _ = write_managed(&s, &entries);
        return Err(e.into());
    }
    s.db.save_host(&Host {
        id: h.id.clone(),
        name: body.name.clone(),
        alias: h.alias.clone(),
    })?;
    Ok(Json(json!({"connection":body})))
}
pub fn removed_host(s: &AppState, id: &str) -> anyhow::Result<()> {
    if s.db.take_record("ssh-host", id)?.is_some() {
        write_managed(s, &s.db.records("ssh-host", "")?)?;
    }
    Ok(())
}
#[derive(Deserialize)]
pub struct Bootstrap {
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub private_key: String,
    pub confirm: String,
}
pub async fn bootstrap(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Bootstrap>,
) -> Result<Json<Value>> {
    let h = host(&s, &id)?;
    ensure(
        body.confirm == h.alias,
        "Confirm the SSH alias to authorize Dockmin",
    )?;
    ensure(
        (!body.password.is_empty() || !body.private_key.is_empty())
            && body.password.len() <= 1024
            && !body.password.contains(['\n', '\r', '\0']),
        "Enter the remote account’s SSH password or one-time private key",
    )?;
    ensure(
        body.private_key.len() <= 64 * 1024
            && !body.private_key.contains('\0')
            && (body.private_key.is_empty() || body.private_key.starts_with("-----BEGIN ")),
        "Invalid one-time SSH private key",
    )?;
    let _gate = s.mutations.lock().await;
    let _lock = s.host_lock(&id).try_lock_owned().map_err(|_| {
        crate::api::ApiError(axum::http::StatusCode::CONFLICT, "Host is busy".into())
    })?;
    let _slot = s.try_slot().ok_or_else(|| {
        crate::api::ApiError(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "Service is busy".into(),
        )
    })?;
    let (_, scanned, _) = keys(&s, &h).await?;
    ensure(
        scanned.iter().any(|k| k.trusted),
        "Independently verify and enroll the host fingerprint before sending credentials",
    )?;
    let raw = std::fs::read_to_string(crate::config::public_identity(&s.config))
        .map_err(anyhow::Error::from)?;
    let parts: Vec<_> = raw.split_whitespace().collect();
    ensure(parts.len() >= 2, "Service public key is invalid")?;
    key_fingerprint(parts[0], parts[1])?;
    let public = format!("restrict {} {} dockmin", parts[0], parts[1]);
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let folder = crate::config::state_directory(&s.config)
        .join(format!(".bootstrap-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .map_err(anyhow::Error::from)?;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(folder.clone());
    let password_file = folder.join("password");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&password_file)
        .map_err(anyhow::Error::from)?
        .write_all(body.password.as_bytes())
        .map_err(anyhow::Error::from)?;
    let askpass = folder.join("askpass");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&askpass)
        .map_err(anyhow::Error::from)?
        .write_all(
            format!(
                "#!/bin/sh\nexec cat {}\n",
                ssh::quote(&password_file.to_string_lossy())
            )
            .as_bytes(),
        )
        .map_err(anyhow::Error::from)?;
    let original = ssh::stdin_command(&s.config, &h);
    let args: Vec<_> = original.as_std().get_args().map(|a| a.to_owned()).collect();
    let mut command = Command::new("ssh");
    command.args(["-o", "BatchMode=no", "-o", "NumberOfPasswordPrompts=1"]);
    if body.private_key.is_empty() {
        command.args([
            "-o",
            "PubkeyAuthentication=no",
            "-o",
            "PreferredAuthentications=password,keyboard-interactive",
        ]);
    } else {
        let key = folder.join("identity");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&key)
            .map_err(anyhow::Error::from)?
            .write_all(body.private_key.as_bytes())
            .map_err(anyhow::Error::from)?;
        command
            .args([
                "-o",
                "PreferredAuthentications=publickey",
                "-o",
                "IdentitiesOnly=yes",
            ])
            .arg("-i")
            .arg(key);
    }
    command
        .args(args)
        .env("SSH_ASKPASS", &askpass)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "dockmin")
        .env_remove("SSH_AUTH_SOCK");
    let script = crate::remote::python(
        "folder=pathlib.Path.home()/'.ssh'\nfolder.mkdir(mode=0o700,exist_ok=True)\nos.chmod(folder,0o700)\np=folder/'authorized_keys'\nlines=p.read_text().splitlines() if p.exists() else []\nif data['key'].split()[2] not in [part for line in lines for part in line.split()]:\n    with p.open('a') as handle:handle.write('\\n'+data['key']+'\\n')\nos.chmod(p,0o600)\nprint('Dockmin public key authorized.',flush=True)",
        &json!({"key":public}),
    );
    let result = ssh::capture_input(command, Some(script), 45, 8192).await?;
    ensure(
        result.success,
        &format!(
            "SSH authorization failed: {}",
            result.stderr.chars().take(500).collect::<String>()
        ),
    )?;
    let test = ssh::capture(
        ssh::command(&s.config, &h, &ssh::remote_command(None, "test")?),
        30,
        8192,
    )
    .await?;
    Ok(Json(
        json!({"authorized":true,"docker_ready":test.success,"detail":if test.success{test.stdout}else{test.stderr}}),
    ))
}

pub fn clean_bootstrap_files(config: &crate::config::Config) -> anyhow::Result<()> {
    let parent = crate::config::state_directory(config);
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir()
            && name
                .strip_prefix(".bootstrap-")
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

pub fn sync_managed(s: &AppState) -> anyhow::Result<()> {
    let entries = s.db.records("ssh-host", "")?;
    if !entries.is_empty() {
        write_managed(s, &entries)?;
    }
    Ok(())
}
pub fn original_aliases(s: &AppState) -> anyhow::Result<BTreeSet<String>> {
    let mut values = BTreeSet::new();
    read_aliases(&s.config.ssh_config, 0, &mut values)?;
    Ok(values)
}
