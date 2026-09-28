#!/usr/bin/env bash
# Local recovery copy. Off-host encrypted replication is a separate release gate.
set -euo pipefail
umask 077
exec 9>/run/lock/ldw-backup.lock
flock -n 9 || exit 0
target=/opt/ldw/.local/backups
install -d -m 0700 "$target"
stamp=$(date -u +%Y%m%dT%H%M%SZ)
partial="$target/.${stamp}.partial"
mkdir "$partial"
trap 'printf "Backup incomplete: %s\n" "$partial" >&2' ERR
docker compose --env-file /etc/ldw/runtime.env -f /opt/ldw/infra/compose.yaml \
  exec -T db pg_dump -U postgres -d ldw -Fc > "$partial/database.dump"
# Product is not deployed yet. Once writes exist, quiesce them for DB/blob consistency.
tar -C /var/lib/ldw -czf "$partial/blobs.tar.gz" blobs
tar -C /etc -czf "$partial/config.tar.gz" ldw
(cd "$partial" && sha256sum database.dump blobs.tar.gz config.tar.gz > SHA256SUMS)
mv "$partial" "$target/$stamp"
# Keep at least 14 days; these paths are generated solely inside the fixed backup root.
find "$target" -mindepth 1 -maxdepth 1 -type d -name '20??????T??????Z' -mtime +14 -exec rm -rf -- {} +
printf 'Local backup completed: %s\n' "$stamp"
