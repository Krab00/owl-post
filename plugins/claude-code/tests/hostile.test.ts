// Peer and record strings with line breaks and control characters on every screen that draws
// them. The engine refuses a Text holding a control character other than tab and newline and
// a label holding any, and either blanks the whole pane, so each such string is drawn through
// `clip` / `line` (hooks/ui.tsx). Each test feeds hostile values and checks every Text and
// label of each screen it reaches, then the exact line the value is drawn on.
import { expect, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On } from 'claude-code'
import type { Contact, Ev, Thread } from '../hooks/lib'
import { DRAW_MAX, cutNote, hhmm } from '../hooks/ui'
import { BOB, SURFACES, calls, field, footer, pane, panel, press, type, world, type Surface } from './world'

const BAD = '\u0007\u001b[2J\u0085\u009f' // BEL, ESC (a screen clear), NEL, APC
const SEEN = '  [2J  ' // BAD as drawn: each control character a space

const FP = BOB.fingerprint
const CTX = '0199b000-0000-7000-8000-00000000c7c7'
const REQ = '0199b000-0000-7000-8000-0000000abc12'
const A = '0199b000-0000-7000-8000-0000000a0a0a'

const ev = (o: Partial<Ev>): Ev => ({
  ts: '2026-10-04T14:14:00Z', kind: 'received', dir: 'in', record_id: REQ, context_id: CTX, type: 'question', state: 'pending',
  project: 'demo', path: '-', text: 'Where?', ...o,
})
const row = (o: Partial<Thread> = {}): Thread => ({ from: FP, from_name: 'Bob', last_ts: '2026-10-04T14:14:00Z', unseen: 0, open: 0, last_summary: 'x', ...o })

// Every Text and label the pane draws now, after checking none holds what the engine refuses.
async function look($: Engine, surface: Surface) {
  const p = await pane($, surface)
  const texts = (await p.findAll({ type: 'Text' })).map((x) => x.text)
  const labels = (await p.findAll({ type: 'Button' })).map((x) => String(x.props.label ?? x.text))
  await p.unmount()
  for (const x of texts) expect(x).not.toMatch(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/)
  for (const l of labels) expect(l).not.toMatch(/[\u0000-\u001f\u007f-\u009f]/)
  return { texts, labels }
}

test("a peer's thread id with control characters: the person and the thread draw it on one line", async ($, on) => {
  const ctx = `c${BAD}` // its short id is the last six characters: ESC [ 2 J NEL APC
  const timeline = [
    ev({ ts: '2026-10-04T10:00:00Z', kind: 'asked', dir: 'out', record_id: '0199b000-0000-7000-8000-000000000001', context_id: ctx, state: 'answered', text: 'A question' }),
    ev({ ts: '2026-10-04T10:01:00Z', kind: 'answer-received', record_id: A, context_id: ctx, type: 'answer', text: 'An answer' }),
  ]
  world(on, { contacts: [BOB], threads: [row()], timelines: { [FP]: timeline } })
  await panel($)
  for (const surface of SURFACES) {
    await press($, `chat-${FP}`, surface)
    const row = (await look($, surface)).texts
    const i = row.indexOf(' [2J  ') // the short id's cell
    expect(row.slice(i, i + 3)).toEqual([' [2J  ', '✓ answered', '2'])
    await press($, `thread-${ctx}`, surface)
    const { texts, labels } = await look($, surface)
    expect(texts).toEqual(expect.arrayContaining(['Bob ›', 'A question']))
    expect(texts).toContain(` [2J  `) // the BackRow's id
    expect(labels).toContain('Cite')
    expect((await footer($, surface)).note).toBe('thread  [2J   with Bob')
    await press($, 'back', surface)
    await press($, 'back', surface)
  }
})

