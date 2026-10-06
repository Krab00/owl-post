# owlpost — concept

## What it is

A cross-harness plugin (Claude Code, Codex, Kimi Code, opencode) that lets the coding agents of
different people exchange **asynchronous, human-approved questions and answers** over the
network.

Primary use case: person A's agent asks person B's agent *"what happened in this file and why?"*.
B approves (or has auto-accept enabled for trusted peers); B's agent answers from B's local
repo checkout and project notes in a **read-only sandbox**; the answer travels back and is
written into A's agent memory.

Target: a mid-sized engineering team sharing one repository, and beyond that any group of
developers, so the design must be generic (contacts outside one organisation, untrusted networks).

The honest framing: this is **a channel for asking an expert's agent without interrupting the
expert.** Knowledge diffusion across the team is an improvement layer added on top (see below),
not a property of the peer-to-peer topology itself.

## What it is not (explicit scope decisions)

- **Not shared or global memory.** No bulk synchronisation of memory between harnesses.
  Knowledge stays local; only Q&A exchanges cross the network.
- **Not live orchestration or task delegation** between harnesses. No task lifecycle, no
  streaming, no push notifications in the MVP.
- **Not a bug tracker.** Bug reports go through the existing tracker (Jira/GitHub), tagged as
  agent-found. The plugin's added value is closing the loop: once a fix is accepted, the owner's
  agent turns it into a local memory rule.
- **Not a chat.** Messages are discrete, signed, human-gated question/answer pairs with
  minutes-to-hours latency. "agent-chat" is a genre we deliberately are not in.

## Core decisions

### Topology

- **Peer-to-peer, no server that sees content.** Each installed plugin runs a local daemon
  (`owl daemon`) with two listeners: HTTPS on a `host:port` and an iroh endpoint addressed
  by the owner's public key. When two daemons cannot reach each other directly, iroh falls
  back to a relay that forwards end-to-end encrypted QUIC; the relay learns which two keys
  talked and when, never what was said (see Addressing).
- **Best-effort delivery, no queue for questions.** A peer that is offline cannot receive a
  question — the asker gets an immediate "offline, try later". No sender-side retry, no
  delivery guarantee, no presence infrastructure, no heartbeats.
- **Pull model for answers.** An answer is not pushed. It stays in the responder's outbox like a
  mailbox; the asker's daemon knows which peers it has open questions with and polls only those.
  This removes the requirement that both laptops are online at the moment the answer is
  produced (which, hours after the question, is the common case) without introducing a queue:
  offline semantics are unchanged, the daemon simply tries again later.
- Liveness is probed on demand (fetch agent card, 1–2 s timeout, result cached 30–60 s), never
  tracked.

### Identity

- **Canonical contact ID = fingerprint of an ed25519 public key.** Names, emails, endpoints are
  metadata (claims) attached to the key.
- **Git email is only a routing hint**: `git blame` on a file → author emails → contact
  fingerprints in the context of this repo. It answers *whom* to ask about a file, on the
  asker's machine, using public git history. Blame is a heuristic (last toucher, not expert);
  the asker's agent shows the top authors and lets the human choose. A contact carries a list of
  emails, because people use different git identities on different machines.
- **One private key per person**, copied to their machines like an SSH key. The contact format
  can grow a list of public keys later if that hurts.
- Open: whether the harness subscription login (Claude / OpenAI / Moonshot account) can serve as
  an additional identity hint. It is only another claim unless it can be verified.

### Contact list = pluggable providers

The core defines one contact format and does not know where a contact came from. Providers
supply entries:

1. **`repo` provider** (company mode): the directory `.agents/peers/` in the project repo,
   **one file per person** (avoids merge conflicts across dozens of developers). Joining the mesh is a
   reviewed PR; offboarding is deleting the file; sync happens on a normal `git pull`.
2. **`local` provider**: contacts added by hand under `$OWLPOST_HOME/contacts/` — for people
   outside the shared repo. Also holds the per-contact policy overlay for repo contacts.
