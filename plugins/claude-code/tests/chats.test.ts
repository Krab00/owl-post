// The Chats tab (hooks/chats.tsx): the list of people, one person's threads, one thread and
// its reply block. `owl` is the world of tests/world.ts.
import { expect, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { Ev, Thread } from '../hooks/lib'
import { P, SIGN, day, when, whenShort } from '../hooks/ui'
import { ALICE, BOB, SURFACES, activeTab, calls, current, enter, field, footer, pane, panel, press, texts, type, world, type Surface } from './world'

// Thread ids (UUIDv7) and their short ids.
const C_OPEN = '0199ae10-2b3c-7d4e-8f50-6a7b8c4e8b1d' // Bob asks, waiting for us: 4e8b1d
const C_ASK = '0199a8f0-1a2b-7c3d-8e4f-5a6b7cc3f9a2' // our asks: c3f9a2
const C_ANSWERED = '0199a3d0-0f1e-7a2b-8c3d-4e5f6a72a6f0' // Bob asked, we answered: 72a6f0
const R1 = '0199a8f0-1a2b-7c3d-8e4f-000000000001'
const R2 = '0199a8f0-1a2b-7c3d-8e4f-000000000002'
const R3 = '0199a8f0-1a2b-7c3d-8e4f-000000000003'
const R4 = '0199ae10-2b3c-7d4e-8f50-000000000004'
const R5 = '0199a3d0-0f1e-7a2b-8c3d-000000000005'

const ev = (o: Partial<Ev> & Pick<Ev, 'ts' | 'kind' | 'dir' | 'record_id' | 'type' | 'state' | 'text'>): Ev => ({
  context_id: null, project: 'github.com/krab00/owl-post', path: '-', ...o,
})

// `owl thread <bob> --json`, oldest first: one thread we answered (two events, one record),
// one of our asks over two days (three records), one question of Bob's still waiting.
const BOB_TIMELINE: Ev[] = [
  ev({ ts: '2026-10-02T12:00:00Z', kind: 'received', dir: 'in', record_id: R5, context_id: C_ANSWERED, type: 'question', state: 'pending', text: 'What time do you start tomorrow?' }),
  ev({ ts: '2026-10-02T12:20:00Z', kind: 'sent', dir: 'in', record_id: R5, context_id: C_ANSWERED, type: 'question', state: 'sent', text: 'What time do you start tomorrow?', by: 'Alice' }),
  ev({ ts: '2026-10-03T10:08:00Z', kind: 'asked', dir: 'out', record_id: R1, context_id: C_ASK, type: 'question', state: 'waiting', text: 'What is the last commit on your side?' }),
  ev({ ts: '2026-10-03T10:09:00Z', kind: 'answer-received', dir: 'out', record_id: R1, context_id: C_ASK, type: 'question', state: 'answered', text: 'What is the last commit on your side?' }),
  ev({ ts: '2026-10-03T10:09:00Z', kind: 'answer-received', dir: 'in', record_id: R2, context_id: C_ASK, type: 'answer', state: 'received', text: 'Last commit on main: a1b2c3d', harness: 'claude' }),
  ev({ ts: '2026-10-04T10:15:00Z', kind: 'asked', dir: 'out', record_id: R3, context_id: C_ASK, type: 'question', state: 'waiting', text: 'And which branch is feature/x on now?' }),
  ev({ ts: '2026-10-04T12:14:00Z', kind: 'received', dir: 'in', record_id: R4, context_id: C_OPEN, type: 'question', state: 'pending', text: 'Do you have decisions about communication in memory?' }),
]

// `owl thread --json`: newest conversation first.
const THREADS: Thread[] = [
  { from: BOB.fingerprint, from_name: 'Bob', last_ts: '2026-10-04T12:14:00Z', unseen: 1, open: 1, last_summary: 'Do you have decisions about communication in memory?' },
  { from: ALICE.fingerprint, from_name: 'Alice', last_ts: '2026-10-01T09:00:00Z', unseen: 2, open: 0, last_summary: 'We run migrations from deploy/migrate.sh' },
]

const bob = (on: Parameters<typeof world>[0], o: Parameters<typeof world>[1] = {}) =>
  world(on, { threads: THREADS, contacts: [ALICE, BOB], timelines: { [BOB.fingerprint]: BOB_TIMELINE }, ...o })

// One element of the pane by key.
async function el($: Engine, key: string, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const found = await p.find({ key: current(key, surface) })
  await p.unmount()
  return found
}

// The `owl ask` argvs run since `from`.
const asks = (rec: ReturnType<typeof world>, from: number) => rec.runs.slice(from).map((r) => r.argv).filter((a) => a[0] === 'ask')

// Back to the list from anywhere under the Chats tab (the request screen has no Back yet).
const toChats = async ($: Engine, surface: Surface = 'terminal') => {
  await press($, 'tab-contacts', surface)
  await press($, 'tab-chats', surface)
}

const openBobThread = async ($: Engine, context: string, surface: Surface = 'terminal') => {
  await press($, `chat-${BOB.fingerprint}`, surface)
  await press($, `thread-${context}`, surface)
}

test('Chats lists one row per person with the header counts', async ($, on) => {
  bob(on)
  await panel($)
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    expect(t).toContain('3 unseen · 1 waiting for you')
    expect((await el($, `chat-${BOB.fingerprint}`, surface))?.text).toBe('Bob')
    expect((await el($, `chat-${ALICE.fingerprint}`, surface))?.text).toBe('Alice')
    // The table's header, then a rule under it.
    const h = t.indexOf('Name')
    expect(t.slice(h, h + 4)).toEqual(['Name', 'Last message', 'When', 'New'])
    expect(t[h + 4]).toMatch(/^─+$/)
    // Bob: the open sign, needs your answer, the summary, time, 1 unseen; Alice: no open sign, 2 unseen.
    const i = t.indexOf(when(THREADS[0].last_ts))
    // The last-message cell is one Text holding the yellow prefix and the summary.
    expect(t.slice(i - 3, i + 2)).toEqual([SIGN.open, `needs your answer · ${THREADS[0].last_summary}`, 'needs your answer · ', when(THREADS[0].last_ts), ' 1 '])
    const j = t.indexOf(when(THREADS[1].last_ts), i + 1)
    expect(t.slice(j - 2, j + 2)).toEqual([' ', THREADS[1].last_summary, when(THREADS[1].last_ts), ' 2 '])
    expect(t.filter((x) => x.includes('needs your answer'))).toHaveLength(2) // the cell and its prefix
    // One rule between the two rows, after Bob's actions.
    expect(t.slice(i + 2, j).filter((x) => /^─+$/.test(x))).toHaveLength(1)
    const p = await pane($, surface)
    const all = await p.findAll({ type: 'Text' })
    expect(all.find((x) => x.text === 'needs your answer · ')?.props.color).toBe(P.wait)
    expect(all[i - 3].props.color).toBe(P.wait)
    expect(all.find((x) => x.text === 'Name')?.props).toMatchObject({ bold: true, color: P.secondary })
    expect(all.find((x) => /^─+$/.test(x.text))?.props.color).toBe(P.rule)
    await p.unmount()
    const f = await footer($, surface)
    expect(f.note).toBe('enter opens a conversation')
    expect(f.hints).toContain('filter')
    expect(f.hints).toContain('new thread')
  }
})

