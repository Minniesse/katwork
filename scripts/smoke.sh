#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
export COMPOSE_FILE=docker-compose.yml:docker-compose.smoke.yml
export KATWORK_IMAGE=${KATWORK_IMAGE:-katwork-smoke:local}
if [ ! -e .env ]; then KATWORK_ENV_FILE="$root/.env" scripts/init-env.sh; fi
cargo test -p katwork-server --locked --test e2e_compose first_run_and_profile_configuration -- --ignored --exact
# One image serves every isolated test project, including migrate.
docker compose build katwork
cargo test -p katwork-server --locked --test e2e_compose -- --ignored --test-threads=1 --skip first_run_and_profile_configuration --skip upgrade_from_previous_release
printf '%s\n' 'SMOKE OK'
