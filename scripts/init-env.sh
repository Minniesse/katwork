#!/bin/sh
set -eu
umask 077
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
output=${KATWORK_ENV_FILE:-"$root/.env"}
force=0
host=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --force) force=1; shift ;;
        --https)
            [ "$#" -ge 2 ] || { printf '%s\n' 'Usage: scripts/init-env.sh [--force] [--https host]' >&2; exit 1; }
            host=$2
            shift 2
            case "$host" in
                ''|*[!A-Za-z0-9.-]*)
                    printf '%s\n' 'HTTPS host must be a DNS name, without a scheme, port or path.' >&2
                    exit 1 ;;
            esac
            if ! printf '%s\n' "$host" | LC_ALL=C awk '
                length($0) > 253 || $0 !~ /^[A-Za-z0-9.-]+$/ { exit 1 }
                { n=split($0, labels, "."); for (i=1; i<=n; i++)
                    if (length(labels[i]) < 1 || length(labels[i]) > 63 || labels[i] !~ /^[A-Za-z0-9]/ || labels[i] !~ /[A-Za-z0-9]$/) exit 1 }
            '; then
                printf '%s\n' 'HTTPS host must be a DNS name, without a scheme, port or path.' >&2
                exit 1
            fi ;;
        *) printf '%s\n' 'Usage: scripts/init-env.sh [--force] [--https host]' >&2; exit 1 ;;
    esac
done
if [ -d "$output" ] || { [ "$force" -eq 0 ] && { [ -e "$output" ] || [ -L "$output" ]; }; }; then
    printf '%s\n' 'Settings already exist; use --force only to replace them deliberately.' >&2
    exit 1
fi
secret=$(openssl rand -base64 48)
password=$(openssl rand -hex 24)
tmp=$(mktemp "${output}.tmp.XXXXXX")
trap 'rm -f "$tmp"' EXIT HUP INT TERM
while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
        KATWORK_SECRET=*) line="KATWORK_SECRET=$secret" ;;
        POSTGRES_PASSWORD=*) line="POSTGRES_PASSWORD=$password" ;;
        DATABASE_URL=*) line="DATABASE_URL=postgres://katwork:$password@postgres:5432/katwork" ;;
        KATWORK_HTTPS_HOST=*) line="KATWORK_HTTPS_HOST=$host" ;;
    esac
    printf '%s\n' "$line"
done < "$root/.env.example" > "$tmp"
if [ -n "$host" ]; then
    printf '%s\n' 'COMPOSE_FILE=docker-compose.yml:docker-compose.https.yml' >> "$tmp"
fi
if [ "$force" -eq 1 ]; then
    mv -f "$tmp" "$output"
else
    # Hard-link publication refuses even a concurrent writer or a dangling symlink.
    ln "$tmp" "$output"
fi
printf '%s\n' "$output"
