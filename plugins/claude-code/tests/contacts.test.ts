// The Contacts tab (hooks/contacts.tsx): the book, adding a peer file, the per-contact
// policy and removing a contact (which asks first). `owl` is the world of tests/world.ts.
import { expect, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import { VERIFIED } from '../hooks/lib'
import { P, SIGN } from '../hooks/ui'
import { ALICE, BOB, SURFACES, activeTab, calls, current, footer, pane, panel, press, texts, type, world, type Answer, type Surface, type WorldOptions } from './world'

// One element of the pane by key.
async function el($: Engine, key: string, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const found = await p.find({ key: current(key, surface) })
  await p.unmount()
  return found
}

// The buttons of the pane: key → hotkey and label (the tab row and the footer's are out).
async function buttons($: Engine, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const all = (await p.findAll({ type: 'Button' })).map((b) => [b.key, b.props.hotkey ?? '', b.text])
  await p.unmount()
  return all.filter(([k]) => !String(k).startsWith('tab-') && !['close', 'keys'].includes(String(k)))
}

// The `owl add` argvs run since `from` (the JSON has spaces, so argv, not the joined args).
const adds = (rec: ReturnType<typeof world>, from: number) => rec.runs.slice(from).map((r) => r.argv).filter((a) => a[0] === 'add')

const PEER_FILE = '{"name":"Cid","emails":["c@x.io"],"endpoints":[],"pubkey":"ed25519:Xy"}'

test('Contacts lists the book with the count, the policies and the second lines', async ($, on) => {
  world(on, { contacts: [ALICE, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    expect(t[t.indexOf('Contacts') + 1]).toBe('2')
    expect((await el($, `row-${ALICE.fingerprint}`, surface))?.text).toBe('Alice')
    expect((await el($, `row-${BOB.fingerprint}`, surface))?.text).toBe('Bob')
    // The table: its header, then per row presence, seen, policy and source.
    const h = t.indexOf('Name')
    expect(t.slice(h, h + 4)).toEqual(['Name', 'Seen', 'Policy', 'Src'])
    // Seen (secondary words, the sign in its colour), the policy word in its colour, the source.
    const a = t.indexOf('manual')
    expect(t.slice(a - 2, a + 2)).toEqual([`${SIGN.unknown} not probed`, SIGN.unknown, 'manual', 'global'])
    const b = t.indexOf('none')
    expect(t.slice(b - 2, b + 2)).toEqual([`${SIGN.unknown} not probed`, SIGN.unknown, 'none', 'local'])
    // E-mails and fingerprint show under the selected row (Alice) only.
    expect(t).toContain('a@x.io · owl:pfbrpuq3pbfblrnd')
    expect(t.some((x) => x.includes('b@x.io') || x.includes(`· ${BOB.fingerprint}`))).toBe(false)
    expect(t.filter((x) => x === SIGN.selected)).toHaveLength(1)
    expect(t).toContain('A colleague gets yours from the Card tab.')
    const p = await pane($, surface)
    const all = await p.findAll({ type: 'Text' })
    expect(all.find((x) => x.text === 'Name')?.props).toMatchObject({ bold: true, color: P.secondary })
    expect(all.find((x) => x.text === 'manual')?.props.color).toBe(P.wait)
    expect(all.find((x) => x.text === 'none')?.props.color).toBe(P.secondary)
    // Alice's policy row: the current choice drawn chosen (inverse), the others Buttons.
    expect(all.find((x) => x.text === ' manual ')?.props.inverse).toBe(true)
    await p.unmount()
    const f = await footer($, surface)
    // No daemon status in this world: the note says why.
    expect(f.note).toBe('✗ owl: no daemon status yet (daemon.status missing) — is the daemon running? see owl install')
    for (const hint of ['tab move', 'enter open chat', 'k card', 'p policy', 'x remove', 'n add', 'f filter'])
      expect(f.hints).toContain(hint)
  }
})

test('a policy auto or never shows in its own colour', async ($, on) => {
  world(on, { contacts: [{ ...ALICE, policy: { mode: 'auto' } }, { ...BOB, policy: { mode: 'never' } }] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    const p = await pane($, surface)
    const all = await p.findAll({ type: 'Text' })
    expect(all.find((x) => x.text === 'auto')?.props.color).toBe(P.ok)
    expect(all.find((x) => x.text === 'never')?.props.color).toBe(P.bad)
    await p.unmount()
  }
})

test('the actions sit under the selected row and follow the selection', async ($, on) => {
  world(on, { contacts: [ALICE, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    // Under Alice (the first row) by default, each once.
    let b = (await buttons($, surface)).map(([k]) => k)
    for (const key of ['open', 'card', 'policy', 'remove', 'ping']) expect(b.filter((k) => k === key)).toHaveLength(1)
    expect(b.indexOf('open')).toBeGreaterThan(b.indexOf(`row-${ALICE.fingerprint}`))
    expect(b.indexOf('open')).toBeLessThan(b.indexOf(`row-${BOB.fingerprint}`))
    // Selecting Bob (a press on a row that is not the selected one) moves the actions (and
    // the ▸) under his row.
    await press($, `row-${BOB.fingerprint}`, surface)
    b = (await buttons($, surface)).map(([k]) => k)
    expect(b.indexOf('open')).toBeGreaterThan(b.indexOf(`row-${BOB.fingerprint}`))
    await press($, 'remove', surface)
    expect((await el($, 'remove-confirm', surface))?.text).toBe('Confirm remove Bob')
    await press($, 'remove-cancel', surface)
    // Back to Alice for the next surface.
    await press($, `row-${ALICE.fingerprint}`, surface)
    expect((await el($, 'remove', surface))?.text).toBe('Remove')
  }
})

test('Open chat shows the contact under the Chats tab', async ($, on) => {
  world(on, { contacts: [ALICE, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    await press($, 'open', surface)
    expect(await activeTab($)).toBe('Chats 0')
    expect(await texts($, surface)).toContain('Alice')
    expect(await texts($, surface)).toContain(`${SIGN.unknown} not probed · ${ALICE.fingerprint} · policy manual`)
    await press($, 'tab-contacts', surface)
  }
})

test('k opens the contact card in the Card tab', async ($, on) => {
  const rec = world(on, { contacts: [ALICE, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await press($, 'card', surface)
    expect(await activeTab($)).toBe('Card')
    expect(calls(rec, from)).toEqual(['card --json', `card ${ALICE.fingerprint}`])
    await press($, 'tab-contacts', surface)
  }
})

test('p shows the policy choices and each runs its owl command; a refusal lands in the note', async ($, on) => {
  const answers: Record<string, string | { exitCode: number; stderr: string }> = {
    [`allow ${ALICE.fingerprint} --always ${VERIFIED}`]: 'policy of Alice is auto',
    [`allow ${ALICE.fingerprint}`]: 'policy of Alice is manual',
    [`deny ${ALICE.fingerprint}`]: { exitCode: 1, stderr: 'owl: a hand-added contact needs --i-verified-the-fingerprint' },
  }
  // Alice with no policy written, so none of the three is the current one.
  const rec = world(on, { contacts: [{ ...ALICE, policy: undefined }, BOB], answers })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    // The choices are always drawn under the selected row, as the mock; p runs nothing.
    for (const key of ['policy-auto', 'policy-manual', 'policy-never']) expect(await el($, key, surface)).toBeDefined()
    const before = rec.runs.length
    await press($, 'policy', surface)
    expect(calls(rec, before)).toEqual([])

    // Alice is global (added by hand): auto runs nothing and asks for the human's word first.
    let from = rec.runs.length
    await press($, 'policy-auto', surface)
    expect(calls(rec, from)).toEqual([])
    const t = await texts($, surface)
    expect(t).toContain(`Policy auto answers Alice without asking you. Compare this fingerprint with Alice out of band (a call, a chat) first:`)
    expect(t).toContain(ALICE.fingerprint) // the full fingerprint, a Text of its own
    expect((await el($, 'verify-confirm', surface))?.text).toBe('I verified this fingerprint')
    expect((await el($, 'verify-confirm', surface))?.props.hotkey).toBeUndefined()
    expect((await el($, 'verify-cancel', surface))?.props.hotkey).toBeUndefined()
    // Cancel runs nothing and drops the ask.
    await press($, 'verify-cancel', surface)
    expect(calls(rec, from)).toEqual([])
    expect(await el($, 'verify-confirm', surface)).toBeUndefined()
    // Confirm runs the allow with the flag.
    await press($, 'policy-auto', surface)
    await press($, 'verify-confirm', surface)
    expect(calls(rec, from)).toEqual([`allow ${ALICE.fingerprint} --always ${VERIFIED}`])
    expect((await footer($, surface)).note).toBe('✓ policy of Alice is auto')

    from = rec.runs.length
    await press($, 'policy-manual', surface)
    expect(calls(rec, from)).toEqual([`allow ${ALICE.fingerprint}`])
    expect((await footer($, surface)).note).toBe('✓ policy of Alice is manual')

    from = rec.runs.length
    await press($, 'policy-never', surface)
    expect(calls(rec, from)).toEqual([`deny ${ALICE.fingerprint}`])
    expect((await footer($, surface)).note).toBe('✗ owl: a hand-added contact needs --i-verified-the-fingerprint')
    // The bypass flag ran only from verify-confirm, with the fingerprint it was shown for.
    for (const r of rec.runs.filter((r) => r.argv.includes(VERIFIED)))
      expect(r.args).toBe(`allow ${ALICE.fingerprint} --always ${VERIFIED}`)
  }
})

test('policy auto on a local contact runs allow --always at once, never the flag', async ($, on) => {
  const rec = world(on, { contacts: [BOB], answers: { [`allow ${BOB.fingerprint} --always`]: 'policy of Bob is auto' } })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await press($, 'policy-auto', surface)
    expect(calls(rec, from)).toEqual([`allow ${BOB.fingerprint} --always`])
    expect((await footer($, surface)).note).toBe('✓ policy of Bob is auto')
    expect(await el($, 'verify-confirm', surface)).toBeUndefined() // no verify step
    expect(rec.runs.every((r) => !r.argv.includes(VERIFIED))).toBe(true)
  }
})

test('the verify step closes on another row, a filter keystroke or p, running nothing', async ($, on) => {
  const rec = world(on, { contacts: [ALICE, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    // Selecting another row.
    await press($, 'policy-auto', surface)
    expect(await el($, 'verify-confirm', surface)).toBeDefined()
    let from = rec.runs.length
    await press($, `row-${BOB.fingerprint}`, surface)
    expect(await el($, 'verify-confirm', surface)).toBeUndefined()
    expect(calls(rec, from)).toEqual([])
    // Typing in the filter.
    await press($, `row-${ALICE.fingerprint}`, surface)
    await press($, 'policy-auto', surface)
    expect(await el($, 'verify-confirm', surface)).toBeDefined()
    from = rec.runs.length
    await type($, 'filter', 'a', 'change', surface)
    expect(await el($, 'verify-confirm', surface)).toBeUndefined()
    expect(calls(rec, from)).toEqual([])
    await type($, 'filter', '', 'change', surface)
    // p.
    await press($, 'policy-auto', surface)
    expect(await el($, 'verify-confirm', surface)).toBeDefined()
    from = rec.runs.length
    await press($, 'policy', surface)
    expect(await el($, 'verify-confirm', surface)).toBeUndefined()
    expect(calls(rec, from)).toEqual([])
  }
})

test('the verify step does not come back when you return to its row', async ($, on) => {
  const rec = world(on, { contacts: [ALICE, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await press($, 'policy-auto', surface)
    expect(await el($, 'verify-confirm', surface)).toBeDefined()
    // Away to Bob and back to Alice: the ask must not reappear, and nothing may have run.
    await press($, `row-${BOB.fingerprint}`, surface)
    await press($, `row-${ALICE.fingerprint}`, surface)
    expect(await el($, 'verify-confirm', surface)).toBeUndefined()
    expect(await texts($, surface)).not.toContain(
      'Policy auto answers Alice without asking you. Compare this fingerprint with Alice out of band (a call, a chat) first:',
    )
    expect(calls(rec, from)).toEqual([])
    // Alice's policy is untouched: the bypass flag never ran.
    expect(rec.runs.every((r) => !r.argv.includes(VERIFIED))).toBe(true)
  }
})

test('a policy command closes the verify step: after the confirm and after manual chosen while it is open', async ($, on) => {
  const rec = world(on, {
    contacts: [{ ...ALICE, policy: undefined }, BOB],
    answers: {
      [`allow ${ALICE.fingerprint} --always ${VERIFIED}`]: 'policy of Alice is auto',
      [`allow ${ALICE.fingerprint}`]: 'policy of Alice is manual',
    },
  })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    // The confirm runs the allow with the flag and closes the ask.
    await press($, 'policy-auto', surface)
    expect(await el($, 'verify-confirm', surface)).toBeDefined()
    let from = rec.runs.length
    await press($, 'verify-confirm', surface)
    expect(calls(rec, from)).toEqual([`allow ${ALICE.fingerprint} --always ${VERIFIED}`])
    expect(await el($, 'verify-confirm', surface)).toBeUndefined()
    expect(await texts($, surface)).not.toContain(
      'Policy auto answers Alice without asking you. Compare this fingerprint with Alice out of band (a call, a chat) first:',
    )
    // The choices stay drawn while the check shows; choosing manual closes the check too.
    await press($, 'policy-auto', surface)
    expect(await el($, 'verify-confirm', surface)).toBeDefined()
    from = rec.runs.length
    await press($, 'policy-manual', surface)
    expect(calls(rec, from)).toEqual([`allow ${ALICE.fingerprint}`])
    expect(await el($, 'verify-confirm', surface)).toBeUndefined()
  }
})

test('Remove asks first; the confirm runs contact remove, with --local for a local contact', async ($, on) => {
  const rec = world(on, { contacts: [ALICE, BOB], answers: { [`contact remove ${ALICE.fingerprint}`]: 'Alice removed' } })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    // Asking runs nothing.
    let from = rec.runs.length
    await press($, 'remove', surface)
    expect(calls(rec, from)).toEqual([])
    expect((await el($, 'remove-confirm', surface))?.text).toBe('Confirm remove Alice')
    expect(await el($, 'remove-cancel', surface)).toBeDefined()
    expect(await el($, 'remove', surface)).toBeUndefined()
    // Cancel runs nothing and brings Remove back.
    await press($, 'remove-cancel', surface)
    expect(calls(rec, from)).toEqual([])
    expect(await el($, 'remove', surface)).toBeDefined()
    // Confirm on Alice (global).
    await press($, 'remove', surface)
    from = rec.runs.length
    await press($, 'remove-confirm', surface)
    expect(calls(rec, from)).toEqual([`contact remove ${ALICE.fingerprint}`])
    expect((await footer($, surface)).note).toBe('✓ Alice removed')
    // Bob (local) gets --local.
    await press($, `row-${BOB.fingerprint}`, surface)
    await press($, 'remove', surface)
    from = rec.runs.length
    await press($, 'remove-confirm', surface)
    expect(calls(rec, from)).toEqual([`contact remove ${BOB.fingerprint} --local`])
    // Back to Alice for the next surface.
    await press($, `row-${ALICE.fingerprint}`, surface)
  }
})

test('a failed remove lands in the note', async ($, on) => {
  world(on, {
    contacts: [ALICE, BOB],
    answers: { [`contact remove ${ALICE.fingerprint}`]: { exitCode: 1, stderr: 'owl: no contact owl:pfbrpuq3pbfblrnd' } },
  })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    await press($, 'remove', surface)
    await press($, 'remove-confirm', surface)
    expect((await footer($, surface)).note).toBe('✗ owl: no contact owl:pfbrpuq3pbfblrnd')
    expect(await el($, 'remove', surface)).toBeDefined() // the ask closed, Remove is back
  }
})

test('the remove ask is bound to its contact: a filter or another row drops it', async ($, on) => {
  const rec = world(on, { contacts: [ALICE, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    // Ask on Alice, then filter down to Bob: no confirm for Bob, nothing ran.
    await press($, 'remove', surface)
    expect((await el($, 'remove-confirm', surface))?.text).toBe('Confirm remove Alice')
    const from = rec.runs.length
    await type($, 'filter', 'bob', 'change', surface)
    expect(await el($, 'remove-confirm', surface)).toBeUndefined()
    expect((await el($, 'remove', surface))?.text).toBe('Remove')
    expect(calls(rec, from)).toEqual([])
    // Back to the full book: Alice's confirm is gone too.
    await type($, 'filter', '', 'change', surface)
    expect(await el($, 'remove-confirm', surface)).toBeUndefined()
    // Ask on Alice, select Bob, select Alice: no confirm either.
    await press($, 'remove', surface)
    await press($, `row-${BOB.fingerprint}`, surface)
    await press($, `row-${ALICE.fingerprint}`, surface)
    expect(await el($, 'remove-confirm', surface)).toBeUndefined()
    expect(calls(rec, from)).toEqual([])
  }
})

test('a reload that drops the asked contact drops the remove ask too', async ($, on) => {
  const contacts = [ALICE, BOB]
  const rec = world(on, { contacts })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    await press($, 'remove', surface)
    expect((await el($, 'remove-confirm', surface))?.text).toBe('Confirm remove Alice')
    const from = rec.runs.length
    // Alice leaves the book elsewhere (another session, the CLI); the list reloads without any
    // press or filter change.
    contacts.splice(contacts.indexOf(ALICE), 1)
    await panel($) // close, then reopen to refresh
    await panel($)
    await press($, 'tab-contacts', surface)
    // The selection fell to Bob; the stale ask must not follow him.
    const boxes = await highlighted($, surface)
    expect(boxes).toHaveLength(1)
    expect(boxes[0].text).toContain('Bob')
    expect(await el($, 'remove-confirm', surface)).toBeUndefined()
    expect((await el($, 'remove', surface))?.text).toBe('Remove')
    expect(calls(rec, from)).toEqual([])
    // Put Alice back and reload; the ask, still Alice's, is cancelled for the next surface.
    contacts.push(ALICE)
    await panel($)
    await panel($)
    await press($, 'tab-contacts', surface)
    await press($, 'remove-cancel', surface)
  }
})

test('the filter matches name, e-mail or fingerprint, any case, or says nothing matches', async ($, on) => {
  world(on, { contacts: [ALICE, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    await type($, 'filter', 'ALICE', 'change', surface)
    expect(await el($, `row-${ALICE.fingerprint}`, surface)).toBeDefined()
    expect(await el($, `row-${BOB.fingerprint}`, surface)).toBeUndefined()
    // Spaces around the filter don't matter.
    await type($, 'filter', '  bob ', 'change', surface)
    expect(await el($, `row-${BOB.fingerprint}`, surface)).toBeDefined()
    expect(await el($, `row-${ALICE.fingerprint}`, surface)).toBeUndefined()
    await type($, 'filter', 'b@x.io', 'change', surface)
    expect(await el($, `row-${BOB.fingerprint}`, surface)).toBeDefined()
    expect(await el($, `row-${ALICE.fingerprint}`, surface)).toBeUndefined()
    // The header counts the whole book, not the matches.
    const t = await texts($, surface)
    expect(t[t.indexOf('Contacts') + 1]).toBe('2')
    await type($, 'filter', 'xri6rp', 'submit', surface)
    expect(await el($, `row-${BOB.fingerprint}`, surface)).toBeDefined()
    expect(await el($, `row-${ALICE.fingerprint}`, surface)).toBeUndefined()
    await type($, 'filter', ' nobody ', 'change', surface)
    expect(await texts($, surface)).toContain('No contact matches "nobody".')
    await type($, 'filter', '', 'change', surface)
    expect(await el($, `row-${ALICE.fingerprint}`, surface)).toBeDefined()
    expect(await el($, `row-${BOB.fingerprint}`, surface)).toBeDefined()
  }
})

test('an empty book says so and how to add one', async ($, on) => {
  world(on, { contacts: [] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    expect(await texts($, surface)).toContain("No contacts yet. n: Add contact takes a colleague's peer file.")
    expect(await el($, 'open', surface)).toBeUndefined()
  }
})

test('a failed list is not an empty book: the why shows instead', async ($, on) => {
  const answers: Record<string, string | Answer> = {}
  const o: WorldOptions = { missing: true, answers }
  world(on, o)
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    expect(t).toContain('- owl could not start: is it installed and on PATH?')
    expect(t).not.toContain("No contacts yet. n: Add contact takes a colleague's peer file.")
  }
  // `owl contact list --json` exits 1.
  o.missing = false
  answers['contact list --json'] = { exitCode: 1, stderr: 'owl: broken config' }
  await panel($) // close, then reopen to refresh
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    expect(await texts($, surface)).toContain('- owl contact list failed: owl: broken config')
  }
  // Its stdout is not a list.
  answers['contact list --json'] = 'not json'
  await panel($)
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    expect(await texts($, surface)).toContain('- owl contact list printed no list')
  }
})

test('the add field is always drawn at the bottom (n focuses it); the JSON or a path runs owl add, an empty field runs nothing', async ($, on) => {
  const rec = world(on, { contacts: [ALICE, BOB], answers: { [`add ${PEER_FILE}`]: 'Cid added' } })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    // As the mock: the label, the field and the hint under it, after the table.
    const t = await texts($, surface)
    expect(t.indexOf('A colleague gets yours from the Card tab.')).toBeGreaterThan(t.indexOf('global'))
    expect(await el($, 'add', surface)).toBeDefined()
    expect(await el($, 'use-prompt', surface)).toBeDefined()
    const before = rec.runs.length
    await press($, 'add-focus', surface)
    await press($, 'add-top', surface)
    expect(calls(rec, before)).toEqual([])
    expect(await el($, 'add', surface)).toBeDefined()

    // The peer file JSON itself, one argv entry, spaces and all.
    let from = rec.runs.length
    await type($, 'add', PEER_FILE, 'submit', surface)
    expect(adds(rec, from)).toEqual([['add', PEER_FILE]])
    expect((await footer($, surface)).note).toBe('✓ Cid added')
    expect((await el($, 'add', surface))?.props.value).toBe('') // a success empties the field

    // A path to one.
    from = rec.runs.length
    await type($, 'add', ' /tmp/bob.json ', 'submit', surface)
    expect(adds(rec, from)).toEqual([['add', '/tmp/bob.json']])

    // Empty or spaces: nothing runs, the note says why.
    from = rec.runs.length
    await type($, 'add', '   ', 'submit', surface)
    expect(adds(rec, from)).toEqual([])
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ paste a peer file first')
  }
})

test('u fills the add field from the prompt box and runs nothing by itself', async ($, on) => {
  const rec = world(on, { contacts: [ALICE, BOB], prompt: PEER_FILE })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await press($, 'use-prompt', surface)
    expect((await el($, 'add', surface))?.props.value).toBe(PEER_FILE)
    expect(adds(rec, from)).toEqual([])
    // Submitting the filled field adds it.
    await type($, 'add', PEER_FILE, 'submit', surface)
    expect(adds(rec, from)).toEqual([['add', PEER_FILE]])
  }
})

test('a failed add keeps the text and shows the error', async ($, on) => {
  world(on, { contacts: [ALICE, BOB], answers: { [`add ${PEER_FILE}`]: { exitCode: 1, stderr: 'owl: not a peer file' } } })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    await type($, 'add', PEER_FILE, 'change', surface)
    await type($, 'add', PEER_FILE, 'submit', surface)
    expect((await footer($, surface)).note).toBe('✗ owl: not a peer file')
    expect((await el($, 'add', surface))?.props.value).toBe(PEER_FILE)
    // Clear the kept text for the next surface.
    await type($, 'add', '', 'change', surface)
  }
})

test('every screen draws its buttons with their hotkeys and labels, none shared', async ($, on) => {
  world(on, { contacts: [ALICE, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  // `n` is drawn twice, as the mock (the header and the label over the field): both focus the
  // add field, so the clash (the later wins) does the same either way.
  const noShared = (b: unknown[][]) => {
    const keys = b.filter(([k]) => k !== 'add-top').map(([, h]) => h).filter(Boolean)
    expect(new Set(keys).size).toBe(keys.length)
  }
  for (const surface of SURFACES) {
    const base = await buttons($, surface)
    expect(base).toEqual([
      ['add-top', 'n', 'Add contact'], ['filter-focus', 'f', 'Filter'],
      [`row-${ALICE.fingerprint}`, '', 'Alice'],
      ['open', '', 'Open chat'], ['card', 'k', 'Card'], ['ping', 'g', 'Ping now'], ['remove', 'x', 'Remove'],
      ['policy', 'p', 'Policy'], ['policy-auto', '', 'auto'], ['policy-never', '', 'never'],
      [`row-${BOB.fingerprint}`, '', 'Bob'],
      ['add-focus', 'n', 'Add contact'], ['use-prompt', 'u', ''],
    ])
    noShared(base)
    // After x: the ask replaces Remove.
    await press($, 'remove', surface)
    const asked = await buttons($, surface)
    expect(asked).toContainEqual(['remove-confirm', 'x', 'Confirm remove Alice'])
    expect(asked).toContainEqual(['remove-cancel', '', 'Cancel'])
    expect(asked.filter(([k]) => k === 'remove')).toHaveLength(0)
    noShared(asked)
    await press($, 'remove-cancel', surface)
  }
})

// The row Boxes with a highlight (only the selected row's first line should have one).
async function highlighted($: Engine, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const boxes = (await p.findAll({ type: 'Box' })).filter((b) => b.props.backgroundColor === P.selected)
  await p.unmount()
  return boxes
}

test('the selected row is highlighted and the highlight follows the selection', async ($, on) => {
  world(on, { contacts: [ALICE, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    let boxes = await highlighted($, surface)
    expect(boxes).toHaveLength(1)
    expect(boxes[0].text).toContain('Alice')
    expect(boxes[0].text).toContain(SIGN.selected)
    expect(boxes[0].text).not.toContain('Bob')
    await press($, `row-${BOB.fingerprint}`, surface)
    boxes = await highlighted($, surface)
    expect(boxes).toHaveLength(1)
    expect(boxes[0].text).toContain('Bob')
    expect(boxes[0].text).not.toContain('Alice')
    await press($, `row-${ALICE.fingerprint}`, surface)
  }
})

test('selecting another row moves the policy choices with it and drops the remove ask', async ($, on) => {
  world(on, { contacts: [ALICE, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    // Alice is manual: auto and never are Buttons. Bob has none: all three are.
    expect(await el($, 'policy-manual', surface)).toBeUndefined()
    await press($, `row-${BOB.fingerprint}`, surface)
    for (const key of ['policy-auto', 'policy-manual', 'policy-never']) expect(await el($, key, surface)).toBeDefined()
    await press($, 'remove', surface)
    expect(await el($, 'remove-confirm', surface)).toBeDefined()
    await press($, `row-${ALICE.fingerprint}`, surface)
    expect(await el($, 'remove-confirm', surface)).toBeUndefined()
    expect((await el($, 'remove', surface))?.text).toBe('Remove')
  }
})

test('a successful add clears the field for the next one', async ($, on) => {
  world(on, { contacts: [ALICE, BOB], answers: { [`add ${PEER_FILE}`]: 'Cid added' } })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    await type($, 'add', PEER_FILE, 'change', surface)
    await type($, 'add', PEER_FILE, 'submit', surface)
    expect((await footer($, surface)).note).toBe('✓ Cid added')
    expect((await el($, 'add', surface))?.props.value).toBe('')
  }
})

test('a contact with two e-mails shows both', async ($, on) => {
  world(on, { contacts: [{ ...ALICE, emails: ['a@x.io', 'a2@x.io'] }, BOB] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    expect(await texts($, surface)).toContain(`a@x.io, a2@x.io · ${ALICE.fingerprint}`)
  }
})

test('an empty book with a filter typed still says how to add one', async ($, on) => {
  world(on, { contacts: [] })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    await type($, 'filter', 'bob', 'change', surface)
    const t = await texts($, surface)
    expect(t).toContain("No contacts yet. n: Add contact takes a colleague's peer file.")
    expect(t).not.toContain('No contact matches "bob".')
    await type($, 'filter', '', 'change', surface)
  }
})
