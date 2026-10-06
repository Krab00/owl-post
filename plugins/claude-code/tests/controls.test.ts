// The controls for the archive/delete/undo commands on Chats and on one person's threads (hooks/chats.tsx):
// the selected row, archive / unarchive, delete with its confirm, the footer's Undo, Show
// archived; and presence with Ping now (hooks/contacts.tsx) and the daemon's last pull on
// Chats, Contacts, a person's screen and Settings. `owl` is the world of tests/world.ts, which
// here keeps the archive and the trash as the real CLI does.
import { expect, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On } from 'claude-code'
import type { Ev, Presence, Thread } from '../hooks/lib'
import { connectAddr, rowMove } from '../hooks/lib'
import { P, SIGN, ago, seen as seenAt, when } from '../hooks/ui'
import { ALICE, BOB, SURFACES, calls, footer, pane, panel, press, texts, type, world, type Surface } from './world'

const [FA, FB] = [ALICE.fingerprint, BOB.fingerprint]
const TB: Thread = { from: FB, from_name: 'Bob', last_ts: '2026-10-04T12:14:00Z', unseen: 1, open: 1, last_summary: 'Do you have decisions?' }
const TA: Thread = { from: FA, from_name: 'Alice', last_ts: '2026-10-01T09:00:00Z', unseen: 0, open: 0, last_summary: 'Migrations after the build' }

const C1 = '0199ae10-2b3c-7d4e-8f50-6a7b8c4e8b1d'
const C2 = '0199a8f0-1a2b-7c3d-8e4f-5a6b7cc3f9a2'
const R1 = '0199ae10-2b3c-7d4e-8f50-000000000001'
const R2 = '0199a8f0-1a2b-7c3d-8e4f-000000000002'
const R3 = '0199a8f0-1a2b-7c3d-8e4f-000000000003' // a record without a context id: its own thread

const ev = (record_id: string, context_id: string | null, ts: string, text: string): Ev => ({
  ts, kind: 'asked', dir: 'out', record_id, context_id, type: 'question', state: 'waiting', project: 'demo', path: '-', text,
})
const E1 = ev(R1, C1, '2026-10-04T12:14:00Z', 'First thread')
const E2 = ev(R2, C2, '2026-10-03T10:00:00Z', 'Second thread')
const E3 = ev(R3, null, '2026-10-02T10:00:00Z', 'No context')

// Every Button but the tab row and the footer's `h`: key, hotkey, label.
async function buttons($: Engine, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const all = (await p.findAll({ type: 'Button' })).map((b) => [b.key, b.props.hotkey ?? '', b.text])
  await p.unmount()
  return all.filter(([k]) => !String(k).startsWith('tab-') && !['close', 'keys'].includes(String(k)))
}

// The count next to Show archived ('' when none is drawn).
async function archivedCount($: Engine, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const t = await p.find({ key: 'archived-count' })
  await p.unmount()
  return t?.text ?? ''
}

// One Text of the pane by its exact text.
async function textEl($: Engine, text: string, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const t = (await p.findAll({ type: 'Text' })).find((x) => x.text === text)
  await p.unmount()
  return t
}

// Moves the pane's focus ring as the person's Tab does.
const ring = ($: Engine, element?: string) =>
  $.ui.focus({ component: 'Pane', requestId: 'owlpost', element, plugin: element ? 'owlpost' : undefined, origin: { kind: 'person' } })

// A spool of two people (Bob with three threads) whose archive and trash behave as
// `owl archive|unarchive|delete|undo` do: a mark hides a chat or a thread from `owl thread`,
// a delete moves the records away and undo brings the newest batch back. `fail` makes every
// mutating command exit 1.
function spool(on: On, o: { fail?: boolean } = {}) {
  const st = { chats: new Set<string>(), threads: new Set<string>(), trash: [] as { peer: string; context?: string; evs: Ev[]; row?: Thread }[] }
  const timeline: Record<string, Ev[]> = { [FB]: [E3, E2, E1], [FA]: [] }
  let rows = [TB, TA]
  const name = (fp: string) => (fp === FB ? 'Bob' : 'Alice')
  const rec = world(on, {
    contacts: [ALICE, BOB],
    answers: (a) => {
      const shown = rows.filter((t) => !st.chats.has(t.from))
      const hidden = rows.filter((t) => st.chats.has(t.from))
      if (a === 'thread --json') return shown.length ? JSON.stringify(shown) : { exitCode: 4, stderr: 'owl: no threads' }
      if (a === 'thread --archived --json') return hidden.length ? JSON.stringify(hidden) : { exitCode: 4, stderr: 'owl: no threads' }
      let m = /^thread (?:--archived --json -- )?(owl:\S+)( --json)?$/.exec(a)
      if (m) {
        const archived = a.includes('--archived')
        return JSON.stringify(timeline[m[1]].filter((e) => st.threads.has(`${m![1]} ${e.context_id}`) === archived))
      }
      m = /^(archive|unarchive|delete) (?:--context=(\S+) )?-- (\S+)$/.exec(a)
      if (m) {
        if (o.fail) return { exitCode: 1, stderr: `owl: no thread ${m[2]} with ${name(m[3])}` }
        const [, verb, context, peer] = m
        const key = context ? `${peer} ${context}` : peer
        const marks = context ? st.threads : st.chats
        const what = context ? `thread ${context} with ${name(peer)}` : name(peer)
        if (verb === 'archive') return (marks.add(key), `archived ${what}`)
        if (verb === 'unarchive') return marks.delete(key) ? `unarchived ${what}` : { exitCode: 1, stderr: `owl: ${what} is not archived` }
        const evs = timeline[peer].filter((e) => !context || e.context_id === context)
        timeline[peer] = timeline[peer].filter((e) => !evs.includes(e))
        const row = context ? undefined : rows.find((t) => t.from === peer)
        if (row) rows = rows.filter((t) => t !== row)
        st.trash.push({ peer, context, evs, row })
        return `deleted ${evs.length} records of ${what} — owl undo restores them`
      }
      if (a === 'undo') {
        const b = st.trash.pop()
        if (!b) return { exitCode: 4, stderr: 'owl: nothing to undo' }
        timeline[b.peer] = [...timeline[b.peer], ...b.evs]
        if (b.row) rows = [...rows, b.row]
        return `restored ${b.evs.length} records of ${name(b.peer)}`
      }
      return undefined
    },
  })
  return { rec, st, timeline }
}

