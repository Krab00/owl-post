// The panel's data side, shared by every view: the shapes of `owl`'s JSON, the panel's state,
// the actions a view may call (`Api`), short ids and the threads of one person.
//
// The engine follows `$` only into functions of the hooks module itself, never across an
// import, so every call on `$` lives in owlpost.tsx; a view gets those calls as `Api`.

// `owl contact list --json`: one row per contact; `policy` only when one is written.
export type Contact = {
  name: string
  emails: string[]
  fingerprint: string
  pubkey: string
  endpoints: string[]
  source: string // `global` | `local`
  policy?: { mode: string } // `auto` | `manual` | `never`
}

// `owl thread --json`: one row per person, newest conversation first.
export type Thread = { from: string; from_name: string; last_ts: string; unseen: number; open: number; last_summary: string }

// `owl thread <peer> --json`: every event of every record with that person, oldest first.
// `text` is the record's own message (the question, the answer, `path@ref` of a file
// request, `tool {input}` of a tool call); a draft's text is not here (see `Draft`).
export type Ev = {
  ts: string
  kind: string // received, held, allowed, denied, drafted, sent, asked, answer-received, state-seen, content-*, tool-*, …
  dir: 'in' | 'out'
  record_id: string
  context_id: string | null
  type: string // question | answer | content | content-reply | tool-call | tool-reply
  state: string // inbox: consent | pending | drafted | sent | …; asks: waiting | answered | …
  project: string | null
  path: string // `-` when none
  text: string
  context?: string // the snippet attached to a question
  harness?: string
  by?: string
  detail?: Record<string, unknown>
}

// `owl presence --json`: the daemon's last pull and one row per contact; `online`,
// `probed_at` and `last_seen` are null for a contact the daemon never probed.
export type Probe = { fingerprint: string; name: string; online: boolean | null; probed_at: string | null; last_seen: string | null }
export type Presence = { last_pull_at: string; open_asks: number; peers_probed: number; peers: Probe[] }

// One row of `owl harness list --json`.
export type Harness = { name: string; cmd: string[]; answer_path: string; enabled: boolean; drafting: boolean; found: boolean; path: string | null }

// One row of `owl project list --json`; `exists` is false when the checkout is gone.
export type Project = { name: string; path: string; exists: boolean }

// The `draft` of `owl show <id> --json`.
export type Draft = { text: string; harness: string; drafted_at: string; redactions: number; status: string }

// One row of `owl doctor --json`.
export type Check = { check: string; status: string; detail: string }

export type Route =
  | { name: 'chats' }
  | { name: 'contact'; peer: string } // fingerprint
  | { name: 'thread'; peer: string; context: string } // full context_id
  | { name: 'request'; peer: string; context: string; record: string } // the open incoming record
  | { name: 'contacts' }
  | { name: 'new' }
  | { name: 'card' }
  | { name: 'settings' }
  | { name: 'keys' }

// The panel's state. Module variables, as everywhere in this mod: a view reads them while
// drawing and an action changes them, then calls `redraw`.
export const s = {
  route: { name: 'chats' } as Route,
  prev: { name: 'chats' } as Route, // where `b` returns to from Keys
  note: '', // the last action's result; cleared on every route change
  paneOpen: false,
  unseen: { count: 0, questions: 0 },
  contacts: [] as Contact[],
  threads: [] as Thread[],
  archivedChats: [] as Thread[], // `owl thread --archived`
  timeline: [] as Ev[], // the open peer's events (routes contact / thread / request)
  // Show archived on a person's threads: the timeline is then `owl thread <peer> --archived`.
  // Any route without a peer (Keys aside) turns it off.
  archived: false,
  archivedThreads: 0, // how many of the open peer's threads are archived
  presence: null as Presence | null, // `owl presence`; null when it failed (see `failed.presence`)
  daemonAddr: '', // where the daemon is reachable (`daemon.addr` of the owl home, see `connectAddr`); '' unknown
  band: true,
  watch: false,
  // Handed between views: New's addressee, and the peer whose card the Card tab fetches.
  compose: { to: '' },
  cardPeer: '',
  // Why the last `owl contact list` / `owl thread` failed ('' when it did not): a list that
  // failed is not an empty one.
  failed: { contacts: '', threads: '', archived: '', presence: '' },
  // What the footer's `u: Undo` runs after an archive, unarchive or delete (`owl unarchive`,
  // `owl archive`, `owl undo`); empty when there is nothing to undo. Cleared on every route change.
  undo: [] as string[],
  // The key of the element the pane's focus ring is on ('' when none of ours).
  ring: '',
  // How many times Enter was pressed in each Input, by its key (see `fields` in ui.tsx).
  entered: {} as Record<string, number>,
  // The Claude Code theme (`$.config.list()`'s `theme` row), which picks the bubble fills.
  theme: '',
  // The session checkout's files for the `@` picker (`git ls-files`, else the top directory);
  // null until first asked for, then kept for the session.
  files: null as string[] | null,
  // Each person's newest event (`owl thread <peer> --json`), for the sign column of Chats.
  newest: {} as Record<string, Ev>,
  // The pane's body width in cells (the Pane's `bodyColumns`), for widths a Box cannot cap.
  cols: 100,
}

