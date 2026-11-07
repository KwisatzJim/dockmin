use crate::{
    config::Config,
    db::{Host, Stack},
};
use std::process::Stdio;
use tokio::process::Command;

pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn validate_host(h: &Host) -> anyhow::Result<()> {
    anyhow::ensure!(
        !h.name.trim().is_empty() && h.name.len() <= 100,
        "Host name is required (max 100 bytes)"
    );
    anyhow::ensure!(
        !h.alias.is_empty()
            && h.alias.len() <= 100
            && h.alias
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            && !h.alias.starts_with('-'),
        "SSH alias must contain only letters, numbers, dots, underscores, or hyphens and cannot start with a hyphen"
    );
    Ok(())
}
pub fn validate_stack(s: &Stack) -> anyhow::Result<()> {
    anyhow::ensure!(
        !s.name.trim().is_empty() && s.name.len() <= 100,
        "Stack name is required (max 100 bytes)"
    );
    anyhow::ensure!(
        s.directory.starts_with('/')
            && s.directory.len() <= 4096
            && !s.directory.chars().any(char::is_control),
        "Stack directory must be an absolute path without control characters"
    );
    anyhow::ensure!(
        s.project.len() <= 100
            && s.project
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
            && !s.project.starts_with(['-', '_']),
        "Invalid Compose project name"
    );
    anyhow::ensure!(
        s.files.len() <= 8
            && s.files
                .iter()
                .all(|f| !f.is_empty() && f.len() <= 4096 && !f.chars().any(char::is_control)),
        "Invalid Compose files (max 8)"
    );
    Ok(())
}
pub fn compose(s: &Stack) -> String {
    let mut cmd = "docker compose".to_string();
    if !s.project.is_empty() {
        cmd.push_str(&format!(" --project-name {}", quote(&s.project)));
    }
    for file in &s.files {
        cmd.push_str(&format!(" --file {}", quote(file)));
    }
    cmd
}
pub fn remote_command(stack: Option<&Stack>, action: &str) -> anyhow::Result<String> {
    if let Some(s) = stack {
        let c = compose(s);
        let sub = match action {
            "refresh" => format!("{c} ps --all --format json"),
            "start" => {
                return Ok(crate::remote::python(
                    "result=compose(['up','-d'])\nprint(result.stdout,flush=True)",
                    &serde_json::json!({"stack":s}),
                ));
            }
            "stop" => format!("{c} stop"),
            "restart" => format!("{c} restart"),
            "update" => format!("{c} pull && {c} up -d"),
            "check-updates" => format!(
                r#"command -v docker >/dev/null && docker buildx version >/dev/null && ids=$({c} ps -q) && for id in $ids; do
image=$(docker inspect --format '{{{{.Config.Image}}}}' "$id") || continue
printf 'DOCKMIN_IMAGE %s\n' "$image"
image_id=$(docker inspect --format '{{{{.Image}}}}' "$id")
printf 'LOCAL\n'
docker image inspect --format '{{"RepoDigests":{{{{json .RepoDigests}}}},"Os":{{{{json .Os}}}},"Architecture":{{{{json .Architecture}}}},"Variant":{{{{json .Variant}}}}}}' "$image_id" 2>/dev/null || printf '{{}}\n'
printf 'REMOTE\n'
docker buildx imagetools inspect "$image" --format '{{{{json .Manifest}}}}' 2>/dev/null || printf '{{}}\n'
printf '\nEND_IMAGE\n'
done"#
            ),
            "logs" => format!("{c} logs --no-color --tail 150"),
            _ => anyhow::bail!("Unsupported stack action"),
        };
        Ok(format!("cd -- {} && {sub}", quote(&s.directory)))
    } else {
        Ok(match action {
            "test" => {
                "docker version --format '{{.Server.Version}}' && docker compose version".into()
            }
            "disk" => "docker system df".into(),
            "prune-images" => "docker image prune --force".into(),
            "prune-build" => "docker builder prune --force".into(),
            _ => anyhow::bail!("Unsupported host action"),
        })
    }
}
pub fn shell_wrapper(remote: &str) -> String {
    use base64ct::{Base64, Encoding};
    format!(
        r#"printf %s {} | base64 -d | sh -c 'script=$(cat); [ -n "$script" ] || exit 125; exec sh -c "$script"'"#,
        Base64::encode_string(remote.as_bytes())
    )
}
pub fn command(cfg: &Config, h: &Host, remote: &str) -> Command {
    let mut c = Command::new("ssh");
    c.arg("-F")
        .arg(crate::config::ssh_file(cfg))
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", cfg.known_hosts.display()))
        .args([
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=2",
            "-T",
            "--",
        ])
        .arg(&h.alias)
        .arg(shell_wrapper(remote))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    c
}
pub fn parse_status(output: &str) -> anyhow::Result<Vec<serde_json::Value>> {
    let output = output.trim();
    if output.is_empty() {
        return Ok(vec![]);
    }
    if output.starts_with('[') {
        return Ok(serde_json::from_str(output)?);
    }
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(Into::into))
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn stack() -> Stack {
        Stack {
            id: "s".into(),
            host_id: "h".into(),
            name: "test".into(),
            directory: "/srv/it's $(touch /tmp/evil)".into(),
            project: "test".into(),
            files: vec!["compose.yml".into(), "prod.yml".into()],
        }
    }
    #[test]
    fn paths_are_shell_quoted() {
        let cmd = remote_command(Some(&stack()), "update").unwrap();
        assert!(cmd.starts_with("cd -- '/srv/it'\\''s $(touch /tmp/evil)' &&"));
        assert!(cmd.contains("--file 'prod.yml' pull && docker compose"));
    }
    #[test]
    fn start_keeps_base_files_before_rollback_overrides() {
        use std::os::unix::fs::PermissionsExt;
        let folder = std::env::temp_dir().join(format!("dockmin-start-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(folder.join("bin")).unwrap();
        std::fs::create_dir_all(folder.join(".dockmin")).unwrap();
        let docker = folder.join("bin/docker");
        std::fs::write(&docker, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(folder.join("compose.yaml"), "services: {}\n").unwrap();
        let active = folder.join(".dockmin/active-images.json");
        let mut target = stack();
        target.directory = folder.to_string_lossy().into_owned();
        for files in [vec![], vec!["compose.yaml".into(), "prod.yml".into()]] {
            target.files = files;
            for rollback in [false, true] {
                if rollback {
                    std::fs::write(&active, "{}").unwrap();
                } else if active.exists() {
                    std::fs::remove_file(&active).unwrap();
                }
                let command = remote_command(Some(&target), "start").unwrap();
                let output = std::process::Command::new("sh")
                    .args(["-c", &command])
                    .env(
                        "PATH",
                        format!(
                            "{}:{}",
                            folder.join("bin").display(),
                            std::env::var("PATH").unwrap()
                        ),
                    )
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let mut expected = vec![
                    "compose".to_string(),
                    "--project-name".into(),
                    target.project.clone(),
                ];
                let sources = if target.files.is_empty() {
                    vec!["compose.yaml".to_string()]
                } else {
                    target.files.clone()
                };
                for file in sources {
                    expected.extend([
                        "--file".into(),
                        folder.join(file).to_string_lossy().into_owned(),
                    ]);
                }
                if rollback {
                    expected.extend(["--file".into(), active.to_string_lossy().into_owned()]);
                }
                expected.extend([
                    "--project-directory".into(),
                    target.directory.clone(),
                    "up".into(),
                    "-d".into(),
                ]);
                let stdout = String::from_utf8(output.stdout).unwrap();
                assert_eq!(
                    stdout
                        .lines()
                        .filter(|line| !line.is_empty())
                        .collect::<Vec<_>>(),
                    expected
                );
            }
        }
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn reject_options_as_hostnames() {
        let h = Host {
            id: "h".into(),
            name: "h".into(),
            alias: "-oProxyCommand=evil".into(),
        };
        assert!(validate_host(&h).is_err());
    }
    #[test]
    fn no_arbitrary_actions() {
        assert!(remote_command(Some(&stack()), "down --volumes").is_err());
        assert!(remote_command(None, "prune-volumes").is_err());
    }
    #[test]
    fn image_command_has_valid_shell_syntax_and_only_safe_metadata() {
        let cmd = remote_command(Some(&stack()), "check-updates").unwrap();
        assert!(!cmd.contains("{{json .}}"));
        assert!(cmd.contains("\"RepoDigests\":{{json .RepoDigests}}"));
        assert!(cmd.contains("\"Variant\":{{json .Variant}}}"));
        assert!(
            std::process::Command::new("sh")
                .args(["-n", "-c", &cmd])
                .status()
                .unwrap()
                .success()
        );
    }
    #[test]
    fn remote_scripts_work_with_posix_and_fish_login_shells() {
        let value = "/srv/it's $(printf substituted)";
        let script = format!("printf '%s' {}", quote(value));
        let wrapped = shell_wrapper(&script);
        for shell in ["sh", "/usr/bin/fish"] {
            if shell == "/usr/bin/fish" && !std::path::Path::new(shell).exists() {
                continue;
            }
            let mut command = std::process::Command::new(shell);
            if shell.ends_with("fish") {
                command.args(["--no-config", "--private"]);
            }
            let output = command.args(["-c", &wrapped]).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&output.stdout), value);
        }
    }
    #[test]
    fn registry_output_without_trailing_newline_is_framed_correctly() {
        use std::os::unix::fs::PermissionsExt;
        let folder =
            std::env::temp_dir().join(format!("dockmin-image-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(folder.join("bin")).unwrap();
        let fake = folder.join("bin/docker");
        std::fs::write(&fake,r#"#!/bin/sh
case "$1 $2" in
  'buildx version') exit 0 ;;
  'compose --project-name') printf 'container1\n' ;;
  'inspect --format') case "$3" in
      *Config.Image*) printf 'app:latest\n' ;;
      *) printf 'sha256:local\n' ;;
    esac ;;
  'image inspect') printf '%s\n' '{"RepoDigests":["app@sha256:current"],"Os":"linux","Architecture":"amd64","Variant":""}' ;;
  'buildx imagetools') printf '%s' '{"digest":"sha256:current"}' ;;
  *) exit 2 ;;
