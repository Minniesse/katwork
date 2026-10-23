# KatWork

**Planned: the self-hosted AI team office.**
**อยู่ในแผนพัฒนา: ห้องทำงานทีมผู้ช่วย AI บนเซิร์ฟเวอร์ขององค์กร**

The AI coworkers are called **Kats** in English and Thai. See [ADR-0006](docs/adr/0006-rename-dots-to-kats.md).

Pre-alpha planning status. Public-alpha target: week of 2027-01-25.
No capability is verified “in alpha” in this README.

[Status](#status) · [Setup](#setup) · [Sample](#sample) · [Scope](#scope) · [Data path](#data-path) · [ภาษาไทย](#thai) · [Evaluation](#evaluation)

<a id="status"></a>
## Status

Use planned / in alpha / later beside each capability. “In alpha” requires a named
release, reproducible checks and known limitations. Synthetic inputs do not prove
working automation. Unbuilt UI must say “concept mockup — planned.”

<a id="setup"></a>
## Setup

Planned: single-host Docker Compose installation, Rust core, React/Vite workspace
and Apache-2.0 core licensing. No verified installation command is supplied here.
Publish exercised steps and dependency notices with the actual release.

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