// The chat rows as drawn: [the selection sign, the name] per row.
async function chatRows($: Engine, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const out: string[][] = []
  for (const b of await p.findAll({ type: 'Button' })) if (b.key?.startsWith('chat-')) out.push([b.key])
  const t = (await p.findAll({ type: 'Text' })).map((x) => x.text)
  await p.unmount()
  return { keys: out.map(([k]) => k), marks: t.filter((x) => x === SIGN.selected).length, t }
}

// Texts without the note line (`✓ …` after an action, else the default note).
const withoutNote = (t: string[]) => t.filter((x) => !x.startsWith('✓') && x !== 'enter opens a conversation')

// ---------- the selected chat row, the unseen block

test('the first chat row is selected (▸) until the ring moves; the unseen count is an inverse block', async ($, on) => {
  world(on, { contacts: [ALICE, BOB], threads: [TB, { ...TA, unseen: 2 }] })
  await panel($)
  for (const surface of SURFACES) {
    const p = await pane($, surface)
    const all = await p.findAll({ type: 'Text' })
    await p.unmount()
    const t = all.map((x) => x.text)
    // ▸ before Bob only; Alice's row has a blank in its place.
    expect(t.filter((x) => x === SIGN.selected)).toHaveLength(1)
    expect(t[t.indexOf(SIGN.selected) + 1]).toBe(SIGN.unknown) // Bob's presence sign follows his (button) name
    const badges = all.filter((x) => x.props.inverse && !x.text.startsWith(' Chats')) // the active tab is inverse too
    expect(badges.map((x) => [x.text, x.props.color, x.props.bold])).toEqual([[' 1 ', P.accent, true], [' 2 ', P.accent, true]])
    expect(await buttons($, surface)).toEqual(expect.arrayContaining([['archive', 'a', 'Archive'], ['delete', 'x', 'Delete']]))
    // The actions sit under the selected row: right after Bob's button.
    const keys = (await buttons($, surface)).map(([k]) => k)
    expect(keys.slice(keys.indexOf(`chat-${FB}`), keys.indexOf(`chat-${FB}`) + 4)).toEqual([`chat-${FB}`, 'open', 'archive', 'delete'])
    // The ring on Alice's row selects her: the actions move under her, ▸ too.
    await ring($, `chat-${FA}`)
    const moved = (await buttons($, surface)).map(([k]) => k)
    expect(moved.slice(moved.indexOf(`chat-${FA}`), moved.indexOf(`chat-${FA}`) + 4)).toEqual([`chat-${FA}`, 'open', 'archive', 'delete'])
    expect(moved.indexOf('archive')).toBeGreaterThan(moved.indexOf(`chat-${FB}`) + 1)
    // Off any row (onto Archive) the selection stays where it was.
    await ring($, 'archive')
    const kept = (await buttons($, surface)).map(([k]) => k)
    expect(kept.indexOf('archive')).toBe(kept.indexOf(`chat-${FA}`) + 2)
    await ring($, `chat-${FB}`)
  }
})

test('the selected row has the selected background, the others none', async ($, on) => {
  world(on, { contacts: [ALICE, BOB], threads: [TB, TA] })
  await panel($)
  const p = await pane($)
  const boxes = (await p.findAll({ type: 'Box' })).filter((b) => b.key === FB || b.key === FA).map((b) => [b.key, b.props.backgroundColor])
  await p.unmount()
  expect(boxes).toEqual([[FB, P.selected], [FA, undefined]])
})

// ---------- RowMove, the key the ring goes back to after a move onto a row

test('rowMove: a move onto another chat, thread or harness row returns that row key', () => {
  expect(rowMove(`chat-${FB}`, `chat-${FA}`)).toBe(`chat-${FA}`)
  expect(rowMove('', `thread-${C1}`)).toBe(`thread-${C1}`)
  expect(rowMove('harness-claude', 'harness-codex')).toBe('harness-codex')
  expect(rowMove('', 'harness-edit-x')).toBe('harness-edit-x') // a name that starts with edit-
})

