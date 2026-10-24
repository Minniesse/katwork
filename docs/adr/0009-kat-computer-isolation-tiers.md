# 0009. Kat computers: agentOS for light code only, a container runtime ladder for Chromium, one egress gateway

- Status: Accepted (2026-10-24); built in sub-project 2b, recorded now
- Decision owner: Founder
- Spec: §1.2 (non-goals), §9, §13 (isolation), §14, §19 item 2; amends §14 item 3

## Context

Spec §9 gives each Kat an agentOS workspace and a separate Chromium container. Before building them, three questions needed answers: what agentOS isolates, what protects the browser, and where egress is enforced.

- **agentOS is process isolation, not a hardware boundary.** Guest JavaScript runs in V8 isolates and WASM and Python in managed executors, all inside one native sidecar process; its own docs call it beta and "still undergoing security review" ([security model](https://github.com/rivet-dev/agentos/blob/main/docs/content/docs/security-model.mdx)). It cannot run Chromium or native binaries ([limitations](https://github.com/rivet-dev/agentos/blob/main/docs/content/docs/limitations.mdx)). Its only snapshot is a size-capped export of the root filesystem.
- **A contributor note in the agentOS repo contradicts the isolation claim.** At v0.4.4, `crates/CLAUDE.md` says "CURRENTLY BROKEN: The execution engine currently spawns real host `node` child processes instead of V8 isolates, and most builtins fall through to real host modules", while `crates/execution/CLAUDE.md` forbids exactly that ([crates/CLAUDE.md](https://github.com/rivet-dev/agentos/blob/v0.4.4/crates/CLAUDE.md), [crates/execution/CLAUDE.md](https://github.com/rivet-dev/agentos/blob/v0.4.4/crates/execution/CLAUDE.md)). If the first note is current, guest JS is not isolated from the host at all.
- **Chromium touches the most hostile input in the alpha** and needs a real boundary. MicroVMs need `/dev/kvm`, which many cloud VMs lack; gVisor needs none ([gVisor platforms](https://gvisor.dev/docs/user_guide/platforms/)).
- **Egress inside the sandbox is not egress control.** agentOS's network rules run inside the sidecar process. Production sandboxes enforce egress on the host side, through a proxy the sandbox cannot route around, and a proxy that trusts the client-supplied hostname can be bypassed by domain fronting ([Claude Code sandboxing](https://code.claude.com/docs/en/sandboxing), [Docker sandboxes](https://docs.docker.com/ai/sandboxes/security/)).
- **Snapshots are two different things.** A disk checkpoint is cheap and safe to restore. A memory snapshot brings back every secret in guest memory, breaks connections, needs the same hardware and software to restore, and its files are trusted input that the hypervisor checks only with a CRC ([Firecracker snapshots](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md)).

## Decision

- **agentOS stays for the alpha as the light code tier only**: JavaScript, Python (Pyodide), WASM and files. Each workspace gets its own sidecar pool, run unprivileged in its own container on a network with no route.
- **Hard gate: no Kat runs code in agentOS until a CI conformance test proves that guest JavaScript cannot reach host files, host processes or the network.** The test runs guest code that tries to read a host canary file, spawn a process (`child_process`, `process.binding`, worker threads) and connect to a host canary listener, the cloud metadata address and an outside address; the host side asserts that the canary was never read, that no new process has the sidecar as an ancestor, and that no connection arrived. It runs on every agentOS version bump, and a failure blocks the bump.
- **Each Kat's Chromium runs in its own container**, on a runtime ladder chosen at install and shown in the admin view:
  1. a microVM when `/dev/kvm` exists;
  2. otherwise gVisor (`runsc`);
  3. otherwise hardened runc (non-root, seccomp, all capabilities dropped, no `--no-sandbox`), with a visible banner in the office and admin views saying the browser runs without a VM or gVisor boundary.
- **Egress goes through one KatWork egress gateway.** Kat containers sit on a network with no route except to the gateway. The gateway applies the per-Kat allowlist after DNS resolution, refuses private, control-plane, link-local and metadata ranges, drops UDP and QUIC, and records audit rows and byte counts. Settings inside agentOS or Chromium are defence in depth, never the control.
- **Kill criteria for switching the code tier** from agentOS to a microVM (where `/dev/kvm` exists) or gVisor, checked in week 4 of sub-project 2b and after each agentOS minor release. Any one of them switches the `Computer` adapter, not its contract:
  - **K1.** A public escape or cross-VM leak in V8, WASM or the sidecar boundary that stays unfixed for 14 days, or a finding from KatWork's own escape tests.
  - **K2.** The command inventory shows that a top-priority alpha template, or three or more templates, need native binaries or syscalls that KatWork Rust tools and pinned containers cannot cover.
  - **K3.** More than one sidecar crash or wedge per 1,000 VM-hours in the soak test, with one pool per workspace.
  - **K4.** Three consecutive agentOS releases each cost more than one engineer-day to absorb.
  - **K5.** A design partner's security review rejects non-hardware isolation for untrusted code, or the founder makes memory snapshot and fork an alpha requirement.
  - **K6.** Idle resident memory above about 150 MiB per Kat on the target 8 GiB host with 10 Kats.

  The thresholds are initial values, tuned after the first soak test. A `Computer` conformance suite runs against agentOS and one other backend, so that a switch is configuration.
- **Spec §14 item 3 is amended**: a disk checkpoint is required, memory snapshot and fork are optional, and snapshot files are secrets. Any snapshot file is encrypted, integrity-checked by KatWork (a manifest of per-file hashes, authenticated with a key derived from `KATWORK_SECRET`), restored only into a quarantined computer and never over live state, and never leaves the install. A memory snapshot is never restored into a session that holds credentials.

## Consequences

- Sub-project 2b starts with the conformance test and the command inventory, because both feed the keep-or-switch decision.
- Installs without `/dev/kvm` or gVisor still run a browser, honestly labelled.
- The egress gateway is new KatWork code, and later also carries credential injection, byte quotas and request signing.
- A backend that offers memory snapshots must also restore them on the same hardware and software configuration; KatWork records those versions in the snapshot manifest and refuses a mismatch.
- Sub-project 2a's document tools run in the server process and need none of this; anything in 2a that executes a model or other code (OCR) already runs in its own container.
