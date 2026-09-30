#!/usr/bin/env bash
# Source this file to write and verify a local DB/blob/config recovery bundle.

write_backup_manifest() (
  set -euo pipefail
  local bundle=$1 created_at=$2 postgres_version=$3 app_revision=$4
  local database_sha blobs_sha config_sha database_bytes blobs_bytes config_bytes
  database_sha=$(sha256sum "$bundle/database.dump")
  blobs_sha=$(sha256sum "$bundle/blobs.tar.gz")
  config_sha=$(sha256sum "$bundle/config.tar.gz")
  database_sha=${database_sha%% *}
  blobs_sha=${blobs_sha%% *}
  config_sha=${config_sha%% *}
  database_bytes=$(stat -c %s "$bundle/database.dump")
  blobs_bytes=$(stat -c %s "$bundle/blobs.tar.gz")
  config_bytes=$(stat -c %s "$bundle/config.tar.gz")
  jq -n \
    --arg createdAtUtc "$created_at" \
    --arg postgresVersion "$postgres_version" \
    --arg appRevision "$app_revision" \
    --arg dbSha "$database_sha" --argjson dbBytes "$database_bytes" \
    --arg blobsSha "$blobs_sha" --argjson blobsBytes "$blobs_bytes" \
    --arg configSha "$config_sha" --argjson configBytes "$config_bytes" \
    '{formatVersion: 1, createdAtUtc: $createdAtUtc,
      versions: {postgres: $postgresVersion, applicationRevision: $appRevision},
      artifacts: {
        "database.dump": {sha256: $dbSha, bytes: $dbBytes},
        "blobs.tar.gz": {sha256: $blobsSha, bytes: $blobsBytes},
        "config.tar.gz": {sha256: $configSha, bytes: $configBytes}
      }}' > "$bundle/manifest.json"
  (cd "$bundle" && sha256sum database.dump blobs.tar.gz config.tar.gz manifest.json > SHA256SUMS)
)

verify_backup_manifest() (
  set -euo pipefail
  local bundle=$1 name expected_hash expected_bytes actual_hash actual_bytes
  (cd "$bundle" && sha256sum -c SHA256SUMS >/dev/null)
  jq -e '
    .formatVersion == 1
    and (.createdAtUtc | type == "string" and test("^[0-9]{8}T[0-9]{6}Z$"))
    and (.versions.postgres | type == "string" and length > 0)
    and (.versions.applicationRevision | type == "string" and length > 0)
    and (.artifacts | keys == ["blobs.tar.gz", "config.tar.gz", "database.dump"])
  ' "$bundle/manifest.json" >/dev/null
  for name in database.dump blobs.tar.gz config.tar.gz; do
    expected_hash=$(jq -er --arg name "$name" '.artifacts[$name].sha256 | select(test("^[0-9a-f]{64}$"))' "$bundle/manifest.json")
    expected_bytes=$(jq -er --arg name "$name" '.artifacts[$name].bytes | select(type == "number" and . >= 0)' "$bundle/manifest.json")
    actual_hash=$(sha256sum "$bundle/$name")
    actual_bytes=$(stat -c %s "$bundle/$name")
    [[ "$expected_hash" == "${actual_hash%% *}" && "$expected_bytes" == "$actual_bytes" ]]
  done
)
