# KatWork

**Planned: the self-hosted AI team office.**
**อยู่ในแผนพัฒนา: ห้องทำงานทีมผู้ช่วย AI บนเซิร์ฟเวอร์ขององค์กร**

The AI coworkers are called **Kats** in English and Thai. See [ADR-0006](docs/adr/0006-rename-dots-to-kats.md).

Pre-alpha planning status. Public-alpha target: week of 2027-01-25.
No capability is verified “in alpha” in this README.

[Status](#status) · [Try it](#try-it) · [Setup](#setup) · [Sample](#sample) · [Scope](#scope) · [Data path](#data-path) · [ภาษาไทย](#thai) · [Evaluation](#evaluation)

<a id="status"></a>
## Status

Use planned / in alpha / later beside each capability. “In alpha” requires a named
release, reproducible checks and known limitations. Synthetic inputs do not prove
working automation. Unbuilt UI must say “concept mockup — planned.”

<a id="setup"></a>
## Setup

KatWork runs on one Linux host with Docker Engine and Docker Compose **2.24.4 or
newer**. The core is Rust, the web app is React/Vite, and the core licence is
Apache-2.0. This is development software, not a public-alpha release. The Compose
smoke checks run in CI; they do not establish production readiness.

<a id="try-it"></a>
### Try it: offline demo

From this checkout, run:

```sh
scripts/demo.sh
```

The first run builds or downloads the images, so it needs internet access.
Once those images are available, the demo works without internet access. Open
**http://localhost:3000** and sign in as **demo@katwork.local**. Open the private
`.demo-credentials` file on the host to read the initial password. If you set
`KATWORK_DEMO_PASSWORD` in `.env`, use that instead. Scripts never print passwords.
Rerunning the script preserves an existing account's password.

The demo includes English `Demo` and Thai `เดโม` workspaces, each with a team of
six Kats. Replies are synthetic and marked as such; no model or mail service is
called. The demo override clears provider and mail settings. It is available
only from this host. Docker keeps a normal app network to publish the local port;
the demo configuration disables external services, rather than acting as a firewall. It exercises the
API and streaming pipeline; available web pages depend on your checkout.

### First account

Run these commands on the Docker host, from this checkout:

1. Create a private settings file with random secrets:

   ```sh
   scripts/init-env.sh
   ```

   This writes `.env` with permissions `600`. Keep it private and back it up
   securely. A second run refuses to replace it. `--force` deliberately replaces
   it, including the database password; do not use that on an existing install.
   Open `.env` in a text editor for optional model or mail settings.

2. Start KatWork:

   ```sh
   docker compose up -d --build
   ```

   Compose waits for PostgreSQL, backs up an existing database, applies
   migrations, then starts KatWork. A fresh database needs no backup. Visit
   **http://localhost:3000**. If it does not start, run `docker compose ps -a` and
   `docker compose logs backup migrate katwork`. A bad setting is reported as
   `config_invalid (NAME)`.

3. Create the first owner and their workspace:

   ```sh
   docker compose exec katwork /app/katwork-server create-user \
     --email you@example.com --workspace "My team" --role owner \
     --space-policy local --locale en
   ```

   Choose `--locale en` or `--locale th`. Thai creates Thai Kat names. The first
   Space, `General`, has six Kats: a Lead and five specialists. `local` keeps
   model calls on your declared local provider; choose `hosted` to allow hosted
   providers. Configure a model before expecting real replies.

4. Open the printed link within 72 hours, choose a password, then sign in.
   The link works once.

Use `docker compose exec`, never `docker compose run`, for account commands:
`run` starts a container whose output Docker's logging driver keeps, so the link
would enter a log. Operator commands intentionally print links to your terminal;
the convenience scripts do not print credentials.

The link uses `KATWORK_PUBLIC_URL` (default `http://localhost:3000`). For people
on other machines, use HTTPS below. Without mail, recover a forgotten password
with `docker compose exec katwork /app/katwork-server reset-link --email you@example.com`.
The link works once within 30 minutes. **Anyone who can run commands on this host
can take over any account with `reset-link`. Give host access only to operators.**

### HTTPS

Point your domain's DNS at this host and allow incoming ports 80 and 443. On a
fresh install, replace `work.example.com` with that domain:

```sh
scripts/init-env.sh --https work.example.com
docker compose up -d --build
```

On an existing install, keep your secrets. Add `KATWORK_HTTPS_HOST=work.example.com`
and `COMPOSE_FILE=docker-compose.yml:docker-compose.https.yml` to `.env`, stop the
old stack with `docker compose down`, then start it again. Open your HTTPS address.
Caddy obtains the certificate and is the only public entry point. KatWork has no
published port in this configuration. Caddy's certificate state lives in the
`caddy_data` and `caddy_config` volumes; keep both across upgrades.

The override trusts only Caddy at `172.31.250.2`, on subnet `172.31.250.0/29`.
The app uses `172.31.250.3`, so it cannot take Caddy's address during startup.
That subnet must be free on your host. Keep `KATWORK_TRUSTED_PROXIES` limited to
proxies you operate. Caddy keeps event streams uncompressed and flushes them
immediately. It bounds header and idle waits and enables TCP keepalive on the
browser connection. The app still limits total connections and authenticated
streams; stock Caddy does not provide a per-client connection quota. Add edge
rate limits before exposing a large public installation.

Demo mode is refused on HTTPS unless `KATWORK_ALLOW_DEMO_ON_HTTPS=1` is explicitly
set. Keep demo mode off for normal installations. `localhost` uses Caddy's local
certificate authority, suitable for testing; a browser must trust that authority.

### Backups and upgrades

Before an upgrade, stop the running app, keep its volumes, build the new version,
then start it:

```sh
docker compose down
git pull --ff-only
docker compose build katwork
docker compose up -d
```

Do not add `-v` to `down`: that deletes the database and certificate volumes.
Stopping first matters: a failed backup cannot stop an app that was already
running. Every startup waits for the backup to succeed before it runs migrations
or starts the app. Do not run migrations by hand around that gate.

Existing databases are saved under `backups/katwork-<UTC timestamp>-<suffix>.dump`.
On Linux, these private files belong to the container's root user; use operator
privileges to copy or restore them. The script writes a private `.partial` file and renames it only after `pg_dump`
succeeds. A failed backup stops startup. Check free disk space and file permissions,
fix the cause, then start again. No retention policy deletes old backups; copy them
to secure storage and remove old files yourself after checking a restore.

A dump contains **one database**, including users and migration history. It does
not contain PostgreSQL roles or tablespaces, `.env`, or Caddy's certificates.
Keep those separately. To restore into a fresh PostgreSQL 17 server, first create
the role and empty database, using that server's operator connection settings:

```sh
createuser --login --pwprompt katwork
createdb --owner=katwork katwork
pg_restore --exit-on-error --no-owner --username=katwork --dbname=katwork /path/to/backup.dump
```

Set the restored role's password to match your private settings. Stop KatWork
while restoring, and point its `DATABASE_URL` at the restored database before
starting it. The restore smoke check verifies the demo user and all migration
versions in a separate PostgreSQL 17 container.

### Local model provider (development)

Set `KATWORK_LOCAL_BASE_URL` to an OpenAI-compatible API root such as
`http://local-llm:8000/v1`, and set `KATWORK_LOCAL_MODEL` to a loaded model ID.
`KATWORK_LOCAL_API_KEY` is optional; without it KatWork sends no Authorization
header. The base URL must not contain a query, fragment or username/password;
use the API key variable for authentication.
Startup does not discover models or call the model server. The same model
serves all three tiers, with output caps of 2,048, 4,096 and 8,192 tokens.

From the KatWork container, reach a model server on the Docker host at
`http://host.docker.internal:<port>/v1`, never `localhost`, which names the
container itself. Compose maps this hostname to the host gateway on Linux too.
Ollama defaults to port `11434`, and vLLM to `8000`; for llama.cpp use `8080`
(set `--port 8080` explicitly because defaults vary across releases).
For Ollama on another machine, set `OLLAMA_HOST=0.0.0.0` on that machine and use
its LAN hostname/IP in the base URL. The server must also listen on an interface
reachable from the container when it runs on the Docker host.

A local-only Space selects this provider without demo mode. A Space allowing
external providers prefers a configured hosted entry. Configuring a local provider
disables demo fallback. Local entries allow 120 seconds to first content, 60 seconds
between content chunks and a 300-second run budget; hosted and demo entries use
30, 30 and 90 seconds. Task and job deadlines still bound runs.

**Residency is operator-declared.** KatWork does not verify that the URL stays
inside your network. A remote URL configured here receives local-only prompts.
Local providers ignore `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` environment
variables, including lowercase variants; hosted providers keep proxy support.
Redirects are disabled. Responses are capped at 16 MiB, error bodies at 64 KiB,
and SSE frames at 1 MiB, before Rig buffers or decodes them.

The tools capability describes the adapter, not a guarantee about your model.
[Ollama](https://docs.ollama.com/api/openai-compatibility) needs the selected model
pulled; KatWork sends tools and requests streamed usage without forcing
`tool_choice`. [vLLM](https://docs.vllm.ai/en/latest/features/tool_calling/) needs
a compatible chat template, `--enable-auto-tool-choice` and a matching
`--tool-call-parser`. [llama.cpp server](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)
needs `--jinja` and a compatible tool template. Model support also varies in
[LM Studio](https://lmstudio.ai/docs/developer/openai-compat/tools).

Ollama's default context window can be small and silently truncate prompts.
Raise the model's `num_ctx` or the server's `OLLAMA_CONTEXT_LENGTH` to fit the
prompt and output budget; KatWork's OpenAI-compatible requests cannot set
`num_ctx`. See [Ollama context configuration](https://docs.ollama.com/context-length).
For vLLM, `--max-model-len` must fit the prompt plus the Strong tier's 8,192 output
tokens, or requests fail with HTTP 400. See [vLLM server options](https://docs.vllm.ai/en/latest/cli/serve/).
For thinking models, enable the server's reasoning parser (vLLM
[`--reasoning-parser`](https://docs.vllm.ai/en/latest/features/reasoning_outputs/),
llama.cpp `--reasoning-format deepseek`), or `<think>` text appears in Kats' replies.

Usage-only tails are accepted; absent counters stay unknown. Complete streamed
arguments are buffered until the turn is accepted; malformed or truncated
arguments never become executable calls. No local model is qualified for Thai
office work by these synthetic compatibility tests. Conservative missing-usage
charging assumes at most one token per UTF-8 byte, which operators must verify
for their selected model.

<a id="sample"></a>
## Sample

Planned: a synthetic folder-to-DOCX/XLSX exercise with a metric source map,
missing/conflicting-evidence list and human review. No downloadable fixture is
claimed by this README. Publish the files and test record when they exist.

<a id="scope"></a>
## Scope

| Capability | Status | Boundary |
|---|---|---|
| Named assistants, roles, memory and delegation | planned | Access and recovery tests required |
| Rivet agentOS workspaces and separate browser containers/profiles | planned | Isolation must be demonstrated; no per-assistant VM claim |
| 3D office, live agent-call graph, chat and accessible tasks-and-files board | planned | Views must match actual events |
| Human approval bound to action/artifact revision and cryptographic digest | planned | Stale-approval and changed-target tests required |
| Source extraction, citations, calculations and gap/conflict register | planned | Numeric audit required; no perfect-accuracy claim |
| DOCX/XLSX and PDF handling, Thai OCR, TH Sarabun rendering | planned | Named supported subsets and fidelity tests required |
| English/Thai UI and outputs; qualified local/external models | planned | Publish tested model/language combinations |
| LINE, Slack, Teams, RustChat/Mattermost, Telegram, Discord, official WhatsApp | planned | Per-channel message, attachment, identity and approval checks |
| Web, Tauri desktop and Capacitor mobile | planned | Per-platform setup/workflow checks |
| University SAR / annual report template | planned — alpha target | Agreed DOCX template and evidence mapping |
| One Report sustainability section template | planned — alpha target | Bilingual DOCX and XLSX appendix |
| Meeting-minutes template | planned — alpha target | Sourced decisions and action list |
| Research-brief template | planned — alpha target | Cited DOCX; PPTX later |
| Monthly board commentary template | planned — alpha target | Bilingual DOCX and XLSX variance/lineage; PPTX later |
| Hospital HA v6 self-assessment rubric pack | planned — alpha target | SAR-engine rubric; redaction check; no clinical trial |
| Federation between two installs | planned — alpha target | First cut if delivery slips; publish any cut |
| Craft apps | planned — gated evaluation | Not promised; app-specific integration/licence/Thai/fidelity/approval gates |
| PPTX and Thai OCR at scale | later | No guaranteed alpha output |
| Enterprise administration and paid cloud | later | Separate validated scope and terms |

<a id="data-path"></a>
## Data path

Planned: customer-operated deployment with approved provider choices. Self-hosting
does not establish locality of model, OCR, connector, log or backup traffic.
KatWork will not train its own models on pilot data; external providers need
separate retention/training review. Use public or approved retrospective samples.

<a id="thai"></a>
## ภาษาไทย

KatWork ยังอยู่ระหว่างพัฒนาครับ การช่วยเตรียมร่างรายงานพร้อมแหล่งอ้างอิง
และการอนุมัติโดยผู้รับผิดชอบยังอยู่ในแผนพัฒนา (planned)
ตารางข้างต้นแยกความสามารถที่อยู่ในแผนพัฒนาและแผนระยะถัดไป (later)
ไม่มีรายการใดในเอกสารนี้ที่ยืนยันว่ามีในรุ่น alpha แล้ว (in alpha)
เป้าหมายเปิด public alpha คือสัปดาห์เริ่มวันที่ 25 มกราคม 2570
กรุณาใช้ข้อมูลสาธารณะหรือข้อมูลย้อนหลังที่ได้รับอนุญาตในการหารือเบื้องต้น

<a id="evaluation"></a>
## Evaluation

Apply through the project's verified contact route once published. An LOI reserves
planning, not immediate access. The proposed 30 days begin at agreed readiness.
Agree scope, reviewer, data path, compute allowance/payer and support windows first.
No purchase obligation or unlimited use is implied.

## Licence

Apache-2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
