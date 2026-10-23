#!/bin/sh
set -eu
umask 077
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
