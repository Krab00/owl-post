// The test world every `*.test.ts` of the mod shares: `owl`, plugin.json, the prompt box, the
// clipboard and the pane are answered from memory and every call is recorded; nothing touches
// a real home. Shapes are the real `owl … --json` output (see hooks/lib.ts).
import { mock } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On } from 'claude-code'
import type { Contact, Ev, Harness, Presence, Thread } from '../hooks/lib'

export const HOME = '/scratch/owlpost'
export const SURFACES = ['terminal', 'desktop'] as const
export type Surface = (typeof SURFACES)[number]

export const PANE = {
  component: 'Pane',
  requestId: 'owlpost',
  props: { title: 'owlpost', isFocused: true, bodyColumns: 100, placement: 'dock', scroll: { offset: 0, bodyRows: 40 }, view: {} },
} as const

// What `owl` answers for one argv (without the leading `owl`): exit code, stdout, stderr.
export type Answer = { exitCode?: number; stdout?: string; stderr?: string }

export type WorldOptions = {
  unseen?: number | { count: number; questions: number }
  contacts?: Contact[]
  threads?: Thread[]
  timelines?: Record<string, Ev[]> // `owl thread <fingerprint> --json`, by fingerprint
  archivedChats?: Thread[] // `owl thread --archived --json`
  archivedTimelines?: Record<string, Ev[]> // `owl thread --archived --json -- <fingerprint>`, by fingerprint
  presence?: Presence // `owl presence --json`; absent: exit 4 as with no daemon status yet
  daemonAddr?: string // `daemon.addr` of the owl home; absent: no such file
  harnesses?: Harness[] // `owl harness list --json`
  // Any other argv, joined with spaces (`card --json`, `send <id>`); a function sees every
  // call first and answers it when it returns something (a promise: `owl` still runs until then).
  answers?: Record<string, Answer | string> | ((args: string) => Answer | string | undefined | Promise<Answer | string | undefined>)
  stored?: { value?: unknown } // plugin.json: absent when undefined, a string as written, else JSON
  prompt?: string // the text in the main prompt box
  fillRefused?: boolean // the prompt box does not take a fill (a dialog holds the keys)
  ringRefused?: string // why the engine keeps the pane's focus ring where it is
  unplaced?: boolean // the engine leaves an opened pane waiting undrawn (read at each open)
  missing?: boolean // `owl` is not on PATH: every `$.process.run` rejects
  files?: string[] // `git ls-files` of the session's checkout; absent: git is no owl, refused
  links?: string[] // names in the working directory that are symbolic links (`$.fs.list`); absent: an empty directory
}

export type Run = { args: string; argv: string[]; stdin?: string; timeoutMs?: number }

// The long flags of every command the mod runs, as `owl <command> --help` lists them (--home,
// --json and --quiet are on all of them).
const FLAGS: Record<string, string[]> = {
  add: ['--local'],
  allow: ['--once', '--always', '--i-verified-the-fingerprint'],
  ask: ['--project', '--wait', '--no-cache', '--file', '--peer', '--reply-to', '--context'],
  call: ['--input', '--project', '--reply-to'],
  card: [],
  'contact list': ['--global', '--local'],
  'contact remove': ['--local'],
  'contact export': [],
  archive: ['--context'],
  unarchive: ['--context'],
  delete: ['--context'],
  undo: [],
  deny: [],
  doctor: [],
  draft: ['--harness', '--text', '--agent', '--prompt'],
  'harness list': [],
  'harness scan': [],
  'harness add': ['--answer-path'],
  'harness edit': [],
  'harness remove': [],
  'harness use': [],
  ping: [],
  presence: [],
  inbox: ['--count', '--new', '--all', '--format', '--follow', '--session', '--hook-event'],
  install: ['--dry-run'],
  reject: [],
  request: ['--ref', '--memory', '--reply-to'],
  send: [],
  show: ['--format'],
  thread: ['--since', '--context', '--archived'],
  uninstall: [],
  update: ['--source', '--dry-run'],
}