test('rowMove: the same key, the actions, the Inputs, tabs and other rows return ""', () => {
  expect(rowMove(`chat-${FA}`, `chat-${FA}`)).toBe('')
  for (const k of ['archive', 'delete', 'delete-confirm', 'delete-cancel', 'harness-edit', 'harness-remove', 'harness-scan', 'harness-add', 'harness-name', 'harness-cmd', 'harness-cmd-edit~2', 'tab-chats', `cite-${R1}`, `row-${FA}`])
    expect(rowMove(`chat-${FB}`, k)).toBe('')
  // A harness named `edit` would be drawn under `harness-edit` — the Edit action's key: the
  // action wins and that one row gets no refocus.
  expect(rowMove('harness-claude', 'harness-edit')).toBe('')
})

// ---------- archive, unarchive, delete, undo on a chat

test('a: archives the selected chat; it leaves the list, Alice stays as she was; u: Undo unarchives it', async ($, on) => {
  const { rec, st } = spool(on)
  await panel($)
  for (const surface of SURFACES) {
    const before = (await chatRows($, surface)).t
    let from = rec.runs.length
    await press($, 'archive', surface)
    expect(calls(rec, from)).toEqual([`archive -- ${FB}`])
    expect([...st.chats]).toEqual([FB])
    const after = await chatRows($, surface)
    expect(after.keys).toEqual([`chat-${FA}`])
    // Alice's row is drawn as before (now selected), and the archived count is 1.
    expect(after.t).toEqual(expect.arrayContaining([when(TA.last_ts), TA.last_summary]))
    expect(await archivedCount($, surface)).toBe('1')
    // The note line holds the note and the Undo button (the test reads both as one text).
    expect((await footer($, surface)).note).toBe('✓ archived Bob ·Undo')
    expect(await buttons($, surface)).toContainEqual(['undo', 'u', 'Undo'])
    from = rec.runs.length
    await press($, 'undo', surface)
    expect(calls(rec, from)).toEqual([`unarchive -- ${FB}`])
    expect(st.chats.size).toBe(0)
    expect(withoutNote((await chatRows($, surface)).t)).toEqual(withoutNote(before))
    expect((await footer($, surface)).note).toBe('✓ unarchived Bob')
    expect((await buttons($, surface)).map(([k]) => k)).not.toContain('undo')
  }
})

test('x: Delete asks first; Cancel runs nothing; the confirm deletes and u: Undo restores the batch', async ($, on) => {
  const { rec, st } = spool(on)
  await panel($)
  for (const surface of SURFACES) {
    const before = (await chatRows($, surface)).t
    const from = rec.runs.length
    await press($, 'delete', surface)
    // As the mock: the question in place of the actions, y: Delete, n: Keep, and that Bob keeps his.
    let b = await buttons($, surface)
    expect(b).toContainEqual(['delete-confirm', 'y', 'Delete'])
    expect(b).toContainEqual(['delete-cancel', 'n', 'Keep'])
    expect(b.map(([k]) => k)).not.toContain('delete')
    expect(await texts($, surface)).toEqual(expect.arrayContaining(['Delete this conversation from this machine?', 'Bob keeps their copy.']))
    await press($, 'delete-cancel', surface)
    b = await buttons($, surface)
    expect(b).toContainEqual(['delete', 'x', 'Delete'])
    expect(b.map(([k]) => k)).not.toContain('delete-confirm')
    expect(calls(rec, from)).toEqual([])
    await press($, 'delete', surface)
    await press($, 'delete-confirm', surface)
    expect(calls(rec, from)).toEqual([`delete -- ${FB}`])
    expect(st.trash).toHaveLength(1)
    expect((await chatRows($, surface)).keys).toEqual([`chat-${FA}`])
    expect((await footer($, surface)).note).toBe('✓ deleted 3 records of Bob — owl undo restores them ·Undo')
    // The confirm is gone with the row; Alice's row offers Delete again.
    expect(await buttons($, surface)).toContainEqual(['delete', 'x', 'Delete'])
    await press($, 'undo', surface)
    expect(calls(rec, from)).toEqual([`delete -- ${FB}`, 'undo'])
    expect(st.trash).toHaveLength(0)
    expect((await footer($, surface)).note).toBe('✓ restored 3 records of Bob')
    // Bob is back (the spool appends him last; owl sorts by time, this world does not): the
    // same texts but the note.
    expect(new Set(withoutNote((await chatRows($, surface)).t))).toEqual(new Set(withoutNote(before)))
    await ring($, `chat-${FB}`)
  }
})

test('a delete waiting for its confirm is dropped when the selection moves, the filter changes or archived is shown', async ($, on) => {
  const { rec } = spool(on)
  await panel($)
  const from = rec.runs.length
  const asked = async () => (await buttons($)).map(([k]) => k).includes('delete-confirm')
  await press($, 'delete')
  expect(await asked()).toBe(true)
  // The ring onto Alice: her row has Delete, no confirm; back on Bob the ask is not there either.
  await ring($, `chat-${FA}`)
  expect(await asked()).toBe(false)
  await ring($, `chat-${FB}`)
  expect(await asked()).toBe(false)
  await press($, 'delete')
  await type($, 'filter', 'b', 'change')
  expect(await asked()).toBe(false)
  await type($, 'filter', '', 'change')
  await press($, 'delete')
  await type($, 'filter', '', 'submit')
  expect(await asked()).toBe(false)
  await press($, 'delete')
  await press($, 'archived')
  await press($, 'archived')
  expect(await asked()).toBe(false)
  expect(calls(rec, from)).toEqual([])
})

