// `claude plugin test plugins/claude-code`: the hint-line segment, the band, `/owlpost:panel`
// and the stored switches, and the panel's shell: tabs, Keys, close, the
// note and the footer, short ids and the palette. The world is tests/world.ts.
import { expect, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On } from 'claude-code'
import { BOB, HOME, SURFACES, activeTab, calls, footer, pane, panel, press, texts, world } from './world'
import { replyTarget, short, shorten, type Ev } from '../hooks/lib'
import { P } from '../hooks/ui'

const HINT = { component: 'PromptHint', props: { isDraft: false, isWorking: false, hint: '? for shortcuts' } } as const
const BAND = {
  component: 'AbovePrompt',
  props: { hasSurvey: false, isWorking: false, maxRows: 5, bodyColumns: 120, scroll: { offset: 0, bodyRows: 5 }, view: {} },
} as const

// Switches the open pane to another tab, as a press on the tab does.
const goTo = ($: Engine, tab: 'contacts' | 'new') => press($, `tab-${tab}`)

test('the segment shows 0 and no band is drawn at 0 unseen', async ($, on) => {
  world(on, { unseen: 0 })
  await panel($) // loads the count
  for (const surface of SURFACES) {
    const hint = await $.ui.mount({ plugin: 'owlpost', surface, ...HINT })
    expect((await hint.find({ type: 'Button', key: 'owlpost-count' }))?.text).toBe('🦉 📩 0')
    await hint.unmount()
    const band = await $.ui.mount({ plugin: 'owlpost', surface, ...BAND })
    expect(await band.find({ text: /waiting/ })).toBeUndefined()
    expect(await band.find({ type: 'Button' })).toBeUndefined()
    await band.unmount()
  }
})

test('one unseen question draws the band with its text and one Open panel button', async ($, on) => {
  world(on, { unseen: 1 })
  await panel($)
  await panel($) // closed: the band's button shows only while the pane is closed
  for (const surface of SURFACES) {
    const band = await $.ui.mount({ plugin: 'owlpost', surface, ...BAND })
    expect(await band.find({ type: 'Text', text: '1 question waiting' })).toBeDefined()
    const buttons = await band.findAll({ type: 'Button' })
    expect(buttons.map((b) => b.text)).toEqual(['Open panel'])
    await band.unmount()
    const hint = await $.ui.mount({ plugin: 'owlpost', surface, ...HINT })
    expect((await hint.find({ key: 'owlpost-count' }))?.text).toBe('🦉 📩 1')
    await hint.unmount()
  }
})

test('two unseen questions read in the plural', async ($, on) => {
  world(on, { unseen: 2 })
  await panel($)
  const band = await $.ui.mount({ plugin: 'owlpost', surface: 'terminal', ...BAND })
  expect(await band.find({ type: 'Text', text: '2 questions waiting' })).toBeDefined()
  await band.unmount()
})

// A docked pane leaves the band a thin column: no word may break there. The label and the
// button never shrink, the count is one line cut at its end.
test('the band is one line that never breaks a word: label and button fixed, the count truncates', async ($, on) => {
  world(on, { unseen: 3 })
  await panel($)
  await panel($)
  for (const surface of SURFACES) {
    for (const bodyColumns of [120, 40, 20]) {
      const band = await $.ui.mount({ plugin: 'owlpost', surface, ...BAND, props: { ...BAND.props, bodyColumns } })
      const row = (await band.findAll({ type: 'Box' }))[0]
      expect(row.props.flexDirection).toBe('row')
      expect(row.props.flexWrap ?? 'nowrap').toBe('nowrap')
      expect((await band.find({ key: 'band-label' }))?.props.flexShrink).toBe(0)
      expect((await band.find({ key: 'band-open' }))?.props.flexShrink).toBe(0)
      expect((await band.find({ key: 'band-count' }))?.props.flexShrink).toBe(1)
      const label = await band.find({ type: 'Text', text: 'owlpost' })
      expect(label?.text).toBe('owlpost')
      const count = await band.find({ type: 'Text', text: '3 questions waiting' })
      expect(count?.props.wrap).toBe('truncate')
      expect((await band.findAll({ type: 'Button' })).map((b) => b.text)).toEqual(['Open panel'])
      await band.unmount()
    }
  }
})