test('a row without unseen messages shows no count', async ($, on) => {
  world(on, { threads: [{ ...THREADS[1], unseen: 0 }] })
  await panel($)
  const t = await texts($)
  const i = t.indexOf(when(THREADS[1].last_ts))
  expect(t.slice(i - 1, i + 1)).toEqual([THREADS[1].last_summary, when(THREADS[1].last_ts)])
  expect(t).not.toContain(' 0 ')
  expect(t).toContain('0 unseen · 0 waiting for you')
})

test('no conversations at all says so', async ($, on) => {
  world(on)
  await panel($)
  const t = await texts($)
  expect(t).toContain('No conversations yet. n: New thread asks a colleague; their questions land here.')
  expect(t).toContain('0 unseen · 0 waiting for you')
})

test('the filter matches name or summary, any case, or says nothing matches', async ($, on) => {
  const rec = bob(on)
  await panel($)
  for (const surface of SURFACES) {
    await type($, 'filter', 'bob', 'change', surface)
    expect(await el($, `chat-${BOB.fingerprint}`, surface)).toBeDefined()
    expect(await el($, `chat-${ALICE.fingerprint}`, surface)).toBeUndefined()
    await type($, 'filter', 'MIGRATIONS', 'submit', surface)
    expect(await el($, `chat-${BOB.fingerprint}`, surface)).toBeUndefined()
    expect(await el($, `chat-${ALICE.fingerprint}`, surface)).toBeDefined()
    await type($, 'filter', ' nobody ', 'change', surface)
    expect(await texts($, surface)).toContain('No conversation matches "nobody".')
    expect(await el($, `chat-${ALICE.fingerprint}`, surface)).toBeUndefined()
    await type($, 'filter', '', 'change', surface)
    expect(await el($, `chat-${BOB.fingerprint}`, surface)).toBeDefined()
    expect(await el($, `chat-${ALICE.fingerprint}`, surface)).toBeDefined()
  }
})

test("a row opens the person: name, fingerprint, policy, threads grouped by context, newest first", async ($, on) => {
  bob(on, { contacts: [ALICE, { ...BOB, name: 'Bob Nolan' }] })
  await panel($)
  for (const surface of SURFACES) {
    await press($, `chat-${BOB.fingerprint}`, surface)
    expect(await activeTab($)).toBe('Chats 0')
    const t = await texts($, surface)
    expect(t).toContain('Bob Nolan')
    expect(t).toContain(`${SIGN.unknown} not probed · ${BOB.fingerprint} · policy none`)
    expect(t).toContain('Threads')
    expect(t[t.indexOf('Threads') + 1]).toBe('3')
    // The table's header, then per thread: short id, (the opener, a Button), status of its
    // newest event, message count, newest time; no second line.
    const h = t.indexOf('ID')
    expect(t.slice(h, h + 5)).toEqual(['ID', 'First message', 'Status', 'Msg', 'Last'])
    const rows = ['4e8b1d', 'c3f9a2', '72a6f0'].map((id) => t.slice(t.indexOf(id), t.indexOf(id) + 4))
    expect(rows).toEqual([
      ['4e8b1d', `${SIGN.open} needs answer`, '1', whenShort('2026-10-04T12:14:00Z')],
      ['c3f9a2', `${SIGN.wait} waiting`, '3', whenShort('2026-10-04T10:15:00Z')],
      ['72a6f0', `${SIGN.ok} answered`, '1', whenShort('2026-10-02T12:20:00Z')],
    ])
    expect(t.some((x) => x.startsWith('thread '))).toBe(false)
    // The rules of the tab row and the footer, one under the header, one between each two rows
    // and one closing the table above Show archived.
    expect(t.filter((x) => /^─+$/.test(x))).toHaveLength(6)
    expect((await el($, `thread-${C_OPEN}`, surface))?.text).toBe('Do you have decisions about communication in memory?')
    expect((await el($, `thread-${C_ASK}`, surface))?.text).toBe('What is the last commit on your side?')
    expect(t.some((x) => x.includes(C_ASK))).toBe(false)
    expect((await footer($, surface)).note).toBe('enter opens a thread')
    await press($, 'back', surface)
    expect(await el($, `chat-${ALICE.fingerprint}`, surface)).toBeDefined()
  }
})