test("two of a person's threads whose ids end in the same six hex are two rows, each opening its own", async ($, on) => {
  const [C1, C2] = ['11111111-1111-7111-8111-aaaaaa000001', '22222222-2222-7222-8222-bbbbbb000001']
  const timeline = [
    ev({ ts: '2026-10-04T10:00:00Z', kind: 'asked', dir: 'out', record_id: '11111111-1111-7111-8111-00000000000a', context_id: C1, state: 'waiting', text: 'first' }),
    ev({ ts: '2026-10-04T11:00:00Z', kind: 'asked', dir: 'out', record_id: '22222222-2222-7222-8222-00000000000b', context_id: C2, state: 'waiting', text: 'second' }),
  ]
  world(on, { contacts: [BOB], threads: [row()], timelines: { [FP]: timeline } })
  await panel($)
  for (const surface of SURFACES) {
    await press($, `chat-${FP}`, surface)
    const p = await pane($, surface)
    const rows = (await p.findAll({ type: 'Button' })).filter((b) => b.props.key?.startsWith('thread-')).map((b) => [b.props.key, b.text])
    await p.unmount()
    expect(rows).toEqual([[`thread-${C2}`, 'second'], [`thread-${C1}`, 'first']])
    await press($, `thread-${C1}`, surface)
    expect((await look($, surface)).texts).toEqual(expect.arrayContaining(['Bob ›', 'first']))
    await press($, 'back', surface)
    await press($, `thread-${C2}`, surface)
    expect((await look($, surface)).texts).toEqual(expect.arrayContaining(['Bob ›', 'second']))
    await press($, 'back', surface)
    await press($, 'back', surface)
  }
})

test('lists owl could not give, with control characters in the error, say why on one screen each', async ($, on) => {
  const fail = { exitCode: 1, stderr: `owl: broken${BAD}home` }
  world(on, { answers: { 'contact list --json': fail, 'thread --json': fail } })
  await panel($)
  for (const surface of SURFACES) {
    expect((await look($, surface)).texts).toContain(`- owl thread failed: owl: broken${SEEN}home`)
    await press($, 'tab-contacts', surface)
    expect((await look($, surface)).texts).toContain(`- owl contact list failed: owl: broken${SEEN}home`)
    await press($, 'tab-chats', surface)
  }
})

test("a contact's name, fingerprint and policy with line breaks and control characters", async ($, on) => {
  const fp = `owl:eve${BAD}`
  const name = `Eve\n${BAD}`
  const shown = `Eve ${SEEN}`
  const eve: Contact = { name, emails: ['e@x.io'], fingerprint: fp, pubkey: 'ed25519:e', endpoints: [], source: 'global', policy: { mode: `manual${BAD}` } }
  const timeline = [ev({ record_id: REQ, context_id: CTX, state: 'consent', text: 'Hey' })]
  world(on, { contacts: [eve], threads: [row({ from: fp, from_name: name })], timelines: { [fp]: timeline } })
  await panel($)
  for (const surface of SURFACES) {
    // Chats: the person, then the request waiting for consent.
    await press($, `chat-${fp}`, surface)
    let seen = await look($, surface)
    expect(seen.texts).toEqual(expect.arrayContaining([shown, `? not probed · owl:eve${SEEN} · policy manual${SEEN}`]))
    await press($, `thread-${CTX}`, surface)
    seen = await look($, surface)
    expect(seen.texts).toEqual(expect.arrayContaining([`${shown} ›`, `Hey`]))
    expect(seen.labels).toEqual(expect.arrayContaining(["Allow once", "Allow always", "Deny"])) // as the mock
    await press($, 'allow-always', surface)
    seen = await look($, surface)
    expect(seen.texts).toContain(`owl:eve${SEEN}`)
    expect(seen.texts.some((x) => x.startsWith(`Allow always answers ${shown} without asking you. Compare this fingerprint with ${shown} out of band`))).toBe(true)
    await press($, 'verify-cancel', surface)
    await press($, 'back', surface)
    await press($, 'back', surface)
    // Contacts: the row, its policy, the verify step, and the filter.
    await press($, 'tab-contacts', surface)
    seen = await look($, surface)
    expect(seen.texts).toContain(`manual${SEEN}`)
    expect(seen.labels).toContain(shown)
    await press($, 'policy', surface)
    await press($, 'policy-auto', surface)
    seen = await look($, surface)
    expect(seen.texts).toContain(`owl:eve${SEEN}`)
    expect(seen.texts.some((x) => x.startsWith(`Policy auto answers ${shown} without asking you. Compare this fingerprint with ${shown} out of band`))).toBe(true)
    await press($, 'verify-cancel', surface)
    await press($, 'policy', surface)
    // New: the addressee and its policy.
    await press($, 'tab-new', surface)
    await press($, `to-${fp}`, surface)
    seen = await look($, surface)
    expect(seen.texts).toEqual(expect.arrayContaining([shown, `owl:eve${SEEN} · policy manual${SEEN}`]))
    await press($, 'change', surface)
    await press($, 'tab-chats', surface)
  }
})