test('answers are counted apart from questions, questions first', async ($, on) => {
  world(on, { unseen: { count: 3, questions: 1 } })
  await panel($)
  for (const surface of SURFACES) {
    const band = await $.ui.mount({ plugin: 'owlpost', surface, ...BAND })
    expect(await band.find({ type: 'Text', text: '1 question, 2 answers waiting' })).toBeDefined()
    await band.unmount()
    // The segment counts every unseen record, answers included.
    const hint = await $.ui.mount({ plugin: 'owlpost', surface, ...HINT })
    expect((await hint.find({ key: 'owlpost-count' }))?.text).toBe('🦉 📩 3')
    await hint.unmount()
  }
})

test('answers alone read without a questions part', async ($, on) => {
  world(on, { unseen: { count: 1, questions: 0 } })
  await panel($)
  const band = await $.ui.mount({ plugin: 'owlpost', surface: 'terminal', ...BAND })
  expect((await band.find({ type: 'Text', text: /waiting/ }))?.text).toBe('1 answer waiting')
  await band.unmount()
})

test('the band yields to a survey', async ($, on) => {
  world(on, { unseen: 1 })
  await panel($)
  const band = await $.ui.mount({ plugin: 'owlpost', surface: 'terminal', ...BAND, props: { ...BAND.props, hasSurvey: true } })
  expect(await band.find({ text: /waiting/ })).toBeUndefined()
  await band.unmount()
})

test('a press on the segment toggles the pane; the band button opens it', async ($, on) => {
  const { opened, closed } = world(on, { unseen: 1 })
  await panel($) // opens once and loads the count
  await panel($) // closes it again
  for (const surface of SURFACES) {
    const hint = await $.ui.mount({ plugin: 'owlpost', surface, ...HINT })
    await goTo($, 'contacts') // so the Chats the press lands on is the press's doing
    const before = opened.length
    const shut = closed.length
    await hint.press({ key: 'owlpost-count' }) // closed: opens it on Chats
    expect([opened.length, closed.length]).toEqual([before + 1, shut])
    expect(await activeTab($)).toBe('Chats 1')
    await hint.press({ key: 'owlpost-count' }) // open: closes it
    expect([opened.length, closed.length]).toEqual([before + 1, shut + 1])
    await hint.press({ key: 'owlpost-count' }) // and opens it again after its own close
    expect([opened.length, closed.length]).toEqual([before + 2, shut + 1])
    await hint.press({ key: 'owlpost-count' })
    expect([opened.length, closed.length]).toEqual([before + 2, shut + 2])
    await hint.unmount()
    await goTo($, 'contacts')
    const band = await $.ui.mount({ plugin: 'owlpost', surface, ...BAND })
    await band.press({ key: 'open-panel' })
    expect(opened.length).toBe(before + 3)
    expect(await activeTab($)).toBe('Chats 1')
    await band.unmount()
    await panel($) // closed again for the next surface
  }
  expect(opened.every((id) => id === 'owlpost')).toBe(true)
})

test('every open the person causes reaches the engine before the press or command awaits anything', async ($, on) => {
  const { opened } = world(on, { unseen: 1 })
  // The theme read is the first thing `open` awaits; each one records how many opens came before.
  const atRead: number[] = []
  on('config.list', async () => {
    atRead.push(opened.length)
    return { value: [] }
  })
  const first = async (act: () => Promise<unknown>) => {
    const [o, r] = [opened.length, atRead.length]
    await act()
    expect(opened.length).toBe(o + 1)
    expect(atRead[r]).toBe(o + 1) // the open came first
  }
  await first(() => panel($))
  await panel($)
  for (const surface of SURFACES) {
    const hint = await $.ui.mount({ plugin: 'owlpost', surface, ...HINT })
    await first(() => hint.press({ key: 'owlpost-count' }))
    await hint.press({ key: 'owlpost-count' })
    await hint.unmount()
    const band = await $.ui.mount({ plugin: 'owlpost', surface, ...BAND })
    await first(() => band.press({ key: 'open-panel' }))
    await band.unmount()
    await panel($)
  }
})