test("a person not in the contact book is named from the thread list; a written policy shows", async ($, on) => {
  world(on, { threads: THREADS, contacts: [ALICE], timelines: {} })
  await panel($)
  await press($, `chat-${BOB.fingerprint}`)
  let t = await texts($)
  expect(t).toContain('Bob')
  expect(t).toContain(`${SIGN.unknown} not probed · ${BOB.fingerprint} · policy none`)
  await press($, 'back')
  await press($, `chat-${ALICE.fingerprint}`)
  t = await texts($)
  expect(t).toContain('Alice')
  expect(t).toContain(`${SIGN.unknown} not probed · ${ALICE.fingerprint} · policy manual`)
})

test('a fingerprint known nowhere is its own name', async ($, on) => {
  let threads = THREADS
  bob(on, { contacts: [], answers: (a) => (a === 'thread --json' ? JSON.stringify(threads) : undefined) })
  await panel($)
  await openBobThread($, C_ASK)
  expect(await texts($)).toEqual(expect.arrayContaining(['Bob ›', 'What is the last commit on your side?']))
  // The reply reloads everything; Bob has left the thread list by then.
  threads = []
  await type($, 'reply', 'Hey')
  await press($, 'send')
  expect(await texts($)).toContain(`${BOB.fingerprint} ›`)
})

test('a thread opens its messages in order, one per record, with a date rule per day', async ($, on) => {
  bob(on)
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    expect(await activeTab($)).toBe('Chats 0')
    const t = await texts($, surface)
    // The crumb, the title in bold and the short id at the right edge.
    expect(t.slice(t.indexOf('Bob ›'), t.indexOf('Bob ›') + 3)).toEqual(['Bob ›', 'What is the last commit on your side?', 'c3f9a2'])
    expect(t.some((x) => x.includes(C_ASK))).toBe(false)
    // A thin rule between two messages of one day; none next to a date rule.
    const body = t
      .slice(t.indexOf('to main agent') + 1, t.indexOf('Reply to Bob'))
      .filter((x) => x.startsWith('──') || ['What is the last commit on your side?', 'Last commit on main: a1b2c3d', 'And which branch is feature/x on now?'].includes(x))
      .map((x) => (/^─+$/.test(x) ? 'rule' : x))
    expect(body).toEqual([
      `── ${day('2026-10-03T10:08:00Z')} ──`,
      'What is the last commit on your side?',
      'rule',
      'Last commit on main: a1b2c3d',
      `── ${day('2026-10-04T10:15:00Z')} ──`,
      'And which branch is feature/x on now?',
      'rule', // above the reply block
    ])
    // The date rule is secondary text, readable on every theme (`rule` is too faint).
    const p = await pane($, surface)
    const rules = (await p.findAll({ type: 'Text' })).filter((x) => x.text.startsWith('── '))
    await p.unmount()
    expect(rules.map((x) => x.props.color)).toEqual([P.secondary, P.secondary])
    // Centred as the mock: the box around each date rule is a column, so the rule's own row
    // (justifyContent center) takes the whole width instead of its text's.
    const q = await pane($, surface)
    const around = (await q.findAll({ type: 'Box' })).filter((b) => b.text.startsWith('── ') && (b.children[0] as { props?: { justifyContent?: string } })?.props?.justifyContent === 'center')
    await q.unmount()
    expect(around.map((b) => b.props.flexDirection)).toEqual(['column', 'column'])
    // Our first ask shows its newest state; the answer is Bob's; the last ask still waits.
    // Ours carry their state in the header; Bob's answer says who drafted and approved it.
    expect(t.filter((x) => x.startsWith(`${SIGN.ok} `) || x.startsWith(`${SIGN.wait} `))).toEqual([`${SIGN.ok} answered`, `${SIGN.wait} waiting`])
    expect(t.filter((x) => /^(▸ )?\d\d:\d\d · /.test(x)).map((x) => x.replace(/^(▸ )?\d\d:\d\d · /, ''))).toEqual([
      `you · ${SIGN.ok} answered`, 'answer · drafted by claude · approved by Bob', `you · ${SIGN.wait} waiting`,
    ])
    expect(t.some((x) => x.includes('What time') || x.includes('Do you have'))).toBe(false)
    expect((await footer($, surface)).note).toBe('thread c3f9a2 with Bob')
    // b: thread → contact → chats.
    await press($, 'back', surface)
    expect(await el($, `thread-${C_ASK}`, surface)).toBeDefined()
    await press($, 'back', surface)
    expect(await el($, `chat-${BOB.fingerprint}`, surface)).toBeDefined()
  }
})

test('a thread whose newest incoming question waits opens the request screen', async ($, on) => {
  bob(on)
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_OPEN, surface)
    expect(await activeTab($)).toBe('Chats 0')
    // The request screen (hooks/request.tsx): its note and its Draft button.
    expect((await footer($, surface)).note).toBe('nothing leaves this machine until you press s')
    expect(await el($, 'draft', surface)).toBeDefined()
    await toChats($, surface)
  }
  // An answered question of Bob's opens as messages.
  await openBobThread($, C_ANSWERED)
  expect(await el($, 'draft')).toBeUndefined()
  expect(await texts($)).toEqual(expect.arrayContaining(['Bob ›', 'What time do you start tomorrow?']))
})

