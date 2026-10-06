# owlpost — architecture

Companion to [concept.md](concept.md) (what/why) and [technical-design.md](technical-design.md)
(exact formats and contracts). This document describes the moving parts and how a message
travels.

## 1. Components

```
machine A (asker)                                   machine B (responder)
┌──────────────────────────────┐                    ┌──────────────────────────────┐
│ harness session (Claude Code)│                    │ harness session (any/none)   │
│  hooks → `owl inbox --count` │                    │  hooks → `owl inbox --count` │
│  skill → `owl ask …`         │                    │  skill → `owl draft/send`    │
│         │ exec               │                    │         │ exec               │
│  ┌──────▼──────┐             │  iroh (QUIC by key,│  ┌──────▼──────┐             │
│  │ owl (CLI)   │──┐          │  hole punch/relay) │  │ owl (CLI)   │──┐          │
│  └──────┬──────┘  │ spool    │  or HTTPS + mTLS   │  └─────────────┘  │ spool    │
│         │ /v1/local (forward)│  POST /v1/questions│                   │          │
│  ┌──────▼──────┐  │ (files)  │ ─────────────────► │  ┌─────────────┐  │ (files)  │
│  │ owl daemon  │◄─┘          │  GET  /v1/outbox   │  │ owl daemon  │◄─┘          │
│  │ (launchd)   │ pull loop   │ ◄───────────────── │  │ (launchd)   │─► runner ─► headless
│  └─────────────┘ OS toast    │                    │  └─────────────┘   read-only session
└──────────────────────────────┘                    └──────────────────────────────┘
                                   ┌───────────┐
              (fallback path only) │ iroh relay│ forwards encrypted QUIC; sees keys + timing
                                   └───────────┘
```

One binary, `owl`, in two roles:

| Role | Lifetime | Responsibilities |
|---|---|---|
| `owl daemon` | resident, managed by launchd / systemd user unit, restarted by the OS | HTTPS/mTLS listener and iroh listener (one handler, two transports); verify signatures, replay, rate limits; write to spool; OS notifications; pull loop for answers (iroh first, then `endpoints`); auto-accept runs; consent holding; the owner-only forward route the CLI uses to reach peers over iroh |
| `owl <cmd>` | one-shot, milliseconds | everything the human or the agent does: `ask`, `inbox`, `show`, `draft`, `send`, `reject`, `allow/deny`, `history`, `add`, `card`, `whoami`, `install`, `watch` |