test('a refused archive says why and offers no Undo', async ($, on) => {
  const { rec, st } = spool(on, { fail: true })
  await panel($)
  const from = rec.runs.length
  await press($, 'archive')
  expect(calls(rec, from)).toEqual([`archive -- ${FB}`])
  expect(st.chats.size).toBe(0)
  expect((await footer($)).note).toBe('✗ owl: no thread undefined with Bob')
  expect((await buttons($)).map(([k]) => k)).not.toContain('undo')
  await press($, 'delete')
  await press($, 'delete-confirm')
  expect(calls(rec, from)).toEqual([`archive -- ${FB}`, `delete -- ${FB}`])
  // The refused delete closes its confirm: Bob's row offers Delete again, no Undo.
  let b = (await buttons($)).map(([k]) => k)
  expect(b).not.toContain('undo')
  expect(b).toContain('delete')
  expect(b).not.toContain('delete-confirm')
  // While a delete waits for its confirm, Archive is not drawn; n: Keep closes the confirm.
  await press($, 'delete')
  expect((await buttons($)).map(([k]) => k)).not.toContain('archive')
  await press($, 'delete-cancel')
  b = (await buttons($)).map(([k]) => k)
  expect(b).toContain('delete')
  expect(b).not.toContain('delete-confirm')
})

test('Undo is gone after a route change', async ($, on) => {
  spool(on)
  await panel($)
  await press($, 'archive')
  expect((await buttons($)).map(([k]) => k)).toContain('undo')
  await press($, 'tab-contacts')
  await press($, 'tab-chats')
  expect((await buttons($)).map(([k]) => k)).not.toContain('undo')
  await press($, 'archived')
  await press($, 'archive') // unarchive Bob again
  await press($, 'archived')
})

test('v: Show archived lists the archived chats with Unarchive; Undo of an unarchive archives again; v hides them', async ($, on) => {
  const { rec, st } = spool(on)
  await panel($)
  for (const surface of SURFACES) {
    expect(await buttons($, surface)).toContainEqual(['archived', 'v', 'Show archived'])
    expect(await archivedCount($, surface)).toBe('0')
    await press($, 'archive', surface)
    await press($, 'archived', surface)
    let t = await texts($, surface)
    expect(t).toContain('Archived')
    expect(await archivedCount($, surface)).toBe('')
    expect((await chatRows($, surface)).keys).toEqual([`chat-${FB}`])
    const b = await buttons($, surface)
    expect(b).toContainEqual(['archive', 'a', 'Unarchive'])
    expect(b).toContainEqual(['archived', 'v', 'Hide archived'])
    expect(await footer($, surface).then((f) => f.hints)).toContain('unarchive')
    const from = rec.runs.length
    await press($, 'archive', surface)
    expect(calls(rec, from)).toEqual([`unarchive -- ${FB}`])
    expect(await texts($, surface)).toContain('No archived conversations.')
    await press($, 'undo', surface)
    expect(calls(rec, from)).toEqual([`unarchive -- ${FB}`, `archive -- ${FB}`])
    expect([...st.chats]).toEqual([FB])
    await press($, 'archive', surface)
    await press($, 'archived', surface)
    t = await texts($, surface)
    expect(t).not.toContain('Archived')
    expect((await chatRows($, surface)).keys).toEqual([`chat-${FB}`, `chat-${FA}`])
  }
})

test('archived chats owl could not list say why, there only', async ($, on) => {
  world(on, { contacts: [BOB], threads: [TB], answers: { 'thread --archived --json': { exitCode: 1, stderr: 'owl: archive.json unreadable' } } })
  await panel($)
  expect(await texts($)).not.toContain('- owl thread --archived failed: owl: archive.json unreadable')
  await press($, 'archived')
  expect(await texts($)).toContain('- owl thread --archived failed: owl: archive.json unreadable')
  await press($, 'archived')
})

test("a chat whose fingerprint starts with - reaches owl after --", async ($, on) => {
  const odd = { ...TB, from: '-x' }
  const rec = world(on, { contacts: [], threads: [odd], answers: (a) => (/^(archive|delete) /.test(a) ? 'ok' : undefined) })
  await panel($)
  const from = rec.runs.length
  await press($, 'archive')
  await press($, 'delete')
  await press($, 'delete-confirm')
  expect(calls(rec, from)).toEqual(['archive -- -x', 'delete -- -x'])
})

// ---------- on one person's threads

