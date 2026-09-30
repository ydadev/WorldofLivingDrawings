#!/usr/bin/env bash
# Exercise the production DB/blob barrier against the PostgreSQL CI service.
set -euo pipefail

container=$(docker ps --filter ancestor=postgres:18 --format '{{.ID}}')
[[ -n "$container" && "$container" != *$'\n'* ]]
db_exec=(docker exec -i "$container")
root=$(mktemp -d)
table=ldw_backup_consistency_test
backup_pid=
cleanup() {
  touch "$root/release" 2>/dev/null || true
  if [[ -n "$backup_pid" ]]; then wait "$backup_pid" 2>/dev/null || true; fi
  "${db_exec[@]}" psql -X -q -w -U postgres -d postgres -c "DROP TABLE IF EXISTS $table" >/dev/null 2>&1 || true
  "${db_exec[@]}" dropdb --if-exists -w -U postgres ldw_backup_restore_test >/dev/null 2>&1 || true
  rm -rf -- "$root"
}
trap cleanup EXIT

mkdir -p "$root/blobs/blobs" "$root/bin"
printf 'private PNG fixture\n' > "$root/blobs/blobs/fixture"
"${db_exec[@]}" psql -X -q -w -v ON_ERROR_STOP=1 -U postgres -d postgres \
  -c "CREATE TABLE $table (id integer PRIMARY KEY); INSERT INTO $table VALUES (1)"

# Pause archiving after pg_dump, while the exporter must still hold the lock.
cat > "$root/bin/tar" <<'EOF'
#!/usr/bin/env bash
touch "$LDW_BACKUP_TEST_ROOT/archive_started"
for ((attempt=0; attempt<100; attempt++)); do
  [[ -e "$LDW_BACKUP_TEST_ROOT/release" ]] && exec /usr/bin/tar "$@"
  sleep 0.1
done
exit 1
EOF
chmod +x "$root/bin/tar"
export LDW_BACKUP_TEST_ROOT=$root
export PATH="$root/bin:$PATH"
source infra/backup-consistent.sh
consistent_db_blob_backup postgres "$root/database.dump" "$root/blobs" \
  "$root/blobs.tar.gz" "${db_exec[@]}" &
backup_pid=$!

for ((attempt=0; attempt<200; attempt++)); do
  [[ -e "$root/archive_started" ]] && break
  kill -0 "$backup_pid" 2>/dev/null || { wait "$backup_pid"; exit 1; }
  sleep 0.1
done
[[ -e "$root/archive_started" ]]
lock_available=$("${db_exec[@]}" psql -X -A -t -q -w -U postgres -d postgres \
  -c 'SELECT pg_try_advisory_lock(72111401)')
[[ "$lock_available" == f ]]
"${db_exec[@]}" psql -X -q -w -v ON_ERROR_STOP=1 -U postgres -d postgres \
  -c "INSERT INTO $table VALUES (2)"
touch "$root/release"
wait "$backup_pid"
backup_pid=

"${db_exec[@]}" createdb -w -U postgres ldw_backup_restore_test
"${db_exec[@]}" pg_restore -w -U postgres -d ldw_backup_restore_test < "$root/database.dump"
rows=$("${db_exec[@]}" psql -X -A -t -q -w -U postgres -d ldw_backup_restore_test \
  -c "SELECT string_agg(id::text, ',' ORDER BY id) FROM $table")
[[ "$rows" == 1 ]]
"${db_exec[@]}" dropdb -w -U postgres ldw_backup_restore_test
[[ "$(tar -tzf "$root/blobs.tar.gz")" == *'blobs/fixture'* ]]
[[ "$(tar -xOzf "$root/blobs.tar.gz" blobs/fixture)" == 'private PNG fixture' ]]
mkdir "$root/config"
printf 'test configuration\n' > "$root/config/example"
tar -C "$root/config" -czf "$root/config.tar.gz" example
source infra/backup-manifest.sh
postgres_version=$("${db_exec[@]}" psql -X -A -t -q -w -U postgres -d postgres -c 'SHOW server_version')
write_backup_manifest "$root" 20260929T031500Z "$postgres_version" unknown
verify_backup_manifest "$root"
[[ "$(jq -r '.versions.postgres' "$root/manifest.json")" == "$postgres_version" ]]
printf 'corruption\n' >> "$root/config.tar.gz"
if verify_backup_manifest "$root" >/dev/null 2>&1; then
  printf 'Corrupted backup passed verification\n' >&2
  exit 1
fi
printf 'Consistent DB/blob backup test passed\n'
