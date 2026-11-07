#!/usr/bin/env bash
set -euo pipefail
if [[ $EUID -ne 0 ]]; then
  echo 'Run this installer as root on the ARM64 Linux server.' >&2
  exit 1
fi
if [[ $(uname -m) != aarch64 ]]; then
  echo 'This package targets ARM64 Linux (aarch64).' >&2
  exit 1
fi
PACKAGE_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
command -v ssh >/dev/null || { echo 'Install openssh-client first.' >&2; exit 1; }
if ! id dockmin >/dev/null 2>&1; then
  useradd --system --user-group --home-dir /var/lib/dockmin --shell /usr/sbin/nologin dockmin
fi
install -d -m 0755 /opt/dockmin /opt/dockmin/web
install -d -m 0750 -o root -g dockmin /etc/dockmin
install -d -m 0700 -o dockmin -g dockmin /var/lib/dockmin
install -m 0755 "$PACKAGE_DIR/dockmin" /opt/dockmin/dockmin
cp -R "$PACKAGE_DIR/web/." /opt/dockmin/web/
# Static files must remain readable by the unprivileged service, even with umask 077.
find /opt/dockmin/web -type d -exec chmod 0755 {} +
find /opt/dockmin/web -type f -exec chmod 0644 {} +
chown -R root:root /opt/dockmin
if [[ ! -e /etc/dockmin/dockmin.toml ]]; then
  install -m 0640 -o root -g dockmin "$PACKAGE_DIR/deploy/dockmin.toml" /etc/dockmin/dockmin.toml
fi
if [[ ! -e /etc/dockmin/ssh_config ]]; then
  install -m 0640 -o root -g dockmin "$PACKAGE_DIR/deploy/ssh_config.example" /etc/dockmin/ssh_config
fi
if [[ ! -e /var/lib/dockmin/known_hosts ]]; then
  if [[ -e /etc/dockmin/known_hosts ]]; then
    install -m 0600 -o dockmin -g dockmin /etc/dockmin/known_hosts /var/lib/dockmin/known_hosts
  else
    install -m 0600 -o dockmin -g dockmin /dev/null /var/lib/dockmin/known_hosts
  fi
fi
# Migrate the old default trust-file location so host enrollment can write
# within the service's systemd ReadWritePaths; custom paths are preserved.
if [[ -e /etc/dockmin/known_hosts ]]; then
  if [[ ! -e /etc/dockmin/dockmin.toml.before-host-enrollment ]]; then
    cp -a /etc/dockmin/dockmin.toml /etc/dockmin/dockmin.toml.before-host-enrollment
  fi
  sed -i 's|"/etc/dockmin/known_hosts"|"/var/lib/dockmin/known_hosts"|g' /etc/dockmin/dockmin.toml
fi
install -m 0644 "$PACKAGE_DIR/deploy/dockmin.service" /etc/systemd/system/dockmin.service
systemctl daemon-reload
cat <<'MESSAGE'
Installed. Finish setup before starting:
  1. Edit /etc/dockmin/dockmin.toml and /etc/dockmin/ssh_config.
  2. Provision the service SSH key and verified known-host fingerprints.
  3. Initialize the admin password (see README).
  4. Configure LAN HTTPS and Tailscale Serve.
  5. systemctl enable --now dockmin
Existing configuration, credentials, and database are preserved on upgrades.
MESSAGE