// How many arguments a command takes after `--`, where the mod puts a record id or free text
// (`owl <command> --help`); more is refused, so a flag written after `--` is an error too.
const ARGS: Record<string, number> = {
  show: 1, send: 1, reject: 1, draft: 1, allow: 1, deny: 1, thread: 1, card: 1, 'contact remove': 1, call: 2, request: 3,
  archive: 1, unarchive: 1, delete: 1, ping: 1, 'harness remove': 1, 'harness use': 1,
}

// What the real CLI refuses before any answer above is looked at, so a mocked success can never
// hide it: a command it does not have, a flag that command does not have or an argument starting
// with `-` before a `--` that is not a flag, or more arguments after `--` than the command takes
// (clap, exit 2), and `allow <fp> --always` for a contact that is not `local` without
// `--i-verified-the-fingerprint` (src/cli/allow.rs, exit 1).
function refused(argv: string[], contacts: Contact[]): Answer | undefined {
  if (argv.length === 1 && argv[0] === '--version') return undefined
  const cmd = argv[0] === 'contact' || argv[0] === 'harness' ? `${argv[0]} ${argv[1]}` : argv[0]
  const flags = FLAGS[cmd]
  if (!flags) return { exitCode: 2, stderr: `error: unrecognized subcommand '${cmd}'` }
  const end = argv.indexOf('--')
  const bad = (end < 0 ? argv : argv.slice(0, end)).find(
    (a) => a.startsWith('-') && a !== '-' && ![...flags, '--home', '--json', '--quiet'].includes(a.split('=')[0]),
  )
  if (bad) return { exitCode: 2, stderr: `error: unexpected argument '${bad}' found` }
  const extra = end < 0 ? undefined : argv.slice(end + 1)[ARGS[cmd] ?? Infinity]
  if (extra !== undefined) return { exitCode: 2, stderr: `error: unexpected argument '${extra}' found` }
  // `owl harness add|edit <name> -- <cmd>…`: the name goes before the `--` (clap's `last`
  // argument takes everything after it), the command is at least one word.
  if (cmd === 'harness add' || cmd === 'harness edit') {
    const names = argv.slice(2, end < 0 ? undefined : end).filter((a) => !a.startsWith('-'))
    if (names.length !== 1 || end < 0 || end === argv.length - 1) return { exitCode: 2, stderr: 'error: the following required arguments were not provided' }
  }
  if (argv[0] === 'allow' && argv.includes('--always') && !argv.includes('--i-verified-the-fingerprint')) {
    const c = contacts.find((x) => x.fingerprint === argv[1])
    if (c?.source !== 'local') return { exitCode: 1, stderr: `owl: ${c?.name} (${argv[1]}) was added by hand … repeat with --always --i-verified-the-fingerprint` }
  }
  return undefined
}

// The engine's Input as the person sees it, by surface and the key the view gives it (the mod
// draws it as `key`, `key~1`, … see `fields` in hooks/ui.tsx). Enter empties the field; a
// drawing puts `value` back only when the Input is new (another key) or its `value` differs from
// the one drawn before; a change leaves the typed text. Seen at every mount of the pane.
type Field = { key: string; value: string; text: string }
let inputs = new Map<string, Field>()

async function observe(p: Awaited<ReturnType<Engine['ui']['mount']>>, surface: Surface) {
  for (const i of await p.findAll({ type: 'Input' })) {
    const key = i.key ?? ''
    const value = String(i.props.value ?? '')
    const k = `${surface}:${key.split('~')[0]}`
    const was = inputs.get(k)
    inputs.set(k, { key, value, text: was && was.key === key && was.value === value ? was.text : value })
  }
}

// The key the Input a view calls `key` is drawn under now (any other key passes through).
export const current = (key: string, surface: Surface = 'terminal') => inputs.get(`${surface}:${key}`)?.key ?? key

