#!/bin/sh
# Copies one files backup, backups/katwork-<database>-<timestamp>-<suffix>.files, back into the
# artifacts volume, owned by the app's user. Files the volume already holds stay; each file is
# checked against its SHA-256 name, and a damaged one is reported and never copied in.
set -eu
umask 077
export LC_ALL=C
directory=${KATWORK_BACKUP_DIR:-/backups}
files_root=${KATWORK_ARTIFACT_DIR:-/var/lib/katwork/artifacts}
owner=${KATWORK_ARTIFACT_OWNER:-65532:65532}
usage() {
    printf '%s\n' 'Usage: docker compose run --rm restore-files katwork-DATABASE-TIMESTAMP-SUFFIX.files' >&2
    exit 2
}
[ "$#" -eq 1 ] || usage
backup=$1
case "$backup" in
    */* | .*) usage ;;
    katwork-*.files) ;;
    *) usage ;;
esac
source="$directory/$backup"
if [ ! -d "$source" ]; then
    printf 'No files backup named %s in the backup folder.\n' "$backup" >&2
    exit 1
fi
digest() {
    if command -v sha256sum > /dev/null 2>&1; then sha256sum; else shasum -a 256; fi | cut -c1-64
}
list=$(mktemp)
partial=''
trap 'rm -f "$list"; [ -z "$partial" ] || rm -f "$partial"' EXIT HUP INT TERM
find "$source" -type f > "$list"
damaged=0
while IFS= read -r file; do
    relative=${file#"$source"/}
    name=${file##*/}
    rest=${name#??}
    case "$name" in *[!0-9a-f]*) continue ;; esac
    [ "${#name}" -eq 64 ] || continue
    [ "$relative" = "${relative%%/*}/sha256/${name%"$rest"}/${rest%"${rest#??}"}/$name" ] || continue
    target="$files_root/$relative"
    [ -d "${target%/*}" ] || mkdir -p "${target%/*}"
    partial="$target.restoring"
    cp "$file" "$partial"
    if [ "$(digest < "$partial")" = "$name" ]; then
        mv "$partial" "$target"
    else
        rm -f "$partial"
        printf 'Not restored, damaged in the backup: %s\n' "$relative" >&2
        damaged=1
    fi
    partial=''
done < "$list"
# The app's user owns the volume and everything in it, and its folder stays private.
chmod 0700 "$files_root"
chown -R "$owner" "$files_root"
if [ "$damaged" -ne 0 ]; then
    printf 'Some files of %s were damaged and not restored.\n' "$backup" >&2
    exit 1
fi
printf 'Restored the files of %s.\n' "$backup"
