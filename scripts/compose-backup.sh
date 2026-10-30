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
# The artifacts volume, mounted read-only: the uploaded files and the files Kats write.
files_root=${KATWORK_ARTIFACT_DIR:-/var/lib/katwork/artifacts}
folder_error() {
    printf 'Cannot write backup folder %s. On Linux, fix its host folder (backups/; demo: backups-demo/) with: sudo chown 0 FOLDER && sudo chmod 0755 FOLDER\n' "$directory" >&2
    exit 1
}
if [ ! -d "$files_root" ]; then
    printf 'Cannot read the files volume at %s.\n' "$files_root" >&2
    exit 1
fi
mkdir -p "$directory" || folder_error
# The random suffix prevents two starts in one second from replacing a good archive.
base=$(mktemp "$directory/katwork-$PGDATABASE-$(date -u +%Y%m%dT%H%M%SZ)-XXXXXXXXXXXX") || folder_error
dump="$base.dump.partial"
files="$base.files.partial"
list=''
trap 'rm -rf "$base" "$dump" "$files"; [ -z "$list" ] || rm -f "$list"' EXIT HUP INT TERM
mv "$base" "$dump"
pg_dump --format=custom --file "$dump"
# The files, copied after the dump so the copy holds every file the dump names. Each stored
# file is named by its SHA-256 and never changed, so a name the newest earlier backup holds is
# linked from it, costing no space; a new one is copied and checked against its name.
digest() {
    if command -v sha256sum > /dev/null 2>&1; then sha256sum; else shasum -a 256; fi | cut -c1-64
}
previous=$(ls -1dt "$directory"/katwork-*.files 2> /dev/null | head -n 1)
list=$(mktemp)
find "$files_root" -type f -path "$files_root/*/sha256/*" > "$list"
mkdir "$files"
while IFS= read -r source; do
    relative=${source#"$files_root"/}
    name=${source##*/}
    rest=${name#??}
    case "$name" in *[!0-9a-f]*) continue ;; esac
    # Only <workspace>/sha256/<aa>/<bb>/<64 hex digits>, as the app stores a file.
    [ "${#name}" -eq 64 ] || continue
    [ "$relative" = "${relative%%/*}/sha256/${name%"$rest"}/${rest%"${rest#??}"}/$name" ] || continue
    target="$files/$relative"
    [ -d "${target%/*}" ] || mkdir -p "${target%/*}"
    if [ -n "$previous" ] && [ -f "$previous/$relative" ] && ln "$previous/$relative" "$target" 2> /dev/null; then
        continue
    fi
    cp "$source" "$target"
    if [ "$(digest < "$target")" != "$name" ]; then
        rm -f "$target"
        printf 'Skipped a damaged file: %s does not match its SHA-256 name.\n' "$relative" >&2
    fi
done < "$list"
mv "$files" "$base.files"
mv "$dump" "$base.dump"
