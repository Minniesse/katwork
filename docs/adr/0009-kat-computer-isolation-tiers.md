# 0009. Kat computers: agentOS for light code until KatsAgentOS, a container runtime ladder for Chromium, one egress gateway

- Status: Accepted (2026-10-24), amended the same day after plan review and after the final reviews (the code tier's switch target is gVisor only); built in sub-project 2b, recorded now
- Decision owner: Founder
- Spec: §1.2 (non-goals), §4 (Task), §9, §13 (isolation), §14, §19 item 2; amends §1.2, §4 and §14 item 3

## Context

Spec §9 gives each Kat an agentOS workspace and a separate Chromium container. Before building them, three questions needed answers: what agentOS isolates, what protects the browser, and where egress is enforced.

- **agentOS is process isolation, not a hardware boundary.** Guest JavaScript runs in V8 isolates and WASM and Python in managed executors, all inside one native sidecar process; its own docs call it beta and "still undergoing security review" ([security model](https://github.com/rivet-dev/agentos/blob/main/docs/content/docs/security-model.mdx)). It cannot run Chromium or native binaries ([limitations](https://github.com/rivet-dev/agentos/blob/main/docs/content/docs/limitations.mdx)). Its only snapshot is a size-capped export of the root filesystem.
- **A contributor note in the agentOS repo contradicts the isolation claim.** At v0.4.4, `crates/CLAUDE.md` says "CURRENTLY BROKEN: The execution engine currently spawns real host `node` child processes instead of V8 isolates, and most builtins fall through to real host modules", while `crates/execution/CLAUDE.md` forbids exactly that ([crates/CLAUDE.md](https://github.com/rivet-dev/agentos/blob/v0.4.4/crates/CLAUDE.md), [crates/execution/CLAUDE.md](https://github.com/rivet-dev/agentos/blob/v0.4.4/crates/execution/CLAUDE.md)). If the first note is current, guest JavaScript is not isolated from the host at all.
- **Chromium touches the most hostile input in the alpha** and needs a real boundary. MicroVMs need `/dev/kvm`, which many cloud VMs lack; gVisor needs none ([gVisor platforms](https://gvisor.dev/docs/user_guide/platforms/)).
- **Egress inside the sandbox is not egress control.** agentOS's network rules run inside the sidecar process. Production sandboxes enforce egress on the host side, through a proxy the sandbox cannot route around, and a proxy that trusts the client-supplied hostname can be bypassed by domain fronting ([Claude Code sandboxing](https://code.claude.com/docs/en/sandboxing), [Docker sandboxes](https://docs.docker.com/ai/sandboxes/security/)).
- **Snapshots are two different things.** A disk checkpoint is cheap and safe to restore. A memory snapshot brings back every secret in guest memory, breaks connections, needs the same hardware and software to restore, and its files are trusted input that the hypervisor checks only with a CRC ([Firecracker snapshots](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md)).

## Decision

- **agentOS is temporary.** It stays for the alpha as the light code tier only: JavaScript, Python (Pyodide), WASM and files. Its replacement is **KatsAgentOS**, the in-house sandbox the lab builds after the alpha from spec §14's requirements. Each workspace gets its own agentOS sidecar pool, run unprivileged in its own container on a network with no route.
- **Hard gate: no Kat runs code in agentOS until a CI conformance test proves that guest code cannot reach host files, host processes, the network or another Kat**, for every enabled runtime: JavaScript, Python through Pyodide, and WASM. For each runtime, guest code tries to read a host canary file, spawn a process (`child_process`, `process.binding` and worker threads in JavaScript; `os`, `subprocess` and `ctypes` in Pyodide; WASI calls in WASM), connect to a host canary listener, the cloud metadata address and an outside address, and read a file of another Kat's VM. The host side asserts that the canary was never read, that no new process has the sidecar as an ancestor, that no connection arrived, and that the other VM's file was not read. It is the **entry gate of sub-project 2b**, run on the first pinned agentOS version before any Kat code runs, and again on every version bump; a failure blocks the bump. A runtime whose test does not pass stays disabled.
- **Each Kat's Chromium runs in its own container**, on a runtime ladder chosen at install and shown in the admin view:
  1. a microVM when `/dev/kvm` exists;
  2. otherwise gVisor (`runsc`);
  3. otherwise hardened runc (non-root, seccomp, all capabilities dropped, no `--no-sandbox`), with a visible banner in the office and admin views saying the browser runs without a VM or gVisor boundary.

  This amends spec §1.2: there is no VM per Kat **for code** in the alpha, but Chromium may use a microVM when KVM exists.
- **Egress goes through one KatWork egress gateway.** Kat containers sit on a network with no route except to the gateway. The gateway applies the per-Kat allowlist after DNS resolution, refuses private, control-plane, link-local and metadata ranges, drops UDP and QUIC, and records audit rows and byte counts. Settings inside agentOS or Chromium are defence in depth, never the control. **Accepted risk:** until the gateway inspects TLS (decided in sub-project 2b), a hostname allowlist can be bypassed by domain fronting to an allowed host's front end; sub-project 2b decides whether to terminate TLS at the gateway.
- **Snapshots** (amends spec §14 item 3 and §4):
  - **Disk checkpoints are required**: encrypted, with a manifest of per-file hashes authenticated by a key derived from `KATWORK_SECRET`, restored only into a quarantined computer and never over live state, and never leaving the install.
  - **Memory snapshot and fork are optional and later**, on the microVM tier only. A memory snapshot is never restored into a session that holds credentials, and is refused on a different hardware or software configuration.
  - A Kat **"tries two approaches" by restarting from a disk checkpoint**, not by forking memory.
- **Kill criteria for switching the code tier** from agentOS to **gVisor** (`runsc`), checked in week 4 of sub-project 2b and after each agentOS minor release. Any one switches the `Computer` adapter, not its contract. In the alpha the switch target is gVisor only, so spec §1.2's "no VM per Kat for code" holds whichever criterion fires; on a host without gVisor, Kat code execution is turned off until it is installed. A per-Kat microVM for code is reserved for KatsAgentOS after the alpha. The thresholds are frozen before the first soak test; a change is a new, preregistered version and a founder decision, never an adjustment after seeing a result.
  - **K1.** A public escape or cross-VM leak in V8, WASM or the sidecar boundary that stays unfixed for 14 days, or a failure of KatWork's own conformance test.
  - **K2.** The command inventory shows that the meeting-minutes, SAR or One Report template, or any three of the six alpha templates (spec §8), need native binaries or syscalls that KatWork Rust tools and pinned containers cannot cover.
  - **K3.** More than one sidecar crash or wedge per 1,000 VM-hours over a soak of at least 2,000 VM-hours spread over at least 7 days, running the command inventory's workload with one pool per workspace. A **wedge** is a sidecar child that answers no health request for 30 s, or a VM whose exec has no completion 60 s after its deadline.
  - **K4.** Three consecutive agentOS minor releases each need adapter changes beyond `Cargo.toml` and `Cargo.lock` of more than 200 changed lines.
  - **K5.** A design partner's security review rejects non-hardware isolation for untrusted code, or the founder makes memory snapshot and fork an alpha requirement. gVisor answers K5 only if that partner accepts it; otherwise a microVM for code in the alpha is a founder decision that amends spec §1.2 again, never a silent switch.
  - **K6.** Idle memory above 150 MiB per Kat on the target 8 GiB host with 10 Kats. **Idle** means no exec for 10 minutes. Memory is the proportional set size (PSS) of the sidecar process tree plus each VM's own accounting, sampled every 60 s for one hour; a shared sidecar's PSS is divided equally among the Kats on it.

  A `Computer` conformance suite runs against agentOS and gVisor, so that a switch is configuration.

## Alternatives considered

- **A microVM or gVisor code tier now**: rejected for the alpha; it needs KVM or a runtime many installs lack, and agentOS's light tier covers the alpha's JS, Python and WASM work if it passes the gate.
- **A hosted sandbox service**: rejected; documents and credentials stay on the install.
- **Egress rules inside agentOS and Chromium only**: rejected; the control must be outside the sandbox.
- **Memory snapshot and fork as an alpha requirement**: rejected; restarting from a disk checkpoint covers "try two approaches" without restoring secrets.

## Consequences

- Sub-project 2b starts with the conformance test and the command inventory, because both feed the keep-or-switch decision.
- Installs without `/dev/kvm` or gVisor still run a browser, honestly labelled.
- The egress gateway is new KatWork code, and later also carries credential injection, byte quotas and request signing.
- KatsAgentOS is a separate track after the alpha; sub-project 2b builds only the adapter for agentOS and the `Computer` contract.
- Sub-project 2a's document tools run in the server process and need none of this; everything in 2a that executes a model or a native tool (OCR, Craft tools) already runs in its own container.
