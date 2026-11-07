# Dockmin initial design

Status: version 0.2 implements the administration features described below.
The initial design is followed by expanded implementation notes.

## Agreed scope

Dockmin is a service with a web interface, hosted on an ARM64 Linux server.
It manages Docker Compose stacks across hosts on the local network.
The interface must be accessible from the LAN and through Tailscale.

The deployment target is ARM64 Linux with systemd, developed on
Raspberry Pi OS Trixie.

The initial release focused on administering existing stacks. Version 0.2 adds
stack creation, Compose editing, and standalone-container management.

Reference project: https://github.com/KwisatzJim/dockman

## Proposed architecture

One Rust service serves a compiled Svelte frontend and a same-origin HTTP API.
The frontend requires no Node runtime on the Dockmin server. SQLite stores configuration and
operation history. A systemd unit starts the service and restarts it on failure.

The service invokes OpenSSH to run fixed Docker Compose operations on configured
remote hosts. No Dockmin agent is required on the managed PCs. Docker and the
Compose plugin must already be installed there.

SSH runs under a dedicated service account using explicitly configured keys,
SSH configuration, and known-hosts files. Use noninteractive authentication,
strict host-key verification, connection timeouts, and safely quoted remote
paths. Existing service credentials are never returned to the browser. New host
bootstrap accepts one-time credentials over authenticated HTTPS and removes its
private temporary files after use. Host enrollment must report
unknown host keys and allow their fingerprints to be checked before trusting
them; never silently accept changed keys.

Host identity is a configured SSH alias. Stack identity includes host, absolute
working directory, and optional explicit Compose project name and ordered
Compose files. This supports installations that use multiple Compose files.

## Access

LAN access uses a configurable address and port. Tailscale Serve can proxy to
the service's loopback listener and provide HTTPS within the tailnet. LAN HTTPS
can be provided by a reverse proxy or configured certificate; the installation
guide must distinguish the two access paths.

A single administrator account is the initial authentication model, with a
locally bootstrapped password, password hashing, server-side sessions, and CSRF
and origin validation for changes. Plain HTTP must not be presented as secure
for password login. Network location alone does not grant administration.

Expose only defined administration operations, with validated host and stack
identifiers. Do not expose an arbitrary remote-command endpoint.

## Initial interface

- Overview: configured hosts, connectivity, stack states, and refresh timestamps.
- Hosts: configure SSH aliases, test connectivity and Docker prerequisites.
- Stacks: register existing directories and optional Compose settings.
- Stack details: services, health, image names, published ports, and recent logs.
- Actions: start, stop, restart, and update using pull followed by up -d.
- Bulk actions: refresh and update selected stacks or hosts with a visible summary.
- Operations: live output, status, timestamps, and retained history.
- Image checks: report up to date, update available, or unknown with a reason.
- Maintenance: inspect Docker disk usage and confirm host-wide cleanup.

Use a responsive layout suitable for phones. Stop, update, and cleanup actions
show the affected host and stack. Volume deletion requires a separate explicit
confirmation and must never be included in a default cleanup operation.

## Execution and reliability

Operations are server-owned jobs, independent of the browser connection.
Persist job identity and state before execution. Stream output through
authenticated server-sent events, with bounded retention and reconnection
support. The browser can reopen an operation after navigating away.

Serialize mutations per stack and initially run one mutating job at a time per
host, with a small global concurrency limit. Host-wide maintenance excludes
other mutations on that host. Status polling must be bounded and should back
off for unreachable hosts. Display stale status explicitly.

Use timeouts and capture exit status and bounded stdout/stderr. A dropped SSH
connection or service restart can leave a remote command's result uncertain.
Mark these operations interrupted or unknown and refresh actual stack status;
never automatically retry a mutation whose outcome is uncertain. A browser
disconnect does not cancel a job. An update is not transactional and does not
promise automatic rollback.

Compose files remain on the managed PCs and are the source of truth. Dockmin
stores references to them, not copies. Record administration actions in history and limit command output and container
logs. Avoid inspecting environment variables or credentials. Application logs
can still contain secrets; access and backups must be restricted accordingly.

## Delivery stages

1. Service foundation: configuration, authentication, persistence, SSH transport,
   prerequisite checks, systemd unit, and deployment documentation.
2. Core workflow: host and stack registration, status, stack actions, durable job
   records, streamed output, and responsive dashboard.
3. Administration completion: bulk actions, image checks, logs, disk usage,
   confirmed cleanup, and optional import of Dockman's hosts.json after verifying
   its actual schema.

Validate command construction and quoting, authorization and CSRF protections,
job concurrency and failure handling, and parsing against representative Compose
output. Integration checks must use disposable stacks, not existing LAN stacks.

## Deployment target

Build for aarch64 Linux and package the service with a systemd unit. Compile the
frontend during the build and serve its static assets from the service; the Dockmin server does
not need a Node runtime. Validate binary compatibility on the Dockmin server before deployment.

## Convenience functions implemented

A persisted runtime settings record controls health/browser polling, operation
concurrency, timeouts, and retained history. Changes apply to newly started work;
existing operations keep their current deadline. Background health checks use
the same host locks and global capacity as operations, skip hosts with queued
work, and back off offline hosts. They write separate connectivity/status rows
without creating jobs or changing image-update evidence.

Live logs have four separate stream permits and no mutation lock. Closing a
stream kills its SSH child; sessions are revalidated every five seconds and
streams expire after one hour. The browser retains at most 128 KiB.

Imports validate the entire configuration before a transaction merges aliases
and exact stack definitions. No credentials are imported or exported. Online
SQLite backups use a private temporary file, remove sessions with secure_delete,
vacuum, and exclude SSH identities. The UI exposes only public identity files.
Host keys are scanned, independently verified by the administrator, rescanned
at enrollment, and atomically appended to service-owned known_hosts. Existing
trust is preserved; rotation is manual.

## Administration extensions (0.2)

Persisted schedule timestamps defer busy hosts and prevent duplicate immediate
checks after restarts. A transactional notification signal suppresses repeated
unchanged alerts. Notification delivery is in-app and optionally browser-based.

The editor stages regular files beside their originals, validates with Compose,
checks source hashes, writes private previous versions, and atomically replaces
files. Last-good deployment files are retained separately from pending edits.
Sensitive script payloads travel over SSH stdin rather than process arguments.

Container recreation negotiates the host Engine API version and preserves
HostConfig, networks, environment, ports, and named/anonymous volume sources.
Old standalone containers and tagged stack images provide rollback checkpoints.
Container terminals use a Docker exec TTY with one-time session-bound tickets;
no SSH PTY or recorded terminal output is required.

Cold data backups check mount ownership/sharing, use a restricted networkless
GNU tar helper, and save checksummed manifests. Restore creates a safety backup;
incomplete data replacement keeps services stopped. Transfer is streamed through
the Dockmin server. Imported outer archives accept only bounded flat regular files and matching
stack/mount paths. Actual volume deletion is never part of these workflows.

Managed SSH connections are private state files layered over the original SSH
configuration. Fingerprint verification precedes one-time credential bootstrap.
Those credentials are private temporary files with cleanup on completion/startup;
configuration exports include connection addresses but never identity secrets.

ntfy delivery uses the documented JSON publishing API with optional Bearer
credentials. Inbox creation and a title-only outbox entry share a SQLite
transaction. A bounded background worker persists retry state, disables redirects,
and times out requests after ten seconds. A destination generation prevents
replaying historical alerts after changing notification settings. Tokens are
redacted by the settings API and omitted from configuration exports; database
backups remain private because they include the stored token.
