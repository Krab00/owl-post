// The incoming-request screen (hooks/request.tsx), reached as a person does: Chats → Bob →
// the thread whose request waits. `owl` is the world of tests/world.ts.
import { expect, test } from 'claude-code/testing'
import type { On } from 'claude-code'
import type { Engine } from 'claude-code/testing'
import { enter as enterView, s, short, type Api, type Contact, type Draft, type Ev } from '../hooks/lib'
import { request } from '../hooks/request'
import { DRAW_MAX, cutNote, day, hhmm } from '../hooks/ui'
import { BOB, SURFACES, calls, current, enter, field, footer, pane, panel, press, texts, type, world, type Answer, type Surface } from './world'

const FP = BOB.fingerprint
// The mock's footer: one line for every state of the request.
const HINTS = 'keystab moves sende editd draftx rejecto allow oncew allow alwaysc citeb back'
const CTX = '0199a000-0000-7000-8000-00000000c7c7'
const EARLIER = '0199a000-0000-7000-8000-00000000e1e1'
const REQ = '0199a000-0000-7000-8000-0000000abc12'
const OTHER = '0199a000-0000-7000-8000-0000000d0d0d' // a record of another thread with Bob
const ACTIONS = ['allow-once', 'allow-always', 'deny', 'draft', 'send', 'edit', 'reject', 'use-prompt', 'answer']

type Kind = 'question' | 'content' | 'tool-call'

// The thread with Bob as `owl thread <fp> --json` prints it: an earlier ask of ours, then
// Bob's request in `state`, with the events that brought it there.
function timeline(type: Kind, text: string, state: string): Ev[] {
  const base = { context_id: CTX, project: 'demo', path: '-' }
  const req = { ...base, dir: 'in' as const, record_id: REQ, type, state, text }
  const evs: Ev[] = [
    { ...base, context_id: null, ts: '2026-10-03T09:00:00Z', kind: 'received', dir: 'in', record_id: OTHER, type: 'question', state: 'sent', text: 'Another thread?' },
    { ...base, ts: '2026-10-04T12:00:00Z', kind: 'asked', dir: 'out', record_id: EARLIER, type: 'question', state: 'answered', text: 'Where is the config?', by: 'human' },
    { ...base, ts: '2026-10-04T12:05:00Z', kind: 'answer-received', dir: 'out', record_id: EARLIER, type: 'question', state: 'answered', text: 'Where is the config?' },
    { ...req, ts: '2026-10-04T14:14:00Z', kind: 'received' },
  ]
  if (state === 'consent') return [...evs, { ...req, ts: '2026-10-04T14:14:01Z', kind: 'held' }]
  evs.push({ ...req, ts: '2026-10-04T14:15:00Z', kind: 'allowed', by: 'human', detail: { scope: 'once' } })
  if (state !== 'pending' && state !== 'denied') evs.push({ ...req, ts: '2026-10-04T14:16:00Z', kind: 'drafted', by: 'claude' })
  if (['sent', 'rejected', 'denied'].includes(state)) evs.push({ ...req, ts: '2026-10-04T14:17:00Z', kind: state })
  return evs
}

const DRAFT: Draft = { text: 'It is in config/owl.toml.', harness: 'claude', drafted_at: '2026-10-04T14:16:00Z', redactions: 0, status: 'ok' }
const REDRAFT: Draft = { text: 'Timeouts live in src/net.rs.', harness: 'agent', drafted_at: '2026-10-04T14:18:00Z', redactions: 0, status: 'ok' }