test('a pane the engine left waiting unplaced is not open: the next /owlpost:panel opens it', async ($, on) => {
  const o = { unseen: 1, unplaced: true }
  const { opened, closed } = world(on, o)
  await panel($) // waits unplaced
  o.unplaced = false
  await panel($) // opens it again (asked), never closes it
  expect([opened.length, closed.length]).toEqual([2, 0])
  await panel($) // now placed: closes it
  expect([opened.length, closed.length]).toEqual([2, 1])
})

test('the band keeps its Open panel button while the pane is open (as the mock), and after it closes', async ($, on) => {
  world(on, { unseen: 1 })
  await panel($)
  for (const surface of SURFACES) {
    let band = await $.ui.mount({ plugin: 'owlpost', surface, ...BAND })
    expect(await band.find({ type: 'Text', text: '1 question waiting' })).toBeDefined()
    expect((await band.find({ key: 'open-panel' }))?.text).toBe('Open panel')
    await band.unmount()
    await press($, 'close', surface) // the pane's q: ×
    band = await $.ui.mount({ plugin: 'owlpost', surface, ...BAND })
    expect((await band.find({ key: 'open-panel' }))?.text).toBe('Open panel')
    await band.press({ key: 'open-panel' })
    await band.unmount()
  }
})

test('/owlpost:panel opens the pane, and closes it while it is open', async ($, on) => {
  const { opened, closed } = world(on, { unseen: 0 })
  expect(await panel($)).toEqual({})
  expect([opened, closed]).toEqual([['owlpost'], []])
  expect(await activeTab($)).toBe('Chats 0')
  await goTo($, 'contacts') // the next open starts on Chats again, whatever was shown
  await panel($)
  expect([opened, closed]).toEqual([['owlpost'], ['owlpost']])
  await panel($)
  expect([opened, closed]).toEqual([['owlpost', 'owlpost'], ['owlpost']])
  expect(await activeTab($)).toBe('Chats 0')
})

test('/owlpost:stop closes an open pane and still runs as a command', async ($, on) => {
  const { opened, closed } = world(on, { unseen: 0 })
  const stop = () =>
    $.command.run({ command: 'owlpost:stop', args: '', origin: { kind: 'composer' }, presentation: { isFullscreen: true, columns: 200 } })
  await stop() // pane closed: nothing to close
  expect([opened, closed]).toEqual([[], []])
  await panel($)
  await stop()
  expect([opened, closed]).toEqual([['owlpost'], ['owlpost']])
})

test('plugin.json that is not JSON, or a band that is not a bool, keeps the band on and the watch off', async ($, on) => {
  const stored: { value: unknown } = { value: '{not json' }
  world(on, { unseen: 1, stored })
  on('classic.SessionStart', async () => ({ watchPaths: [WAKE, ...OTHERS] }))
  await panel($)
  let band = await $.ui.mount({ plugin: 'owlpost', surface: 'terminal', ...BAND })
  expect(await band.find({ text: '1 question waiting' })).toBeDefined()
  await band.unmount()
  expect((await $.classic.SessionStart({ source: 'startup', session_id: 's1' })).watchPaths).toEqual(KEPT)
  stored.value = { band: 'false' }
  await panel($) // closes the pane
  await panel($) // opens it again, which reloads the switches
  band = await $.ui.mount({ plugin: 'owlpost', surface: 'terminal', ...BAND })
  expect(await band.find({ text: '1 question waiting' })).toBeDefined()
  await band.unmount()
})

