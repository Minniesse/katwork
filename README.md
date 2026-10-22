# KatWork

**The self-hosted AI team office.** Hire a team of named AI coworkers ("Dots") that do real office work on their own isolated computers and hand back finished files with proof. Every number is cited to its source, and every risky action waits for your approval.

> Status: **pre-alpha, under construction.** Nothing here is ready to install yet. The public alpha is planned for late January 2027.

## What it will do

- **A team anyone can build.** Pick a template or hire coworkers: Lead, Researcher, Writer, Analyst, Scheduler, Reviewer.
- **Finished work with proof.** You get documents in your own template (DOCX, XLSX, PDF). Each claim is marked *passed*, *failed* or *unverified*, and there is a missing-evidence list.
- **A safe computer per coworker.** Each Dot works in a sandbox with a browser and office apps you can watch live and take over.
- **Approvals.** Sending, publishing, paying and deleting always wait for a person.
- **Talk from anywhere.** Web, desktop and mobile, plus LINE, Slack, Teams, RustChat/Mattermost, Telegram, Discord and WhatsApp.
- **Self-hosted.** One `docker compose up`, with any model provider, in English and Thai.

## Design

See [`docs/spec/katwork-design.md`](docs/spec/katwork-design.md).

## Stack

- Rust core: Axum, Tokio, SQLx/PostgreSQL, Rig, rmcp.
- Sandbox: rivet agentOS.
- UI: React + Vite, react-three-fiber.
- Shells: Tauri for desktop, Capacitor for mobile.

## Contributing

Work is tracked in GitHub Issues. Each issue is small, test-first, and reviewed before merge. Agent workflow rules are in [`CLAUDE.md`](CLAUDE.md).

## Licence

Apache-2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