// A world where Bob's request moves as `owl` would move it: allow → pending, draft → drafted,
// send / reject / deny → done. `fail` makes the argv starting with it exit 1; while `held` is
// pending, `owl show` has not answered yet.
// `redraft()` is the model's later draft landing on the record (`/owlpost:draft` writes it
// after the press): a newer `drafted` event and a new draft text.
function setup(on: On, o: { type?: Kind; text?: string; state: string; draft?: Draft; prompt?: string; contact?: Contact }) {
  const cur = { state: o.state, draft: o.draft ?? DRAFT, fail: '', gone: false, redrafted: false, redraft: () => {}, held: undefined as Promise<void> | undefined }
  cur.redraft = () => {
    cur.redrafted = true
    cur.draft = REDRAFT
  }
  const type: Kind = o.type ?? 'question'
  const text = o.text ?? 'Where do you keep the timeouts?'
  const answers = (args: string): Answer | string | undefined | Promise<string> => {
    if (args === `thread ${FP} --json`) {
      const evs = timeline(type, text, cur.state)
      if (cur.redrafted) evs.push({ ...evs.at(-1)!, ts: '2026-10-04T14:18:00Z', kind: 'drafted', by: 'agent' })
      return JSON.stringify(cur.gone ? [] : evs)
    }
    if (args === `show --json -- ${REQ}`) {
      const shown = JSON.stringify({ id: REQ, state: cur.state, draft: cur.state === 'drafted' ? cur.draft : null })
      return cur.held ? cur.held.then(() => shown) : shown
    }
    if (cur.fail && args.startsWith(cur.fail)) return { exitCode: 1, stderr: 'owl: no draft to send' }
    const next = { allow: 'pending', draft: 'drafted', send: 'sent', reject: 'rejected', deny: 'denied' }[args.split(' ')[0]]
    if (!next) return undefined
    cur.state = next
    return `${next} ${REQ}`
  }
  const rec = world(on, {
    threads: [{ from: FP, from_name: 'Bob', last_ts: '2026-10-04T14:14:00Z', unseen: 1, open: 1, last_summary: text }],
    contacts: [o.contact ?? BOB],
    answers,
    prompt: o.prompt,
  })
  return { rec, cur }
}

// Chats → Bob → the thread: the shell opens the request screen because the request waits.
async function reach($: Engine, surface: Surface) {
  await panel($)
  await press($, `chat-${FP}`, surface)
  await press($, `thread-${CTX}`, surface)
}

// Closes the pane again (the panel command toggles), so the next surface starts on Chats.
const leave = ($: Engine) => panel($)

// The action elements the screen draws now.
async function shown($: Engine, surface: Surface) {
  const p = await pane($, surface)
  const out: string[] = []
  for (const key of ACTIONS) if (await p.find({ key: current(key, surface) })) out.push(key)
  await p.unmount()
  return out
}

async function label($: Engine, key: string, surface: Surface) {
  const p = await pane($, surface)
  const t = (await p.find({ key: current(key, surface) }))?.text
  await p.unmount()
  return t
}

test('consent: allow once, allow always and deny, in the consent box, run it', async ($, on) => {
  const { rec, cur } = setup(on, { state: 'consent' })
  for (const surface of SURFACES) {
    for (const [key, argv, after] of [
      ['allow-once', ['allow', FP, '--once'], 'pending'],
      ['allow-always', ['allow', FP, '--always'], 'pending'],
      ['deny', ['deny', FP], 'denied'],
    ] as const) {
      cur.state = 'consent'
      await reach($, surface)
      expect(await shown($, surface)).toEqual(['allow-once', 'allow-always', 'deny'])
      // As the mock: in a box with a warning border, `Held for consent` and the three choices.
      expect(await label($, 'allow-once', surface)).toBe('Allow once')
      expect(await label($, 'allow-always', surface)).toBe('Allow always')
      expect(await label($, 'deny', surface)).toBe('Deny')
      expect(await texts($, surface)).toContain('Held for consent')
      const p = await pane($, surface)
      const held = (await p.findAll({ type: 'Box' })).find((b) => b.props.borderColor === 'warning')
      await p.unmount()
      expect(held?.text).toContain('Allow always')
      expect((await footer($, surface)).hints).toBe(HINTS)
      const from = rec.runs.length
      await press($, key, surface)
      expect(rec.runs.slice(from).find((r) => r.argv[0] === argv[0])?.argv).toEqual([...argv])
      expect(calls(rec, from)).toEqual([argv.join(' ')])
      expect(cur.state).toBe(after)
      await leave($)
    }
  }
})