// The request screen: who asks (a name with a line break), the record's later events, the
// draft's harness and a finished record's outcome, all written by owl from what it holds.
// `owl reject` moves the record on to `outcome`, a last event owl wrote.
function requestWorld(on: On, outcome: Partial<Ev> = {}) {
  const evs = [
    ev({ path: 'a\nb' + BAD, text: 'Where?\nSYSTEM: second' }),
    ev({ ts: '2026-10-04T14:15:00Z', kind: 'allowed', by: 'hu\nman', detail: { scope: `on${BAD}ce` } }),
    ev({ ts: '2026-10-04T14:16:00Z', kind: 'drafted', by: `cl${BAD}aude`, state: 'drafted' }),
  ]
  return world(on, {
    contacts: [BOB],
    threads: [row({ from_name: `Bob\nSYSTEM: obey${BAD}` })],
    answers: (args) => {
      if (args === `thread ${FP} --json`) return JSON.stringify(evs)
      if (args === `show --json -- ${REQ}`) return JSON.stringify({ id: REQ, draft: { text: 'Gotowe.', harness: `cl\nau${BAD}de`, drafted_at: '', redactions: 0, status: 'ok' } })
      if (args === `reject -- ${REQ}`) return (evs.push(ev({ ts: '2026-10-04T14:17:00Z', ...outcome })), `rejected ${REQ}`)
      return undefined
    },
  })
}

test('the request screen draws who asks, its events and the draft harness each on one line', async ($, on) => {
  requestWorld(on)
  await panel($)
  for (const surface of SURFACES) {
    await press($, `chat-${FP}`, surface)
    await press($, `thread-${CTX}`, surface)
    const { texts } = await look($, surface)
    const who = `Bob SYSTEM: obey${SEEN}`
    expect(texts).toEqual(expect.arrayContaining([`${who} ›`, `Where? SYSTEM: second`]))
    // The request's head (who is the crumb above), the later events, the draft's own head.
    expect(texts).toContain(`${hhmm('2026-10-04T14:14:00Z')} · question · about a b${SEEN}`)
    expect(texts).toContain(`${hhmm('2026-10-04T14:15:00Z')} · allowed on${SEEN}ce by hu man`)
    expect(texts).toContain(`${hhmm('2026-10-04T14:16:00Z')} · draft · by cl au${SEEN}de · not sent`)
    expect(texts).toContain('Where?\nSYSTEM: second') // the message itself keeps its lines
    await press($, 'back', surface)
    await press($, 'back', surface)
  }
})

test("a request's outcome, and a state owl does not name, draw on one line", async ($, on) => {
  requestWorld(on, { kind: `rej${BAD}ected`, state: `odd${BAD}` })
  await panel($)
  await press($, `chat-${FP}`)
  await press($, `thread-${CTX}`)
  await press($, 'reject')
  const { texts } = await look($, 'terminal')
  expect(texts).toContain(`○ rej${SEEN}ected`) // the outcome under the request
  expect(texts).toContain(`○ odd${SEEN}`) // the message's state
})

test('the request screen: "asks to run" and "asks for" lines are drawn clean and cut', async ($, on) => {
  const long = 'r'.repeat(DRAW_MAX) + 'TAIL'
  let cur = ev({})
  world(on, { contacts: [BOB], threads: [row()], answers: (args) => (args === `thread ${FP} --json` ? JSON.stringify([cur]) : undefined) })
  for (const [type, text, line] of [
    // In the request's head, after its kind, as the mock (`question · asks for a memory entry`).
    ['tool-call', `lint${BAD}{}`, `· tool call · asks to run lint${SEEN}{}`],
    ['content', `a.md${BAD}@main`, `· file request · asks for a.md${SEEN}@main`],
    ['tool-call', long, `· tool call · asks to run ${'r'.repeat(DRAW_MAX - 40)}`],
  ] as const) {
    cur = ev({ type, text })
    await panel($)
    await press($, `chat-${FP}`)
    await press($, `thread-${CTX}`)
    const { texts } = await look($, 'terminal')
    expect(texts.some((x) => x.includes(line))).toBe(true)
    expect(texts.some((x) => x.includes('TAIL'))).toBe(false)
    await panel($)
  }
})