3. Future providers (LDAP, central registry, directory) plug in without core changes. **Do not
   build** a global directory / DHT / internet-wide discovery now.

Contact entry (repo provider) — deliberately minimal:

```json
{
  "name": "Martin",
  "emails": ["martin@company.com", "martin@personal.dev"],
  "pubkey": "ed25519:AAAA…",
  "endpoints": ["martin-laptop.tail1234.ts.net:7411", "martin-desktop.company.vpn:7411"]
}
```

Endpoints carry no scheme: the transport is always HTTPS with mutual TLS. Capabilities,
harness, protocol version live in the agent card served by the daemon, not in the committed
file (changes need no PR).

### Trust = an attribute per contact, derived from provenance

- Contacts from the repo (merged via a reviewed PR) are eligible for auto-accept — granted by
  the owner with `owl allow`, never by the committed file: a `policy` in `.agents/peers/` is
  ignored, so a commit alone cannot open anyone's agent.
- Contacts added by hand → TOFU: the key is pinned on add and the fingerprint verified
  out-of-band (compare a short fingerprint over Slack/phone — the Signal/SSH model).
  **Auto-accept is off by default** for these contacts.
- **Consent per asker, from day one.** The first question from a peer with no policy yet is
  held; the owner sees *"X wants to ask your agent about repo Y — allow once / allow always /
  never"*. The choice becomes the per-contact policy (`manual | auto | never`). `never` makes the
  daemon answer "unavailable", indistinguishable from offline — a block is never disclosed.
- Policy per contact also carries scope (which projects/paths may be asked about) and rate
  limits. In the OSS version this is a security requirement, not a feature: an external contact
  querying a private repo is an exfiltration risk.
- Adding an external contact: `owl add <host:port>` → fetch agent card → show fingerprint + name
  → confirm → save to the `local` provider. Alternative for hosts not yet reachable: a signed,
  self-contained invite bundle / QR (id + pubkey + endpoints).

### Securing the `repo` provider

"Trusted because it is in the repo" is only meaningful if not everyone can put any key there:

- `CODEOWNERS`: `.agents/peers/ @mesh-admins` (2–3 people). Before approving a new peer file the
  admin verifies the fingerprint with the person out-of-band (30 seconds on Slack or at their
  desk). This is the one moment a human binds a key to a person; it cannot be automated without
  losing its meaning.
- Post-MVP: a change to an existing peer file must be signed by the previous key (checked in
  CI), so even an admin approval cannot silently swap someone's key. A lost key = admin deletes
  the file and re-adds the person through the first path.

Connections outside the repo (local provider, OSS) do not use any of this; their trust comes
from fingerprint verification at add time.

### Wire format / protocol

- **HTTPS with mutual TLS, JSON bodies.** Every peer generates a self-signed certificate from the
  *same* ed25519 key that is in the contact list. Verification is one comparison: the public key
  in the peer's certificate equals the pinned pubkey from the contact entry. Unknown key =
  connection refused before a byte of the message is read. No CA, no Let's Encrypt, no renewal:
  the identity is the key, not the certificate.
- **Message structure compatible with A2A**: the agent card is served at
  `/.well-known/agent-card.json` (A2A v1.0 path) plus legacy `/.well-known/agent.json`, so a
  later migration is mechanical. **The MVP does not implement full A2A** — no streaming, no push
  notifications, no task lifecycle. Full A2A is machinery for long-running multi-step tasks and
  automated agent replies; our exchanges are one signed question and one human-gated answer, and
  the "offline = dropped, no queue" rule contradicts A2A's persistent task model. We will adopt
  more of A2A when task delegation between agents becomes a real need — which is explicitly out
  of scope today.