test('Send replies in the thread: text only, with a path, with a context file', async ($, on) => {
  const rec = bob(on)
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    let from = rec.runs.length
    await type($, 'reply', ' A develop? ', 'change', surface)
    await press($, 'send', surface)
    expect(asks(rec, from)).toEqual([['ask', '--peer', BOB.fingerprint, '--reply-to', R3, '--', 'A develop?']])

    from = rec.runs.length
    // Enter in the Reply field sends, with the path stored before it (f opens the attachments).
    await press($, 'attach', surface)
    await type($, 'path', ' src/pull.rs ', 'submit', surface)
    await type($, 'reply', 'What is going on here?', 'submit', surface)
    expect(asks(rec, from)).toEqual([['ask', '--peer', BOB.fingerprint, '--reply-to', R3, '--file', 'src/pull.rs', '--', 'What is going on here?']])

    from = rec.runs.length
    await type($, 'reply', 'Where does this error come from?', 'change', surface)
    await press($, 'attach', surface)
    await type($, 'context', ' /tmp/err.log ', 'change', surface)
    await press($, 'send', surface)
    expect(asks(rec, from)).toEqual([['ask', '--peer', BOB.fingerprint, '--reply-to', R3, '--context', '/tmp/err.log', '--', 'Where does this error come from?']])
    await toChats($, surface)
  }
})

test('path and context together go in that order', async ($, on) => {
  const rec = bob(on)
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    const from = rec.runs.length
    await press($, 'attach', surface)
    await type($, 'path', 'a.rs', 'submit', surface)
    await type($, 'context', 'b.diff', 'submit', surface)
    await type($, 'reply', 'Q', 'change', surface)
    await press($, 'send', surface)
    expect(asks(rec, from)).toEqual([['ask', '--peer', BOB.fingerprint, '--reply-to', R3, '--file', 'a.rs', '--context', 'b.diff', '--', 'Q']])
    await toChats($, surface)
  }
})

test('a reply starting with - goes after --, so owl takes it as text', async ($, on) => {
  const rec = bob(on, { answers: (a) => (a.startsWith('ask ') ? `accepted ${R3}` : undefined) })
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    const from = rec.runs.length
    await type($, 'reply', '- and the tests?', 'change', surface)
    await press($, 'send', surface)
    expect(asks(rec, from)).toEqual([['ask', '--peer', BOB.fingerprint, '--reply-to', R3, '--', '- and the tests?']])
    expect((await texts($, surface)).some((x) => x.startsWith('✓ sent 000003'))).toBe(true)
    await toChats($, surface)
  }
})

test('a list owl could not give says why instead of "No conversations yet"', async ($, on) => {
  const answers: Record<string, string | { exitCode: number; stdout?: string; stderr?: string }> = {}
  const o = { answers, missing: true }
  world(on, o)
  await panel($)
  const cases: [() => void, string][] = [
    [() => {}, '- owl could not start: is it installed and on PATH?'],
    [() => { o.missing = false; answers['thread --json'] = { exitCode: 1, stderr: 'owl: broken spool\nmore' } }, '- owl thread failed: owl: broken spool'],
    [() => { answers['thread --json'] = 'not json' }, '- owl thread printed no list'],
    [() => { answers['thread --json'] = '{"from":"x"}' }, '- owl thread printed no list'],
  ]
  // Closing and opening the pane loads the lists again.
  const reopen = async () => {
    await panel($)
    await panel($)
  }
  for (const [arrange, said] of cases) {
    arrange()
    await reopen()
    for (const surface of SURFACES) {
      const t = await texts($, surface)
      expect(t).toContain(said)
      expect(t).not.toContain('No conversations yet. n: New thread asks a colleague; their questions land here.')
    }
  }
  // `owl thread` exits 4 with `no threads` on an empty spool: that is an empty list.
  answers['thread --json'] = { exitCode: 4, stderr: 'owl: no threads' }
  await reopen()
  for (const surface of SURFACES) expect(await texts($, surface)).toContain('No conversations yet. n: New thread asks a colleague; their questions land here.')
})

test('a sent reply held for consent says whose consent it waits for', async ($, on) => {
  const held = BOB_TIMELINE.map((e) => (e.record_id === R3 ? { ...e, state: 'consent' } : e))
  bob(on, { timelines: { [BOB.fingerprint]: held }, answers: (a) => (a.startsWith('ask ') ? `accepted ${R3} — waiting for the owner's consent` : undefined) })
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    await type($, 'reply', 'Hey', 'change', surface)
    await press($, 'send', surface)
    expect(await texts($, surface)).toContain("✓ sent 000003 · waiting for Bob's consent")
    await toChats($, surface)
  }
})