test('a request whose id starts with - reaches owl after --, so no action reads it as a flag', async ($, on) => {
  const id = '-x1'
  let state = 'pending'
  const rec = world(on, {
    contacts: [BOB],
    threads: [row()],
    answers: (args) => {
      if (args === `thread ${FP} --json`) return JSON.stringify([ev({ record_id: id, context_id: CTX, state })])
      if (args === `show --json -- ${id}`) return JSON.stringify({ id, draft: { text: 'Yes.', harness: 'human', drafted_at: '', redactions: 0, status: 'ok' } })
      if (/^(draft|send|reject) /.test(args)) return `ok ${id}`
      return undefined
    },
  })
  await panel($)
  await press($, `chat-${FP}`)
  await press($, `thread-${CTX}`)
  let from = rec.runs.length
  await type($, 'answer', 'Yes.')
  expect(calls(rec, from)).toEqual([`draft --text=Yes. -- ${id}`])
  expect((await footer($)).note).toBe(`✓ ok ${id}`)
  state = 'drafted'
  await press($, 'back')
  await press($, `thread-${CTX}`)
  from = rec.runs.length
  await press($, 'send')
  expect(calls(rec, from)).toEqual([`show --json -- ${id}`, `send -- ${id}`, `show --json -- ${id}`])
  expect((await footer($)).note).toBe(`✓ ok ${id}`)
  from = rec.runs.length
  await press($, 'reject')
  expect(calls(rec, from)).toEqual([`reject -- ${id}`, `show --json -- ${id}`])
  expect((await footer($)).note).toBe(`✓ ok ${id}`)
})

test('a file request and a tool call with an id starting with - are drafted after --', async ($, on) => {
  const id = '-x2'
  let type = ''
  const rec = world(on, {
    contacts: [BOB],
    threads: [row()],
    answers: (args) => (args === `thread ${FP} --json` ? JSON.stringify([ev({ record_id: id, context_id: CTX, type, text: 'a.md@main' })]) : args.startsWith('draft ') ? `ok ${id}` : undefined),
  })
  for (type of ['content', 'tool-call']) {
    await panel($)
    await press($, `chat-${FP}`)
    await press($, `thread-${CTX}`)
    const from = rec.runs.length
    await press($, 'draft')
    expect(calls(rec, from)[0]).toBe(`draft -- ${id}`)
    expect((await footer($)).note).toBe(`✓ ok ${id}`)
    await panel($)
  }
})

test('doctor rows, the version and the raw card with control characters', async ($, on) => {
  const card = { name: `Me${BAD}`, version: '1', provider: { url: 'mailto:m@x.io' } }
  world(on, {
    answers: {
      '--version': `owl 0.3.0${BAD}`,
      'doctor --json': { exitCode: 1, stdout: JSON.stringify([{ check: `key${BAD}`, status: 'fail', detail: `bad\n${BAD}detail` }]) },
      'card --json': JSON.stringify(card),
    },
  })
  await panel($)
  for (const surface of SURFACES) {
    await press($, 'tab-settings', surface)
    await press($, 'doctor', surface)
    const { texts } = await look($, surface)
    expect(texts).toContain(`0.3.0${SEEN}`) // the Version row: owlpost and the number
    await press($, 'tab-card', surface)
    expect((await look($, surface)).texts).toContain(`Me${SEEN}`) // the Name row
    await press($, 'tab-settings', surface)
    await press($, 'doctor', surface)
    expect(texts).toContain(`key${SEEN}`)
    expect(texts).toContain(`bad\n${SEEN}detail`)
    await press($, 'tab-card', surface)
    await press($, 'raw', surface)
    // JSON escapes C0 itself; C1 it writes as is.
    expect((await look($, surface)).texts).toContain(JSON.stringify(card, null, 2).replace(/[\u0085\u009f]/g, ' '))
    await press($, 'raw', surface)
    await press($, 'tab-chats', surface)
  }
})