test('the screen draws the thread, the request, its events and the default note', async ($, on) => {
  setup(on, { state: 'pending' })
  for (const surface of SURFACES) {
    await reach($, surface)
    const t = await texts($, surface)
    expect(t).toEqual(expect.arrayContaining(['Bob ›', 'Where is the config?']))
    expect(t).toContain(short(CTX))
    expect(t).toContain('Where is the config?')
    expect(t.filter((x) => x === 'Where do you keep the timeouts?')).toHaveLength(1)
    expect(t).not.toContain('Another thread?')
    expect(t).toContain(`${hhmm('2026-10-04T14:15:00Z')} · allowed once by you`)
    expect(t).toContain('! needs your answer')
    expect(t).toContain(`── ${day('2026-10-04T14:14:00Z')} ──`)
    const f = await footer($, surface)
    expect(f.hints).toContain('c cite') // the mock's hints: no u (it works, undrawn)
    expect(f.note).toBe('nothing leaves this machine until you press s')
    expect(f.hints).toBe(HINTS)
    expect(await shown($, surface)).toEqual(['draft', 'reject', 'use-prompt', 'answer'])
    expect(await label($, 'draft', surface)).toBe('Draft with Claude')
    await leave($)
  }
})

test('a thin rule parts two earlier messages; none sits next to the date rule', async ($, on) => {
  const base = { context_id: CTX, project: 'demo', path: '-' }
  const evs: Ev[] = [
    { ...base, ts: '2026-10-04T12:00:00Z', kind: 'asked', dir: 'out', record_id: EARLIER, type: 'question', state: 'answered', text: 'Where is the config?' },
    { ...base, ts: '2026-10-04T12:05:00Z', kind: 'answer-received', dir: 'in', record_id: OTHER, type: 'answer', state: 'received', text: 'In config/.' },
    { ...base, ts: '2026-10-04T14:14:00Z', kind: 'received', dir: 'in', record_id: REQ, type: 'question', state: 'pending', text: 'And the timeouts?' },
  ]
  world(on, { contacts: [BOB], threads: [{ from: FP, from_name: 'Bob', last_ts: '2026-10-04T14:14:00Z', unseen: 1, open: 1, last_summary: 'x' }], timelines: { [FP]: evs } })
  for (const surface of SURFACES) {
    await reach($, surface)
    const t = (await texts($, surface))
      .filter((x) => x.startsWith('──') || ['Where is the config?', 'In config/.', 'And the timeouts?'].includes(x))
      .map((x) => (/^─+$/.test(x) ? 'rule' : x))
    // One day: one date rule over the thread, then a thin rule between each two messages.
    expect(t.slice(t.findIndex((x) => x.startsWith('── ')), -1)).toEqual([`── ${day('2026-10-04T14:14:00Z')} ──`, 'Where is the config?', 'rule', 'In config/.', 'rule', 'And the timeouts?'])
    await leave($)
  }
})

test('Draft of a question goes to the model through /owlpost:draft, no owl run', async ($, on) => {
  const { rec } = setup(on, { state: 'pending' })
  for (const surface of SURFACES) {
    await reach($, surface)
    const from = rec.runs.length
    await press($, 'draft', surface)
    expect(rec.commands.at(-1)).toEqual({ command: 'owlpost:draft', args: REQ })
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('… drafting with Claude')
    await leave($)
  }
  expect(rec.commands).toHaveLength(2)
})

