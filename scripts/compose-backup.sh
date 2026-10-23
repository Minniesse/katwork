#!/bin/sh
set -eu
umask 077
export LC_ALL=C
invalid_url() {
    printf '%s\n' 'Unsupported DATABASE_URL: use postgres://USER:PASSWORD@HOST:PORT/DATABASE with plain URL-safe fields, no options.' >&2
    exit 1
}
# shortcut: SP1 accepts simple Compose URLs; use a shared parser before supporting external databases.
case "${DATABASE_URL:-}" in
    postgres://*) target=${DATABASE_URL#postgres://} ;;
    postgresql://*) target=${DATABASE_URL#postgresql://} ;;
    *) invalid_url ;;
esac
authority=${target%%/*}
[ "$authority" != "$target" ] || invalid_url
PGDATABASE=${target#*/}
case "$PGDATABASE" in .|..) invalid_url ;; esac
credentials=${authority%@*}
[ "$credentials" != "$authority" ] || invalid_url
host_port=${authority##*@}
PGUSER=${credentials%%:*}
PGPASSWORD=${credentials#*:}
[ "$PGUSER" != "$credentials" ] || invalid_url
PGHOST=${host_port%:*}
PGPORT=${host_port##*:}
[ "$PGHOST" != "$host_port" ] || invalid_url
for field in "$PGUSER" "$PGDATABASE" "$PGHOST" "$PGPASSWORD"; do
    case "$field" in ''|*[!a-zA-Z0-9_.~-]*) invalid_url ;; esac
done
case "$PGPORT" in ''|*[!0-9]*|??????*) invalid_url ;; esac
[ "$PGPORT" -ge 1 ] && [ "$PGPORT" -le 65535 ] || invalid_url
# Only the environment carries credentials, never a process argument or log line.
export PGHOST PGPORT PGUSER PGDATABASE PGPASSWORD
schema=$(psql -X -v ON_ERROR_STOP=1 -Atc "SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
case "$schema" in
    f) exit 0 ;;
    t) ;;
    *) printf '%s\n' 'Could not determine database migration state.' >&2; exit 1 ;;
esac
directory=${KATWORK_BACKUP_DIR:-/backups}
mkdir -p "$directory"
# The random suffix prevents two starts in one second from replacing a good archive.
partial=$(mktemp "$directory/katwork-$(date -u +%Y%m%dT%H%M%SZ)-XXXXXXXXXXXX")
trap 'rm -f "$partial"' EXIT HUP INT TERM
mv "$partial" "$partial.dump.partial"
partial="$partial.dump.partial"
pg_dump --format=custom --file "$partial"
mv "$partial" "${partial%.partial}"