- **A2A for people, not an A2A server** (2026-09-12): we use A2A's vocabulary where it makes
  the human experience better and keeps a later full binding cheap — the card is an A2A 1.0
  `AgentCard` (interfaces, security scheme, one skill, three owlpost extensions), a question's
  state is exposed as an A2A `Task` (`SUBMITTED` = waiting for the owner's consent,
  `WORKING` = drafting or under review, `COMPLETED`, `REJECTED`) so the asker sees where it
  stands, and a thread id (`contextId`) plus a context snippet let a follow-up question carry
  its history. We do not become an A2A server for stock clients: no JSON-RPC binding, no
  `ListTasks` or cancel, no unsigned messages from foreign clients, no A2A error catalogue.
- All messages are also **signed with ed25519** (over the exact bytes sent), independently of
  the transport: an answer sits in an outbox and is fetched later, and answers may in future be
  relayed through peer caches, so authorship must be verifiable without a live connection to the
  author. Every message carries an id and timestamp; receivers reject stale timestamps and
  already-seen ids (replay protection).

### Addressing — the key is the address

Committing DHCP addresses is out; mDNS does not cross subnets; two people on different
networks (home LAN and a corporate laptop, a team spread across time zones) have no `host:port` the other
can dial. So the primary address of a peer is its **ed25519 public key**: the daemon's iroh
endpoint is derived from the same key as the identity, and iroh (QUIC) finds a direct path by
hole punching or, failing that, forwards through a relay. Nothing to install, no account, no
port forwarding; a contact with an empty `endpoints` list is reachable. Peer authentication
stays key-pinned on this path too: an incoming iroh connection exposes the remote key, and a
key that is not in the contact book is refused before any request is read.