test('a file request runs owl draft, shows the file as the draft, and Send sends it', async ($, on) => {
  const file: Draft = { ...DRAFT, text: 'fn main() {}', harness: 'owl' }
  const { rec, cur } = setup(on, { type: 'content', text: 'src/main.rs@main', state: 'pending', draft: file })
  for (const surface of SURFACES) {
    cur.state = 'pending'
    await reach($, surface)
    expect(await texts($, surface)).toContain(`${hhmm('2026-10-04T14:14:00Z')} · file request · asks for src/main.rs@main`)
    expect(await shown($, surface)).toEqual(['draft', 'reject'])
    expect(await label($, 'draft', surface)).toBe('Draft')
    let from = rec.runs.length
    await press($, 'draft', surface)
    expect(calls(rec, from)).toEqual([`draft -- ${REQ}`, `show --json -- ${REQ}`])
    expect(rec.commands).toEqual([])
    const t = await texts($, surface)
    expect(t.some((x) => x.endsWith('· draft · by owl · not sent'))).toBe(true)
    expect(t).toContain('fn main() {}')
    expect(await shown($, surface)).toEqual(['draft', 'send', 'reject'])
    expect(await label($, 'draft', surface)).toBe('Redraft')
    from = rec.runs.length
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual([`show --json -- ${REQ}`, `send -- ${REQ}`])
    expect((await footer($, surface)).note).toBe(`✓ sent ${short(REQ)}`)
    expect(await texts($, surface)).toContain('✓ sent')
    await leave($)
  }
})

test('a tool call: Run (owl draft) runs owl draft, never the model, and has no own answer', async ($, on) => {
  const { rec, cur } = setup(on, { type: 'tool-call', text: 'grep {"pattern":"x"}', state: 'pending', draft: { ...DRAFT, text: 'a.rs:1: x', harness: 'owl' } })
  for (const surface of SURFACES) {
    cur.state = 'pending'
    await reach($, surface)
    expect(await texts($, surface)).toContain(`${hhmm('2026-10-04T14:14:00Z')} · tool call · asks to run grep {"pattern":"x"}`)
    expect(await shown($, surface)).toEqual(['draft', 'reject'])
    expect(await label($, 'draft', surface)).toBe('Run (owl draft)')
    const hints = (await footer($, surface)).hints
    expect(hints).toBe(HINTS)
    const from = rec.runs.length
    await press($, 'draft', surface)
    expect(calls(rec, from)).toEqual([`draft -- ${REQ}`, `show --json -- ${REQ}`])
    expect(await texts($, surface)).toContain('a.rs:1: x')
    expect(await shown($, surface)).toEqual(['draft', 'send', 'reject'])
    await leave($)
  }
  expect(rec.commands).toEqual([])
})

test('a drafted question shows the draft in its box; Edit puts it into Own answer', async ($, on) => {
  const { rec } = setup(on, { state: 'drafted' })
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await reach($, surface)
    expect(calls(rec, from)).toEqual([`show --json -- ${REQ}`])
    const t = await texts($, surface)
    // As the mock: the draft's head over our bubble, its border dashed in the accent.
    expect(t).toContain(`${hhmm('2026-10-04T14:16:00Z')} · draft · by claude · not sent`)
    expect(t).toContain('It is in config/owl.toml.')
    expect(t).toContain('! needs your answer')
    const p = await pane($, surface)
    const box = (await p.findAll({ type: 'Box' })).find((b) => b.props.borderStyle === 'dashed')
    await p.unmount()
    expect(box?.text).toContain('It is in config/owl.toml.')
    expect(await shown($, surface)).toEqual(['draft', 'send', 'edit', 'reject', 'use-prompt', 'answer'])
    expect(await label($, 'draft', surface)).toBe('Redraft')
    const hints = (await footer($, surface)).hints
    expect(hints).toBe(HINTS)
    expect(await label($, 'answer', surface)).toBe('')
    await press($, 'edit', surface)
    expect(await label($, 'answer', surface)).toBe('It is in config/owl.toml.')
    await leave($)
  }
})

