#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
[[ $(uname -m) == aarch64 ]] || { echo 'Build this package on ARM64 Linux.' >&2; exit 1; }
npm ci --prefix web
npm run build --prefix web
npm test --prefix web
cargo test --locked
python3 tests/container_cleanup.py
python3 tests/recovery_checkpoint.py
python3 tests/update_images.py
cargo build --release --locked
PACKAGE_STAGING=$(mktemp -d)
trap 'rm -rf -- "$PACKAGE_STAGING"' EXIT
mkdir -p "$PACKAGE_STAGING/dockmin/deploy" "$PACKAGE_STAGING/dockmin/web" dist
install -m 0755 target/release/dockmin "$PACKAGE_STAGING/dockmin/dockmin"
cp -R web/dist/. "$PACKAGE_STAGING/dockmin/web/"
cp deploy/dockmin.toml deploy/dockmin.service deploy/ssh_config.example deploy/Caddyfile.example deploy/install.sh "$PACKAGE_STAGING/dockmin/deploy/"
cp README.md DESIGN.md LICENSE "$PACKAGE_STAGING/dockmin/"
mkdir -p "$PACKAGE_STAGING/dockmin/docs"
cp docs/dependency-notes.md "$PACKAGE_STAGING/dockmin/docs/"
tar -C "$PACKAGE_STAGING" -czf dist/dockmin-linux-arm64.tar.gz dockmin
(cd dist && sha256sum dockmin-linux-arm64.tar.gz > dockmin-linux-arm64.tar.gz.sha256)
printf 'Package: dist/dockmin-linux-arm64.tar.gz\n'
