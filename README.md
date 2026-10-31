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

From this checkout on Linux, prepare the demo's backup folder and run:

```sh
sudo install -d -o 0 -g 0 -m 0755 backups-demo
scripts/demo.sh
```

The first run builds or downloads the images, so it needs internet access.
Once those images are available, the demo works without internet access. Open
**http://localhost:3001** and sign in as **demo@katwork.local**. Open the private
`.demo-credentials` file on the host to read the initial password. If you set
`KATWORK_DEMO_PASSWORD` in `.env.demo`, use that instead. Scripts never print passwords.
Rerunning the script preserves an existing account's password.

The demo uses its own `katwork-demo` project, database volumes and `.env.demo`
settings. It does not change your install's database. The script refuses to run
while your install is running; pass `--allow-running-install` to deliberately
run both. Stop the demo with:

```sh
docker compose -p katwork-demo --env-file .env.demo -f docker-compose.yml -f docker-compose.demo.yml down
```

The demo includes English `Demo` and Thai `เดโม` workspaces, each with a team of
six Kats. Replies are synthetic and marked as such; no model or mail service is
called. The demo override clears provider and mail settings. It is available
only from this host. The app has no outbound network route. A small Caddy proxy
publishes the local port and forwards requests to the private app network.
The demo exercises the API and streaming pipeline; available web pages depend
on your checkout.

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

   On Linux, prepare the backup folder once, including if you created it yourself:

   ```sh
   sudo install -d -o 0 -g 0 -m 0755 backups
   ```

   The backup container runs as root with all capabilities dropped. It needs a
   root-owned folder; backup files remain private with permissions `600`.

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
HTTP redirects to HTTPS. HTTPS responses tell browsers to keep using HTTPS for
one year (HSTS); the policy does not preload the domain or cover subdomains.

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

