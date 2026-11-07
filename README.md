# Dockmin

A web interface and Linux service for administering Docker Compose stacks and
standalone containers across Linux hosts over SSH. Packaged for ARM64 Linux
with systemd; developed on Raspberry Pi OS Trixie.
Inspired by [Dockman](https://github.com/KwisatzJim/dockman).

## What works

- Administrator password login, expiring sessions, CSRF and exact-origin checks.
- Host registration, configured-alias suggestions, Compose discovery, and SSH setup with verified fingerprints.
- Existing stack registration, including project names and multiple Compose files.
- Container status, health, images, published ports, and refresh timestamps.
- Start, stop, restart, update (`pull` followed by `up -d`), and recent or live streaming logs.
- Overview opens by default with compact stack summaries and live update badges.
- Check all stacks from Overview; confirmed Update All targets only stacks with updates.
- Image checks run again automatically after stack updates.
- Running-image registry digest checks, including multi-platform manifests.
- Disk usage, dangling-image cleanup, stopped-container cleanup, and build-cache cleanup.
- Persistent operation history and live output through server-sent events.
- One operation per host, configurable global concurrency, bounded queues and output.
- Interrupted-operation recovery after restart; uncertain mutations are never retried automatically.
- Automatic health refresh with offline backoff, independent of image checks and operation history.
- Web settings for polling, concurrency, timeouts, history, and administrator password.
- Configuration export/import (including Dockman hosts.json) and session-free database downloads.
- Daily scheduled image checks, persistent inbox, ntfy delivery with retries, and optional browser alerts.
- Compose creation/editing with validation, diffs, stale-write protection, and file backups.
- Standalone-container creation, lifecycle actions, updates, logs, and retained versions.
- Interactive container terminals with resize, one-time tickets, and session checks.
- Selected-volume/bind-data backups, checksum verification, streaming download/import, and safety backups before restore.
- Deployment health checks, saved image/configuration checkpoints, manual rollback, and opt-in automatic rollback.
- New SSH connections and one-time password/private-key bootstrap through the web interface.
- ARM64 packaging and a hardened systemd unit.

No agent needs to be installed on the managed PCs. Compose files stay on those
PCs. The service account needs noninteractive SSH access and permission to use
Docker on each host, plus POSIX `sh`, the standard `base64` utility, and Python 3
for the editor, container, recovery, and data-backup features. Remote
scripts are encoded for transport so Fish and POSIX login shells handle paths
and scripts consistently. Registry checks additionally require `docker buildx` and
working registry authentication on the managed host.

This remains a single-administrator application. Multiple users/access roles
and host-wide volume deletion are outside this release.
Status is refreshed every 60 seconds by default, manually, and after mutations.
The browser polls saved state every five seconds. Both intervals are configurable
in Settings; health refresh can be disabled. Unreachable hosts back off to a
maximum of one hour. Health checks never pull or update images, and user jobs
take priority between checks. Old status is labelled stale after a minute.
History defaults to 100 recent completed operations and all active operations,
with up to 128 KiB of captured process output per operation.
Container logs can contain sensitive application data: restrict access to the
administrator and back up/protect the database accordingly. Arbitrary log
contents are not guaranteed to be secret-free.

## Build and test

The frontend audit has a documented build-time dependency limitation; see
[dependency advisory assessment](docs/dependency-notes.md) for scope and evidence.

Use Rust stable, a C compiler (for bundled SQLite), and Node.js 22. On Debian-based systems:

```bash
sudo apt install build-essential pkg-config openssh-client
# Install Rust stable and Node.js 22 using your preferred supported installers.
npm ci --prefix web
npm run build --prefix web
npm test --prefix web
cargo test --locked
cargo build --locked
python3 tests/container_cleanup.py
python3 tests/recovery_checkpoint.py
python3 tests/update_images.py
python3 tests/smoke.py
```

The three regression scripts above use temporary files and simulated Docker
responses. The smoke test runs the actual service with disposable configuration and a fake
SSH executable. It never connects to your LAN PCs or changes real containers.

Create the production ARM64 bundle on an ARM64 Linux build host:

```bash
./scripts/package.sh
```

Output: `dist/dockmin-linux-arm64.tar.gz` and its SHA-256 checksum. The bundle
contains the release binary, compiled frontend, service unit, installer, and
configuration examples. Node is needed only when building, not at runtime.

## Install on ARM64 Linux

Copy and extract the bundle on your ARM64 Linux server, then run its installer:

```bash
tar -xzf dockmin-linux-arm64.tar.gz
cd dockmin
sudo ./deploy/install.sh
```

The installer preserves existing configuration, SSH credentials, and data. It
does not start the service until you complete setup. On upgrades, stop Dockmin
before installing, then start it again afterward.

1. Edit `/etc/dockmin/dockmin.toml`: replace the LAN and Tailscale HTTPS origins
   with the exact URLs you will use, without trailing slashes.
2. Edit `/etc/dockmin/ssh_config` with aliases, real addresses, users, and the
   service identity file. The `dockmin` system user has no interactive shell.
3. Generate a dedicated SSH key; authorize its public key on each Docker PC:

```bash
sudo ssh-keygen -t ed25519 -N '' -f /etc/dockmin/id_ed25519
sudo chown dockmin:dockmin /etc/dockmin/id_ed25519
sudo chmod 600 /etc/dockmin/id_ed25519
# Copy the public key to each target user's authorized_keys using your normal
# administrative access. Do not copy your personal private key to the service.
```

Populate `/var/lib/dockmin/known_hosts` with verified host keys. Verify fingerprints
on the target PCs through a trusted connection before trusting scanned keys.
SSH uses strict host-key checking, so unknown or changed keys fail explicitly.
For nondefault ports, use the normal `[hostname]:port` known-hosts entries.

Check access under the actual service user:

```bash
sudo -u dockmin ssh -F /etc/dockmin/ssh_config \
  -o UserKnownHostsFile=/var/lib/dockmin/known_hosts \
  -o GlobalKnownHostsFile=/dev/null -o StrictHostKeyChecking=yes \
  -o BatchMode=yes pc1 'docker version && docker compose version'
```

The systemd service cannot read files under `/home`. Keep its keys and any
included SSH configuration under `/etc/dockmin`. Unattended key authentication
must work without your desktop SSH agent. The installer does not grant the
service user local Docker socket access.

Initialize the administrator password without putting it in shell history or
process arguments:

```bash
read -rsp 'Dockmin admin password: ' DOCKMIN_PASSWORD
printf '\n'
printf '%s\n' "$DOCKMIN_PASSWORD" | sudo -u dockmin /opt/dockmin/dockmin \
  --config /etc/dockmin/dockmin.toml set-password
unset DOCKMIN_PASSWORD
```

Passwords must be at least 12 bytes. Passwords are hashed with Argon2id.
Use Settings to change the password while signed in; other sessions are revoked.
For a forgotten password, stop the service before using the CLI reset above.

## LAN and Tailscale access

The default backend listens on `127.0.0.1:9090`, behind HTTPS proxies. Configure
LAN DNS (or client hosts files) so your chosen LAN name resolves to the Dockmin server, and use
an HTTPS reverse proxy on that server. `deploy/Caddyfile.example` shows a LAN setup with
Caddy's internal CA. Install/trust that CA on every client, or use a certificate
from an issuer those clients already trust. Add the exact LAN HTTPS URL to
`origins`. The HTTP backend is not the browser-facing LAN URL.

For Tailscale access, install and configure Tailscale on the Dockmin server, then run:

```bash
sudo tailscale serve --bg http://127.0.0.1:9090
```

Follow any HTTPS enablement prompts and put the resulting `https://dockmin-host.YOUR-TAILNET.ts.net`
URL in `origins`. Keep normal tailnet access controls. Dockmin requires its
administrator login for both LAN and Tailscale access and does not trust
forwarded identity headers. Tailscale Serve keeps access inside the tailnet.

Then start the service:

```bash
sudo systemctl enable --now dockmin
sudo systemctl status dockmin
sudo journalctl -u dockmin -f
```

Open either configured HTTPS URL, sign in, add your SSH host aliases, test each
connection, and register the absolute Compose directories. Use the same project
name and ordered Compose files as the stack's original deployment. Blank values
use Compose defaults.

## Local development

Create a development configuration using `secure_cookie = false`, loopback
origins only, local database and SSH paths, and `static_dir = "web/dist"`:

```toml
listen = "127.0.0.1:9090"
origins = ["http://127.0.0.1:9090", "http://127.0.0.1:5173"]
database = "./data/dockmin.sqlite"
static_dir = "web/dist"
ssh_config = "/absolute/path/to/your/ssh_config"
known_hosts = "/absolute/path/to/your/known_hosts"
secure_cookie = false
concurrency = 3
command_timeout_seconds = 1800
```

Initialize the development password through stdin, then run `cargo run --
--config development.toml`. For hot reload, run `npm run dev --prefix web` in
another terminal; Vite proxies `/api` to port 9090. Production configuration
rejects HTTP origins when secure cookies are enabled. Insecure cookies are
accepted only for loopback development origins.

## Failure handling and backups

A browser disconnect does not cancel a job. SSH disconnects, timeouts, and
service restarts can leave the remote result uncertain. Inspect Docker status
before retrying; the service never claims a transactional update or rollback.
Stop uses `docker compose stop`, preserving containers and volumes. Cleanup is
host-wide, requires typed confirmation, and never deletes volumes. Removing a
registered stack or host only removes its Dockmin configuration.

Settings provides two downloads:

- **Configuration export:** host aliases, stack paths/options, and preferences. No passwords, operation output, or SSH credentials. Import merges by alias and exact stack configuration; existing names/options are preserved. Dockman `hosts.json` arrays are supported, with Compose project/file defaults because Dockman does not store those options.
- **Database backup:** a consistent SQLite online backup containing configuration, password hash, cached status, and history. Sessions are securely removed and the backup is vacuumed before download. Re-enter your password to authorize it. History may contain application secrets, so keep backups private.

SSH aliases, the dedicated identity, verified host keys, and HTTPS certificates
are separate from the downloaded database. Back up `/etc/dockmin`,
`/var/lib/dockmin/known_hosts`, and HTTPS proxy state securely for disaster
recovery. The running database includes session tokens and is mode 0600.
Service logs are available in the systemd journal.

To restore a downloaded database, first wait for operations to finish, copy the
backup to the Dockmin server, and stop Dockmin. Keep the existing database as a recovery copy.
With the service stopped, move aside `dockmin.sqlite`, `dockmin.sqlite-wal`, and
`dockmin.sqlite-shm` if present, then install the backup as
`/var/lib/dockmin/dockmin.sqlite` owned by `dockmin:dockmin` with mode 0600.
Start Dockmin and sign in using the password current at backup time. Restoring
history never retries unfinished Docker mutations; they are marked interrupted.

Hosts → Add host can register an existing alias or create a new SSH connection.
New connections are saved in the private state directory and included ahead of
`/etc/dockmin/ssh_config`; the original server configuration is preserved.
SSH setup scans fingerprints, shows the service public key, and can authorize
it using a one-time remote password or private key/passphrase. Verify the
fingerprint directly on the remote PC before sending credentials. Temporary
credentials are removed after the request and stale bootstrap files are removed
at startup. Linux, SSH, Docker/Compose access, and Python 3 must already exist on
the target. Existing trusted keys cannot be silently replaced through the UI;
deliberate key rotation remains a manual server administration operation.

Configuration exports use format version 2, including managed SSH connections
and the update schedule. Version 1 and Dockman imports remain supported. Neither
format exports private keys or passwords; database backup/restore reconstructs
managed SSH connections at startup.


## Scheduled checks and notifications

Settings → Scheduled update checks defaults to every 24 hours for all stacks.
Choose an interval from 15 minutes to seven days, select stacks, or disable it.
The first scheduler pass is one minute after startup. Busy hosts are deferred;
checks do not pull images or recreate containers. Scheduling survives restarts.
Notifications records new update availability and failed operations. Repeated
unchanged alerts are suppressed. Browser alerts are opt-in per device and work
while Dockmin is open; the inbox records alerts while you are signed out.

## Compose editing, deployments, and rollback

Compose stacks → Edit Compose loads the registered files. Validate and review
the diff, then confirm the stack name to save. Each file is limited to 256 KiB,
with a 1 MiB combined edit limit. Changed files are backed up under
`STACK_DIRECTORY/.dockmin/compose-history`; files changed outside Dockmin are
rejected until you reload. Validation and saving do not recreate containers.
Create stack requires a new directory and writes a validated `compose.yaml`.

Deploy / rollback applies saved configuration and waits for health. An update
also pulls the configured images. The last successful deployment's files are
saved separately, so editing before deployment does not overwrite the rollback
baseline. Checkpoints are stored on the managed host under
`~/.local/share/dockmin/checkpoints/STACK_ID/`. Images receive protected
`dockmin-rollback/*` tags so dangling-image cleanup cannot remove them.
Manual rollback restores configuration and those images. Optional automatic
rollback runs if applying configuration or health checks fail. It defaults off.
Services without health checks must stay running; services with health checks
must become healthy within the selected deadline. Add application health checks
when running state alone is insufficient.

A rollback keeps application data. Database migrations require a matching data
backup and an explicit data restore. After rollback, the active image override
keeps Start on the saved images until your next deployment/update. Snapshots cover registered Compose files and the project `.env`; other external
configuration/environment files need their own backups. Deliberately editing
files outside Dockmin can bypass the recorded configuration baseline.

## Standalone containers and terminals

Containers lists standalone containers, with an option to show Compose
containers. Create uses an image, optional command arguments, environment,
ports, mounts, and restart policy. Update recreates the container with its
existing configuration, including anonymous volumes, and retains the stopped
original. If replacement fails, the original is restarted. Restore previous
version switches back without undoing data changes. Auto-remove containers and
containers with static IP assignments require explicit manual migration rather
than automatic recreation. Compose containers are managed through their stack.
Removing a stopped container preserves its Docker volumes. Host-wide stopped-container
cleanup preserves containers referenced by retained rollback checkpoints and
Dockmin previous-version containers. Explicitly removing a retained container
still removes its writable-layer data and makes that rollback target unavailable.

Terminal is available beside running services and containers. Confirm the
container and choose sh, bash, or ash. It uses a Docker exec TTY over an
origin-checked WebSocket, so the restricted SSH service key does not require SSH
PTY permission. Tickets last 60 seconds and can be used once. Up to four sessions
can run, each for at most one hour; expired sessions disconnect. Terminal input
and output are not saved. Closing disconnects and does not undo commands already
executed.

## Stack data backups and restores

Data backups lists named/anonymous volumes and regular bind-mounted directories
or files. Select the data to save, especially before an application upgrade.
Read-only mounts are not selected automatically. Other running containers that
share selected data must be stopped first. Dockmin stops the stack during the
backup and restarts only services that were running. Special files, broad system
roots, symlinks, and mounts containing Dockmin's own backup storage are excluded
or refused. External host processes are outside Dockmin's consistency boundary.

Archives are stored under `~/.local/share/dockmin/backups/STACK_ID/BACKUP_ID/` on
the managed host. The official `debian:trixie-slim` helper supplies GNU tar for
ownership, modes, ACLs, and extended attributes. It is downloaded on first use,
runs without network access, and mounts only the selected data and archive
folder. Backup files and metadata are private. Plan free disk space and retain
or remove old archives/checkpoints deliberately; automatic data retention is not
configured.

Download streams a complete `.tar` through the Dockmin server without staging it there. Import
streams it back to the selected host, validates paths and SHA-256 checksums, and
registers it without changing application data. Uploads are limited to 100 GiB
and the configured operation timeout. The registered Compose directory, file
paths, and mount sources must match. On a replacement host, configure matching
containers/mounts before importing. Keep downloaded backups private: they can
include application data and Compose environment/secrets.

Restore verifies all archives before stopping services, then saves the current
data as a safety backup before replacement. Choose whether to also restore the
matching configuration/images. If a saved image is no longer cached and a
registry digest was recorded, Dockmin pulls that exact digest. A partial failed
restore leaves services stopped so you can recover from the safety backup.
No host-wide volume deletion or automatic data rollback is performed.

The optional `tests/live_features.py` suite uses real Docker and is a
site-specific integration harness, not part of the standard test commands.
Before running it, adapt its SSH alias and SSH configuration paths to your test
server. Run the harness on that same server: it shares local fixture paths with
the Docker host. It creates uniquely named `dockmin-test-*` containers, volumes,
and networks, exercises failure/recovery paths, and cleans up its own resources.
Use an isolated test environment; the standard simulated tests need no Docker daemon.

## ntfy notifications

Settings → ntfy notifications accepts your server, topic, and optional access
token. Subscribe to that topic in ntfy, enable delivery, save, and send a test.
The [ntfy publishing API](https://docs.ntfy.sh/publish/) supports public or
self-hosted servers. Use HTTPS and an authenticated or unpredictable topic.
Only alert titles leave Dockmin; detailed operation output remains in the inbox.
The token is never returned by the API or included in configuration exports.
Private database backups include it and must be kept secure.

Delivery state and exponential retries (up to one hour between attempts) survive
restarts. New alerts enter the queue atomically with the inbox entry. Changing
the destination or enabling/disabling starts a new queue for future alerts;
existing inbox entries are not replayed. The latest 200 inbox alerts bound the
queue. A lost acknowledgement can cause a duplicate notification. Delivery
status and the explicit test button are available in Settings. Until a server
and topic are configured, the persistent inbox remains active.