test('{"band": false} turns the band off and {"band": true} back on', async ($, on) => {
  const stored: { value: object } = { value: { watch: true } } // no band key: on
  world(on, { unseen: 1, stored })
  await panel($)
  const unset = await $.ui.mount({ plugin: 'owlpost', surface: 'terminal', ...BAND })
  expect(await unset.find({ text: '1 question waiting' })).toBeDefined()
  await unset.unmount()
  stored.value = { band: false }
  await panel($) // closes the pane
  await panel($) // opens it again, which reloads the switches
  const off = await $.ui.mount({ plugin: 'owlpost', surface: 'terminal', ...BAND })
  expect(await off.find({ text: /waiting/ })).toBeUndefined()
  await off.unmount()
  stored.value = { band: true }
  await panel($) // closes the pane
  await panel($) // opens it again, which reloads the switches
  const on2 = await $.ui.mount({ plugin: 'owlpost', surface: 'terminal', ...BAND })
  expect(await on2.find({ text: '1 question waiting' })).toBeDefined()
  await on2.unmount()
})

const WAKE = `${HOME}/sessions/s1/wake`
// Another session's wake path and another plugin's path: never this mod's to drop.
const OTHERS = [`${HOME}/sessions/s2/wake`, '/elsewhere/watched']
const KEPT = OTHERS

// The classic SessionStart beneath the mod answers this session's wake path and the others.
async function sessionStart(on: On, $: Engine) {
  on('classic.SessionStart', async () => ({ additionalContext: ['ctx'], watchPaths: [WAKE, ...OTHERS] }))
  return $.classic.SessionStart({ source: 'startup', session_id: 's1' })
}

test('live watch is off with no stored switch: the wake path is dropped', async ($, on) => {
  world(on, { unseen: 0 })
  const r = await sessionStart(on, $)
  expect(r.watchPaths).toEqual(KEPT)
  expect(r.additionalContext).toEqual(['ctx'])
})

test('a SessionStart without watchPaths passes through untouched, plugin.json unread', async ($, on) => {
  // What owl answers with `{"watch": false}`, or with no session id: no watch path at all.
  const { reads } = world(on, { stored: { value: { watch: false } } })
  on('classic.SessionStart', async () => ({ additionalContext: ['ctx'] }))
  const r = await $.classic.SessionStart({ source: 'startup', session_id: 's1' })
  expect(r).toEqual({ additionalContext: ['ctx'] })
  expect(reads.plugin).toBe(0)
})

test('live watch stays off with {"watch": false} and with a non-bool value', async ($, on) => {
  const stored: { value: object } = { value: { watch: false } }
  world(on, { unseen: 0, stored })
  expect((await sessionStart(on, $)).watchPaths).toEqual(KEPT)
  stored.value = { watch: 'true' }
  expect((await $.classic.SessionStart({ source: 'startup', session_id: 's1' })).watchPaths).toEqual(KEPT)
})

test('{"watch": true} arms the wake', async ($, on) => {
  world(on, { stored: { value: { watch: true } } })
  expect((await sessionStart(on, $)).watchPaths).toEqual([WAKE, ...OTHERS])
})

// ---------- The panel's shell ----------

const TABS = [
  ['tab-contacts', 'Contacts'],
  ['tab-new', 'New'],
  ['tab-card', 'Card'],
  ['tab-settings', 'Settings'],
  ['tab-chats', 'Chats 2'],
] as const

test('the tab buttons carry the hotkeys 1-5, h opens Keys and q closes', async ($, on) => {
  const { closed } = world(on, { unseen: 2 })
  await panel($)
  for (const surface of SURFACES) {
    const p = await pane($, surface)
    const buttons = await p.findAll({ type: 'Button' })
    const hot = Object.fromEntries(buttons.filter((b) => b.props.hotkey).map((b) => [b.key, b.props.hotkey]))
    expect(hot).toMatchObject({ 'tab-contacts': '2', 'tab-new': '3', 'tab-card': '4', 'tab-settings': '5', close: 'q', keys: 'h' })
    await p.unmount()
  }
  for (const [key, label] of TABS) {
    await press($, key)
    expect(await activeTab($)).toBe(label)
  }
  await press($, 'keys')
  expect(await activeTab($)).toBeUndefined()
  await press($, 'close')
  expect(closed).toEqual(['owlpost'])
})