test("a person's threads: the first is selected with a / x; the ring moves them; a thread without a context id has none", async ($, on) => {
  spool(on)
  await panel($)
  for (const surface of SURFACES) {
    await press($, `chat-${FB}`, surface)
    let keys = (await buttons($, surface)).map(([k]) => k)
    expect(keys).toEqual(['back', 'new', 'card', 'policy', `thread-${C1}`, 'open', 'archive', 'delete', `thread-${C2}`, `thread-${R3}`, 'archived'])
    expect((await texts($, surface)).filter((x) => x === SIGN.selected)).toHaveLength(1)
    await ring($, `thread-${C2}`)
    keys = (await buttons($, surface)).map(([k]) => k)
    expect(keys.slice(keys.indexOf(`thread-${C2}`), keys.indexOf(`thread-${C2}`) + 4)).toEqual([`thread-${C2}`, 'open', 'archive', 'delete'])
    await ring($, `thread-${R3}`)
    keys = (await buttons($, surface)).map(([k]) => k)
    expect(keys).not.toContain('archive')
    expect(keys).not.toContain('delete')
    expect((await texts($, surface)).filter((x) => x === SIGN.selected)).toHaveLength(1)
    // The selection stays with the thread it was on, also after leaving and coming back.
    await press($, 'back', surface)
    await press($, `chat-${FB}`, surface)
    expect((await buttons($, surface)).map(([k]) => k)).not.toContain('archive')
    await ring($, `thread-${C1}`)
    await press($, 'back', surface)
  }
})

test('a thread is archived by its full context id; Show archived lists it with Unarchive, opens it; Undo brings it back', async ($, on) => {
  const { rec, st } = spool(on)
  await panel($)
  await press($, `chat-${FB}`)
  let from = rec.runs.length
  await press($, 'archive')
  expect(calls(rec, from)).toEqual([`archive --context=${C1} -- ${FB}`])
  expect([...st.threads]).toEqual([`${FB} ${C1}`])
  let keys = (await buttons($)).map(([k]) => k)
  expect(keys.filter((k) => String(k).startsWith('thread-'))).toEqual([`thread-${C2}`, `thread-${R3}`])
  let t = await texts($)
  expect(t[t.indexOf('Threads') + 1]).toBe('2')
  expect(await archivedCount($)).toBe('1')
  expect((await footer($)).note).toBe(`✓ archived thread 4e8b1d with Bob ·Undo`)
  // Show archived: the timeline comes from `owl thread --archived`.
  from = rec.runs.length
  await press($, 'archived')
  expect(rec.runs.slice(from).map((r) => r.args)).toContain(`thread --archived --json -- ${FB}`)
  t = await texts($)
  expect(t[t.indexOf('Archived threads') + 1]).toBe('1')
  keys = (await buttons($)).map(([k]) => k)
  expect(keys).toEqual(['back', 'new', 'card', 'policy', `thread-${C1}`, 'open', 'archive', 'delete', 'archived'])
  expect(await archivedCount($)).toBe('')
  expect(await buttons($)).toContainEqual(['archive', 'a', 'Unarchive'])
  // The archived thread opens with its messages, and Back returns to the archived list.
  await press($, `thread-${C1}`)
  expect(await texts($)).toEqual(expect.arrayContaining(['Bob ›', 'First thread']))
  await press($, 'back')
  expect(await texts($)).toContain('Archived threads')
  from = rec.runs.length
  await press($, 'archive')
  expect(calls(rec, from)).toEqual([`unarchive --context=${C1} -- ${FB}`])
  expect(st.threads.size).toBe(0)
  await press($, 'undo')
  expect(calls(rec, from)).toEqual([`unarchive --context=${C1} -- ${FB}`, `archive --context=${C1} -- ${FB}`])
  expect([...st.threads]).toEqual([`${FB} ${C1}`])
  // Leaving the person turns Show archived off: back in, the other threads show.
  await press($, 'back')
  await press($, `chat-${FB}`)
  expect(await texts($)).toContain('Threads')
  await press($, 'archive') // the remaining first thread, C2
  await press($, 'archived')
  await press($, 'archive') // unarchive C1 (newest first)
  await press($, 'archive')
  await press($, 'archived')
})

test('Keys and back keep the archived threads shown; another tab turns them off', async ($, on) => {
  const { st } = spool(on)
  st.threads.add(`${FB} ${C1}`)
  await panel($)
  await press($, `chat-${FB}`)
  await press($, 'archived')
  expect(await texts($)).toContain('Archived threads')
  await press($, 'keys')
  await press($, 'back')
  expect(await texts($)).toContain('Archived threads')
  expect((await buttons($)).map(([k]) => k)).toContain(`thread-${C1}`)
  await press($, 'tab-contacts')
  await press($, 'tab-chats')
  await press($, `chat-${FB}`)
  expect(await texts($)).toContain('Threads')
  expect((await buttons($)).map(([k]) => k)).not.toContain(`thread-${C1}`)
})

test('a thread delete asks first, names the thread, deletes by context id and Undo restores it', async ($, on) => {
  const { rec, st, timeline } = spool(on)
  await panel($)
  await press($, `chat-${FB}`)
  await ring($, `thread-${C2}`)
  const from = rec.runs.length
  await press($, 'delete')
  expect(await buttons($)).toContainEqual(['delete-confirm', 'y', 'Delete'])
  expect(await texts($)).toContain('Delete this thread from this machine?')
  await ring($, `thread-${C1}`)
  expect((await buttons($)).map(([k]) => k)).not.toContain('delete-confirm')
  await ring($, `thread-${C2}`)
  await press($, 'delete')
  await press($, 'delete-confirm')
  expect(calls(rec, from)).toEqual([`delete --context=${C2} -- ${FB}`])
  expect(timeline[FB]).toEqual([E3, E1])
  expect((await buttons($)).map(([k]) => k).filter((k) => String(k).startsWith('thread-'))).toEqual([`thread-${C1}`, `thread-${R3}`])
  await press($, 'undo')
  expect(calls(rec, from)).toEqual([`delete --context=${C2} -- ${FB}`, 'undo'])
  expect(st.trash).toHaveLength(0)
  expect(new Set(timeline[FB])).toEqual(new Set([E1, E2, E3]))
})

