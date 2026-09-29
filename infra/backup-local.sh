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
compose=(docker compose --env-file /etc/ldw/runtime.env -f /opt/ldw/infra/compose.yaml)
trap 'printf "Backup incomplete: %s\n" "$partial" >&2' ERR
source /opt/ldw/infra/backup-consistent.sh
source /opt/ldw/infra/backup-manifest.sh
consistent_db_blob_backup ldw "$partial/database.dump" /var/lib/ldw \
  "$partial/blobs.tar.gz" "${compose[@]}" exec -T db
tar -C /etc -czf "$partial/config.tar.gz" ldw
postgres_version=$("${compose[@]}" exec -T db psql -X -A -t -q -w -U postgres -d ldw -c 'SHOW server_version')
app_revision=unknown
if [[ -r /opt/ldw/APP_REVISION ]]; then
  IFS= read -r app_revision < /opt/ldw/APP_REVISION
  [[ "$app_revision" =~ ^[0-9a-f]{40}$ ]]
fi
write_backup_manifest "$partial" "$stamp" "$postgres_version" "$app_revision"
verify_backup_manifest "$partial"
mv "$partial" "$target/$stamp"
# Keep at least 14 days; these paths are generated solely inside the fixed backup root.
find "$target" -mindepth 1 -maxdepth 1 -type d -name '20??????T??????Z' -mtime +14 -exec rm -rf -- {} +
printf 'Local backup completed: %s\n' "$stamp"