// The key an Input is drawn under now: `text`, then `text~1`, `text~2`, … one more per Enter.
export const fieldKey = (key: string) => (s.entered[key] ? `${key}~${s.entered[key]}` : key)

// The harness row actions and fields, never rows: a harness named `edit` is drawn under
// `harness-edit` too, and there the action's key wins (that one row gets no refocus).
const HARNESS_ACTIONS = ['harness-use', 'harness-edit', 'harness-remove', 'harness-scan', 'harness-add', 'harness-name', 'harness-cmd', 'harness-cmd-edit']

// The key the focus ring goes back to when it moved onto another chat, thread or harness
// row: the engine keeps the ring by position, not by key, and the redraw moves the selected
// row's action buttons, so the ring would land on the new row's `x`. '' for anything that is
// no other row (the actions above, the Inputs — `fieldKey`'s `~N` —, tabs, Contacts rows).
export const rowMove = (from: string, to: string) => {
  const key = to.replace(/~\d+$/, '')
  return to !== from && (/^(chat|thread)-/.test(key) || (key.startsWith('harness-') && !HARNESS_ACTIONS.includes(key))) ? to : ''
}

// What a view may do. owlpost.tsx builds it around `$` for each drawing.
export type Api = {
  // Runs `owl <args>` (see `Ran`).
  owl: (args: string[], opts?: RunOpts) => Promise<Ran>
  // `owl <args> --json` parsed, or `fallback` on a failure.
  json: <T>(args: string[], fallback: T) => Promise<T>
  // Runs one owl action: the note shows `… owl <cmd>`, then its first output line as
  // `✓ …` / `✗ …` (ids shortened); then everything reloads. Resolves to whether it exited 0.
  act: (args: string[], opts?: RunOpts) => Promise<boolean>
  // Shows a route (the note is cleared; a route with a peer loads that person's events).
  go: (route: Route) => Promise<void>
  // One level up: thread or request → contact → chats; Keys → where it was opened from.
  back: () => Promise<void>
  // Opens a thread: the incoming-request screen while a request in it waits, else messages.
  openThread: (peer: string, context: string) => Promise<void>
  // Sets the note line and redraws.
  say: (note: string) => void
  // Draws the pane again after a view changed its own state.
  redraw: () => void
  // Puts text on the clipboard; resolves to whether it took.
  copy: (text: string) => Promise<boolean>
  // The text in the main prompt box, for `u`.
  promptText: () => Promise<string>
  // Writes the main prompt box (over it, or at the cursor); resolves to whether it took.
  fill: (text: string, mode: 'replace' | 'insert') => Promise<boolean>
  // Moves the focus ring onto an element of the pane by key.
  focus: (key: string) => void
  // Draws the pane again and, once it is built, scrolls the element `key` into view (`focus`:
  // and moves the ring onto it, by its key as drawn then).
  reveal: (key: string, focus?: boolean) => void
  // Runs a slash command of the session (`owlpost:draft <id>`).
  command: (command: string, args: string) => Promise<void>
  // Writes one switch of plugin.json and keeps every other key.
  setSwitch: (key: 'band' | 'watch', value: boolean) => Promise<void>
  // The session checkout's files for the `@` picker, loaded once (see `s.files`).
  files: () => Promise<string[]>
  // Loads each listed person's newest event (the sign column of Chats) and redraws.
  loadNewest: () => Promise<void>
  // Writes the peer file `owlpost-peer.json` in the session's working directory; `link` when
  // that name is a symbolic link (nothing written), `failed` when the write did not take.
  writePeerFile: (text: string) => Promise<'written' | 'link' | 'failed'>
}

export type RunOpts = { stdin?: string; timeoutMs?: number }