// `plugin test` has no focus ring to answer the mod's own `$.ui.focus`, so the screen is drawn
// here with an Api that records what the ring is asked for, and Edit pressed on it.
test('Edit asks the focus ring for Own answer, and only for it', async () => {
  const asked: string[] = []
  const pressed: { key?: string; onPress?: () => void }[] = []
  const none = () => null
  const ui = { Box: none, Text: none, Input: none, Select: none, Button: (p: { key?: string; onPress?: () => void }) => (pressed.push(p), null) }
  const api = { json: async () => ({ draft: DRAFT }), focus: (key: string) => void asked.push(key), redraw: () => {}, say: () => {} }
  s.route = { name: 'request', peer: FP, context: CTX, record: REQ }
  s.timeline = timeline('question', 'Where?', 'drafted')
  await enterView.request?.(api as unknown as Api)
  request({ api: api as unknown as Api, ui: ui as never })
  expect(asked).toEqual([])
  pressed.find((p) => p.key === 'edit')?.onPress?.()
  expect(asked).toEqual(['answer'])
  s.route = { name: 'chats' }
  s.timeline = []
})

test('Own answer stores the typed text as the draft; u takes the prompt box; empty runs nothing', async ($, on) => {
  const { rec, cur } = setup(on, { state: 'pending', prompt: 'From the prompt box.' })
  for (const surface of SURFACES) {
    cur.state = 'pending'
    await reach($, surface)
    let from = rec.runs.length
    await type($, 'answer', '   ', 'submit', surface)
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ type an answer first')
    await press($, 'use-prompt', surface)
    expect(await label($, 'answer', surface)).toBe('From the prompt box.')
    from = rec.runs.length
    await type($, 'answer', 'My own words.', 'submit', surface)
    expect(rec.runs.slice(from).find((r) => r.argv[0] === 'draft')?.argv).toEqual(['draft', '--text=My own words.', '--', REQ])
    expect(calls(rec, from)).toEqual([`draft --text=My own words. -- ${REQ}`, `show --json -- ${REQ}`])
    expect(await label($, 'answer', surface)).toBe('')
    expect(await texts($, surface)).toContain('It is in config/owl.toml.')
    await leave($)
  }
})

test('Reject discards the request; once it is done no action is left, only its outcome', async ($, on) => {
  const { rec, cur } = setup(on, { state: 'pending' })
  for (const surface of SURFACES) {
    cur.state = 'pending'
    await reach($, surface)
    const from = rec.runs.length
    await press($, 'reject', surface)
    expect(calls(rec, from)).toEqual([`reject -- ${REQ}`])
    expect(await shown($, surface)).toEqual([])
    expect(await texts($, surface)).toContain('- rejected')
    expect((await footer($, surface)).hints).toBe(HINTS) // the mock's one footer, whatever the state
    await leave($)
  }
})

test('a denied request shows its outcome and no action', async ($, on) => {
  const { cur } = setup(on, { state: 'consent' })
  await reach($, 'terminal')
  await press($, 'deny')
  expect(cur.state).toBe('denied')
  expect(await shown($, 'terminal')).toEqual([])
  expect(await texts($)).toContain('- denied')
})

test('a failed send leaves the error in the note and the draft open', async ($, on) => {
  const { rec, cur } = setup(on, { state: 'drafted' })
  cur.fail = 'send'
  for (const surface of SURFACES) {
    await reach($, surface)
    const from = rec.runs.length
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual([`show --json -- ${REQ}`, `send -- ${REQ}`, `show --json -- ${REQ}`])
    expect((await footer($, surface)).note).toBe('✗ owl: no draft to send')
    expect(cur.state).toBe('drafted')
    expect(await shown($, surface)).toContain('send')
    expect(await texts($, surface)).toContain('It is in config/owl.toml.')
    await leave($)
  }
})

test('a request gone from the thread says so and offers nothing', async ($, on) => {
  const { cur } = setup(on, { state: 'pending' })
  await reach($, 'terminal')
  cur.gone = true
  await press($, 'reject')
  expect(await shown($, 'terminal')).toEqual([])
  expect(await texts($)).toContain('○ this request is not here any more')
})