test('after a sent reply the fields are empty and the note says what owl said', async ($, on) => {
  const rec = bob(on, { answers: (a) => (a.startsWith('ask ') ? `accepted ${R3} — waiting for the owner's consent` : undefined) })
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    await type($, 'reply', 'Hey', 'change', surface)
    await press($, 'attach', surface)
    await type($, 'path', 'x.rs', 'change', surface)
    await type($, 'context', 'y.log', 'change', surface)
    await press($, 'send', surface)
    // One note, under the field as the mock: the short id and where the sent record stands
    // (its state, reloaded). owl's own words are not repeated on the note line.
    const all = await texts($, surface)
    expect(all).toContain('✓ sent 000003 · waiting')
    expect(all.filter((x) => x.includes('000003') && /^[✓✗×]/.test(x))).toEqual(['✓ sent 000003 · waiting'])
    expect((await footer($, surface)).note).not.toContain('000003')
    expect((await el($, 'reply', surface))?.props.value).toBe('')
    // The field the engine shows is empty too, not only the value the mod draws.
    expect(await field($, 'reply', surface)).toBe('')
    // The attachments close, and open again empty.
    for (const key of ['path', 'context']) expect(await el($, key, surface)).toBeUndefined()
    await press($, 'attach', surface)
    for (const key of ['path', 'context']) expect((await el($, key, surface))?.props.value).toBe('')
    await press($, 'attach', surface)
    // Nothing left to send.
    const from = rec.runs.length
    await press($, 'send', surface)
    expect(asks(rec, from)).toEqual([])
    await toChats($, surface)
  }
})

test('a failed send keeps the fields and shows the error', async ($, on) => {
  bob(on, { answers: (a) => (a.startsWith('ask ') ? { exitCode: 3, stderr: 'owl: offline: no endpoint of Bob reachable' } : undefined) })
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    await type($, 'reply', 'Hey', 'change', surface)
    await press($, 'attach', surface)
    await type($, 'path', 'x.rs', 'change', surface)
    await press($, 'send', surface)
    // One note, under the field; the note line does not repeat owl's error.
    expect(await texts($, surface)).toContain('× not sent: owl: offline: no endpoint of Bob reachable · your text stays above')
    expect((await footer($, surface)).note).not.toContain('offline')
    expect((await el($, 'reply', surface))?.props.value).toBe('Hey')
    expect(await field($, 'reply', surface)).toBe('Hey')
    expect((await el($, 'path', surface))?.props.value).toBe('x.rs')
    await press($, 'attach', surface) // closing drops what was attached
    await toChats($, surface)
  }
})

test('an empty reply runs nothing and says so', async ($, on) => {
  const rec = bob(on)
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    const from = rec.runs.length
    await type($, 'reply', '   ', 'change', surface)
    await press($, 'attach', surface)
    await type($, 'path', 'x.rs', 'change', surface)
    await press($, 'send', surface)
    expect(asks(rec, from)).toEqual([])
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ type a reply first')
    await press($, 'attach', surface) // closing drops what was attached
    await toChats($, surface)
  }
})

test('v takes the text of the prompt box into the reply', async ($, on) => {
  const rec = bob(on, { prompt: 'A long question from the prompt' })
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    await press($, 'use-prompt', surface)
    expect((await el($, 'reply', surface))?.props.value).toBe('A long question from the prompt')
    const from = rec.runs.length
    await press($, 'send', surface)
    expect(asks(rec, from)).toEqual([['ask', '--peer', BOB.fingerprint, '--reply-to', R3, '--', 'A long question from the prompt']])
    await toChats($, surface)
  }
})

test('p and l take the newest @file of the prompt box into Path and Context; a typed @ is dropped', async ($, on) => {
  const rec = bob(on, { prompt: 'look at @src/old.rs and @"docs/a b.md" and @owl:msg://x ' })
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    await press($, 'attach', surface)
    await press($, 'path-focus', surface)
    expect((await el($, 'path', surface))?.props.value).toBe('docs/a b.md')
    expect((await footer($, surface)).note).toBe('✓ path docs/a b.md from the prompt box')
    await type($, 'context', '@/tmp/err.log', 'change', surface)
    const from = rec.runs.length
    await type($, 'reply', 'What here?', 'submit', surface)
    expect(asks(rec, from)).toEqual([['ask', '--peer', BOB.fingerprint, '--reply-to', R3, '--file', 'docs/a b.md', '--context', '/tmp/err.log', '--', 'What here?']])
    await toChats($, surface)
  }
})

test('p with no @file in the prompt box leaves Path as it is', async ($, on) => {
  bob(on, { prompt: 'no file here, mail a@b.c' })
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    await press($, 'attach', surface)
    await press($, 'path-focus', surface)
    expect((await el($, 'path', surface))?.props.value).toBe('')
    await press($, 'context-focus', surface)
    expect((await el($, 'context', surface))?.props.value).toBe('')
    await press($, 'attach', surface)
    await toChats($, surface)
  }
})

test('an exchange from before threads has no reply block, and says so', async ($, on) => {
  const OLD = '0199a000-0000-7000-8000-00000000000a'
  bob(on, { timelines: { [BOB.fingerprint]: [...BOB_TIMELINE, ev({ ts: '2026-09-06T23:07:43Z', kind: 'asked', dir: 'out', record_id: OLD, type: 'question', state: 'answered', text: 'How are things?' })] } })
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, OLD, surface)
    expect(await texts($, surface)).toContain('This exchange is from before threads, so it cannot be continued; n: New thread asks Bob.')
    for (const key of ['reply', 'path', 'context', 'send']) expect(await el($, key, surface)).toBeUndefined()
    await toChats($, surface)
  }
})

test('a thread only the peer asked in has the reply block, and a reply goes on from their question', async ($, on) => {
  const rec = bob(on)
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ANSWERED, surface)
    for (const key of ['reply', 'use-prompt', 'send']) expect(await el($, key, surface)).toBeDefined()
    expect((await footer($, surface)).hints).toContain('reply')
    expect((await texts($, surface)).some((x) => x.includes('cannot be continued'))).toBe(false)
    const from = rec.runs.length
    await type($, 'reply', 'At nine?', 'change', surface)
    await press($, 'send', surface)
    expect(asks(rec, from)).toEqual([['ask', '--peer', BOB.fingerprint, '--reply-to', R5, '--', 'At nine?']])
    expect(await field($, 'reply', surface)).toBe('')
    await toChats($, surface)
  }
})

