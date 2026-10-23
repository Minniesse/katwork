#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
mkdir "$tmp/bin" "$tmp/backups"
export KATWORK_BACKUP_DIR="$tmp/backups" PATH="$tmp/bin:$PATH"
export BACKUP_TEST_PASSWORD
BACKUP_TEST_PASSWORD=$(openssl rand -hex 16)
export DATABASE_URL="postgres://katwork:$BACKUP_TEST_PASSWORD@postgres:5432/restored"
cat > "$tmp/bin/psql" <<'SCRIPT'
#!/bin/sh
set -eu
[ "$PGHOST" = postgres ] && [ "$PGPORT" = 5432 ]
[ "$PGUSER" = katwork ] && [ "$PGDATABASE" = restored ]
[ "$PGPASSWORD" = "$BACKUP_TEST_PASSWORD" ]
case "$*" in *"$BACKUP_TEST_PASSWORD"*) exit 2 ;; esac
[ "${PROBE_FAIL:-0}" = 0 ] || exit 1
printf '%s\n' "${HAS_SCHEMA:-t}"
SCRIPT
cat > "$tmp/bin/pg_dump" <<'SCRIPT'
#!/bin/sh
set -eu
[ "$PGDATABASE" = restored ] && [ "$PGPASSWORD" = "$BACKUP_TEST_PASSWORD" ]
[ "$1" = --format=custom ]
[ "$2" = --file ]
case "$3" in *.dump.partial) ;; *) exit 2 ;; esac
printf '%s\n' 'archive fixture' > "$3"
[ "${DUMP_FAIL:-0}" = 0 ]
SCRIPT
chmod +x "$tmp/bin/psql" "$tmp/bin/pg_dump"
for invalid in '' 'postgres://missing-fields' "$DATABASE_URL?dbname=other" "$DATABASE_URL#other" "${DATABASE_URL%/*}/.." "postgres://katwork:$BACKUP_TEST_PASSWORD@postgres:5432/a/b"; do
    if DATABASE_URL="$invalid" "$root/scripts/compose-backup.sh" > "$tmp/out" 2>&1; then exit 1; fi
    if grep -qF "$BACKUP_TEST_PASSWORD" "$tmp/out"; then exit 1; fi
done
HAS_SCHEMA=f "$root/scripts/compose-backup.sh"
[ "$(find "$KATWORK_BACKUP_DIR" -type f | wc -l | tr -d ' ')" = 0 ]
if PROBE_FAIL=1 "$root/scripts/compose-backup.sh"; then exit 1; fi
if DUMP_FAIL=1 "$root/scripts/compose-backup.sh"; then exit 1; fi
[ "$(find "$KATWORK_BACKUP_DIR" -type f | wc -l | tr -d ' ')" = 0 ]
"$root/scripts/compose-backup.sh"
"$root/scripts/compose-backup.sh"
[ "$(find "$KATWORK_BACKUP_DIR" -name '*.dump' | wc -l | tr -d ' ')" = 2 ]
[ "$(find "$KATWORK_BACKUP_DIR" -name '*.partial' | wc -l | tr -d ' ')" = 0 ]
for archive in "$KATWORK_BACKUP_DIR"/*.dump; do
    [ "$(cat "$archive")" = 'archive fixture' ]
    mode=$(stat -c %a "$archive" 2>/dev/null || stat -f %Lp "$archive")
    [ "$mode" = 600 ]
done
printf '%s\n' 'backup tests passed'