export function world(on: On, o: WorldOptions = {}) {
  inputs = new Map()
  const unseen = o.unseen ?? 0
  const inbox = typeof unseen === 'number' ? { count: unseen, questions: unseen } : unseen
  const stored = o.stored ?? {}
  const rec = {
    runs: [] as Run[],
    opened: [] as string[],
    closed: [] as string[],
    writes: [] as { path: string; text: string }[],
    copied: [] as string[],
    commands: [] as { command: string; args: string }[],
    reads: { plugin: 0 },
    stored,
    prompt: { text: o.prompt ?? '', cursor: (o.prompt ?? '').length },
    fills: [] as { text: string; mode: string }[],
  }
  mock.env(on, { OWLPOST_HOME: HOME, HOME: '/scratch' })
  on('fs.read', async (_$, e) => {
    if (e.path === `${HOME}/daemon.addr` && o.daemonAddr !== undefined) return { value: o.daemonAddr }
    if (e.path === `${HOME}/plugin.json`) rec.reads.plugin++
    if (e.path !== `${HOME}/plugin.json` || stored.value === undefined) throw new Error('ENOENT')
    return { value: typeof stored.value === 'string' ? stored.value : JSON.stringify(stored.value) }
  })
  on('fs.write', async (_$, e) => {
    rec.writes.push({ path: e.path, text: e.text })
    if (e.path === `${HOME}/plugin.json`) stored.value = JSON.parse(e.text)
    return { value: undefined }
  })
  on('fs.list', async () => ({
    value: (o.links ?? []).map((name) => ({ name, kind: 'other' as const, size: 0, mtimeMs: 0, isLink: true })),
  }))
  on('process.run', async (_$, e) => {
    if (e.argv[0] === 'git' && o.files) return { value: { exitCode: 0, stdout: o.files.map((f) => `${f}\n`).join(''), stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
    const argv = e.argv.slice(1)
    const args = argv.join(' ')
    rec.runs.push({ args, argv, stdin: e.init?.stdin, timeoutMs: e.init?.timeoutMs })
    // A throwing hook is skipped and nothing else answers, so the mod's `$.process.run` rejects.
    if (o.missing) throw new Error('ENOENT')
    const peer = /^thread (\S+) --json$/.exec(args)?.[1]
    const archivedPeer = /^thread --archived --json -- (\S+)$/.exec(args)?.[1]
    let a: Answer | string | undefined =
      refused(argv, o.contacts ?? []) ?? (typeof o.answers === 'function' ? await o.answers(args) : o.answers?.[args])
    if (a === undefined) {
      if (args === 'inbox --count --json') a = JSON.stringify(inbox)
      else if (args === 'contact list --json') a = JSON.stringify(o.contacts ?? [])
      else if (args === 'thread --json') a = JSON.stringify(o.threads ?? [])
      else if (peer) a = JSON.stringify(o.timelines?.[peer] ?? [])
      // `owl thread` exits 4 on a list with no rows; a timeline is an empty list.
      else if (args === 'thread --archived --json') a = o.archivedChats?.length ? JSON.stringify(o.archivedChats) : { exitCode: 4, stderr: 'owl: no threads' }
      else if (archivedPeer) a = JSON.stringify(o.archivedTimelines?.[archivedPeer] ?? [])
      else if (args === 'presence --json')
        a = o.presence ? JSON.stringify(o.presence) : { exitCode: 4, stderr: 'owl: no daemon status yet (daemon.status missing) — is the daemon running? see owl install' }
      else if (args === 'harness list --json') a = JSON.stringify(o.harnesses ?? [])
      else a = ''
    }
    const r = typeof a === 'string' ? { stdout: a } : a
    return { value: { exitCode: r.exitCode ?? 0, stdout: r.stdout ?? '', stderr: r.stderr ?? '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('prompt.read', async () => ({ value: { text: rec.prompt.text, cursor: rec.prompt.cursor } }))
  // The box as the engine writes it: over the draft, after it, or at the cursor.
  on('prompt.fill', async (_$, e) => {
    rec.fills.push({ text: e.text, mode: e.mode })
    if (o.fillRefused) return { isFilled: false, refusal: 'dialog' as const }
    const p = rec.prompt
    if (e.mode === 'replace') p.text = e.text
    else if (e.mode === 'append') p.text += e.text
    else p.text = p.text.slice(0, p.cursor) + e.text + p.text.slice(p.cursor)
    p.cursor = e.mode === 'insert' ? p.cursor + e.text.length : p.text.length
    return { isFilled: true }
  })
  on('ui.copy', async (_$, e) => {
    rec.copied.push(e.text)
    return { value: { isCopied: true } }
  })
  on('command.run', async (_$, e) => {
    rec.commands.push({ command: e.command, args: e.args })
    return {}
  })
  on('ui.open', async (_$, e) => {
    rec.opened.push(e.id)
    if (o.unplaced) return { value: { isPlaced: false as const, reason: 'unasked: placed from 144 columns, the terminal is 95' } }
    return { value: { isPlaced: true as const } }
  })
  // The engine's own drawing where the mod passes: an empty Box.
  on('ui.render', async ($, e) => $.ui.resolve(e).Box({ key: 'engine' }))
  // The focus ring moves where it is asked to, unless the engine refuses.
  on('ui.focus', async () => (o.ringRefused ? { deny: o.ringRefused } : {}))
  on('ui.close', async (_$, e) => {
    rec.closed.push(e.id)
    return { value: undefined }
  })
  return rec
}

// `/owlpost:panel`: opens the pane on Chats (or closes it while open).
export const panel = ($: Engine) =>
  $.command.run({ command: 'owlpost:panel', args: '', origin: { kind: 'composer' }, presentation: { isFullscreen: true, columns: 200 } })

// Mounts the pane on one surface; unmount it when done.
export async function pane($: Engine, surface: Surface = 'terminal') {
  const p = await $.ui.mount({ plugin: 'owlpost', surface, ...PANE })
  await observe(p, surface)
  return p
}

// The current tab as the tab row marks it (`Chats 0`), or undefined on Keys.
export async function activeTab($: Engine) {
  const p = await pane($)
  const tab = await p.find({ key: 'tab-active' })
  await p.unmount()
  return tab?.text.trim()
}

// Presses one key-addressed element of the pane (a tab is `tab-<name>`).
export async function press($: Engine, key: string, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  await p.press({ key })
  await p.unmount()
}

// Types into one Input of the pane and presses Enter (`kind: 'change'` only edits); resolves to
// what the field shows once the mod has handled it, before any later drawing.
export async function type($: Engine, key: string, text: string, kind: 'change' | 'submit' = 'submit', surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const f = inputs.get(`${surface}:${key}`)
  await p.input({ key: f?.key ?? key, text, kind })
  if (f) f.text = kind === 'submit' ? '' : text
  await observe(p, surface)
  await p.unmount()
  return inputs.get(`${surface}:${key}`)?.text
}

// What one Input of the pane shows now, as the engine draws it.
export async function field($: Engine, key: string, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  await p.unmount()
  return inputs.get(`${surface}:${key}`)?.text
}

// Enter in one Input of the pane with what it shows, typing nothing; resolves as `type`.
export async function enter($: Engine, key: string, surface: Surface = 'terminal') {
  return type($, key, (await field($, key, surface)) ?? '', 'submit', surface)
}

// Every Text of the pane, in order: what a screen says.
export async function texts($: Engine, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const all = (await p.findAll({ type: 'Text' })).map((t) => t.text)
  await p.unmount()
  return all
}

// The pane's note line and footer hints.
export async function footer($: Engine, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const note = (await p.find({ key: 'note' }))?.text ?? ''
  const hints = (await p.find({ key: 'hints' }))?.text ?? ''
  await p.unmount()
  return { note, hints }
}

// The `owl` calls made since `from` (an index into `runs`), polling excluded.
export const calls = (rec: { runs: Run[] }, from = 0) =>
  rec.runs.slice(from).map((r) => r.args).filter((a) => !/^(inbox --count|contact list|thread|presence)( |$)/.test(a))

// Fixtures: two contacts and a conversation with Bob as `owl` prints them.
export const ALICE: Contact = { name: 'Alice', emails: ['a@x.io'], fingerprint: 'owl:pfbrpuq3pbfblrnd', pubkey: 'ed25519:iBPC', endpoints: [], source: 'global', policy: { mode: 'manual' } }
export const BOB: Contact = { name: 'Bob', emails: ['b@x.io'], fingerprint: 'owl:xri6rpdrer5rdmt4', pubkey: 'ed25519:FghbS', endpoints: [], source: 'local' }