The CLI and the daemon share state through the **spool** — a directory of JSON files: the
CLI reads and writes the same directories the daemon does, and the daemon notices new work by
scanning on a short interval. The one live call is the iroh **forward route**: an iroh
endpoint identity is a network singleton (a second endpoint with the same key bumps the
first at the relay), so only the daemon binds one, and `owl ask` sends its iroh requests to
the local daemon over the existing mTLS listener (`/v1/local/…`, accepted only from the
owner's own key), which replays them to the peer.
`// ponytail: no unix socket; add one only when a command needs a live daemon answer`.

Harness **adapters** are configuration packages (hooks, skill, slash commands) that call `owl`.
They contain no protocol logic.

The **responder session** is a child process of the daemon (or of `owl draft`): the owner's own
harness in headless mode with a read-only tool set, given the question and a scope description,
producing a draft answer.

## 2. Data at rest

```
$OWLPOST_HOME/                  default: ~/.config/owlpost   (tests: a temp dir per daemon)
  key                           ed25519 private key, 0600, one per person
  config.json                   listener, endpoints, harness templates, policy defaults
  contacts/<slug or fingerprint>.json   global-scope contacts and per-contact policy overlays
  spool/
    inbox/<id>.json             received questions (state: consent | pending | seen | drafted)
    outbox/<id>.json            answers awaiting pull by the asker
    asks/<id>.json              questions we sent, awaiting an answer (drives the pull loop)
    done/<id>.json              finished exchanges (both directions) — the history
    cache/<hash>.json           answers by question hash (asker side: what we got; responder side: what we sent)
  log/outgoing.jsonl            every answer that left this machine, with redaction report
  seen-ids.txt                  replay window

<repo>/.agents/peers/<slug>.json   local-scope contacts (committed, CODEOWNERS-protected)
```

Writes are atomic (write to a temp file, rename). State transitions are moves between
directories or a `state` field update; either way a crash leaves a consistent file.

## 3. Flows

### 3.1 Ask

1. The agent (or human) runs `owl ask <peer> <path> "<question>"`, optionally `--file` only,
   letting `owl` propose peers from `git blame` (top authors by line share, mapped through
   contact emails; the human picks).
2. `owl` checks the **asker-side cache** (hash of project + path + normalised question). Hit →
   answer returned immediately, nothing sent.
3. Builds the question payload, signs it, and reaches the peer: **iroh first** (dial by the
   contact's key through the local daemon), then the contact's `endpoints` in order over
   mTLS (client cert = our key; server cert must match the peer's pinned pubkey).
4. Responses: `200` with an answer payload (responder cache hit), `202 accepted` with the
   question's A2A task state (queued for the human — `waiting for the owner's consent` — or
   for the agent — `the owner's agent is answering`), `403 unavailable` (never/disabled — same
   wording as offline), `429` rate limited. Connection failure → "offline, try later". Nothing
   is queued locally. `owl ask` prints `accepted <id> — <state>`; `owl status` asks the peer
   again later (`GET /v1/questions/{id}`).
5. On `202`, an `asks/<id>.json` record is written so the daemon's pull loop knows whom to poll.
   With `--wait <secs>` the CLI polls the outbox itself for that long (useful for cache hits and
   auto-accept peers, which answer in seconds) and shows every state transition on the way
   (`waiting for the owner's consent` → `the owner's agent is answering` → the answer, or
   `declined by <name>`).
6. Threads and context: `owl ask --reply-to <id>` continues an earlier exchange (the same
   `context_id`; the responder's prompt then carries the thread's earlier questions and
   answers from its own spool), `owl ask --context <file>` attaches the asker's snippet (a
   diff, an error, an excerpt) as `body.context`. Neither is served from a cache.

### 3.2 Receive (daemon on B)

1. TLS handshake already proved the caller holds a key from B's contact list. The daemon still
   verifies the body signature against that key (defence in depth; the signature is what gets
   stored).
2. Reject if the timestamp is outside the skew window or the id was seen; enforce the per-peer
   rate limit.
3. Look up the policy for this peer:
   - `never` → `403 unavailable`.
   - no policy yet → store with state `consent`, notify *"X wants to ask your agent"*, `202`.
   - `manual` → store `pending`, notify, `202`.
   - `auto` → store `pending`, `202`, and schedule a draft + send (3.4) with redaction and
     logging.
4. Responder-side cache hit (same question hash answered before) → `200` with the cached answer.

The daemon **never runs the LLM on receipt** in manual mode; it is a dumb mailbox with a
signature check. This keeps the attack surface of the always-on network process minimal.

### 3.3 Surface in the harness

- Hooks (`UserPromptSubmit`, and `PostToolUse` where the harness allows injection) run
  `owl inbox --count --format <harness>`. Output is empty when nothing is unseen; otherwise one
  line in the harness's injection format.
- The daemon fires an OS notification on arrival (macOS `osascript`, Linux `notify-send`) —
  the channel that works while the human is idle or has no session open.
- `owl inbox` lists everything and marks it `seen` (nagging stops; the total stays visible via
  `owl inbox --count --all`). `owl show <id|all>` prints full content.

### 3.4 Answer

1. `owl draft <id>` (manual) or the daemon (auto) builds the responder prompt: the question,
   project, path, and the scope description (repo checkout + project files; private memory only
   if the owner opted in).
2. Spawns the configured harness template in headless mode with read-only tools, working
   directory = the repo checkout that matches the question's project. Output is captured.
3. Redaction regexes run over the draft; matches are replaced and reported.
4. Manual: the draft is stored on the inbox record (`drafted`) and printed for preview; the human
   runs `owl send <id>` (optionally after `owl edit <id>` in `$EDITOR`) or `owl reject <id>`.
   Auto: send immediately, append to `log/outgoing.jsonl`.
5. `send` signs the answer payload and moves it to `outbox/`; the question record goes to
   `done/`; the responder-side cache is updated.

### 3.5 Pull

The asker's daemon, every `pull_interval_secs` (default 60) and on `owl ask --wait`, calls
`GET /v1/outbox` on every peer that appears in `asks/`. The responder returns answers addressed
to the caller (identity from the client certificate). The asker verifies each signature, stores
the answer in `inbox/` (type `answer`, state `pending`), updates its cache, moves the ask to
`done/`, notifies, and acknowledges (`POST /v1/outbox/<id>/ack`) so the responder moves it to
`done/`. Unacknowledged outbox entries expire after `outbox_ttl_days` (default 14).

### 3.6 Memory write

When the human reads an answer in a session, the skill instructs the agent to record it in the
harness's native memory (Claude Code: auto-memory / `CLAUDE.md` as appropriate) with provenance
(peer, date, question id). The daemon does not write into any harness's memory.

### 3.7 Content request

A peer asks for a **concrete file** instead of an answer about one:

1. A runs `owl request B <project> <path> [--ref <ref>]` (or `--memory <key>`). The payload is
   a `content` kind and travels the same `POST /v1/questions` route.
2. B's daemon validates the request against the **allowlist** — a path inside a checkout named
   in `config.projects`, or a key inside `responder.memory_root` with
   `scope.private_memory` — and refuses anything else with a `4xx` that never reaches the
   spool. It does not read the disk: that would let an unanswered peer probe for a file by
   timing.
3. The record is spooled in state `consent` **whatever B's policy for A says**. There is no
   mode in which content leaves the machine without a human pressing send.
4. After `owl allow`, `owl draft <id>` resolves the ref in B's checkout, reads that one blob,
   refuses anything that is not UTF-8 text, runs the §10 redaction patterns over it and cuts
   it at 256 KiB. No harness and no model run.
5. The human reads the exact bytes and runs `owl send`, which signs a `content-reply` carrying
   the content and the `sha256` of the whole redacted content.
6. A's pull ingests the reply and `owl show` verifies the digest before printing it. Nothing
   is written into A's working tree: copying the content somewhere is the human's decision.

### 3.8 Tool-call request

A peer asks for the **result of a command** instead of a file:

1. A runs `owl call B test --input in.json`. The payload is a `tool-call` kind and travels the
   same `POST /v1/questions` route; `input` is a JSON object, never a command line.
2. B's daemon validates the name against the **registry** — `responder.tools`, empty unless B
   added a tool by hand — and answers `400 unknown tool` for anything else, deliberately the
   same message whether the tool is unknown or there is no registry at all.
3. The record is spooled in state `consent` **whatever B's policy for A says**, and the
   scheduler is never offered it. Nothing is spawned on arrival, and nothing on `owl allow`.
4. `owl draft <id>`, typed by a human after allowing the request, is the only thing that ever
   runs the tool: the registry's `argv` in the registry's `cwd`, the input on stdin, the run
   bounded by `timeout_ms`, the output redacted by the §10 patterns and cut at 256 KiB.
5. The human reads the exit code and the output and runs `owl send`, which signs a
   `tool-reply`. The draft stays on the finished record, so both sides keep what ran.
6. A's pull ingests the reply; `owl show` prints the exit code, the duration and the output. A
   non-zero exit is an answer, not a failure of the request.

## 4. Trust and cryptography

| Concern | Mechanism |
|---|---|
| Identity | ed25519 keypair per person; fingerprint = `owl:` + first 16 base32 chars of SHA-256(pubkey) |
| Transport confidentiality + peer authentication (`endpoints`) | TLS 1.3 mutual auth; self-signed certs generated from the ed25519 key (rcgen); custom rustls verifiers compare the certificate's SubjectPublicKeyInfo with the pinned pubkey from the contact list; unknown keys are refused at handshake |
| Transport confidentiality + peer authentication (iroh) | QUIC with TLS 1.3 keyed by the same ed25519 key (the iroh endpoint id *is* the contact's pubkey); the daemon reads the remote key off the connection and compares the 32 bytes with the contact book (no certificate to parse); unknown keys are closed before any stream is accepted, never stored |
| Relay visibility | the relay forwards end-to-end encrypted QUIC only when no direct path exists; it learns which two keys connected, when, and from which IPs — never content. Default: n0's public relays; `relay_urls` = a self-hosted relay keeps the metadata in-house; `endpoints`-only contacts avoid relays entirely |
| Message authenticity at rest and via relay | ed25519 signature over the exact HTTP body bytes, carried in `X-Owl-Signature`; stored alongside the body |
| Replay | `id` (UUIDv7) + `ts` (RFC 3339); reject `|now − ts| > 5 min` and ids in the seen window |
| Abuse | per-peer token bucket (default 20 questions/hour), `429`; asker-side and responder-side caches |
| Leakage | responder runs read-only with no shell/write/network tools; scope defaults to repo + project files; preview before send in manual mode; redaction regexes + outgoing log in auto mode. A content request (§3.7) serves **only** the explicit allowlist — one path inside a checkout named in `config.projects`, or one key inside `responder.memory_root` with `scope.private_memory` — and is held for a human every time, whatever the peer's policy. A tool call (§3.8) runs **only** a tool named in `responder.tools`, with the peer's input on stdin and never in argv, and only when a human runs `owl draft` — the daemon never spawns one |
| Prompt injection | incoming text is data; the responder prompt frames it as a quoted question; worst case is a bad answer because the session cannot act |
| Contact list integrity (repo) | `CODEOWNERS` on `.agents/peers/` + out-of-band fingerprint check by an admin; post-MVP: changes signed by the previous key |
| Contact list integrity (local) | key pinned on `owl add`; fingerprint shown for out-of-band comparison; auto-accept off by default |

## 5. Delivery semantics

| Situation | Result |
|---|---|
| Responder offline when asking | immediate failure, nothing queued, ask again later — reported only after **both** transports failed (iroh dial, bounded by a 10 s timeout, then every `endpoint`) |
| Responder behind NAT, no `endpoints` | reached over iroh: direct after hole punching, else through the relay; same request, same statuses |
| Relay unreachable / not configured (`relay_urls: []`) | iroh is dead for that pair; delivery falls back to `endpoints`, and with none the peer is offline |
| Responder `never` / responder disabled | `403 unavailable`, wording identical to offline |
| Responder declines (`owl deny`, `owl reject`) | the peer's Task turns `REJECTED` ("the owner declined"); the asker's pull loop, `owl status` or `owl ask --wait` closes the ask as `done/declined` and notifies `declined by <name>` — a declined question never waits forever |
| Asker offline when the answer is produced | answer waits in the responder's outbox; asker's daemon pulls it when both are online |
| Answer never pulled | expires from outbox after TTL; responder cache still answers a re-ask instantly |
| Daemon crash mid-write | atomic file writes; at worst a message is re-processed, never half-written |
| Same question asked twice by anyone | asker cache (own), then responder cache (`200` without an LLM run) |

## 6. Adapter matrix

Verified against official docs and repos on 2026-09-01. Local test versions: Claude Code
2.1.257, Codex CLI 0.145.0, Kimi Code CLI 0.39.1,
opencode 1.18.18.

| Pillar | Claude Code | Codex CLI | Kimi Code CLI | opencode |
|---|---|---|---|---|
| Counter injection per prompt | ✓ `UserPromptSubmit` → `hookSpecificOutput.additionalContext` | ✓ same field name; 2,500-token cap; user must trust plugin hooks via `/hooks` | ✓ `UserPromptSubmit` stdout appended to context (only injection point) | to verify (plugin hooks exist) |
| Mid-turn injection (`PostToolUse`) | ✓ | ✗ (PostToolUse cannot inject) | ✗ | to verify |
| Idle badge (statusline) | event-driven only; not reliable in idle | ✗ predefined items only (FR #17827) | ✓ custom command polled ~1 Hz, replaces footer line | to verify |
| Wake a live session (`owl watch` as background task) | ✓ | ✗ | ✓ background task completion becomes a new turn | to verify |
| Headless read-only responder | `claude -p --allowed-tools Read,Grep,Glob --output-format json` | `codex exec --sandbox read-only --json --ephemeral` (network blocked by default) — strongest | `kimi -p --output-format stream-json` — no tool flag; read-only only via config allowlist + PreToolUse guard; fail-closed unverified → **disabled by default** | `opencode run --format json --agent owl-readonly` with an agent whose tools disable write/edit/bash |
| Plugin packaging | plugin: hooks + skill + commands | `.codex-plugin/plugin.json`, `codex plugin marketplace add` | `kimi.plugin.json`, `/plugins install <github-url>` | plugin system (to verify) |
| Reads `AGENTS.md` / `CLAUDE.md` | `CLAUDE.md` | `AGENTS.md` | `AGENTS.md` | `AGENTS.md` |

Consequences: the OS notification is the primary idle channel for every harness (Codex has
nothing else; Claude Code's statusline is not timer-driven); `owl ask --wait` matters most on
Codex where `watch` cannot wake a session; the MVP ships the Claude Code adapter, with Codex,
Kimi and opencode adapters as configuration-only follow-ups.

## 7. Decision log

| # | Decision | Why |
|---|---|---|
| D1 | Rust, single static binary, ~10 crates | hooks run on every prompt: ~1 ms startup; author preference over Go; stdlib gap covered by mature crates |
| D2 | HTTPS + mTLS + JSON | pull model is a plain `GET`; curl-debuggable; agent card at well-known path = A2A compatibility for free; TLS via rustls instead of a hand-rolled envelope over raw TCP |
| D3 | Key-pinned self-signed certs, no CA | identity is the key already in the contact list; zero infrastructure |
| D4 | Pull model for answers, fail-fast for questions | removes the both-online-at-answer-time requirement without a queue |
| D5 | Spool on files, no database | atomic rename is enough; SQLite when files hurt |
| D6 | Counter injection, content on demand | portable across harness injection limits; lets humans defer and batch-read |
| D7 | Nothing long-lived in a session | background tasks get killed by accident; hooks and the launchd daemon cannot |
| D8 | Responder = owner's own harness, harness-native context | privacy: nothing extracted by the daemon; owner controls scope and previews |
| D9 | Consent per asker on first contact | being askable is a privacy matter; `never` is indistinguishable from offline |
| D10 | Auto-accept in MVP behind a per-contact flag, off by default | sandbox exists anyway; pilot must measure human-gated latency |
| D11 | Statusline badge post-MVP | unreliable in Claude Code idle, absent in Codex, needs a footer script in Kimi |
| D12 | Full A2A deferred | only request/response is needed; task lifecycle conflicts with no-queue semantics |
| D13 | iroh as the first transport, `endpoints` second | the first cross-machine test had no dialable address; iroh dials by the key we already pin, hole punches and falls back to a relay that sees metadata but no content; the relay can be self-hosted; the daemon owns the single endpoint and forwards for the CLI |