test('every tab draws a note line and the key hints with the keys button', async ($, on) => {
  world(on, { unseen: 0 })
  await panel($)
  for (const key of ['tab-chats', 'tab-contacts', 'tab-new', 'tab-card', 'tab-settings', 'keys']) {
    if (key !== 'tab-chats') await press($, key)
    for (const surface of SURFACES) {
      const { hints } = await footer($, surface)
      expect(hints).toStartWith('keys')
    }
  }
})

test('the command of the tab on show closes the pane, another tab switches to its own', async ($, on) => {
  const { opened, closed } = world(on, { unseen: 0 })
  const run = (command: string) =>
    $.command.run({ command, args: '', origin: { kind: 'composer' }, presentation: { isFullscreen: true, columns: 200 } })
  await run('owlpost:contacts')
  expect(await activeTab($)).toBe('Contacts')
  await run('owlpost:inbox') // pane open on Contacts: switches to Chats
  expect(await activeTab($)).toBe('Chats 0')
  expect(closed).toEqual([])
  await run('owlpost:inbox')
  expect(closed).toEqual(['owlpost'])
  await run('owlpost:ask')
  expect(await activeTab($)).toBe('New')
  expect(opened).toHaveLength(3)
})

test('short ids are the last six hex characters', async () => {
  const a = '01a106b9-52d7-7af3-80bc-0e6e800362f4'
  const b = '01a106b9-8e9e-7ad0-bf5b-410a7c351bd1'
  expect(short(a)).toBe('0362f4')
  expect(short(b)).toBe('351bd1')
  expect(shorten(`accepted ${a} — waiting`)).toBe('accepted 0362f4 — waiting')
})

test('the palette is theme keys only', async () => {
  const keys = ['claude', 'inactive', 'subtle', 'success', 'warning', 'error', 'suggestion', 'userMessageBackground', 'diffRemovedDimmed', 'selectionBg']
  for (const v of Object.values(P)) expect(keys).toContain(v)
})

test('a thread continues from our newest request or the newest record received, never our answer', async () => {
  const ev = (record_id: string, dir: 'in' | 'out', type: string) =>
    ({ ts: '2026-10-04T10:00:00Z', kind: 'x', dir, record_id, context_id: 'c', type, state: 'sent', project: null, path: '-', text: '' }) as Ev
  // The peer's question received after their answer is the newest record received.
  expect(replyTarget([ev('q1', 'out', 'question'), ev('a1', 'in', 'answer'), ev('q2', 'in', 'question')])).toBe('q2')
  expect(replyTarget([ev('a1', 'in', 'answer'), ev('r1', 'out', 'content')])).toBe('r1')
  expect(replyTarget([ev('t1', 'in', 'tool-reply')])).toBe('t1')
  // Only the peer asked, and our answer went out: the thread goes on from their question.
  expect(replyTarget([ev('q1', 'in', 'question'), ev('q1', 'in', 'question'), ev('x1', 'out', 'answer')])).toBe('q1')
  // Our answer alone is nothing to continue from.
  expect(replyTarget([ev('x1', 'out', 'answer')])).toBe('')
  // An exchange from before threads (no context_id): `owl ask --reply-to` refuses it
  // (`<id> carries no thread id`), so there is nothing to continue from.
  expect(replyTarget([{ ...ev('q1', 'out', 'question'), context_id: null }])).toBe('')
})

// ---------- Hand-overs between views ----------

const CTX = '0199ae10-2b3c-7d4e-8f50-6a7b8c4e8b1d'
const ASKED = {
  ts: '2026-10-04T07:15:00Z', kind: 'asked', dir: 'out', record_id: '0199ae10-2b3c-7d4e-8f50-6a7b8c000001', context_id: CTX,
  type: 'question', state: 'waiting', project: 'demo', path: '-', text: 'What is your last commit?',
} as Ev
const BOB_THREAD = { from: BOB.fingerprint, from_name: 'Bob', last_ts: ASKED.ts, unseen: 0, open: 0, last_summary: ASKED.text }

