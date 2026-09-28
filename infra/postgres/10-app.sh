#!/bin/sh
set -eu
# Only the first initialization of an empty volume runs this file.
LDW_APP_PASSWORD=$(cat /run/secrets/app_db_password)
export LDW_APP_PASSWORD
psql -v ON_ERROR_STOP=1 --username postgres --dbname postgres <<'SQL'
\getenv app_password LDW_APP_PASSWORD
CREATE ROLE ldw_app LOGIN PASSWORD :'app_password' NOSUPERUSER NOCREATEDB NOCREATEROLE;
CREATE DATABASE ldw OWNER ldw_app;
REVOKE CONNECT ON DATABASE ldw FROM PUBLIC;
GRANT CONNECT ON DATABASE ldw TO ldw_app;
SQL
unset LDW_APP_PASSWORD
