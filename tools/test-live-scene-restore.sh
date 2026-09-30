#!/usr/bin/env bash
# Called after the real browser has published both fish to the running fixture.
set -euo pipefail

root=$1 session_id=$2 coral_blob=$3 stream_blob=$4
container=$(docker ps --filter ancestor=postgres:18 --format '{{.ID}}')
[[ -n "$container" && "$container" != *$'\n'* ]]
db_exec=(docker exec -i "$container")
bundle="$root/recovery-bundle"
restore_root="$root/recovered"
mkdir -m 0700 "$bundle" "$restore_root"
restore_db=ldw_ui_restore
restored=0
cleanup() {
  if (( restored )); then
    "${db_exec[@]}" dropdb --if-exists -w -U postgres "$restore_db" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

source infra/backup-consistent.sh
source infra/backup-manifest.sh
consistent_db_blob_backup ldw_ui_fixture "$bundle/database.dump" "$root" \
  "$bundle/blobs.tar.gz" "${db_exec[@]}"
printf 'browser integration fixture\n' > "$root/recovery-config"
tar -C "$root" -czf "$bundle/config.tar.gz" recovery-config
postgres_version=$("${db_exec[@]}" psql -X -A -t -q -w -U postgres -d postgres -c 'SHOW server_version')
write_backup_manifest "$bundle" "$(date -u +%Y%m%dT%H%M%SZ)" "$postgres_version" fixture
verify_backup_manifest "$bundle"

"${db_exec[@]}" createdb -w -U postgres "$restore_db"
restored=1
"${db_exec[@]}" pg_restore -w -U postgres -d "$restore_db" --exit-on-error < "$bundle/database.dump"
tar -C "$restore_root" -xzf "$bundle/blobs.tar.gz"
LDW_RESTORE_DATABASE_URL="${DATABASE_URL%/*}/$restore_db" \
LDW_RESTORE_BLOB_DIR="$restore_root/blobs" \
LDW_RESTORE_SESSION_ID="$session_id" \
LDW_RESTORE_CORAL_BLOB="$coral_blob" \
LDW_RESTORE_STREAM_BLOB="$stream_blob" \
./target/debug/examples/restore_probe
printf 'Live scene DB/blob restore and private PNG access: PASS\n'