test("n on a person's screen and in a thread opens New addressed to that person", async ($, on) => {
  const rec = world(on, { contacts: [BOB], threads: [BOB_THREAD], timelines: { [BOB.fingerprint]: [ASKED] }, prompt: 'Which branch?' })
  await panel($)
  for (const surface of SURFACES) {
    for (const where of ['contact', 'thread']) {
      await press($, `chat-${BOB.fingerprint}`, surface)
      if (where === 'thread') await press($, `thread-${CTX}`, surface)
      await press($, 'new', surface)
      expect(await activeTab($)).toBe('New')
      expect(await texts($, surface)).toContain(`${BOB.fingerprint} · policy none`)
      // The addressee is the one New sends to.
      await press($, 'use-prompt', surface)
      const from = rec.runs.length
      await press($, 'send', surface)
      expect(calls(rec, from)).toEqual([`ask --peer ${BOB.fingerprint} -- Which branch?`])
      await press($, 'change', surface)
      await press($, 'tab-chats', surface)
    }
  }
})

test('a note lasts until the next screen, which shows its own', async ($, on) => {
  world(on, { answers: { 'contact export': '{"name":"Alice"}' } })
  await panel($)
  await press($, 'tab-card')
  await press($, 'copy')
  expect((await footer($)).note).toBe('✓ peer file copied')
  await press($, 'tab-chats')
  expect((await footer($)).note).not.toBe('✓ peer file copied')
  await press($, 'tab-card')
  expect((await footer($)).note).toBe('nothing here leaves the machine until you paste it')
})

test('in a thread our messages sit right and theirs left, so direction needs no colour', async ($, on) => {
  const answer = { ...ASKED, ts: '2026-10-04T07:20:00Z', kind: 'answer-received', dir: 'in', record_id: '0199ae10-2b3c-7d4e-8f50-6a7b8c000002', type: 'answer', state: 'pending', text: 'a1b2c3d' } as Ev
  world(on, { contacts: [BOB], threads: [BOB_THREAD], timelines: { [BOB.fingerprint]: [ASKED, answer] } })
  await panel($)
  await press($, `chat-${BOB.fingerprint}`)
  await press($, `thread-${CTX}`)
  for (const surface of SURFACES) {
    const p = await pane($, surface)
    const sides = (await p.findAll({ type: 'Box' })).map((b) => b.props.alignItems).filter(Boolean)
    expect(sides).toEqual(['flex-end', 'flex-start'])
    await p.unmount()
  }
})

test('the current tab keeps its digit: on every tab it is bound, and on a deeper screen it goes back to the first', async ($, on) => {
  world(on, { contacts: [BOB], threads: [BOB_THREAD], timelines: { [BOB.fingerprint]: [ASKED] } })
  await panel($)
  for (const surface of SURFACES) {
    for (const [digit, name, label] of [['1', 'chats', 'Chats 0'], ['2', 'contacts', 'Contacts'], ['3', 'new', 'New'], ['4', 'card', 'Card'], ['5', 'settings', 'Settings']]) {
      await press($, `tab-${name}`, surface)
      const p = await pane($, surface)
      const hot = Object.fromEntries((await p.findAll({ type: 'Button' })).filter((b) => b.props.hotkey).map((b) => [b.props.hotkey, b.key]))
      await p.unmount()
      // Every digit presses a tab, the open one included, so none falls through to the prompt.
      expect(hot).toMatchObject({ '1': 'tab-chats', '2': 'tab-contacts', '3': 'tab-new', '4': 'tab-card', '5': 'tab-settings' })
      expect(await activeTab($)).toBe(label)
      expect(digit).toBe(String(Object.keys(hot).find((k) => hot[k] === `tab-${name}`)))
    }
    // `1` in a thread under Chats: back to the list, still on Chats.
    await press($, 'tab-chats', surface)
    await press($, `chat-${BOB.fingerprint}`, surface)
    await press($, `thread-${CTX}`, surface)
    expect(await texts($, surface)).toContain(`thread ${short(CTX)} with Bob`)
    await press($, 'tab-chats', surface)
    expect(await activeTab($)).toBe('Chats 0')
    expect(await texts($, surface)).toContain('enter opens a conversation')
  }
})