// ---------- presence, Ping now, the daemon's last pull

const NOW = Date.now()
const iso = (secsAgo: number) => new Date(NOW - secsAgo * 1000).toISOString().replace(/\.\d+Z$/, 'Z')
const CARL = { ...ALICE, name: 'Carl', fingerprint: 'owl:carlcarlcarlcarl' }
const DORA = { ...ALICE, name: 'Dora', fingerprint: 'owl:doradoradoradora' }
const PRESENCE: Presence = {
  last_pull_at: iso(150),
  open_asks: 2,
  peers_probed: 1,
  peers: [
    { fingerprint: FA, name: 'Alice', online: true, probed_at: iso(3_700), last_seen: iso(3_700) },
    { fingerprint: FB, name: 'Bob', online: false, probed_at: iso(150), last_seen: '2026-09-28T10:00:00Z' },
    { fingerprint: CARL.fingerprint, name: 'Carl', online: false, probed_at: iso(150), last_seen: null },
    { fingerprint: DORA.fingerprint, name: 'Dora', online: null, probed_at: null, last_seen: null },
  ],
}
const SUMMARY = 'last pull 2m ago · 2 open asks · 1 peer probed'

test('Contacts: each row shows presence (online, last seen, offline, not probed) and the summary line', async ($, on) => {
  world(on, { contacts: [ALICE, BOB, CARL, DORA], presence: PRESENCE })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    expect(t).toContain(SUMMARY)
    // In row order: the sign before the name, the Seen cell ("last seen" is the column's name,
    // so the cell keeps the time), then that row's policy.
    const seen = [`${SIGN.online} online`, `${SIGN.offline} ${seenAt('2026-09-28T10:00:00Z')}`, `${SIGN.offline} offline`, `${SIGN.unknown} not probed`]
    const cells = seen.map((x) => t.indexOf(x))
    expect(cells.every((i, n) => i > 0 && (n === 0 || i > cells[n - 1]))).toBe(true)
    expect(cells.map((i) => t[i - 1])).toEqual([SIGN.online, SIGN.offline, SIGN.offline, SIGN.unknown])
    // The cell is secondary words with the sign in its colour (as the mock); then the policy word
    // in its own colour, no sign.
    expect(cells.map((i) => t[i + 1])).toEqual([SIGN.online, SIGN.offline, SIGN.offline, SIGN.unknown])
    expect(cells.map((i) => t[i + 2])).toEqual(['manual', 'none', 'manual', 'manual'])
    const p = await pane($, surface)
    const all = await p.findAll({ type: 'Text' })
    await p.unmount()
    expect(cells.map((i) => all[i]!.props.color)).toEqual([P.secondary, P.secondary, P.secondary, P.secondary])
    expect(cells.map((i) => all[i + 1]!.props.color)).toEqual([P.ok, P.bad, P.bad, P.secondary])
    expect(cells.map((i) => all[i + 2]!.props.color)).toEqual([P.wait, P.secondary, P.wait, P.wait])
    // The legend under the header says what the signs mean.
    expect(t).toContain(`${SIGN.online} online  ${SIGN.offline} offline  ${SIGN.unknown} unknown`)
    expect((await textEl($, SUMMARY, surface))?.props.color).toBe(P.secondary)
  }
})

test('without a daemon status the rows say not probed and the summary line says why, in red', async ($, on) => {
  world(on, { contacts: [ALICE] })
  await panel($)
  await press($, 'tab-contacts')
  const why = 'owl: no daemon status yet (daemon.status missing) — is the daemon running? see owl install'
  const t = await texts($)
  // The note line says why, its sign in red.
  expect((await footer($)).note).toBe(`✗ ${why}`)
  expect(t[t.indexOf(`${SIGN.unknown} not probed`) - 1]).toBe(SIGN.unknown)
  expect((await textEl($, '✗'))?.props.color).toBe(P.bad)
})

test('a status that is no presence object is said as such', async ($, on) => {
  world(on, { contacts: [ALICE], answers: { 'presence --json': '{"peers": 5}' } })
  await panel($)
  await press($, 'tab-contacts')
  expect((await footer($)).note).toBe('✗ owl presence printed no status')
})

