#!/usr/bin/env bash
# Source this file and call consistent_db_blob_backup with a database name,
# output paths, blob root, and a command prefix for psql/pg_dump.

consistent_db_blob_backup() (
  set -euo pipefail
  local database_name=$1 dump_path=$2 blob_root=$3 blob_archive=$4
  shift 4
  local -a db_exec=("$@")
  local snapshot_pid= snapshot_in= snapshot_out= snapshot=
  cleanup() {
    local code=$? holder_code=0
    trap - EXIT
    if [[ -n "$snapshot_pid" ]]; then
      if [[ -n "$snapshot_in" ]]; then
        printf 'ROLLBACK;\n\\q\n' >&"$snapshot_in" 2>/dev/null || true
      fi
      wait "$snapshot_pid" 2>/dev/null || holder_code=$?
    fi
    if (( code == 0 && holder_code != 0 )); then code=$holder_code; fi
    exit "$code"
  }
  trap cleanup EXIT

  # Finalization and GC use this transaction-level advisory lock. Export the
  # snapshot only after acquiring it, then keep the transaction alive until
  # both the database dump and the immutable blob archive are complete.
  coproc LDW_SNAPSHOT {
    "${db_exec[@]}" psql -X -A -t -q -w -v ON_ERROR_STOP=1 -U postgres -d "$database_name"
  }
  snapshot_pid=$LDW_SNAPSHOT_PID
  snapshot_in=${LDW_SNAPSHOT[1]}
  snapshot_out=${LDW_SNAPSHOT[0]}
  printf 'BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;\nSELECT pg_export_snapshot() FROM (SELECT pg_advisory_xact_lock(72111401)) AS lock;\n' >&"$snapshot_in"
  IFS= read -r -t 60 snapshot <&"$snapshot_out"
  [[ -n "$snapshot" && "$snapshot" != *[[:space:]]* ]]
  "${db_exec[@]}" pg_dump -w -U postgres -d "$database_name" -Fc --snapshot="$snapshot" > "$dump_path"
  kill -0 "$snapshot_pid"
  tar -C "$blob_root" -czf "$blob_archive" blobs
  kill -0 "$snapshot_pid"
)