test("pressing the open tab's own digit keeps that tab and its screen", async ($, on) => {
  const rec = world(on, { contacts: [BOB], threads: [BOB_THREAD], timelines: { [BOB.fingerprint]: [ASKED] } })
  await panel($)
  for (const surface of SURFACES) {
    for (const [digit, name, label] of [['1', 'chats', 'Chats 0'], ['2', 'contacts', 'Contacts'], ['3', 'new', 'New'], ['4', 'card', 'Card'], ['5', 'settings', 'Settings']]) {
      await press($, `tab-${name}`, surface)
      const before = await texts($, surface)
      const p = await pane($, surface)
      const own = (await p.findAll({ type: 'Button' })).find((b) => b.props.hotkey === digit)
      await p.unmount()
      expect(own?.key).toBe(`tab-${name}`)
      const from = rec.runs.length
      await press($, own!.key!, surface)
      expect(await activeTab($)).toBe(label)
      expect(await texts($, surface)).toEqual(before)
      expect(calls(rec, from).filter((a) => !/^(card|--version|harness list)( |$)/.test(a))).toEqual([])
    }
  }
})

test('plugin.json holding an array is no object: a switch writes only its own key', async ($, on) => {
  const stored: { value: unknown } = { value: [true, { band: true }] }
  const rec = world(on, { stored })
  await panel($)
  await press($, 'tab-settings')
  await press($, 'band')
  expect(rec.writes.at(-1)).toEqual({ path: `${HOME}/plugin.json`, text: '{"band":false}\n' })
})

test('the pane leaves its last two columns empty (cmux hides the terminal edge)', async ($, on) => {
  world(on, {})
  await panel($)
  for (const surface of SURFACES) {
    const p = await pane($, surface)
    const [root] = await p.findAll({ type: 'Box' })
    expect(root.props.paddingRight).toBe(2)
    await p.unmount()
  }
})

// The mock draws each footer key in body text and its meaning in secondary: the key is a
// sibling Text of its own colour (none: body text), never nested in the secondary one.
test('footer: each key in body text (or its own colour), its meaning in secondary, on every tab', async ($, on) => {
  world(on)
  await panel($)
  for (const tab of ['chats', 'contacts', 'new', 'card', 'settings']) {
    await press($, `tab-${tab}`)
    for (const surface of SURFACES) {
      const p = await pane($, surface)
      const hints = await p.find({ key: 'hints' })
      await p.unmount()
      type El = { type: string; props?: Record<string, unknown>; children: (El | string)[] }
      const [head, ...rest] = (hints?.children ?? []) as El[]
      expect(head.props).toMatchObject({ bold: true, color: P.secondary })
      // The hidden `h` Button (Keys, absolute) closes the row; every hint is a row Box.
      const rows = rest.filter((r) => r.props?.position !== 'absolute')
      expect(rows.length).toBeGreaterThan(2)
      for (const row of rows) {
        expect(row.type).toBe('Box')
        const [key, meaning, ...rest] = row.children as El[]
        expect(rest).toEqual([])
        expect(key.type).toBe('Text')
        expect(key.children.every((x) => typeof x === 'string')).toBe(true)
        if (meaning) {
          expect(meaning.type).toBe('Text')
          expect(meaning.props?.color).toBe(P.secondary)
        }
      }
      // The plain keys (tab, enter, …) carry no colour of their own: body text.
      const plain = rows.filter((r) => ['tab', 'enter', 'h', 'y', 'e'].includes(String(r.props?.key)))
      expect(plain.length).toBeGreaterThan(0)
      for (const r of plain) expect((r.children[0] as El).props?.color).toBeUndefined()
    }
  }
})