For an office LAN, ask your network administrator to point `work.home.arpa`
at this server's LAN address. Use `scripts/init-env.sh --https work.home.arpa`
on a fresh install, or change `KATWORK_HTTPS_HOST` in the existing `.env`.
Start with the HTTPS override as above. Caddy automatically uses its private
certificate authority for `.home.arpa` and `.internal` names; see
[Caddy's local HTTPS guide](https://caddyserver.com/docs/automatic-https#local-https).
Export the public root certificate:

```sh
docker compose cp caddy:/data/caddy/pki/authorities/local/root.crt ./katwork-root.crt
```

Give only this `.crt` file to office users, never the authority's private key.
Have your administrator install it in Windows' **Trusted Root Certification
Authorities**, or the macOS **System** keychain with **Always Trust**. On iPhones
and iPads, install the certificate profile, then enable it under **Settings →
General → About → Certificate Trust Settings**. On Android, use **Settings →
Security → Install a certificate → CA certificate** (names vary by device).
Managed devices may require the administrator to do this. Then open
**https://work.home.arpa**; do not bypass certificate warnings.

### Backups and upgrades

Before an upgrade, stop the running app, keep its volumes, build the new version,
then start it:

```sh
docker compose down
git pull --ff-only
docker compose build katwork
docker compose up -d
```

Do not add `-v` to `down`: that deletes the database, uploaded files and certificate volumes.
Stopping first matters: a failed backup cannot stop an app that was already
running. Every startup waits for the backup to succeed before it runs migrations
or starts the app. Do not run migrations by hand around that gate.

Every startup saves the existing database, then the uploaded files and the files
Kats write (the `artifacts` volume), as a pair that shares one name:

- `backups/katwork-<database>-<UTC timestamp>-<suffix>.dump`, the database;
- `backups/katwork-<database>-<UTC timestamp>-<suffix>.files/`, a folder of the files,
  copied just after that dump, so it holds every file the dump names.

The timestamp uses sortable `YYYYMMDDTHHMMSSZ` format. The demo writes only to
`backups-demo/`, using the same naming pattern. Check the folder and database
name when choosing a backup; do not restore a demo backup into your real installation.

On Linux, prepare each folder you use with the root owner and permissions `755`:

```sh
sudo install -d -o 0 -g 0 -m 0755 backups
sudo install -d -o 0 -g 0 -m 0755 backups-demo
```

These private files belong to the container's root user; use operator privileges
to copy or restore them. If startup reports a folder permission error, run the
matching command above and start again. The script writes a private `.partial` file
and folder and renames them only after `pg_dump` and the file copy succeed. A failed
backup stops startup. Check free disk space and file permissions, fix the cause, then
start again. No retention policy deletes old backups; copy them to secure storage and
remove old pairs yourself, the `.dump` and its `.files/` together, after checking a
restore.

Each file is stored under its SHA-256 and never changes, so a backup copies only the
files that are new since the previous backup and hard-links the others to it: a backup
adds the size of its new files, and removing an old pair frees only the files no newer
backup holds. Each new file is checked against its SHA-256 as it is copied; a damaged
one is left out and named in `docker compose logs backup`. To copy backups elsewhere,
keep the hard links (`sudo cp -a backups DESTINATION`, or `rsync -aH`), or each folder
takes its full size.

A dump contains **one database**, including users and migration history. It does
not contain PostgreSQL roles or tablespaces, `.env`, or Caddy's certificates.
Keep those separately. SP1 Compose supports only its bundled PostgreSQL 17
service. Backup and migrate stay on the private database network; external
database hosts are not supported by these files.

To restore safely, stop the app, start only PostgreSQL, and restore into a new
database. Replace `katwork-katwork-YOUR_TIMESTAMP-SUFFIX.dump` with the real
archive filename from `backups/`, checking its database name. The bundled
server already has the `katwork` role; do not create it again. On Linux, `sudo`
lets Docker read the root-owned backup:

```sh
docker compose down
docker compose up -d --wait postgres
docker compose exec -T postgres createdb -U katwork -O katwork restored
sudo docker compose cp backups/katwork-katwork-YOUR_TIMESTAMP-SUFFIX.dump postgres:/tmp/restore.dump
docker compose exec -T postgres pg_restore --exit-on-error --no-owner -U katwork -d restored /tmp/restore.dump
docker compose exec -T postgres rm /tmp/restore.dump
```

Then copy back the files saved with that dump: its `.files` folder of the same name.
This adds the backup's files to the `artifacts` volume and keeps the files already
there. It checks each file against its SHA-256 and gives it to the app's user, and
it fails, naming the file, if one was damaged in the backup:

```sh
docker compose run --rm restore-files katwork-katwork-YOUR_TIMESTAMP-SUFFIX.files
```

In `.env`, change only the final database name in `DATABASE_URL` from `/katwork`
to `/restored`, preserving the user, password, `postgres` host and port `5432`.
Run `docker compose up -d`. Backup reads this same URL, so it saves the restored
database and the files before migration. Check your data before removing the old
database. If `restored` already exists, choose a new name rather than overwriting it.

Keep the simple URL form written by `init-env.sh`:
`postgres://USER:PASSWORD@HOST:PORT/DATABASE`. Fields accept letters, digits,
`_`, `.`, `~` and `-`; the port must be 1–65535. Percent escapes, query options,
IPv6 addresses and multi-host URLs are rejected before migration. The generated
password already fits this form. The restore checks verify the documented
commands, the selected database, the demo user, every migration version and a
restored file.

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
reachable from the container when it runs on the Docker host. On a Linux Docker
host, Ollama on the same host listens on `127.0.0.1`, which the container cannot
reach. Set `OLLAMA_HOST` to the Docker bridge address (for example `172.17.0.1`;
`ip -4 addr show docker0` shows it), or to `0.0.0.0` behind a firewall.
With a default-deny firewall such as ufw, also allow KatWork's Compose network to
reach that port: `sudo ufw allow from <compose-subnet> to 172.17.0.1 port 11434 proto tcp`
(`172.31.250.0/29` with the HTTPS override; otherwise see `docker network inspect`).
Ollama has no authentication: anyone who can reach its port can use it.

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

### เริ่มใช้งาน

กรุณาติดตั้ง Docker และ Docker Compose ก่อน แล้วเปิดเทอร์มินัลในโฟลเดอร์ KatWork ครับ
สร้างไฟล์ตั้งค่าส่วนตัวและเริ่มระบบด้วยคำสั่งนี้:

```sh
scripts/init-env.sh
sudo install -d -o 0 -g 0 -m 0755 backups
docker compose up -d --build
docker compose exec katwork /app/katwork-server create-user \
  --email you@example.com --workspace "ทีมของฉัน" --role owner \
  --space-policy local --locale th
```

เปลี่ยนอีเมลเป็นของคุณ แล้วเปิดลิงก์ที่ปรากฏเพื่อตั้งรหัสผ่านภายใน 72 ชั่วโมงครับ
คำสั่ง `sudo install` เตรียมโฟลเดอร์สำรองข้อมูลบน Linux ให้ root เป็นเจ้าของ โดยไม่เปิดสิทธิ์เขียนให้ทุกคน
เข้าใช้งานที่ **http://localhost:3000** หากทีมใช้เครื่องอื่น กรุณาตั้งค่า HTTPS ตามขั้นตอนด้านล่าง
ทีมจะมี Kats หกตัว หากต้องการคำตอบจริง กรุณาตั้งค่าโมเดลก่อนใช้งาน

หากต้องการทดลองแยกจากข้อมูลจริง บน Linux ให้เตรียมโฟลเดอร์เดโมก่อน แล้วเริ่มระบบ:

```sh
sudo install -d -o 0 -g 0 -m 0755 backups-demo
scripts/demo.sh
```

เปิด **http://localhost:3001**
เข้าสู่ระบบด้วย `demo@katwork.local` และอ่านรหัสผ่านเริ่มต้นจากไฟล์ `.demo-credentials`
หากตั้ง `KATWORK_DEMO_PASSWORD` ใน `.env.demo` ให้ใช้รหัสนั้นครับ
การดาวน์โหลดอิมเมจครั้งแรกต้องใช้อินเทอร์เน็ต หลังจากนั้นเดโมใช้งานแบบออฟไลน์ได้
พื้นที่ `เดโม` มี Kats ที่ตอบด้วยข้อมูลจำลอง โดยไม่ส่งคำขอออกไปยังบริการภายนอก
เดโมใช้ฐานข้อมูลแยกต่างหาก และจะไม่เริ่มหากระบบหลักกำลังทำงาน เว้นแต่ระบุ `--allow-running-install`

### HTTPS สำหรับเครือข่ายสำนักงาน

ให้ผู้ดูแลเครือข่ายตั้งชื่อ `work.home.arpa` ให้ชี้มาที่เครื่องเซิร์ฟเวอร์
หากเริ่มติดตั้งใหม่ ให้ใช้คำสั่งนี้แทน `scripts/init-env.sh` ข้างต้น:

```sh
scripts/init-env.sh --https work.home.arpa
```

หากมี `.env` อยู่แล้ว ให้แก้ `KATWORK_HTTPS_HOST=work.home.arpa` และ
`COMPOSE_FILE=docker-compose.yml:docker-compose.https.yml` ในไฟล์นั้น แล้วเริ่มระบบใหม่:

```sh
docker compose down
docker compose up -d --build
```

ไม่ว่าจะติดตั้งใหม่หรือแก้ `.env` เดิม ต้องนำใบรับรองหลัก (root certificate) ออกมา
และให้เครื่องของผู้ใช้เชื่อถือใบรับรองนี้ เมื่อระบบเริ่มทำงานแล้ว กรุณาใช้คำสั่งนี้:

```sh
docker compose cp caddy:/data/caddy/pki/authorities/local/root.crt ./katwork-root.crt
```

ให้ผู้ดูแลติดตั้ง `katwork-root.crt` เป็นใบรับรองที่เชื่อถือบนเครื่องของผู้ใช้ แล้วเปิด **https://work.home.arpa**
แจกเฉพาะไฟล์ `.crt` นี้ ห้ามแจกกุญแจส่วนตัว และอย่าข้ามคำเตือนใบรับรอง
วิธีติดตั้งในแต่ละระบบมีรายละเอียดภาษาอังกฤษในหัวข้อ [HTTPS](#https)

### สำรองข้อมูลก่อนอัปเกรด

ทุกครั้งที่เริ่มระบบ ฐานข้อมูลเดิมจะถูกสำรองก่อนปรับโครงสร้างข้อมูล ตามด้วยไฟล์ที่อัปโหลดและไฟล์ที่ Kat เขียน (volume `artifacts`) หากสำรองไม่สำเร็จ ระบบจะไม่เริ่ม
ข้อมูลสำรองแต่ละชุดมีสองส่วนที่ใช้ชื่อเดียวกัน: ฐานข้อมูลใน `backups/katwork-<database>-<UTC timestamp>-<suffix>.dump` และไฟล์ในโฟลเดอร์ `backups/katwork-<database>-<UTC timestamp>-<suffix>.files/` ส่วนเดโมอยู่ใน `backups-demo/` เท่านั้น
ชื่อบอกชื่อฐานข้อมูลและเวลา UTC รูปแบบ `YYYYMMDDTHHMMSSZ` อย่าเลือกข้อมูลสำรองของเดโมมาใช้กับข้อมูลจริง
ไฟล์ที่ไม่เปลี่ยนตั้งแต่การสำรองครั้งก่อนเป็น hard link ไปยังชุดก่อน การสำรองแต่ละครั้งจึงเพิ่มขนาดเท่าไฟล์ใหม่เท่านั้น เมื่อลบชุดเก่า ให้ลบทั้ง `.dump` และ `.files/` พร้อมกัน
ไฟล์ใหม่แต่ละไฟล์ถูกตรวจ SHA-256 ขณะคัดลอก ไฟล์ที่เสียหายจะไม่ถูกสำรอง และมีชื่อแจ้งไว้ใน `docker compose logs backup`
หากมีข้อผิดพลาดเรื่องสิทธิ์โฟลเดอร์ บน Linux ให้รันคำสั่ง `sudo install` ของโฟลเดอร์นั้นอีกครั้ง
ก่อนอัปเกรด ให้หยุดระบบโดยเก็บข้อมูลไว้ แล้วเริ่มรุ่นใหม่:

```sh
docker compose down
git pull --ff-only
docker compose build katwork
docker compose up -d
```

ห้ามเติม `-v` หลัง `down` เพราะจะลบฐานข้อมูล ไฟล์ที่อัปโหลด และใบรับรอง
เก็บสำเนาข้อมูลสำรองในที่ปลอดภัย และเก็บ `.env` กับใบรับรองแยกไว้ด้วย เพราะข้อมูลสำรองไม่มีสองอย่างนี้
เมื่อคัดลอกข้อมูลสำรองไปที่อื่น ให้คง hard link ไว้ (เช่น `sudo cp -a backups DESTINATION` หรือ `rsync -aH`)

### กู้คืนข้อมูล

เลือกข้อมูลสำรองจาก `backups/` และตรวจชื่อฐานข้อมูลในชื่อไฟล์ก่อน
แทน `katwork-katwork-YOUR_TIMESTAMP-SUFFIX.dump` ด้วยชื่อไฟล์จริง แล้วกู้คืนลงฐานข้อมูลใหม่:

```sh
docker compose down
docker compose up -d --wait postgres
docker compose exec -T postgres createdb -U katwork -O katwork restored
sudo docker compose cp backups/katwork-katwork-YOUR_TIMESTAMP-SUFFIX.dump postgres:/tmp/restore.dump
docker compose exec -T postgres pg_restore --exit-on-error --no-owner -U katwork -d restored /tmp/restore.dump
docker compose exec -T postgres rm /tmp/restore.dump
```

จากนั้นคัดลอกไฟล์ของชุดเดียวกัน (โฟลเดอร์ `.files` ที่ชื่อเดียวกับไฟล์ `.dump`) กลับเข้า volume `artifacts`
คำสั่งนี้เพิ่มไฟล์จากข้อมูลสำรองโดยไม่ลบไฟล์ที่มีอยู่ ตรวจ SHA-256 ของทุกไฟล์ และแจ้งชื่อไฟล์ที่เสียหาย:

```sh
docker compose run --rm restore-files katwork-katwork-YOUR_TIMESTAMP-SUFFIX.files
```

ใน `.env` เปลี่ยนเฉพาะชื่อฐานข้อมูลท้าย `DATABASE_URL` จาก `/katwork` เป็น `/restored` แล้วรัน `docker compose up -d`
หากมีฐานข้อมูล `restored` อยู่แล้ว ให้เลือกชื่อใหม่ อย่าเขียนทับข้อมูลเดิม
ตรวจข้อมูลให้ครบก่อนลบฐานข้อมูลเก่า รายละเอียดข้อจำกัดของ URL อยู่ในหัวข้อ [Backups and upgrades](#backups-and-upgrades) ภาษาอังกฤษ

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