test('Chats: a dot per person, the note says the last pull; a person shows online and when probed', async ($, on) => {
  world(on, { contacts: [ALICE, BOB], threads: [TB, TA], presence: PRESENCE })
  await panel($)
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    // Each row: ▸, the presence sign, the needs-your-answer column, ...; Bob is offline,
    // Alice is online.
    expect(t.slice(t.indexOf('▸'), t.indexOf('▸') + 3)).toEqual(['▸', SIGN.offline, SIGN.open])
    expect(t.slice(t.indexOf(TA.last_summary) - 2, t.indexOf(TA.last_summary))).toEqual([SIGN.online, ' '])
    expect((await footer($, surface)).note).toBe('last pull 2m ago')
    await press($, `chat-${FA}`, surface)
    // One secondary line, the presence in its colour first (as the mock).
    const c = await texts($, surface)
    expect(c).toEqual(expect.arrayContaining([`${SIGN.online} online · probed 1h ago · ${FA} · policy manual`, `${SIGN.online} online`]))
    await press($, 'back', surface)
    await press($, `chat-${FB}`, surface)
    expect(await texts($, surface)).toContain(`${SIGN.offline} last seen ${seenAt('2026-09-28T10:00:00Z')} · probed 2m ago · ${FB} · policy none`)
    await press($, 'back', surface)
  }
})

test('Settings: the daemon section shows the last pull', async ($, on) => {
  world(on, { presence: PRESENCE })
  await panel($)
  await press($, 'tab-settings')
  // The Last pull row as the mock: the label, then the summary without its `last pull`.
  const t = await texts($)
  const row = SUMMARY.replace(/^last pull /, '')
  expect(t[t.indexOf(row) - 1]).toBe('Last pull')
  expect(t.indexOf(row)).toBeGreaterThan(t.indexOf('Daemon'))
  expect(t.indexOf(row)).toBeLessThan(t.indexOf('Doctor'))
})

test('Settings: s scans PATH with owl harness scan; the note is its first line and the list is read again', async ($, on) => {
  let scanned = false
  const rec = world(on, {
    answers: (a) =>
      a === 'harness scan'
        ? ((scanned = true), 'scan: claude, codex on PATH · opencode not found\nadded codex')
        : a === 'harness list --json'
          ? JSON.stringify([{ name: 'claude', cmd: ['claude'], answer_path: 'raw', enabled: true, drafting: true, found: true, path: '/bin/claude' }, ...(scanned ? [{ name: 'codex', cmd: ['codex'], answer_path: 'raw', enabled: true, drafting: false, found: true, path: '/bin/codex' }] : [])])
          : undefined,
  })
  await panel($)
  await press($, 'tab-settings')
  expect((await buttons($)).map(([k]) => k)).not.toContain('harness-codex')
  const from = rec.runs.length
  await press($, 'harness-scan')
  expect(calls(rec, from)).toEqual(['harness scan', 'harness list --json'])
  expect((await footer($)).note).toBe('✓ scan: claude, codex on PATH · opencode not found')
  expect((await buttons($)).map(([k]) => k)).toEqual(expect.arrayContaining(['harness-claude', 'harness-codex']))
})

test('Settings: the Service row says where the daemon is reachable (daemon.addr), else reachable alone', async ($, on) => {
  world(on, { presence: PRESENCE, daemonAddr: '[::]:7411\n' })
  await panel($)
  await press($, 'tab-settings')
  const t = await texts($)
  expect(t[t.indexOf('Service') + 1]).toBe(`${SIGN.online} reachable at [::1]:7411`)
})

test('Settings: a reachable daemon whose daemon.addr is not host:port says reachable alone', async ($, on) => {
  world(on, { presence: PRESENCE, daemonAddr: 'junk' })
  await panel($)
  await press($, 'tab-settings')
  const t = await texts($)
  expect(t[t.indexOf('Service') + 1]).toBe(`${SIGN.online} reachable`)
})

test('connectAddr: 0.0.0.0 and [::] are the loopback; other hosts as they are; junk is empty', () => {
  expect(['0.0.0.0:7411\n', '127.0.0.1:7411', '192.168.1.5:80', '[::]:7411', '[::1]:9', 'junk', 'a.b:7411', ''].map(connectAddr))
    .toEqual(['127.0.0.1:7411', '127.0.0.1:7411', '192.168.1.5:80', '[::1]:7411', '[::1]:9', '', '', ''])
})

test('Settings: without a daemon status the daemon section says why, in red', async ($, on) => {
  world(on)
  await panel($)
  await press($, 'tab-settings')
  const why = 'owl: no daemon status yet (daemon.status missing) — is the daemon running? see owl install'
  const t = await texts($)
  const service = `${SIGN.offline} not reachable · ${why}`
  expect(t.indexOf(service)).toBeGreaterThan(t.indexOf('Daemon'))
  expect(t[t.indexOf(service) + 1]).toBe(SIGN.offline)
  expect((await textEl($, SIGN.offline))?.props.color).toBe(P.bad)
})

test('ago: seconds, minutes, hours, days; never negative; a time that does not parse as it is', () => {
  const now = Date.parse('2026-10-05T12:00:00Z')
  expect(['2026-10-05T11:59:41Z', '2026-10-05T11:59:00Z', '2026-10-05T11:00:01Z', '2026-10-05T11:00:00Z', '2026-10-04T12:00:01Z', '2026-10-04T12:00:00Z', '2026-10-05T12:00:30Z', 'x\n'].map((x) => ago(x, now)))
    .toEqual(['19s ago', '1m ago', '59m ago', '1h ago', '23h ago', '1d ago', '0s ago', 'x\n'])
})