test('n opens New from the list, the person and the thread', async ($, on) => {
  bob(on)
  await panel($)
  for (const surface of SURFACES) {
    await press($, 'new', surface)
    expect(await activeTab($)).toBe('New')
    await toChats($, surface)
    await press($, `chat-${BOB.fingerprint}`, surface)
    await press($, 'new', surface)
    expect(await activeTab($)).toBe('New')
    await toChats($, surface)
    await openBobThread($, C_ASK, surface)
    await press($, 'new', surface)
    expect(await activeTab($)).toBe('New')
    await toChats($, surface)
  }
})

test("k opens the person's card in the Card tab", async ($, on) => {
  const rec = bob(on)
  await panel($)
  for (const surface of SURFACES) {
    await press($, `chat-${BOB.fingerprint}`, surface)
    const from = rec.runs.length
    await press($, 'card', surface)
    expect(await activeTab($)).toBe('Card')
    expect(calls(rec, from)).toEqual(['card --json', `card ${BOB.fingerprint}`])
    await toChats($, surface)
  }
})

// The buttons of each screen: key → hotkey and label (focus buttons are pressed by the person).
async function buttons($: Engine, surface: Surface) {
  const p = await pane($, surface)
  const all = (await p.findAll({ type: 'Button' })).map((b) => [b.key, b.props.hotkey ?? '', b.text])
  await p.unmount()
  return all.filter(([k]) => !String(k).startsWith('tab-') && !['close', 'keys'].includes(String(k)))
}

test('every screen draws its buttons with their hotkeys and labels', async ($, on) => {
  bob(on)
  await panel($)
  for (const surface of SURFACES) {
    expect(await buttons($, surface)).toEqual([
      ['new', 'n', 'New thread'], ['filter-focus', 'f', 'Filter'],
      [`chat-${BOB.fingerprint}`, '', 'Bob'], ['open', '', 'Open'], ['archive', 'a', 'Archive'], ['delete', 'x', 'Delete'],
      [`chat-${ALICE.fingerprint}`, '', 'Alice'], ['archived', 'v', 'Show archived'],
    ])
    await press($, `chat-${BOB.fingerprint}`, surface)
    expect(await buttons($, surface)).toEqual([
      ['back', 'b', 'Back'], ['new', 'n', 'New thread'], ['card', 'k', 'Card'], ['policy', 'p', 'Policy'],
      [`thread-${C_OPEN}`, '', 'Do you have decisions about communication in memory?'], ['open', '', 'Open'], ['archive', 'a', 'Archive'], ['delete', 'x', 'Delete'],
      [`thread-${C_ASK}`, '', 'What is the last commit on your side?'],
      [`thread-${C_ANSWERED}`, '', 'What time do you start tomorrow?'],
      ['archived', 'v', 'Show archived'],
    ])
    await press($, `thread-${C_ASK}`, surface)
    // As the Thread artboard: the header row, the answer's Reply / Copy / Cite / Apply, then the
    // reply block (r, Send on its fill, v, f).
    expect(await buttons($, surface)).toEqual([
      ['back', 'b', 'Back'], ['whole-cite', 't', 'Cite thread'], ['whole-apply', 'u', 'Apply thread'], ['new', 'n', 'New thread'],
      [`reply-${R2}`, 'r', 'Reply'], [`copy-${R2}`, 'y', 'Copy'], [`cite-${R2}`, 'c', 'Cite'], [`apply-${R2}`, 'a', 'Apply'],
      ['reply-focus', 'r', ''], ['send', '', 'Send'], ['use-prompt', 'v', 'paste from prompt box'], ['attach', 'f', '+ Attach'],
    ])
    const hints = 'keystab move↑↓ scrollr replyenter sendv paste promptf attachp pathl local filey copyc citea applyt cite threadb back'
    expect((await footer($, surface)).hints).toBe(hints)
    // f opens the attachments: p and l, each over its field.
    await press($, 'attach', surface)
    expect((await buttons($, surface)).slice(-3)).toEqual([
      ['attach', 'f', '− Attach'], ['path-focus', 'p', 'Ask about a file in their repo'], ['context-focus', 'l', 'Attach a local file'],
    ])
    expect((await footer($, surface)).hints).toBe(hints)
    await press($, 'attach', surface)
    await toChats($, surface)
  }
})

// The engine empties a field on Enter (tests/world.ts): Enter in Reply keeps the text for Send
// and the field shows it, Enter again too; after a failed Send the fields still show their text
// and the next Send sends it again. Only a send that went out empties the field.
test('Enter in the reply sends; a failed send keeps the reply, and Enter or Send again resends it', async ($, on) => {
  const fail = { on: true }
  const rec = bob(on, { answers: (a) => (fail.on && a.startsWith('ask ') ? { exitCode: 3, stderr: 'owl: offline: no endpoint of Bob reachable' } : undefined) })
  await panel($)
  for (const surface of SURFACES) {
    fail.on = true
    await openBobThread($, C_ASK, surface)
    await press($, 'attach', surface)
    await type($, 'path', 'x.rs', 'submit', surface)
    let from = rec.runs.length
    expect(await type($, 'reply', 'Hey', 'submit', surface)).toBe('Hey')
    expect(await enter($, 'reply', surface)).toBe('Hey')
    await press($, 'send', surface)
    const argv = ['ask', '--peer', BOB.fingerprint, '--reply-to', R3, '--file', 'x.rs', '--', 'Hey']
    expect(asks(rec, from)).toEqual([argv, argv, argv])
    expect(await texts($, surface)).toContain('× not sent: owl: offline: no endpoint of Bob reachable · your text stays above')
    expect(await field($, 'reply', surface)).toBe('Hey')
    expect(await field($, 'path', surface)).toBe('x.rs')
    fail.on = false
    from = rec.runs.length
    await enter($, 'reply', surface)
    expect(asks(rec, from)).toEqual([argv])
    expect(await field($, 'reply', surface)).toBe('')
    expect(await el($, 'path', surface)).toBeUndefined()
    await toChats($, surface)
  }
})