esac
"#).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut target = stack();
        target.directory = folder.to_string_lossy().into_owned();
        let command = remote_command(Some(&target), "check-updates").unwrap();
        let output = std::process::Command::new("sh")
            .args(["-c", &command])
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", folder.join("bin").display()),
            )
            .output()
            .unwrap();
        let summary = crate::images::summarize(&String::from_utf8_lossy(&output.stdout));
        let _ = std::fs::remove_dir_all(&folder);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(summary.contains("up to date"), "{summary}");
    }
    #[test]
    fn compose_json_formats() {
        assert_eq!(parse_status("[{\"State\":\"running\"}]").unwrap().len(), 1);
        assert_eq!(
            parse_status("{\"State\":\"running\"}\n{\"State\":\"exited\"}")
                .unwrap()
                .len(),
            2
        );
        assert!(parse_status("not json").is_err());
    }
}

pub struct Capture {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub code: Option<i32>,
}
pub(crate) async fn read_limited<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> anyhow::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        anyhow::ensure!(
            data.len() + n <= limit,
            "Remote response exceeded the output limit"
        );
        data.extend_from_slice(&chunk[..n]);
    }
    Ok(data)
}
pub async fn capture(command: Command, seconds: u64, limit: usize) -> anyhow::Result<Capture> {
    capture_input(command, None, seconds, limit).await
}
pub(crate) async fn capture_input(
    mut command: Command,
    input: Option<String>,
    seconds: u64,
    limit: usize,
) -> anyhow::Result<Capture> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let writer = if let Some(input) = input {
        let mut stdin = child.stdin.take().unwrap();
        Some(tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(input.as_bytes()).await
        }))
    } else {
        None
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let output = async {
        let (stdout, stderr) =
            tokio::try_join!(read_limited(stdout, limit), read_limited(stderr, limit))?;
        let exit = child.wait().await?;
        if let Some(writer) = writer {
            writer.await??;
        }
        Ok::<_, anyhow::Error>(Capture {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            success: exit.success(),
            code: exit.code(),
        })
    };
    match tokio::time::timeout(std::time::Duration::from_secs(seconds), output).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            anyhow::bail!("Remote request timed out after {seconds} seconds")
        }
    }
}

/// Send scripts through stdin so edited file contents never enter process argv.
pub fn stdin_command(cfg: &Config, h: &Host) -> Command {
    let original = command(cfg, h, "");
    let args: Vec<_> = original.as_std().get_args().map(|a| a.to_owned()).collect();
    let mut c = Command::new("ssh");
    c.args(&args[..args.len() - 1])
        .arg("exec sh -s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    c
}
pub async fn capture_script(
    cfg: &Config,
    h: &Host,
    script: &str,
    seconds: u64,
    limit: usize,
) -> anyhow::Result<Capture> {
    capture_input(
        stdin_command(cfg, h),
        Some(script.to_string()),
        seconds,
        limit,
    )
    .await
}

pub fn python_command(cfg: &Config, h: &Host, program: &str) -> Command {
    use base64ct::{Base64, Encoding};
    let original = stdin_command(cfg, h);
    let args: Vec<_> = original.as_std().get_args().map(|a| a.to_owned()).collect();
    let mut command = Command::new("ssh");
    command
        .args(&args[..args.len() - 1])
        .arg(format!(
            "exec python3 -c 'import base64;exec(base64.b64decode(\"{}\"))'",
            Base64::encode_string(program.as_bytes())
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}