// One `owl` run: `ok` on exit 0; `code` the exit code, -1 when `owl` did not start; `out` the
// trimmed stdout, or stderr on a failure (one plain line when `owl` did not start); `stdout`
// always stdout (`owl doctor --json` prints its rows and exits 1 when a check fails).
export type Ran = { ok: boolean; code: number; out: string; stdout: string }

// `owl allow <fp> --always` for a contact added by hand (source `global`) also needs this flag:
// the human's word that they compared the fingerprint out of band. A repo contact (source
// `local`) came through a reviewed PR and needs none. The mod passes it only after the human
// pressed "I verified this fingerprint" under the full fingerprint.
export const VERIFIED = '--i-verified-the-fingerprint'
export const mustVerify = (fp: string) => s.contacts.find((x) => x.fingerprint === fp)?.source !== 'local'

// What a view loads when its route is shown (the Card's card, Settings' version): each view
// file fills in its own entry.
export const enter: { [K in Route['name']]?: (api: Api) => Promise<void> } = {}

// ------------------------------------------------------------------ short ids

// A UUIDv7 starts with a timestamp, so the short id is the last six hex characters.
export const short = (id: string) => id.replace(/-/g, '').slice(-6)

// Every UUID in `text` written as its short id, for the note line.
export const shorten = (text: string) => text.replace(/\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/g, short)

// ------------------------------------------------------------------ threads of one person

export type Group = { context: string; events: Ev[]; first: Ev; last: Ev }

// One person's events grouped by `context_id` (a record without one is its own thread),
// newest thread first; each group keeps its events oldest first.
export function groups(events: Ev[]): Group[] {
  const by = new Map<string, Ev[]>()
  for (const e of events) {
    const k = e.context_id ?? e.record_id
    by.set(k, [...(by.get(k) ?? []), e])
  }
  return [...by.entries()]
    .map(([context, evs]) => ({ context, events: evs, first: evs[0], last: evs[evs.length - 1] }))
    .sort((a, b) => b.last.ts.localeCompare(a.last.ts))
}

const REQUESTS = ['question', 'content', 'tool-call']
const OPEN = ['consent', 'pending', 'drafted']

// The open incoming request of a thread: the newest incoming question, file request or tool
// call still waiting for the owner, judged by that record's newest event.
export function openRequest(events: Ev[]): Ev | undefined {
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i]
    if (e.dir !== 'in' || !REQUESTS.includes(e.type) || !OPEN.includes(e.state)) continue
    if (events.map((x) => x.record_id).lastIndexOf(e.record_id) === i) return e
  }
  return undefined
}

// The record `--reply-to` continues a thread with: its newest request (ours or the peer's,
// open, answered or rejected) or reply received (what `owl ask` accepts); '' when its records
// carry no thread id (an exchange from before threads, where `owl ask --reply-to` fails with
// `<id> carries no thread id`).
export function replyTarget(events: Ev[]): string {
  const ok = (e: Ev) => !!e.context_id && (e.dir === 'in' || REQUESTS.includes(e.type))
  return [...events].reverse().find(ok)?.record_id ?? ''
}

// ------------------------------------------------------------------ the bridge

// `@owl:msg://<id>`, what Cite and Apply put in the prompt box: one inbox message by its full
// id, so it names one message whatever else the spool holds. The shell's `prompt.submit` hook
// attaches that message as context. The id comes from the peer's signed payload, so only a
// canonical UUID is ever mentioned or looked up.
const UUID = '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'
export const isUuid = (id: string) => new RegExp(`^${UUID}$`).test(id)
export const mention = (id: string) => `@owl:msg://${id}`
export const MENTIONS = new RegExp(`@owl:msg://(${UUID})(?![0-9a-f-])`, 'g')

// `daemon.addr` (the daemon's bound `host:port`) as `owl doctor` connects to it: an
// unspecified host (0.0.0.0, ::) is the loopback. Anything that is not `host:port` is ''.
export function connectAddr(raw: string): string {
  const m = /^(\[[0-9a-fA-F:.]+\]|[0-9.]+):(\d{1,5})$/.exec(raw.trim())
  if (!m) return ''
  const host = m[1] === '0.0.0.0' ? '127.0.0.1' : m[1] === '[::]' ? '[::1]' : m[1]
  return `${host}:${m[2]}`
}

// The Daemon / Service row of a reachable daemon: ` reachable at <addr>` as the mock, or
// ` reachable` while the address is unknown.
export const reachable = () => (s.daemonAddr ? ` reachable at ${s.daemonAddr}` : ' reachable')