test('the @ picker lists matching files in one keyed box, and a pick redraws the field after the path', async ($, on) => {
  bob(on, { files: ['src/main.rs', 'src/lib.rs', 'README.md'] })
  await panel($)
  await openBobThread($, C_ASK)
  await press($, 'attach')
  await type($, 'path', 'see @mai', 'change')
  await type($, 'path', 'see @main', 'change') // the files are listed by now
  const p = await pane($)
  // The list is one keyed Box, what the mod scrolls into view (`$.ui.scroll` by key) as the
  // query changes; the scroll itself moves the engine's window, which a test cannot see here.
  expect((await p.find({ key: 'picks-path' }))?.text).toContain('src/main.rs')
  expect((await p.findAll({ type: 'Button' })).filter((b) => b.key?.startsWith('pick-path-')).map((b) => b.text)).toEqual(['src/main.rs'])
  await p.unmount()
  const was = current('path')
  await press($, 'pick-path-0')
  // The field is drawn afresh under its next key, holding the path in place of the word.
  expect(await field($, 'path')).toBe('see src/main.rs')
  expect(current('path')).toBe('path~1')
  expect(was).toBe('path')
})

// `owl ask` as the CLI runs it: the ask is written to `asks/` as `waiting` before it
// is sent, so the timeline lists it while the send is in flight; an offline peer ends the send
// with exit 2 and the reservation is removed again. `release` ends every send in flight.
function inFlight(timeline: Ev[], ok: () => boolean) {
  let n = 0
  const gates: (() => void)[] = []
  const answers = async (a: string) => {
    if (!a.startsWith('ask ')) return undefined
    const id = `0199b000-0000-7000-8000-00000000000${++n}`
    const text = a.slice(a.lastIndexOf(' -- ') + 4)
    const e = ev({ ts: '2026-10-05T21:57:40Z', kind: 'asked', dir: 'out', record_id: id, context_id: C_ASK, type: 'question', state: 'waiting', text })
    timeline.push(e)
    await new Promise<void>((done) => gates.push(done))
    if (ok()) return { stdout: `accepted ${id} — waiting` }
    timeline.splice(timeline.indexOf(e), 1)
    return { exitCode: 2, stderr: 'owl: offline: no endpoint of Bob reachable (iroh: dial timeout after 10s)' }
  }
  return { answers, release: async () => { while (gates.length) gates.shift()!(); await new Promise((r) => setTimeout(r, 20)) } }
}

const bubbles = async ($: Engine, text: string) => (await texts($)).filter((x) => x === text).length

test('a reply sent while the first send is still in flight goes out once: one bubble', async ($, on) => {
  const timeline = [...BOB_TIMELINE]
  const f = inFlight(timeline, () => true)
  const rec = bob(on, { timelines: { [BOB.fingerprint]: timeline }, answers: f.answers })
  await panel($)
  await openBobThread($, C_ASK)
  const from = rec.runs.length
  await type($, 'reply', 'Lorem Ipsum?', 'change')
  // Nothing seems to happen for the 10 s of a dial: the person presses Send again, and again.
  await press($, 'send')
  await press($, 'send')
  await press($, 'send')
  await toChats($) // a reload of the timeline while the sends are in flight
  await openBobThread($, C_ASK)
  expect(await bubbles($, 'Lorem Ipsum?')).toBe(1)
  expect(asks(rec, from)).toHaveLength(1)
  await f.release()
  expect(await bubbles($, 'Lorem Ipsum?')).toBe(1)
})

test('Enter in the reply field while its send is in flight sends nothing more: one ask, one bubble', async ($, on) => {
  const timeline = [...BOB_TIMELINE]
  const f = inFlight(timeline, () => false)
  const rec = bob(on, { timelines: { [BOB.fingerprint]: timeline }, answers: f.answers })
  await panel($)
  await openBobThread($, C_ASK)
  const from = rec.runs.length
  // Enter sends; the field is drawn again under its next key with the text still in it, so
  // an Enter there (and Send beside it) would hand `send` the same text again.
  expect(await type($, 'reply', 'Another question I have\n\nLorem Ipsum?')).toBe('Another question I have\n\nLorem Ipsum?')
  await enter($, 'reply')
  await press($, 'send')
  await enter($, 'reply')
  expect(asks(rec, from)).toHaveLength(1)
  await f.release()
  expect(asks(rec, from)).toHaveLength(1)
  expect(await bubbles($, 'Another question I have\n\nLorem Ipsum?')).toBe(1)
  expect((await texts($)).some((x) => / · you · not sent · peer offline$/.test(x))).toBe(true)
})