test('g: Ping now runs owl ping on the selected contact and says online, offline with why, or the failure', async ($, on) => {
  const answers: Record<string, { exitCode: number; stdout?: string; stderr?: string }> = {}
  const rec = world(on, { contacts: [ALICE, BOB], presence: PRESENCE, answers })
  await panel($)
  await press($, 'tab-contacts')
  const rows = async () => (await texts($)).filter((x) => !/^[✓✗]/.test(x) && !x.startsWith('last pull') && !x.startsWith(' Alice is'))
  const before = await rows()
  for (const [answer, note] of [
    [{ exitCode: 0, stdout: JSON.stringify({ fingerprint: FA, name: 'Alice', online: true, probed_at: iso(0) }) }, '✓ Alice is online · probed just now'],
    [{ exitCode: 2, stdout: JSON.stringify({ fingerprint: FA, name: 'Alice', online: false, probed_at: iso(0), error: 'offline: iroh: dial timeout after 10s' }), stderr: 'owl: offline: Alice' }, '✗ Alice is offline · offline: iroh: dial timeout after 10s'],
    [{ exitCode: 1, stderr: 'owl: no contact matches "owl:pfbrpuq3pbfblrnd"\nhint: owl contact list' }, '✗ owl: no contact matches "owl:pfbrpuq3pbfblrnd"'],
  ] as const) {
    answers[`ping --json -- ${FA}`] = answer
    const from = rec.runs.length
    await press($, 'ping')
    expect(calls(rec, from)).toEqual([`ping --json -- ${FA}`])
    expect((await footer($)).note).toBe(note)
    // The rows stay as the daemon wrote them.
    expect(await rows()).toEqual(before)
  }
  // On Bob's row it pings Bob.
  await press($, `row-${FB}`)
  const from = rec.runs.length
  await press($, 'ping')
  expect(calls(rec, from)).toEqual([`ping --json -- ${FB}`])
})

// ---------- hostile values on the new controls

const BAD = '\u0007\u001b[2J\u0085\u009f'
const SEEN = '  [2J  '

async function look($: Engine, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const t = (await p.findAll({ type: 'Text' })).map((x) => x.text)
  const labels = (await p.findAll({ type: 'Button' })).map((x) => String(x.props.label ?? x.text))
  await p.unmount()
  for (const x of t) expect(x).not.toMatch(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/)
  for (const l of labels) expect(l).not.toMatch(/[\u0000-\u001f\u007f-\u009f]/)
  return { texts: t, labels }
}

test('names, presence times, owl output and ping errors with line breaks, control characters or 12 000 characters draw on one line', async ($, on) => {
  const name = `Eve\n${BAD}`
  const fp = `owl:eve${BAD}`
  const ctx = `ctx${BAD}`
  const long = 'L'.repeat(12_000)
  const eve = { ...ALICE, name, fingerprint: fp }
  const rec = world(on, {
    contacts: [eve],
    threads: [{ ...TB, from: fp, from_name: name }],
    archivedChats: [{ ...TB, from: fp, from_name: name }],
    timelines: { [fp]: [ev(R1, ctx, '2026-10-04T10:00:00Z', `Q${BAD}`)] },
    presence: { last_pull_at: `x${BAD}\ny`, open_asks: 1, peers_probed: 1, peers: [{ fingerprint: fp, name, online: false, probed_at: `p${BAD}\nq`, last_seen: long }] },
    answers: (a) => {
      if (a.startsWith('archive ')) return `archived ${name}\n${BAD}`
      if (a.startsWith('ping ')) return { exitCode: 2, stdout: JSON.stringify({ error: `off\n${BAD}${long}` }) }
      return undefined
    },
  })
  await panel($)
  for (const surface of SURFACES) {
    let seen = await look($, surface)
    expect((await footer($, surface)).note).toBe(`last pull x${SEEN} y`)
    await press($, 'delete', surface)
    seen = await look($, surface)
    expect(seen.texts).toContain('Eve keeps their copy.')
    await press($, 'delete-cancel', surface)
    await press($, 'archive', surface)
    seen = await look($, surface)
    expect((await footer($, surface)).note).toBe('✓ archived Eve ·Undo')
    await press($, 'archived', surface)
    await look($, surface)
    await press($, 'archived', surface)
    await press($, `chat-${fp}`, surface)
    seen = await look($, surface)
    expect(seen.texts).toContain(`${SIGN.offline} last seen NaN undefined NaN:NaN · probed p${SEEN} q · ${fp.replace(BAD, SEEN)} · policy manual`)
    await press($, 'delete', surface)
    expect((await look($, surface)).texts).toContain('Delete this thread from this machine?')
    const from = rec.runs.length
    await press($, 'delete-confirm', surface)
    expect(rec.runs.slice(from)[0].argv).toEqual(['delete', `--context=${ctx}`, '--', fp])
    await press($, 'back', surface)
    await press($, 'tab-contacts', surface)
    seen = await look($, surface)
    expect(seen.texts).toContain(`last pull x${SEEN} y · 1 open ask · 1 peer probed`)
    await press($, 'ping', surface)
    const note = (await footer($, surface)).note
    expect(note.startsWith(`✗ Eve ${SEEN} is offline · off ${SEEN}LLL`)).toBe(true)
    expect(note).not.toMatch(/\n/)
    await look($, surface)
    await press($, 'tab-chats', surface)
  }
})