Trade-off, stated rather than hidden: the relay is a third party. It carries only end-to-end
encrypted traffic, but it sees **metadata** — which two keys connected and when, and the IP
addresses involved. The default relays are n0's public ones; `config.json` `relay_urls` points
both sides at a self-hosted relay (iroh's relay server is open source) so a company keeps even
the metadata in-house. Users who refuse any relay keep the second transport: a contact's
`endpoints` (`host:port` over mTLS — company VPN with DNS, Tailscale/headscale MagicDNS, a
LAN), tried after iroh and still the faster path when they resolve. A contact has multiple
endpoints (multiple machines), tried in order.

## Security requirements (non-negotiable)

1. **The responder session is sandboxed**: a separate headless session, **read-only** (repo +
   permitted notes), no shell, no write tools, no network tools. This is the condition under
   which auto-accept is defensible at all — the worst outcome of a prompt injection is a bad
   answer.
2. **Answer preview before sending is the default** in manual mode. The leak risk lives in the
   answer (local notes can contain paths, secrets, private remarks), not in the question. With
   auto-accept: redaction rules (regexes for secrets) + a full outgoing log.
3. **Privacy of the responder's context.** The responder is the owner's own agent reading what it
   normally reads; nothing is extracted by the daemon. Default scope is the repo checkout and
   project-level files (`CLAUDE.md` / `AGENTS.md` — already team knowledge). The owner's private
   memory directory is **opt-in** per owner. Session transcripts are never in scope in the MVP.
   Honest consequence for the pilot: without opt-in, answers come from the repo alone, which the
   asker's agent could largely derive itself; the preview gate is what should make opt-in
   acceptable.
4. Incoming messages are untrusted data, never instructions.
5. **Rate limit per peer** and **asker-side cache** (hash of question + file) from day one. The
   responder pays with their own tokens/API budget and interruptions; popular module owners
   would otherwise be effectively DDoS-ed by their colleagues' agents. The responder also caches
   its own answers by question hash, so a lost delivery costs nothing to re-ask.

## Message schema

Force concreteness — vague entries are worthless to agents:

- **Question**: project, file/module, the concrete question.
- **Lesson / bug report** (post-MVP type): project, file/module, what was wrong, reproduction,
  and **a proposed memory entry as a ready rule** (e.g. "in module X always await Y") that the
  receiving human only accepts or edits. Never "be more careful next time".

## Knowledge diffusion (improvement layer, post-MVP)

Without it every exchange is a private, ephemeral 1:1 (like a DM) and the same expert answers
the same question repeatedly:

1. The answer is written to the asker's memory as part of the protocol (not optional).
2. Peer-visible answer cache: the question hash is checked at known peers first; the expert is
   asked only on a cache miss.
3. Questions repeated N times → an automatic PR proposal to `AGENTS.md` / `CLAUDE.md` in the
   repo (all harnesses read them natively; durable project knowledge belongs to the repo).

## Harness adapters

The core is one binary (`owl`) shared by all harnesses; an adapter per harness is a thin
package of configuration — hooks, a skill, slash commands — that all call `owl`. Zero protocol
logic lives in adapters.

Two things are visible inside a harness session:

- **A counter, never content.** Hooks inject one line ("2 new questions from Martin, 1 from
  Owen") only when there are unseen messages; the full text is shown on demand. This is both the
  UX decision ("defer reading, read many at once") and the only portable model: Codex caps
  injected context at 2,500 tokens and Kimi has a single injection point.
- **Actions through the CLI** via the harness's shell tool, guided by the skill: `owl inbox`,
  `owl show`, `owl draft`, `owl send`.

Nothing long-lived runs inside a session; the only resident process is the daemon under
launchd/systemd. A message arriving while the human is idle is surfaced by an **OS
notification** from the daemon — the universal idle channel for all harnesses, and the only one
that works when no session is open. `owl watch` (block until a message arrives) exists as an
opt-in accelerator for an agent that wants to resume work when its answer lands; if it dies the
message is still in the spool and surfaces through the hooks.

Answering needs no harness at all: notification → any terminal → `owl inbox` → `owl draft 7`
→ preview → `owl send 7`.

Verified harness capabilities (2026-09) are in [architecture.md](architecture.md#adapter-matrix).

## MVP scope

1. Local daemon with HTTPS/mTLS listener serving the agent card.
2. Contact providers `repo` (`.agents/peers/`, one file per person) and `local` with `owl add`
   and pinned keys; per-contact policy with consent prompt.
3. Signed question → consent/accept → read-only responder session → preview → signed answer in
   outbox → pulled by the asker's daemon.
4. Rate limit per peer, asker-side and responder-side caches, replay protection.
5. Auto-accept per contact **behind a flag, off by default** (the sandbox is in the MVP anyway;
   the pilot must measure human-gated vs automatic latency).
6. Claude Code adapter first (hooks + skill + slash commands + memory write on accepted answer).
7. Responder runner templates for Claude Code, Codex, opencode; Kimi disabled until its
   read-only enforcement is verified live.

**Out of MVP**: full A2A, peer-visible cache, `AGENTS.md` PR automation, Codex/Kimi/opencode
adapters, statusline badge, lesson/bug-report message type, signed peer-file changes.

## Rollout

Pilot with a handful of people before a team-wide rollout. One-command install (a harness ×
person configuration matrix is otherwise unmaintainable). If the pilot does not show value
in ~2 weeks, adoption will fail regardless of code quality. To validate in the pilot: do answers
actually beat what an agent extracts itself from `git blame`, PR descriptions and issues? The
unique value is the delta — private notes and context absent from repo history — which depends
on owners opting their notes in.

## Open questions

- ~~Which stable-DNS option does a team have?~~ Moot for reachability since the iroh
  transport (peers are dialed by key); still relevant for whoever wants the relay-free
  `endpoints` path. Open instead: does a team want a self-hosted relay (`relay_urls`)
  so that no public relay sees who talks to whom?
- Kimi headless read-only: does the `[tools]` allowlist / PreToolUse deny behave fail-closed
  under `kimi -p`? Needs a live test before enabling the responder on Kimi.
- Custom statusline in Codex: track upstream FR openai/codex#17827.
- Subscription login as identity hint: is it accessible to a plugin, and is it verifiable?