test('a reply to an offline peer stays in the thread as not delivered, once', async ($, on) => {
  const timeline = [...BOB_TIMELINE]
  const f = inFlight(timeline, () => false)
  bob(on, { timelines: { [BOB.fingerprint]: timeline }, answers: f.answers })
  await panel($)
  await openBobThread($, C_ASK)
  await type($, 'reply', 'Lorem Ipsum?', 'change')
  await press($, 'send')
  await f.release()
  // The reservation is gone from the spool; the message is still on screen, with why.
  expect(timeline.some((e) => e.text === 'Lorem Ipsum?')).toBe(false)
  expect(await bubbles($, 'Lorem Ipsum?')).toBe(1)
  // Drawn as an unsent draft: `not sent` and why in the header (no `-` sign, no `not
  // delivered`), the text in the dashed box.
  const t = await texts($)
  expect(t.some((x) => / · you · not sent · peer offline$/.test(x))).toBe(true)
  expect(t.some((x) => x.includes('not delivered') || x.includes(`${SIGN.bad} not`))).toBe(false)
  expect(await dashed($)).toEqual(['Lorem Ipsum?'])
  // Sent again and still offline: still one bubble.
  await press($, 'send')
  await f.release()
  expect(await bubbles($, 'Lorem Ipsum?')).toBe(1)
  // Leaving the thread and coming back keeps it.
  await toChats($)
  await openBobThread($, C_ASK)
  expect(await bubbles($, 'Lorem Ipsum?')).toBe(1)
})

test('a not delivered reply sent again once the peer is back is one bubble, the record', async ($, on) => {
  const timeline = [...BOB_TIMELINE]
  let online = false
  const f = inFlight(timeline, () => online)
  bob(on, { timelines: { [BOB.fingerprint]: timeline }, answers: f.answers })
  await panel($)
  await openBobThread($, C_ASK)
  await type($, 'reply', 'Lorem Ipsum?', 'change')
  await press($, 'send')
  await f.release()
  expect(await bubbles($, 'Lorem Ipsum?')).toBe(1)
  online = true
  await press($, 'send')
  await f.release()
  expect(await bubbles($, 'Lorem Ipsum?')).toBe(1)
  expect((await texts($)).some((x) => x.includes('not sent'))).toBe(false)
  expect(await dashed($)).toEqual([])
})

// The texts of the dashed boxes on the pane (the unsent draft style).
async function dashed($: Engine) {
  const p = await pane($)
  const boxes = (await p.findAll({ type: 'Box' })).filter((b) => b.props.borderStyle === 'dashed').map((b) => b.text)
  await p.unmount()
  return boxes
}

// The engine draws nothing while a person types, so the reply was last drawn with '' and a
// redraw with '' would leave the typed text in the field (tests/world.ts: `value` is put back
// only when it differs, or under a new key). After a send that went out it comes under a new
// key, empty; after a failed one it keeps its key and its text.
test('Send without Enter empties the field the engine shows, a failed Send keeps it', async ($, on) => {
  const fail = { on: false }
  bob(on, { answers: (a) => (fail.on && a.startsWith('ask ') ? { exitCode: 3, stderr: 'owl: offline' } : undefined) })
  await panel($)
  for (const surface of SURFACES) {
    await openBobThread($, C_ASK, surface)
    const p = await pane($, surface)
    const reply = async () => (await p.findAll({ type: 'Input' })).find((i) => (i.key ?? '').split('~')[0] === 'reply')
    const before = (await reply())?.key
    await p.input({ key: before ?? 'reply', text: 'Hey', kind: 'change' })
    fail.on = true
    await p.press({ key: 'send' })
    expect((await reply())?.key).toBe(before)
    expect((await reply())?.props.value).toBe('Hey')
    fail.on = false
    await p.press({ key: 'send' })
    expect((await reply())?.key).not.toBe(before)
    expect((await reply())?.props.value).toBe('')
    await p.unmount()
    await toChats($, surface)
  }
})

// The text typed in one thread is no draft of another: entering a different thread draws the
// reply field afresh (a new key), empty, with no attachments and no note under it; back in the
// thread it was typed in, without visiting another, it keeps its text.
test('another thread shows an empty reply field, the same thread keeps its text', async ($, on) => {
  bob(on)
  await panel($)
  for (const surface of SURFACES) {
    const p = await pane($, surface)
    const reply = async () => (await p.findAll({ type: 'Input' })).find((i) => (i.key ?? '').split('~')[0] === 'reply')
    const shown = async (k: string) => (await p.findAll({ type: 'Input' })).some((i) => (i.key ?? '').split('~')[0] === k)
    await p.press({ key: `chat-${BOB.fingerprint}` })
    await p.press({ key: `thread-${C_ASK}` })
    const before = (await reply())?.key
    await p.input({ key: before ?? 'reply', text: 'Hey', kind: 'change' })
    await p.press({ key: 'attach' })
    await p.input({ key: (await p.findAll({ type: 'Input' })).find((i) => (i.key ?? '').split('~')[0] === 'path')?.key ?? 'path', text: 'src/x.rs', kind: 'change' })
    // Back and into the same thread: the text and the attachment stay.
    await p.press({ key: 'back' })
    await p.press({ key: `thread-${C_ASK}` })
    expect((await reply())?.props.value).toBe('Hey')
    expect(await shown('path')).toBe(true)
    // Back and into another thread: empty, under a new key, no attachments.
    await p.press({ key: 'back' })
    await p.press({ key: `thread-${C_ANSWERED}` })
    expect((await reply())?.key).not.toBe(before)
    expect((await reply())?.props.value).toBe('')
    expect(await shown('path')).toBe(false)
    // And back in the first thread the text is gone too: the draft was left behind.
    await p.press({ key: 'back' })
    await p.press({ key: `thread-${C_ASK}` })
    expect((await reply())?.props.value).toBe('')
    await p.unmount()
    await toChats($, surface)
  }
})