test("the request's header is its own arrival (time, sender), not a later event's", async ($, on) => {
  setup(on, { state: 'pending' })
  for (const surface of SURFACES) {
    await reach($, surface)
    const t = await texts($, surface)
    // Bob's question arrived at 14:14; the 14:15 `allowed · by human` event is a line of its own.
    expect(t).toContain(`${hhmm('2026-10-04T14:14:00Z')} · question`)
    expect(t.some((x) => x.includes('question') && x.includes('by human'))).toBe(false)
    // An earlier record of the thread keeps its first event's time as well.
    expect(t).toContain(`${hhmm('2026-10-04T12:00:00Z')} · you · ✓ answered`)
    await leave($)
  }
})

// The text of the dashed draft box, or undefined when none is drawn.
async function box($: Engine, surface: Surface) {
  const p = await pane($, surface)
  const b = (await p.findAll({ type: 'Box' })).find((x) => x.props.borderStyle === 'dashed')
  await p.unmount()
  return b?.text
}

test('Allow always for a hand-added contact shows the full fingerprint and runs only after "I verified this fingerprint"', async ($, on) => {
  const { rec, cur } = setup(on, { state: 'consent', contact: { ...BOB, source: 'global' } })
  for (const surface of SURFACES) {
    cur.state = 'consent'
    await reach($, surface)
    let from = rec.runs.length
    await press($, 'allow-always', surface)
    expect(calls(rec, from)).toEqual([])
    const t = await texts($, surface)
    expect(t).toContain(FP)
    expect(t).toContain('Allow always answers Bob without asking you. Compare this fingerprint with Bob out of band (a call, a chat) first:')
    const p = await pane($, surface)
    const confirm = await p.find({ key: 'verify-confirm' })
    const cancel = await p.find({ key: 'verify-cancel' })
    await p.unmount()
    expect(confirm?.text).toBe('I verified this fingerprint')
    expect(confirm?.props.hotkey).toBeUndefined()
    expect(cancel?.props.hotkey).toBeUndefined()
    // Cancel runs nothing and takes the step away; the consent stays.
    await press($, 'verify-cancel', surface)
    expect(calls(rec, from)).toEqual([])
    expect(await shown($, surface)).toEqual(['allow-once', 'allow-always', 'deny'])
    const after = await pane($, surface)
    expect(await after.find({ key: 'verify-confirm' })).toBeUndefined()
    await after.unmount()
    expect(await texts($, surface)).not.toContain(FP)
    // Asked again and confirmed: owl gets the flag, and the request is released.
    await press($, 'allow-always', surface)
    from = rec.runs.length
    await press($, 'verify-confirm', surface)
    expect(calls(rec, from)).toEqual([`allow ${FP} --always --i-verified-the-fingerprint`])
    expect(cur.state).toBe('pending')
    await leave($)
  }
  // The flag never left without the press.
  expect(rec.runs.filter((r) => r.argv.includes('--i-verified-the-fingerprint'))).toHaveLength(SURFACES.length)
})

test('leaving the request with the fingerprint check open and coming back drops the check', async ($, on) => {
  const { rec, cur } = setup(on, { state: 'consent', contact: { ...BOB, source: 'global' } })
  for (const surface of SURFACES) {
    cur.state = 'consent'
    await reach($, surface)
    await press($, 'allow-always', surface)
    expect(await label($, 'verify-confirm', surface)).toBe('I verified this fingerprint')
    expect(await texts($, surface)).toContain(FP)
    await press($, 'back', surface)
    const from = rec.runs.length
    await press($, `thread-${CTX}`, surface)
    expect(await shown($, surface)).toEqual(['allow-once', 'allow-always', 'deny'])
    expect(await label($, 'verify-confirm', surface)).toBeUndefined()
    expect(await texts($, surface)).not.toContain(FP)
    expect(calls(rec, from)).toEqual([])
    expect(cur.state).toBe('consent')
    await leave($)
  }
})