test('Edit puts a draft with control characters in the field as spaces; one too long for the field stays out', async ($, on) => {
  let text = `Yes${BAD}\nno`
  world(on, {
    contacts: [BOB],
    threads: [row()],
    answers: (args) =>
      args === `thread ${FP} --json`
        ? JSON.stringify([ev({ kind: 'drafted', state: 'drafted' })])
        : args === `show --json -- ${REQ}`
          ? JSON.stringify({ id: REQ, draft: { text, harness: 'h', drafted_at: '', redactions: 0, status: 'ok' } })
          : undefined,
  })
  await panel($)
  await press($, `chat-${FP}`)
  await press($, `thread-${CTX}`)
  await press($, 'edit')
  expect(await field($, 'answer')).toBe(`Yes${SEEN}\nno`)
  for (const [n, kept] of [[10_000, true], [10_001, false]] as const) {
    text = 'x'.repeat(n)
    await press($, 'back')
    await press($, `thread-${CTX}`)
    await press($, 'edit')
    expect(await field($, 'answer')).toBe(kept ? text : '')
    expect((await footer($)).note).toBe(kept ? 'nothing leaves this machine until you press s' : `✗ the draft has ${n} characters, too many to edit here`)
  }
})

// A colleague's card as their machine served it (`owl card <peer>` prints it unchanged), with
// fields of the wrong type: each such field is left out (or shown as text, for a number or a
// boolean), the pane draws, and the rows around it and the own card stay as they were.
test("a colleague's card with fields of the wrong type draws every other row; one that is no object is no card", async ($, on) => {
  const ext = (name: string, params: unknown) => ({ uri: `urn:owlpost:ext:${name}:v1`, params })
  const card = (o: Record<string, unknown> = {}) => ({
    name: 'Bob',
    version: '0.3.0',
    provider: { url: 'mailto:b@x.io' },
    supportedInterfaces: [{ url: 'owl-iroh://bob' }],
    capabilities: {
      extensions: [
        ext('identity', { fingerprint: FP, pubkey: 'ed25519:bob' }),
        ext('repo-question', { projects: ['demo'] }),
        ext('human-gate', { harness: 'codex', responds: true }),
      ],
    },
    ...o,
  })
  const ROWS: Record<string, string> = {
    Name: 'Bob', 'E-mail': 'b@x.io', Fingerprint: FP, 'Public key': 'ed25519:bob', Transport: 'iroh, pinned to this key',
    Projects: 'demo', Answers: 'yes, a human approves every answer', Harness: 'codex',
  }
  const hostile = { toString: 1, valueOf: 1 } // String(), a template and join() all throw on it
  let deep: unknown = 'x'
  for (let i = 0; i < 2000; i++) deep = [deep]
  const gate = (params: unknown) => ({ capabilities: { extensions: [card().capabilities.extensions[0], card().capabilities.extensions[1], ext('human-gate', params)] } })
  const shapes: [string, unknown, Partial<Record<string, string>>][] = [
    ['interfaces a string', card({ supportedInterfaces: 'not-an-array' }), { Transport: '' }],
    ['interfaces holding null', card({ supportedInterfaces: [null, { url: 'owl-iroh://bob' }] }), {}],
    ['interfaces holding numbers, strings, a url of the wrong type', card({ supportedInterfaces: [5, 'x', { url: hostile }, { url: 7 }] }), { Transport: '7' }],
    ['interfaces nested deep', card({ supportedInterfaces: deep }), { Transport: '' }],
    ['interfaces 50 000 of them', card({ supportedInterfaces: Array(50_000).fill({ url: 'owl-iroh://b' }) }), { Transport: 'iroh' }],
    ['extensions not an array', card({ capabilities: { extensions: 'x' } }), { Transport: 'iroh', Fingerprint: '', 'Public key': '', Projects: 'none', Answers: 'no', Harness: '-' }],
    ['capabilities null', card({ capabilities: null }), { Transport: 'iroh', Fingerprint: '', 'Public key': '', Projects: 'none', Answers: 'no', Harness: '-' }],
    ['capabilities an array', card({ capabilities: [1] }), { Transport: 'iroh', Fingerprint: '', 'Public key': '', Projects: 'none', Answers: 'no', Harness: '-' }],
    ['extensions holding null, numbers and a uri of the wrong type', card({ capabilities: { extensions: [null, 5, 'x', { uri: hostile }, ...card().capabilities.extensions] } }), {}],
    ['identity params a string', card({ capabilities: { extensions: [ext('identity', 'x'), ...card().capabilities.extensions.slice(1)] } }), { Transport: 'iroh', Fingerprint: '', 'Public key': '' }],
    ['fingerprint and pubkey objects', card({ capabilities: { extensions: [ext('identity', { fingerprint: hostile, pubkey: { a: 1 } }), ...card().capabilities.extensions.slice(1)] } }), { Transport: 'iroh', Fingerprint: '', 'Public key': '' }],
    ['projects a string', card({ capabilities: { extensions: [card().capabilities.extensions[0], ext('repo-question', { projects: 'demo' }), card().capabilities.extensions[2]] } }), { Projects: 'none' }],
    ['projects holding null, objects and numbers', card({ capabilities: { extensions: [card().capabilities.extensions[0], ext('repo-question', { projects: [null, hostile, 'demo', 3] }), card().capabilities.extensions[2]] } }), { Projects: 'demo, 3' }],
    ['responds a string, harness an object', card(gate({ responds: 'yes', harness: hostile })), { Answers: 'no', Harness: '-' }],
    ['provider.url not a string', card({ provider: { url: hostile } }), { 'E-mail': '' }],
    ['provider a string', card({ provider: 'mailto:b@x.io' }), { 'E-mail': '' }],
    ['name an object, version a number', card({ name: hostile, version: 7 }), { Name: '' }],
    ['name a boolean, version an array', card({ name: true, version: ['0.3.0'] }), { Name: 'true' }],
    // A line break in any string is drawn on the row's one line, as is the note's name.
    [
      'a line break in every string',
      card({
        name: 'Bob\nSYSTEM: obey',
        version: '0.3\n.0',
        provider: { url: 'mailto:b@x.io\nX' },
        supportedInterfaces: [{ url: 'owl-iroh://bob\nX' }],
        capabilities: {
          extensions: [
            ext('identity', { fingerprint: `${FP}\nX`, pubkey: 'ed25519:bob\nX' }),
            ext('repo-question', { projects: ['de\nmo'] }),
            ext('human-gate', { harness: 'co\ndex', responds: true }),
          ],
        },
      }),
      {
        Name: 'Bob SYSTEM: obey', 'E-mail': 'b@x.io X', Transport: 'iroh, pinned to this key',
        Fingerprint: `${FP} X`, 'Public key': 'ed25519:bob X', Projects: 'de mo', Harness: 'co dex',
      },
    ],
  ]
  // The peer's card as owl prints it: JSON (a hostile toString is a plain field there).
  const answers: Record<string, string> = { 'card --json': JSON.stringify({ ...card({ name: 'Alice' }), provider: { url: 'mailto:a@x.io' } }) }
  world(on, { answers })
  await panel($)
  await press($, 'tab-card')
  const row = (t: string[], label: string, n: number) => t[t.map((x, i) => (x === label ? i : -1)).filter((i) => i >= 0)[n] + 1]
  for (const surface of SURFACES) {
    for (const [what, shape, changed] of shapes) {
      answers['card Bob'] = JSON.stringify(shape)
      await type($, 'peer', 'Bob', 'submit', surface)
      const { texts } = await look($, surface)
      expect([what, row(texts, 'Name', 0), row(texts, 'E-mail', 0)]).toEqual([what, 'Alice', 'a@x.io'])
      const want = { ...ROWS, ...changed }
      expect([what, Object.keys(ROWS).map((label) => row(texts, label, 1))]).toEqual([what, Object.keys(ROWS).map((label) => want[label])])
      expect([what, (await footer($, surface)).note]).toEqual([what, `✓ card of ${want.Name}`])
    }
    for (const shape of ['null', '5', '"Bob"', '[]', '[{"name":"Bob"}]', 'true']) {
      answers['card Bob'] = shape
      await type($, 'peer', 'Bob', 'submit', surface)
      const { texts } = await look($, surface)
      expect([shape, row(texts, 'Name', 0), texts.filter((x) => x === 'Name').length]).toEqual([shape, 'Alice', 1])
      expect([shape, (await footer($, surface)).note]).toEqual([shape, '✗ owl card Bob printed no card'])
    }
  }
})

test('an own card that is no JSON object is no card', async ($, on) => {
  const answers: Record<string, string> = {}
  world(on, { answers })
  await panel($)
  for (const shape of ['null', '5', '"Alice"', '[]', '[{"name":"Alice"}]', 'true']) {
    answers['card --json'] = shape
    await press($, 'tab-chats')
    await press($, 'tab-card')
    const { texts } = await look($, SURFACES[0])
    expect([shape, texts.includes('- no card: is owl set up? Run doctor in Settings.'), texts.includes('Name')]).toEqual([shape, true, false])
  }
})
