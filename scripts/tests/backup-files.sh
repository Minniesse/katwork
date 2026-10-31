#!/bin/sh
# The files half of the automatic backup (scripts/compose-backup.sh) and its restore
# (scripts/compose-restore-files.sh), on a fake artifacts folder with fake psql and pg_dump.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
mkdir "$tmp/bin" "$tmp/backups" "$tmp/artifacts" "$tmp/restored"
export KATWORK_BACKUP_DIR="$tmp/backups" KATWORK_ARTIFACT_DIR="$tmp/artifacts" PATH="$tmp/bin:$PATH"
export DATABASE_URL="postgres://katwork:fixture@postgres:5432/katwork"
cat > "$tmp/bin/psql" <<'SCRIPT'
#!/bin/sh
printf '%s\n' t
SCRIPT
cat > "$tmp/bin/pg_dump" <<'SCRIPT'
#!/bin/sh
set -eu
printf '%s\n' 'archive fixture' > "$3"
# A file stored while the dump runs.
if [ -n "${DUMP_ADDS:-}" ]; then
    mkdir -p "${DUMP_ADDS%/*}"
    printf '%s' "$DUMP_ADDS_TEXT" > "$DUMP_ADDS"
fi
[ "${DUMP_FAIL:-0}" = 0 ]
SCRIPT
chmod +x "$tmp/bin/psql" "$tmp/bin/pg_dump"
digest() {
    if command -v sha256sum > /dev/null 2>&1; then sha256sum; else shasum -a 256; fi | cut -c1-64
}
mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
inode() { stat -c %i "$1" 2>/dev/null || stat -f %i "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }
# The relative path at which the app stores TEXT under WORKSPACE.
stored_path() {
    hex=$(printf '%s' "$2" | digest)
    printf '%s\n' "$1/sha256/$(printf '%s' "$hex" | cut -c1-2)/$(printf '%s' "$hex" | cut -c3-4)/$hex"
}
# Stores TEXT the way the app does, under WORKSPACE, and prints its relative path.
blob() {
    relative=$(stored_path "$1" "$2")
    mkdir -p "$tmp/artifacts/${relative%/*}"
    printf '%s' "$2" > "$tmp/artifacts/$relative"
    printf '%s\n' "$relative"
}
backups() { find "$KATWORK_BACKUP_DIR" -mindepth 1 -maxdepth 1 -name "*$1" | sort; }
w1=01900000-0000-7000-8000-000000000001
w2=01900000-0000-7000-8000-000000000002
first=$(blob "$w1" 'first file')
second=$(blob "$w2" 'second file')
# Not files the app stores: a staging file, a stray file and a file whose name is not its hash.
mkdir -p "$tmp/artifacts/$w1/tmp"
printf 'half' > "$tmp/artifacts/$w1/tmp/upload"
printf 'stray' > "$tmp/artifacts/$w1/notes"
damaged=$(blob "$w1" 'damaged file')
printf 'changed' > "$tmp/artifacts/$damaged"

# The first backup: the dump and a .files folder of the same name, with the stored files only.
"$root/scripts/compose-backup.sh" 2> "$tmp/err"
grep -qF "${damaged##*/}" "$tmp/err" || fail 'a damaged file is reported'
[ "$(backups .dump | wc -l | tr -d ' ')" = 1 ] || fail 'one dump'
[ "$(backups .files | wc -l | tr -d ' ')" = 1 ] || fail 'one files folder'
dump=$(backups .dump)
one="${dump%.dump}.files"
[ -d "$one" ] || fail 'the files folder shares the dump name'
[ "$(cat "$one/$first")" = 'first file' ] || fail 'first file copied'
[ "$(cat "$one/$second")" = 'second file' ] || fail 'second file copied'
[ "$(find "$one" -type f | wc -l | tr -d ' ')" = 2 ] || fail 'only stored, undamaged files'
[ "$(mode "$one")" = 700 ] && [ "$(mode "${one}/$w1")" = 700 ] || fail 'private folders'
[ "$(mode "$one/$first")" = 600 ] || fail 'private files'
[ -z "$(find "$KATWORK_BACKUP_DIR" -name '*.partial')" ] || fail 'no partial left'

# The second backup links the files it already holds and copies only the new one.
third=$(blob "$w1" 'third file')
rm "$tmp/artifacts/$damaged"
"$root/scripts/compose-backup.sh"
[ "$(backups .dump | wc -l | tr -d ' ')" = 2 ] || fail 'two dumps'
two=$(backups .files | grep -vxF "$one")
[ "$(inode "$two/$first")" = "$(inode "$one/$first")" ] || fail 'an unchanged file is a hard link'
[ "$(inode "$two/$second")" = "$(inode "$one/$second")" ] || fail 'every unchanged file is linked'
[ "$(cat "$two/$third")" = 'third file' ] || fail 'a new file is copied'
[ "$(find "$two" -type f | wc -l | tr -d ' ')" = 3 ] || fail 'three files in the second backup'

# A file gone from the volume is gone from the next backup; older backups keep it.
rm "$tmp/artifacts/$second"
"$root/scripts/compose-backup.sh"
three=$(backups .files | grep -vxF -e "$one" -e "$two")
[ ! -e "$three/$second" ] || fail 'a removed file leaves the next backup'
[ -f "$two/$second" ] || fail 'an older backup keeps it'

# The copy comes after the dump, so it holds every file the dump can name: here, one stored
# while the dump runs.
during=$(stored_path "$w2" 'stored during the dump')
DUMP_ADDS="$tmp/artifacts/$during" DUMP_ADDS_TEXT='stored during the dump' "$root/scripts/compose-backup.sh" 2> /dev/null
four=$(backups .files | grep -vxF -e "$one" -e "$two" -e "$three")
[ "$(cat "$four/$during" 2> /dev/null)" = 'stored during the dump' ] || fail 'the copy comes after the dump'

# A failed dump publishes neither half.
before=$(find "$KATWORK_BACKUP_DIR" | wc -l | tr -d ' ')
if DUMP_FAIL=1 "$root/scripts/compose-backup.sh" 2> /dev/null; then fail 'a failed dump fails'; fi
[ "$(find "$KATWORK_BACKUP_DIR" | wc -l | tr -d ' ')" = "$before" ] || fail 'nothing published'
# No files volume: the backup fails before the dump.
if KATWORK_ARTIFACT_DIR="$tmp/missing" "$root/scripts/compose-backup.sh" 2> "$tmp/err"; then
    fail 'a missing files volume fails'
fi
grep -qF "$tmp/missing" "$tmp/err" || fail 'the missing volume is named'
[ "$(find "$KATWORK_BACKUP_DIR" | wc -l | tr -d ' ')" = "$before" ] || fail 'still nothing published'

# The restore copies one backup into the volume, owned by the app's user.
restore() { KATWORK_ARTIFACT_DIR="$tmp/restored" KATWORK_ARTIFACT_OWNER="$(id -u):$(id -g)" "$root/scripts/compose-restore-files.sh" "$@"; }
chmod 0755 "$tmp/restored"
restore "${two##*/}" > "$tmp/out"
grep -qF "${two##*/}" "$tmp/out" || fail 'the restore names its backup'
[ "$(cat "$tmp/restored/$first")" = 'first file' ] || fail 'first file restored'
[ "$(cat "$tmp/restored/$second")" = 'second file' ] || fail 'second file restored'
[ "$(cat "$tmp/restored/$third")" = 'third file' ] || fail 'third file restored'
[ "$(find "$tmp/restored" -type f | wc -l | tr -d ' ')" = 3 ] || fail 'only the backup files'
[ "$(mode "$tmp/restored")" = 700 ] || fail 'the volume folder is private'
[ "$(mode "$tmp/restored/$w1")" = 700 ] && [ "$(mode "$tmp/restored/$first")" = 600 ] || fail 'private restored files'
# Restoring again changes nothing; restoring an older backup only adds what it holds.
restore "${two##*/}" > /dev/null
restore "${three##*/}" > /dev/null
[ "$(find "$tmp/restored" -type f | wc -l | tr -d ' ')" = 3 ] || fail 'restores merge'
# Names outside the backup folder, missing backups and a damaged file are refused.
for name in '' "../${two##*/}" "${two##*/}/.." 'katwork-x.dump' 'katwork-missing.files'; do
    if restore "$name" 2> /dev/null; then fail "restore of '$name' is refused"; fi
done
printf 'tampered' > "$two/$third"
if restore "${two##*/}" 2> "$tmp/err"; then fail 'a damaged backup file fails the restore'; fi
grep -qF "${third##*/}" "$tmp/err" || fail 'the damaged file is named'
[ "$(cat "$tmp/restored/$third")" = 'third file' ] || fail 'a damaged copy never replaces a good file'
[ -z "$(find "$tmp/restored" -name '*.restoring')" ] || fail 'no partial restore left'
printf '%s\n' 'files backup tests passed'