test('Allow always for a repo (local) contact runs at once, without the flag', async ($, on) => {
  const { rec } = setup(on, { state: 'consent' })
  await reach($, 'desktop')
  const from = rec.runs.length
  await press($, 'allow-always', 'desktop')
  expect(calls(rec, from)).toEqual([`allow ${FP} --always`])
  expect(await texts($, 'desktop')).not.toContain('I verified this fingerprint')
})

test('a redraft that landed after the screen was drawn is shown before Send can send it', async ($, on) => {
  const { rec, cur } = setup(on, { state: 'drafted' })
  for (const surface of SURFACES) {
    cur.redrafted = false
    cur.draft = DRAFT
    await reach($, surface)
    await press($, 'draft', surface) // Redraft: the model writes the new draft later
    expect(await box($, surface)).toContain('It is in config/owl.toml.')
    cur.redraft()
    // `s` with the old text on screen sends nothing and shows what the record holds now.
    let from = rec.runs.length
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual([`show --json -- ${REQ}`, `show --json -- ${REQ}`]) // the check, then the box
    expect((await footer($, surface)).note).toBe('✗ the draft changed: read the new one, then press s again')
    const b = await box($, surface)
    expect(b).toContain('Timeouts live in src/net.rs.')
    expect((await texts($, surface)).some((x) => x.endsWith('· draft · by agent · not sent'))).toBe(true)
    expect(b).not.toContain('It is in config/owl.toml.')
    // The second `s` sends the draft now on screen.
    from = rec.runs.length
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual([`show --json -- ${REQ}`, `send -- ${REQ}`])
    cur.state = 'drafted'
    await leave($)
  }
})

test('a refresh that brings a newer draft event reloads the box', async ($, on) => {
  const { rec, cur } = setup(on, { state: 'drafted' })
  on('classic.FileChanged', async () => ({}))
  for (const surface of SURFACES) {
    cur.redrafted = false
    cur.draft = DRAFT
    await reach($, surface)
    expect(await box($, surface)).toContain('It is in config/owl.toml.')
    cur.redraft()
    const from = rec.runs.length
    await $.classic.FileChanged({ file_path: '/scratch/owlpost/sessions/s1/wake', event: 'change' } as never)
    await new Promise((r) => setTimeout(r, 50)) // the refresh runs in the background
    await texts($, surface) // a drawing notices the newer event and loads its draft
    await new Promise((r) => setTimeout(r, 50))
    expect(calls(rec, from)).toContain(`show --json -- ${REQ}`)
    const b = await box($, surface)
    expect(b).toContain('Timeouts live in src/net.rs.')
    expect((await texts($, surface)).some((x) => x.endsWith('· draft · by agent · not sent'))).toBe(true)
    await leave($)
  }
})

// Between a newer draft event and its `owl show`, the record holds a draft the screen has not
// read: the old box is gone and nothing offers to send until the new one is shown.
test('a newer draft event not read yet shows no box and no Send until its draft is loaded', async ($, on) => {
  const { cur } = setup(on, { state: 'drafted' })
  on('classic.FileChanged', async () => ({}))
  for (const surface of SURFACES) {
    cur.redrafted = false
    cur.draft = DRAFT
    await reach($, surface)
    expect(await shown($, surface)).toEqual(['draft', 'send', 'edit', 'reject', 'use-prompt', 'answer'])
    let release = () => {}
    cur.held = new Promise((r) => { release = r })
    cur.redraft()
    await $.classic.FileChanged({ file_path: '/scratch/owlpost/sessions/s1/wake', event: 'change' } as never)
    await new Promise((r) => setTimeout(r, 50)) // the refresh runs in the background
    expect(await box($, surface)).toBeUndefined()
    expect(await shown($, surface)).toEqual(['draft', 'reject', 'use-prompt', 'answer'])
    expect(await label($, 'draft', surface)).toBe('Draft with Claude')
    cur.held = undefined
    release()
    await new Promise((r) => setTimeout(r, 50))
    expect(await box($, surface)).toContain('Timeouts live in src/net.rs.')
    expect(await shown($, surface)).toEqual(['draft', 'send', 'edit', 'reject', 'use-prompt', 'answer'])
    await leave($)
  }
})

test('an own answer starting with - reaches owl as text; a failed one stays in the field', async ($, on) => {
  const { rec, cur } = setup(on, { state: 'pending' })
  for (const surface of SURFACES) {
    cur.state = 'pending'
    await reach($, surface)
    cur.fail = 'draft'
    let from = rec.runs.length
    await type($, 'answer', '- yes, in config/', 'submit', surface)
    expect(rec.runs.slice(from).find((r) => r.argv[0] === 'draft')?.argv).toEqual(['draft', '--text=- yes, in config/', '--', REQ])
    expect((await footer($, surface)).note).toBe('✗ owl: no draft to send')
    expect(await label($, 'answer', surface)).toBe('- yes, in config/')
    cur.fail = ''
    from = rec.runs.length
    await type($, 'answer', '- yes, in config/', 'submit', surface)
    expect(calls(rec, from)).toEqual([`draft --text=- yes, in config/ -- ${REQ}`, `show --json -- ${REQ}`])
    expect(cur.state).toBe('drafted')
    expect(await label($, 'answer', surface)).toBe('')
    await leave($)
  }
})

// The engine empties a field on Enter (tests/world.ts): a failed save leaves the typed answer in
// the field even when the mod drew it before Enter, and the next Enter saves that text again.
test('a failed own answer stays in the field through two failures; the next Enter saves it', async ($, on) => {
  const { rec, cur } = setup(on, { state: 'pending' })
  for (const surface of SURFACES) {
    cur.state = 'pending'
    await reach($, surface)
    cur.fail = 'draft'
    await type($, 'answer', 'My own words.', 'change', surface)
    await texts($, surface)
    let from = rec.runs.length
    await enter($, 'answer', surface)
    await enter($, 'answer', surface)
    expect(rec.runs.slice(from).filter((r) => r.argv[0] === 'draft').map((r) => r.argv)).toEqual([
      ['draft', '--text=My own words.', '--', REQ],
      ['draft', '--text=My own words.', '--', REQ],
    ])
    expect((await footer($, surface)).note).toBe('✗ owl: no draft to send')
    expect(await field($, 'answer', surface)).toBe('My own words.')
    cur.fail = ''
    from = rec.runs.length
    await enter($, 'answer', surface)
    expect(calls(rec, from)).toEqual([`draft --text=My own words. -- ${REQ}`, `show --json -- ${REQ}`])
    expect(cur.state).toBe('drafted')
    expect(await field($, 'answer', surface)).toBe('')
    await leave($)
  }
})

test('a file request with a 20 000-character path and draft draws both cut, with its actions', async ($, on) => {
  const path = 'p'.repeat(20_000) + 'TAIL@main'
  setup(on, { type: 'content', text: path, state: 'drafted', draft: { ...DRAFT, text: 'd'.repeat(20_000) + 'TAIL' } })
  for (const surface of SURFACES) {
    await reach($, surface)
    const all = await texts($, surface)
    // The request's head is one line, cut with the cut said; the draft bubble says its cut under it.
    const head = `${hhmm('2026-10-04T14:14:00Z')} · file request · asks for `
    expect(all).toContain(`${head}${'p'.repeat(DRAW_MAX - head.length)} ${cutNote(path.length + head.length)}`)
    expect(all).toContain('d'.repeat(DRAW_MAX))
    expect(all).toContain(cutNote(20_004))
    expect(all.some((x) => x.includes('TAIL'))).toBe(false)
    expect(await shown($, surface)).toContain('send')
    await leave($)
  }
})
