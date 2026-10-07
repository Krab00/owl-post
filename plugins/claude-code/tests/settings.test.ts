// The Settings tab (hooks/settings.tsx) and the Keys list (hooks/keys.tsx): the inbox
// switches, the daemon service with its uninstall question, the doctor rows, the version,
// and every key the panel binds. `owl` is the world of tests/world.ts.
import { expect, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { Check, Harness, Project } from '../hooks/lib'
import { P, SIGN } from '../hooks/ui'
import { HOME, SURFACES, activeTab, calls, current, field, footer, pane, panel, press, texts, type, world, type Answer, type Surface } from './world'

// `owl doctor --json` rows.
const rows = (...r: Check[]) => JSON.stringify(r)

// The label of one Button of the pane, by key.
const labelOf = async ($: Engine, key: string, surface: Surface = 'terminal') => {
  const p = await pane($, surface)
  const b = await p.find({ key })
  await p.unmount()
  return b?.props.label
}

// The hotkey of one Button of the pane, by key.
const hotkeyOf = async ($: Engine, key: string, surface: Surface = 'terminal') => {
  const p = await pane($, surface)
  const b = await p.find({ key })
  await p.unmount()
  return b?.props.hotkey
}

test('live watch turns on and off, keeping the other switch', async ($, on) => {
  const rec = world(on, { stored: { value: { band: false } } })
  await panel($)
  await press($, 'tab-settings')
  for (const [i, surface] of SURFACES.entries()) {
    const watching = i === 0
    await press($, 'watch', surface)
    expect(rec.writes.at(-1)?.path).toBe(`${HOME}/plugin.json`)
    expect(rec.stored.value).toEqual({ band: false, watch: watching })
    const t = await texts($, surface)
    // The current position drawn chosen (inverse), the other one the Button that flips it.
    expect(t[t.indexOf('Live watch') + 1]).toBe(watching ? ' on ' : ' off ')
    expect(await labelOf($, 'watch', surface)).toBe(watching ? 'off' : 'on')
    expect((await footer($, surface)).note).toBe(
      watching ? '✓ live watch on — from the next session start' : '✓ live watch off — new sessions do not wake',
    )
  }
})

test('the band turns off and on, keeping the other switch', async ($, on) => {
  const rec = world(on, { stored: { value: { watch: true } } })
  await panel($)
  await press($, 'tab-settings')
  for (const [i, surface] of SURFACES.entries()) {
    const off = i === 0
    await press($, 'band', surface)
    expect(rec.writes.at(-1)?.path).toBe(`${HOME}/plugin.json`)
    expect(rec.stored.value).toEqual({ watch: true, band: !off })
    const t = await texts($, surface)
    expect(t[t.indexOf('Band above the prompt') + 1]).toBe(off ? ' off ' : ' on ')
    expect(await labelOf($, 'band', surface)).toBe(off ? 'on' : 'off')
    expect((await footer($, surface)).note).toBe(off ? '✓ band off' : '✓ band on')
  }
})

test('each switch row says under it what the switch does', async ($, on) => {
  world(on)
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    expect(t.indexOf('wake new sessions when a message arrives')).toBeGreaterThan(t.indexOf('Live watch'))
    expect(t.indexOf('shows the unseen count and opens this panel')).toBeGreaterThan(t.indexOf('Band above the prompt'))
  }
})

test('the switch buttons offer the opposite of the current state', async ($, on) => {
  world(on, { stored: { value: { band: true, watch: false } } })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    expect(await labelOf($, 'watch', surface)).toBe('on')
    expect(await labelOf($, 'band', surface)).toBe('off')
    await press($, 'watch', surface)
    await press($, 'band', surface)
    expect(await labelOf($, 'watch', surface)).toBe('off')
    expect(await labelOf($, 'band', surface)).toBe('on')
    await press($, 'watch', surface)
    await press($, 'band', surface)
  }
})

test('i installs the daemon service; a failure says why', async ($, on) => {
  const answers: Record<string, string | Answer> = { install: 'installed launchd service' }
  const rec = world(on, { answers })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await press($, 'install', surface)
    expect(calls(rec, from)).toEqual(['install'])
    expect((await footer($, surface)).note).toBe('✓ installed launchd service')
  }
  answers['install'] = { exitCode: 1, stderr: 'owl: launchctl failed' }
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await press($, 'install', surface)
    expect(calls(rec, from)).toEqual(['install'])
    expect((await footer($, surface)).note).toBe('✗ owl: launchctl failed')
  }
})

test('uninstall asks first and runs only on confirm', async ($, on) => {
  const rec = world(on, { answers: { uninstall: 'removed launchd service' } })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    let from = rec.runs.length
    await press($, 'uninstall', surface)
    expect(calls(rec, from)).toEqual([])
    expect(await texts($, surface)).toContain('Uninstall the daemon service?')
    await press($, 'uninstall-cancel', surface)
    expect(calls(rec, from)).toEqual([])
    expect(await texts($, surface)).not.toContain('Uninstall the daemon service?')
    await press($, 'uninstall', surface)
    from = rec.runs.length
    await press($, 'uninstall-confirm', surface)
    expect(calls(rec, from)).toEqual(['uninstall'])
    expect((await footer($, surface)).note).toBe('✓ removed launchd service')
  }
})

test('Uninstall has x (as the mock), its Confirm and its Cancel none; Install has i', async ($, on) => {
  world(on)
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    expect(await hotkeyOf($, 'install', surface)).toBe('i')
    expect(await hotkeyOf($, 'uninstall', surface)).toBe('x')
    await press($, 'uninstall', surface)
    expect(await hotkeyOf($, 'uninstall-confirm', surface)).toBeUndefined()
    expect(await hotkeyOf($, 'uninstall-cancel', surface)).toBeUndefined()
    await press($, 'uninstall-cancel', surface)
  }
})

test('leaving the tab drops the uninstall question', async ($, on) => {
  world(on)
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await press($, 'uninstall', surface)
    expect(await texts($, surface)).toContain('Uninstall the daemon service?')
    await press($, 'tab-card', surface)
    await press($, 'tab-settings', surface)
    expect(await texts($, surface)).not.toContain('Uninstall the daemon service?')
    expect(await labelOf($, 'uninstall', surface)).toBe('Uninstall')
  }
})

test('d runs the doctor and lists every check', async ($, on) => {
  const rec = world(on, {
    answers: {
      'doctor --json': rows(
        { check: 'key', status: 'ok', detail: 'owl:pfbrpuq3pbfblrnd' },
        { check: 'daemon', status: 'ok', detail: 'reachable' },
      ),
    },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await press($, 'doctor', surface)
    expect(calls(rec, from)).toEqual(['doctor --json'])
    const t = await texts($, surface)
    for (const shown of ['key', 'owl:pfbrpuq3pbfblrnd', 'daemon', 'reachable', '✓']) expect(t).toContain(shown)
    expect((await footer($, surface)).note).toBe('✓ doctor: all checks ok')
  }
})

test('a failed check is marked and counted, a warn one too', async ($, on) => {
  world(on, {
    answers: {
      'doctor --json': {
        exitCode: 1,
        stdout: rows(
          { check: 'key', status: 'ok', detail: 'owl:pfbrpuq3pbfblrnd' },
          { check: 'endpoints', status: 'warn', detail: 'none configured' },
          { check: 'daemon', status: 'fail', detail: 'not reachable' },
        ),
        stderr: 'owl: 1 check(s) failed',
      },
    },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await press($, 'doctor', surface)
    const t = await texts($, surface)
    // The mock's words, each in its colour.
    expect(t[t.indexOf('key') - 1]).toBe('ok')
    expect(t[t.indexOf('endpoints') - 1]).toBe('warn')
    expect(t[t.indexOf('daemon') - 1]).toBe('fail')
    const p = await pane($, surface)
    const all = await p.findAll({ type: 'Text' })
    await p.unmount()
    expect(all.find((x) => x.text === 'ok')?.props.color).toBe(P.ok)
    expect(all.find((x) => x.text === 'warn')?.props.color).toBe(P.wait)
    expect(all.find((x) => x.text === 'fail')?.props.color).toBe(P.bad)
    expect((await footer($, surface)).note).toBe('✗ doctor: 1 check(s) failed')
  }
})

test('only fail rows count as failed; warn rows do not', async ($, on) => {
  const answers: Record<string, string | Answer> = {
    'doctor --json': {
      exitCode: 1,
      stdout: rows(
        { check: 'key', status: 'warn', detail: 'old key format' },
        { check: 'endpoints', status: 'warn', detail: 'none configured' },
        { check: 'daemon', status: 'fail', detail: 'not reachable' },
      ),
      stderr: 'owl: 1 check(s) failed',
    },
  }
  world(on, { answers })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await press($, 'doctor', surface)
    expect((await footer($, surface)).note).toBe('✗ doctor: 1 check(s) failed')
  }
  answers['doctor --json'] = rows(
    { check: 'key', status: 'ok', detail: 'owl:pfbrpuq3pbfblrnd' },
    { check: 'endpoints', status: 'warn', detail: 'none configured' },
  )
  for (const surface of SURFACES) {
    await press($, 'doctor', surface)
    expect((await footer($, surface)).note).toBe('✓ doctor: all checks ok')
  }
})

test('doctor with owl missing says so plainly', async ($, on) => {
  world(on, { missing: true })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await press($, 'doctor', surface)
    expect((await footer($, surface)).note).toBe('✗ owl could not start: is it installed and on PATH?')
  }
})

// Real `owl doctor --json` repeats check names (one row per harness), so the rows are keyed
// by index and every one is drawn.
test('two checks of the same name are both drawn', async ($, on) => {
  world(on, {
    answers: {
      'doctor --json': rows(
        { check: 'harness', status: 'ok', detail: 'claude: claude' },
        { check: 'harness', status: 'ok', detail: 'codex: codex' },
      ),
    },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await press($, 'doctor', surface)
    const t = await texts($, surface)
    expect(t.filter((x) => x === 'harness')).toHaveLength(2)
    expect(t).toContain('claude: claude')
    expect(t).toContain('codex: codex')
  }
})

test('doctor json that is not an array of rows draws no rows', async ($, on) => {
  world(on, { answers: { 'doctor --json': '{"check":"key"}' } })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await press($, 'doctor', surface)
    expect((await footer($, surface)).note).toBe('✗ {"check":"key"}')
    const t = await texts($, surface)
    expect(t).not.toContain('key')
    expect(t).not.toContain('✓')
  }
})

test('a doctor answer that is not rows clears the list and says the first line', async ($, on) => {
  const answers: Record<string, string | Answer> = {
    'doctor --json': rows({ check: 'key', status: 'ok', detail: 'owl:pfbrpuq3pbfblrnd' }),
  }
  world(on, { answers })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) await press($, 'doctor', surface)
  expect(await texts($)).toContain('key')
  answers['doctor --json'] = 'not json'
  for (const surface of SURFACES) {
    await press($, 'doctor', surface)
    expect((await footer($, surface)).note).toBe('✗ not json')
    expect(await texts($, surface)).not.toContain('key')
  }
  answers['doctor --json'] = { exitCode: 2, stderr: 'owl: no key' }
  for (const surface of SURFACES) {
    await press($, 'doctor', surface)
    expect((await footer($, surface)).note).toBe('✗ owl: no key')
    expect(await texts($, surface)).not.toContain('key')
  }
})

test('the version comes from owl --version and u updates', async ($, on) => {
  const answers: Record<string, string | Answer> = { '--version': 'owl 0.3.0', update: 'updated to owl 0.3.1' }
  const rec = world(on, { answers })
  await panel($)
  await press($, 'tab-settings')
  expect(calls(rec)).toContain('--version')
  expect(await texts($)).toContain('0.3.0') // the number only, as the mock
  answers['--version'] = { exitCode: 127, stderr: 'not found' }
  await press($, 'tab-card')
  await press($, 'tab-settings')
  expect(await texts($)).toContain('- version unknown')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await press($, 'update', surface)
    expect(calls(rec, from)).toEqual(['update'])
    expect((await footer($, surface)).note).toBe('✓ updated to owl 0.3.1')
  }
})

test('u updates with a ten-minute timeout', async ($, on) => {
  const rec = world(on, { answers: { update: 'updated to owl 0.3.1' } })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await press($, 'update', surface)
    const run = rec.runs.slice(from).find((r) => r.args === 'update')
    expect(run?.timeoutMs).toBe(600_000)
  }
})

test('the footer shows the settings hints and the default note', async ($, on) => {
  world(on)
  await panel($)
  await press($, 'tab-settings')
  const f = await footer($)
  expect(f.hints).toBe('keystab move↑↓ scrollenter use for draftse edits scana addp project pathn project namer remove projectd doctoru update')
  expect(f.note).toBe('switches are stored in plugin.json of the owlpost home')
})

// The Keys list, row by row as keys.tsx declares it: the key and right after it what it does.
const KEY_ROWS: [string, string][] = [
  ['/owlpost:panel', 'open or focus the panel, again to close'],
  ['ctrl+x tab', 'move the keys into the panel'],
  ['click', 'a click in the panel does the same'],
  ['1–5', 'tabs'],
  ['tab', 'move the focus ring'],
  ['↑ ↓', 'scroll the panel'],
  ['enter', 'press the focused control'],
  ['esc', 'keys back to the prompt, panel stays'],
  ['h q', 'this list, close the panel'],
  ['in a text field', 'letters type; tab leaves the field'],
  ['f', 'filter'],
  ['n', 'new thread, add contact'],
  ['b', 'back'],
  ['k p', 'card, policy'],
  ['a v', 'archive, show archived'],
  ['r', 'reply'],
  ['c', 'cite the message in the prompt'],
  ['a', 'apply it in the prompt'],
  ['t u', 'cite, apply the whole thread'],
  ['y', 'copy'],
  ['o w', 'allow once, allow always'],
  ['d', 'draft with Claude'],
  ['e s', 'edit, send'],
  ['x', 'reject, deny, remove, delete'],
]

test('the Keys list has every section, every row, the paragraph and its note', async ($, on) => {
  world(on)
  await panel($)
  await press($, 'tab-settings')
  await press($, 'keys')
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    for (const title of ['From the prompt', 'Everywhere in the panel', 'Lists', 'Thread', 'Requests', 'Left to Claude Code'])
      expect(t).toContain(title)
    let at = 0
    for (const [k, what] of KEY_ROWS) {
      const i = t.indexOf(k, at)
      expect(i).toBeGreaterThan(-1)
      expect(t[i + 1]).toBe(what)
      at = i + 1
    }
    expect(t).toContain(
      'The panel binds bare letters and digits only. They work while the panel holds the keyboard and the focus is on a row or button, never while you type in a field. Arrows, tab, enter, esc and ctrl chords keep their Claude Code meaning.',
    )
    expect((await footer($, surface)).note).toBe('')
  }
})

test('h lists the keys and b returns to the tab it was opened from', async ($, on) => {
  world(on)
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await press($, 'keys', surface)
    const t = await texts($, surface)
    expect(t).toContain('Keys')
    expect(t).toContain('/owlpost:panel')
    expect(t).toContain('reject, deny, remove, delete')
    expect(t).toContain('cite the message in the prompt')
    expect(t).toContain('archive, show archived')
    expect(await activeTab($)).toBeUndefined()
    expect((await footer($, surface)).hints).toContain('b back')
    await press($, 'back', surface)
    expect(await activeTab($)).toBe('Settings')
  }
  await press($, 'tab-card')
  for (const surface of SURFACES) {
    await press($, 'keys', surface)
    await press($, 'back', surface)
    expect(await activeTab($)).toBe('Card')
  }
})

// ------------------------------------------------------------------- harnesses (`owl harness`)

// One row of `owl harness list --json`.
const hz = (o: Partial<Harness> & Pick<Harness, 'name'>): Harness => ({
  cmd: [o.name], answer_path: '', enabled: true, drafting: false, found: true, path: `/usr/local/bin/${o.name}`, ...o,
})

// One element of the pane by key (an Input may be drawn under a re-keyed name, see `fields`).
async function el($: Engine, key: string, surface: Surface = 'terminal') {
  const p = await pane($, surface)
  const found = await p.find({ key: current(key, surface) })
  await p.unmount()
  return found
}

// Moves the pane's focus ring as the person's Tab does.
const ring = ($: Engine, element?: string) =>
  $.ui.focus({ component: 'Pane', requestId: 'owlpost', element, plugin: element ? 'owlpost' : undefined, origin: { kind: 'person' } })

test('the harness rows draw the name, the command, the status and the drafts badge', async ($, on) => {
  world(on, {
    harnesses: [
      hz({ name: 'claude', drafting: true }),
      hz({ name: 'codex', cmd: ['codex', 'run'] }),
      hz({ name: 'fake', found: false, path: null }),
      hz({ name: 'old', enabled: false }),
    ],
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    expect((await el($, 'harness-claude', surface))?.props.label).toBe('claude')
    expect((await el($, 'harness-fake', surface))?.props.label).toBe('fake')
    const t = await texts($, surface)
    expect(t).toContain('claude') // the command of the first row
    // A multi-word command draws its program only on the row (Edit still holds it whole).
    expect(t).toContain('codex')
    expect(t).not.toContain('codex run')
    const p = await pane($, surface)
    const all = await p.findAll({ type: 'Text' })
    const byText = (x: string) => all.find((y) => y.text === x)
    expect(byText(`${SIGN.ok} found`)?.props.color).toBe(P.ok)
    expect(byText('warn not found')?.props.color).toBe(P.wait)
    expect(byText(`${SIGN.bad} disabled`)?.props.color).toBe(P.bad)
    const badge = byText(' drafts ')
    expect(badge?.props.color).toBeUndefined() // the mock's light-on-dark badge: inverse body text
    expect(badge?.props.inverse).toBe(true)
    await p.unmount()
  }
})

test('an empty harness list says how scan adds the known ones', async ($, on) => {
  world(on, { harnesses: [] })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    expect(await texts($, surface)).toContain('No harnesses. s: Scan PATH for harnesses adds the known ones found.')
  }
})

test('the first harness row is selected by default and carries the actions', async ($, on) => {
  world(on, { harnesses: [hz({ name: 'claude' }), hz({ name: 'codex' })] })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    expect(t.filter((x) => x === SIGN.selected)).toHaveLength(1)
    expect(t[t.indexOf(SIGN.selected) + 1]).toBe('claude') // the selected row's command
    const p = await pane($, surface)
    expect((await p.find({ key: 'claude' }))?.props.backgroundColor).toBe(P.selected)
    expect((await p.find({ key: 'codex' }))?.props.backgroundColor).toBeUndefined()
    const keys = (await p.findAll({ type: 'Button' })).map((b) => b.props.key)
    await p.unmount()
    // Edit and Remove sit under the selected row only, between its button and the next row's.
    expect(keys.indexOf('harness-edit')).toBeGreaterThan(keys.indexOf('harness-claude'))
    expect(keys.indexOf('harness-edit')).toBeLessThan(keys.indexOf('harness-codex'))
    expect(keys.filter((k) => k === 'harness-edit')).toHaveLength(1)
    expect(keys.filter((k) => k === 'harness-remove')).toHaveLength(1)
  }
})

test('the ring moves the selection; e and x act on the ringed row', async ($, on) => {
  const rec = world(on, {
    harnesses: [hz({ name: 'claude' }), hz({ name: 'codex', cmd: ['codex', 'run'] })],
    answers: { 'harness remove -- codex': 'removed harness codex' },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await ring($, 'harness-codex')
    const t = await texts($, surface)
    expect(t[t.indexOf(SIGN.selected) + 1]).toBe('codex') // the row draws the program only
    // e opens the ringed row's command.
    await press($, 'harness-edit', surface)
    expect(await field($, 'harness-cmd-edit', surface)).toBe('codex run')
    // Moving the ring away closes the field without running anything.
    await ring($, 'harness-claude')
    expect(await el($, 'harness-cmd-edit', surface)).toBeUndefined()
    // x removes the ringed row.
    await ring($, 'harness-codex')
    const from = rec.runs.length
    await press($, 'harness-remove', surface)
    expect(calls(rec, from)).toEqual(['harness remove -- codex', 'harness list --json'])
    expect((await footer($, surface)).note).toBe('✓ removed harness codex')
  }
  await ring($, 'harness-claude')
})

test('pressing a harness row uses it for drafts', async ($, on) => {
  const rec = world(on, {
    harnesses: [hz({ name: 'claude' }), hz({ name: 'codex' })],
    answers: { 'harness use -- codex': 'codex now drafts answers' },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await press($, 'harness-codex', surface)
    expect(calls(rec, from)).toEqual(['harness use -- codex', 'harness list --json'])
    expect((await footer($, surface)).note).toBe('✓ codex now drafts answers')
    // Every row is still drawn.
    expect(await el($, 'harness-claude', surface)).toBeDefined()
    expect(await el($, 'harness-codex', surface)).toBeDefined()
  }
})

test('using a disabled harness says what owl said', async ($, on) => {
  const rec = world(on, {
    harnesses: [hz({ name: 'claude' }), hz({ name: 'old', enabled: false })],
    answers: { 'harness use -- old': { exitCode: 1, stderr: 'owl: harness old is disabled: enable it in harnesses.json' } },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await press($, 'harness-old', surface)
    expect(calls(rec, from)).toEqual(['harness use -- old', 'harness list --json'])
    expect((await footer($, surface)).note).toBe('✗ owl: harness old is disabled: enable it in harnesses.json')
    expect(await texts($, surface)).toContain(`${SIGN.bad} disabled`)
  }
})

test('x removes the selected harness and the reloaded list no longer holds it', async ($, on) => {
  let list = [hz({ name: 'claude' }), hz({ name: 'codex' })]
  const rec = world(on, {
    answers: (args) => {
      if (args === 'harness remove -- codex') {
        list = list.filter((h) => h.name !== 'codex')
        return 'removed harness codex'
      }
      if (args === 'harness list --json') return JSON.stringify(list)
      return undefined
    },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    // Back to a list of two, reloaded by entering the tab again.
    list = [hz({ name: 'claude' }), hz({ name: 'codex' })]
    await press($, 'tab-card', surface)
    await press($, 'tab-settings', surface)
    await ring($, 'harness-codex')
    const from = rec.runs.length
    await press($, 'harness-remove', surface)
    expect(calls(rec, from)).toEqual(['harness remove -- codex', 'harness list --json'])
    expect((await footer($, surface)).note).toBe('✓ removed harness codex')
    expect(await el($, 'harness-codex', surface)).toBeUndefined() // the target is gone
    expect(await el($, 'harness-claude', surface)).toBeDefined() // the other row stayed
  }
  await ring($, 'harness-claude')
})

test('removing the drafting harness is refused: the list stays and the note says why', async ($, on) => {
  const rec = world(on, {
    harnesses: [hz({ name: 'claude', drafting: true }), hz({ name: 'codex' })],
    answers: { 'harness remove -- claude': { exitCode: 1, stderr: 'owl: claude drafts answers — choose another with owl harness use first' } },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await ring($, 'harness-claude')
    const from = rec.runs.length
    await press($, 'harness-remove', surface)
    expect(calls(rec, from)).toEqual(['harness remove -- claude', 'harness list --json'])
    expect((await footer($, surface)).note).toBe('✗ owl: claude drafts answers — choose another with owl harness use first')
    // Both rows stay, the badge included.
    expect(await el($, 'harness-claude', surface)).toBeDefined()
    expect(await el($, 'harness-codex', surface)).toBeDefined()
    expect(await texts($, surface)).toContain(' drafts ')
  }
})

test('e opens the command prefilled; Enter saves it word by word and closes the field', async ($, on) => {
  let list = [hz({ name: 'claude' }), hz({ name: 'codex', cmd: ['codex', 'run', '--fast'] })]
  const rec = world(on, {
    answers: (args) => {
      if (args === 'harness edit codex -- codex run --slow') {
        list = [list[0], hz({ name: 'codex', cmd: ['codex', 'run', '--slow'] })]
        return 'harness codex now runs codex run --slow'
      }
      if (args === 'harness list --json') return JSON.stringify(list)
      return undefined
    },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    // Back to the old command, reloaded by entering the tab again.
    list = [hz({ name: 'claude' }), hz({ name: 'codex', cmd: ['codex', 'run', '--fast'] })]
    await press($, 'tab-card', surface)
    await press($, 'tab-settings', surface)
    await ring($, 'harness-codex')
    await press($, 'harness-edit', surface)
    expect(await field($, 'harness-cmd-edit', surface)).toBe('codex run --fast') // prefilled
    const from = rec.runs.length
    await type($, 'harness-cmd-edit', 'codex run --slow', 'submit', surface)
    expect(calls(rec, from)).toEqual(['harness edit codex -- codex run --slow', 'harness list --json'])
    expect((await footer($, surface)).note).toBe('✓ harness codex now runs codex run --slow')
    expect(await el($, 'harness-cmd-edit', surface)).toBeUndefined() // a success closes the field
    const t = await texts($, surface)
    // The reloaded row draws the program only now; the note above holds the whole command.
    expect(t).not.toContain('codex run --slow')
    expect(t).not.toContain('codex run --fast')
    expect(t).toContain('claude') // the other row unchanged
  }
  await ring($, 'harness-claude')
})

test('an empty edit runs nothing and keeps the field', async ($, on) => {
  const rec = world(on, { harnesses: [hz({ name: 'claude' }), hz({ name: 'codex', cmd: ['codex', 'run'] })] })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await ring($, 'harness-codex')
    await press($, 'harness-edit', surface)
    const from = rec.runs.length
    await type($, 'harness-cmd-edit', '   ', 'submit', surface)
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ type a command first')
    expect(await el($, 'harness-cmd-edit', surface)).toBeDefined()
    expect(await field($, 'harness-cmd-edit', surface)).toBe('   ') // the text is kept
    await ring($, 'harness-claude') // closes the field for the next surface
  }
})

test('editing a harness whose name starts with - is refused before owl runs', async ($, on) => {
  const rec = world(on, { harnesses: [hz({ name: 'claude' }), hz({ name: '-x', cmd: ['x'] })] })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await ring($, 'harness--x')
    await press($, 'harness-edit', surface)
    expect(await field($, 'harness-cmd-edit', surface)).toBe('x')
    const from = rec.runs.length
    await type($, 'harness-cmd-edit', 'x --fast', 'submit', surface)
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ a harness name cannot start with -')
    expect(await field($, 'harness-cmd-edit', surface)).toBe('x --fast') // the text is kept
    await ring($, 'harness-claude')
  }
})

test('a failed edit keeps the field and its text', async ($, on) => {
  const rec = world(on, {
    harnesses: [hz({ name: 'claude' }), hz({ name: 'codex', cmd: ['codex', 'run'] })],
    answers: { 'harness edit codex -- codex run --slow': { exitCode: 1, stderr: 'owl: harness codex is disabled: enable it first' } },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await ring($, 'harness-codex')
    await press($, 'harness-edit', surface)
    const from = rec.runs.length
    await type($, 'harness-cmd-edit', 'codex run --slow', 'submit', surface)
    expect(calls(rec, from)).toEqual(['harness edit codex -- codex run --slow', 'harness list --json'])
    expect((await footer($, surface)).note).toBe('✗ owl: harness codex is disabled: enable it first')
    expect(await el($, 'harness-cmd-edit', surface)).toBeDefined()
    expect(await field($, 'harness-cmd-edit', surface)).toBe('codex run --slow') // the text is kept
    await ring($, 'harness-claude') // closes the field for the next surface
  }
})

test('the edit field closes when the selection moves to another row and does not come back', async ($, on) => {
  const rec = world(on, { harnesses: [hz({ name: 'claude' }), hz({ name: 'codex', cmd: ['codex', 'run'] })] })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await ring($, 'harness-codex')
    await press($, 'harness-edit', surface)
    await type($, 'harness-cmd-edit', 'codex run --slow', 'change', surface) // typed, not submitted
    const from = rec.runs.length
    await ring($, 'harness-claude')
    expect(await el($, 'harness-cmd-edit', surface)).toBeUndefined()
    expect(calls(rec, from)).toEqual([])
    // Back on codex the field stays closed; the typed text never ran.
    await ring($, 'harness-codex')
    expect(await el($, 'harness-cmd-edit', surface)).toBeUndefined()
    expect(calls(rec, from)).toEqual([])
    await ring($, 'harness-claude')
  }
})

test('a command too long for the field is not opened for editing', async ($, on) => {
  const rec = world(on, { harnesses: [hz({ name: 'big', cmd: ['x'.repeat(10_001)] })] })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await ring($, 'harness-big')
    const from = rec.runs.length
    await press($, 'harness-edit', surface)
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ the command has 10001 characters, too many to edit here')
    expect(await el($, 'harness-cmd-edit', surface)).toBeUndefined()
  }
  await ring($)
})

test('a opens the add fields; Enter in the name hands over, Add runs, clears and closes', async ($, on) => {
  let list = [hz({ name: 'claude' })]
  const rec = world(on, {
    answers: (args) => {
      if (args === 'harness add gemini -- gemini --fast') {
        list = [...list, hz({ name: 'gemini', cmd: ['gemini', '--fast'] })]
        return 'added harness gemini'
      }
      if (args === 'harness list --json') return JSON.stringify(list)
      return undefined
    },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await press($, 'harness-add', surface)
    expect(await el($, 'harness-name', surface)).toBeDefined()
    expect(await el($, 'harness-cmd', surface)).toBeDefined()
    let from = rec.runs.length
    await type($, 'harness-name', 'gemini', 'submit', surface) // Enter stores the name, no owl run
    expect(calls(rec, from)).toEqual([])
    expect(await el($, 'harness-name', surface)).toBeDefined() // the block stays open
    from = rec.runs.length
    await type($, 'harness-cmd', 'gemini --fast', 'submit', surface)
    expect(calls(rec, from)).toEqual(['harness add gemini -- gemini --fast', 'harness list --json'])
    expect((await footer($, surface)).note).toBe('✓ added harness gemini')
    // The block closed, the new row is drawn, the old one stayed.
    expect(await el($, 'harness-name', surface)).toBeUndefined()
    expect(await el($, 'harness-cmd', surface)).toBeUndefined()
    expect(await el($, 'harness-gemini', surface)).toBeDefined()
    expect(await el($, 'harness-claude', surface)).toBeDefined()
    // Reopened, both fields are empty.
    await press($, 'harness-add', surface)
    expect(await field($, 'harness-name', surface)).toBe('')
    expect(await field($, 'harness-cmd', surface)).toBe('')
    await press($, 'harness-add', surface) // close for the next surface
    list = [hz({ name: 'claude' })]
  }
})

test('add refuses an empty name, an empty command and a dashed name without running owl', async ($, on) => {
  const rec = world(on, { harnesses: [hz({ name: 'claude' })] })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await press($, 'harness-add', surface)
    let from = rec.runs.length
    await type($, 'harness-cmd', 'gemini --fast', 'submit', surface) // no name yet
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ type a name first')
    await type($, 'harness-name', 'gemini', 'submit', surface)
    from = rec.runs.length
    await type($, 'harness-cmd', '   ', 'submit', surface) // a name, no command
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ type a command first')
    await type($, 'harness-name', '-x', 'change', surface)
    from = rec.runs.length
    await type($, 'harness-cmd', 'x --fast', 'submit', surface) // a dashed name
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ a harness name cannot start with -')
    // Every refusal kept both fields.
    expect(await field($, 'harness-name', surface)).toBe('-x')
    expect(await field($, 'harness-cmd', surface)).toBe('x --fast')
    // Clear and close for the next surface.
    await type($, 'harness-name', '', 'change', surface)
    await type($, 'harness-cmd', '', 'change', surface)
    await press($, 'harness-add', surface)
  }
})

test('a failed add keeps both fields', async ($, on) => {
  const rec = world(on, {
    harnesses: [hz({ name: 'claude' })],
    answers: { 'harness add claude -- claude': { exitCode: 1, stderr: 'owl: harness claude exists — use owl harness edit' } },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await press($, 'harness-add', surface)
    await type($, 'harness-name', 'claude', 'submit', surface)
    const from = rec.runs.length
    await type($, 'harness-cmd', 'claude', 'submit', surface)
    expect(calls(rec, from)).toEqual(['harness add claude -- claude', 'harness list --json'])
    expect((await footer($, surface)).note).toBe('✗ owl: harness claude exists — use owl harness edit')
    expect(await el($, 'harness-name', surface)).toBeDefined()
    expect(await field($, 'harness-name', surface)).toBe('claude')
    expect(await field($, 'harness-cmd', surface)).toBe('claude')
    // Clear and close for the next surface.
    await type($, 'harness-name', '', 'change', surface)
    await type($, 'harness-cmd', '', 'change', surface)
    await press($, 'harness-add', surface)
  }
})

test('the settings footer hints cover the harness keys', async ($, on) => {
  world(on, { harnesses: [hz({ name: 'claude' })] })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const f = await footer($, surface)
    for (const hint of ['tab move', 'enter use for drafts', 'e edit', 's scan', 'a add', 'd doctor', 'u update'])
      expect(f.hints).toContain(hint)
  }
})

test('the harness hotkeys stay unique on the screen', async ($, on) => {
  world(on, { harnesses: [hz({ name: 'claude' }), hz({ name: 'codex' })] })
  await panel($)
  await press($, 'tab-settings')
  await ring($, 'harness-claude')
  for (const surface of SURFACES) {
    const p = await pane($, surface)
    const hotkeys = (await p.findAll({ type: 'Button' })).map((b) => b.props.hotkey).filter(Boolean)
    await p.unmount()
    expect(new Set(hotkeys).size).toBe(hotkeys.length)
    expect(await hotkeyOf($, 'harness-edit', surface)).toBe('e')
    expect(await hotkeyOf($, 'harness-remove', surface)).toBe('x')
    expect(await hotkeyOf($, 'harness-add', surface)).toBe('a')
    expect(await hotkeyOf($, 'harness-scan', surface)).toBe('s')
  }
})

// One `x` on the screen: the selected harness row's Remove holds it while the ring is on that
// row or its actions, the daemon's Uninstall otherwise; never both, and pressing Uninstall
// still only asks.
test('x is Remove on a ringed harness row and Uninstall elsewhere, never both', async ($, on) => {
  const rec = world(on, { harnesses: [hz({ name: 'claude' }), hz({ name: 'codex' })] })
  await panel($)
  await press($, 'tab-settings')
  const xs = async (surface: Surface) => {
    const p = await pane($, surface)
    const keys = (await p.findAll({ type: 'Button' })).filter((b) => b.props.hotkey === 'x').map((b) => b.key)
    await p.unmount()
    return keys
  }
  for (const surface of SURFACES) {
    await ring($, 'install')
    expect(await xs(surface)).toEqual(['uninstall'])
    for (const el of ['harness-codex', 'harness-use', 'harness-edit', 'harness-remove']) {
      await ring($, el)
      expect(await xs(surface)).toEqual(['harness-remove'])
    }
    await ring($, 'harness-scan')
    expect(await xs(surface)).toEqual(['uninstall'])
    const from = rec.runs.length
    await press($, 'uninstall', surface)
    expect(await texts($, surface)).toContain('Uninstall the daemon service?')
    expect(calls(rec, from)).not.toContain('uninstall')
    await press($, 'uninstall-cancel', surface)
  }
})

// A name and a program a hostile `owl harness list --json` could hold: line breaks, control
// characters and a program far over the drawing limit. Nothing the engine refuses may be drawn.
test('a hostile harness name and command draw on one line each', async ($, on) => {
  const BAD = '\u0007\u001b[2J\u0085\u009f' // BEL, ESC (a screen clear), NEL, APC
  const name = `ev\nil${BAD}`
  const long = `${'y'.repeat(12_000)}${BAD}`
  world(on, { harnesses: [hz({ name, cmd: [long, 'run'], found: false, path: null })] })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const p = await pane($, surface)
    const all = (await p.findAll({ type: 'Text' })).map((x) => x.text)
    const labels = (await p.findAll({ type: 'Button' })).map((x) => String(x.props.label ?? x.text))
    await p.unmount()
    for (const x of all) expect(x).not.toMatch(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/)
    for (const l of labels) expect(l).not.toMatch(/[\u0000-\u001f\u007f-\u009f]/)
    expect(labels).toContain('ev il  [2J  ') // the name on one line
    // The program on one line, cut (a joined command would say 12009): only it is drawn.
    expect(all.some((x) => x.endsWith('… cut, 12007 characters in all'))).toBe(true)
    expect(all).toContain('warn not found')
  }
})

test('a command with line breaks, tabs and control characters goes into the edit field on one line', async ($, on) => {
  const BAD = '\u0007\u001b[2J\u0085\u009f'
  world(on, { harnesses: [hz({ name: 'odd', cmd: ['run\n--x', `a${BAD}\tb`] })] })
  await panel($)
  await press($, 'tab-settings')
  await press($, 'harness-edit')
  expect(await field($, 'harness-cmd-edit')).toBe('run --x a  [2J   b')
  await ring($)
})

test('a command of exactly INPUT_MAX characters still opens for editing', async ($, on) => {
  world(on, { harnesses: [hz({ name: 'big', cmd: ['x'.repeat(10_000)] })] })
  await panel($)
  await press($, 'tab-settings')
  await press($, 'harness-edit')
  expect(await field($, 'harness-cmd-edit')).toBe('x'.repeat(10_000))
  await ring($)
})

test('edit and add pass each word of the command as its own argument', async ($, on) => {
  const rec = world(on, { harnesses: [hz({ name: 'codex', cmd: ['codex'] })] })
  await panel($)
  await press($, 'tab-settings')
  await press($, 'harness-edit')
  let from = rec.runs.length
  await type($, 'harness-cmd-edit', '  codex   exec\t--json  ', 'submit')
  expect(rec.runs.slice(from)[0].argv).toEqual(['harness', 'edit', 'codex', '--', 'codex', 'exec', '--json'])
  await press($, 'harness-add')
  await type($, 'harness-name', ' kimi ', 'submit')
  from = rec.runs.length
  await type($, 'harness-cmd', 'kimi -p {prompt}', 'submit')
  expect(rec.runs.slice(from)[0].argv).toEqual(['harness', 'add', 'kimi', '--', 'kimi', '-p', '{prompt}'])
})

test('a opens and closes the add block; what was typed in the edit field survives leaving the tab', async ($, on) => {
  world(on, { harnesses: [hz({ name: 'codex', cmd: ['codex'] })] })
  await panel($)
  await press($, 'tab-settings')
  await press($, 'harness-add')
  expect(await el($, 'harness-name')).toBeDefined()
  await press($, 'harness-add')
  expect(await el($, 'harness-name')).toBeUndefined()
  expect(await el($, 'harness-cmd')).toBeUndefined()
  await press($, 'harness-edit')
  await type($, 'harness-cmd-edit', 'codex --typed', 'change')
  await press($, 'tab-card')
  await press($, 'tab-settings')
  expect(await field($, 'harness-cmd-edit')).toBe('codex --typed')
  await ring($)
})

// ------------------------------------------------------------------ projects

const pj = (name: string, path = `/code/${name}`, exists = true): Project => ({ name, path, exists })

test('no projects: one line says none are registered and how to add one', async ($, on) => {
  world(on)
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    expect(t).toContain("No projects registered. Add one below: a path (empty = this session's directory), then Enter.")
    expect(await el($, 'project-remove', surface)).toBeUndefined()
    expect(await el($, 'project-path', surface)).toBeDefined()
    expect(await el($, 'project-name', surface)).toBeDefined()
  }
})

test('the project rows draw name → path, the first selected, a gone checkout marked missing', async ($, on) => {
  world(on, { projects: [pj('github.com/acme/app'), pj('old', '/gone/old', false)] })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    expect(t).not.toContain("No projects registered. Add one below: a path (empty = this session's directory), then Enter.")
    expect(t).toContain(' → /code/github.com/acme/app')
    expect(t).toContain(' → /gone/old')
    expect(t.filter((x) => x === `${SIGN.bad} missing`)).toHaveLength(1)
    expect(t.indexOf(`${SIGN.bad} missing`)).toBeGreaterThan(t.indexOf(' → /gone/old'))
    expect(t.filter((x) => x === SIGN.selected)).toHaveLength(1)
    expect(await labelOf($, 'project-row-github.com/acme/app', surface)).toBe('github.com/acme/app')
    expect(await labelOf($, 'project-remove', surface)).toBe('Remove github.com/acme/app')
    const p = await pane($, surface)
    expect((await p.find({ key: 'project-github.com/acme/app' }))?.props.backgroundColor).toBe(P.selected)
    expect((await p.find({ key: 'project-old' }))?.props.backgroundColor).toBeUndefined()
    await p.unmount()
  }
})

test('Add runs owl project add with the path and the name, reloads the list and clears both fields', async ($, on) => {
  let list: Project[] = []
  const rec = world(on, {
    answers: (args) => {
      if (args === 'project add --name acme -- src/app') {
        list = [pj('acme', '/w/src/app')]
        return 'added acme → /w/src/app'
      }
      if (args === 'project list --json') return JSON.stringify(list)
      return undefined
    },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await type($, 'project-path', '@src/app', 'change', surface) // typed as in the prompt box
    await type($, 'project-name', 'acme', 'change', surface)
    const from = rec.runs.length
    await press($, 'project-add', surface)
    expect(calls(rec, from)).toEqual(['project add --name acme -- src/app', 'project list --json'])
    expect((await footer($, surface)).note).toBe('✓ added acme → /w/src/app')
    expect(await field($, 'project-path', surface)).toBe('')
    expect(await field($, 'project-name', surface)).toBe('')
    expect(await texts($, surface)).toContain(' → /w/src/app')
    list = []
  }
})

test('Enter with both fields empty adds the session directory under the detected name', async ($, on) => {
  const rec = world(on, { answers: { 'project add': 'added github.com/acme/app → /w' } })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await type($, 'project-path', '', 'submit', surface)
    expect(calls(rec, from)).toEqual(['project add', 'project list --json'])
    expect((await footer($, surface)).note).toBe('✓ added github.com/acme/app → /w')
    // Enter in the name field adds too.
    const again = rec.runs.length
    await type($, 'project-name', '', 'submit', surface)
    expect(calls(rec, again)).toEqual(['project add', 'project list --json'])
  }
})

test('a failed add says what owl said and keeps both fields', async ($, on) => {
  const rec = world(on, { answers: { 'project add --name x -- /nope': { exitCode: 1, stderr: 'owl: /nope is not a directory' } } })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await type($, 'project-name', 'x', 'change', surface)
    const from = rec.runs.length
    await type($, 'project-path', '/nope', 'submit', surface)
    expect(calls(rec, from)).toEqual(['project add --name x -- /nope', 'project list --json'])
    expect((await footer($, surface)).note).toBe('✗ owl: /nope is not a directory')
    expect(await field($, 'project-path', surface)).toBe('/nope')
    expect(await field($, 'project-name', surface)).toBe('x')
    // Clear for the next surface.
    await type($, 'project-path', '', 'change', surface)
    await type($, 'project-name', '', 'change', surface)
  }
})

test('p takes the newest @path of the prompt box into the path field', async ($, on) => {
  const rec = world(on, { prompt: 'map @"old dir" and @../other/ please' })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    expect(await hotkeyOf($, 'project-path-focus', surface)).toBe('p')
    expect(await hotkeyOf($, 'project-name-focus', surface)).toBe('n')
    const from = rec.runs.length
    await press($, 'project-path-focus', surface)
    expect(calls(rec, from)).toEqual([])
    expect(await field($, 'project-path', surface)).toBe('../other/')
    expect((await footer($, surface)).note).toBe('✓ path ../other/ from the prompt box')
    await type($, 'project-path', '', 'change', surface)
  }
})

test('r asks first; Confirm removes the selected project and the reloaded list no longer holds it', async ($, on) => {
  let list = [pj('acme'), pj('beta')]
  const rec = world(on, {
    answers: (args) => {
      if (args === 'project remove -- beta') {
        list = list.filter((x) => x.name !== 'beta')
        return 'removed beta'
      }
      if (args === 'project list --json') return JSON.stringify(list)
      return undefined
    },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await ring($, 'project-row-beta')
    expect(await hotkeyOf($, 'project-remove', surface)).toBe('r')
    let from = rec.runs.length
    await press($, 'project-remove', surface)
    expect(calls(rec, from)).toEqual([])
    expect(await texts($, surface)).toContain('Remove project beta?')
    expect(await hotkeyOf($, 'project-remove-confirm', surface)).toBeUndefined()
    await press($, 'project-remove-cancel', surface)
    expect(calls(rec, from)).toEqual([])
    expect(await texts($, surface)).not.toContain('Remove project beta?')
    await press($, 'project-remove', surface)
    from = rec.runs.length
    await press($, 'project-remove-confirm', surface)
    expect(calls(rec, from)).toEqual(['project remove -- beta', 'project list --json'])
    expect((await footer($, surface)).note).toBe('✓ removed beta')
    expect(await el($, 'project-row-beta', surface)).toBeUndefined()
    expect(await el($, 'project-row-acme', surface)).toBeDefined()
    list = [pj('acme'), pj('beta')]
    await press($, 'tab-card', surface)
    await press($, 'tab-settings', surface)
  }
  await ring($)
})

test('a refused remove says why and keeps the row; leaving the tab drops the question', async ($, on) => {
  const rec = world(on, {
    projects: [pj('acme')],
    answers: { 'project remove -- acme': { exitCode: 1, stderr: 'owl: tool lint runs in acme — change its cwd first' } },
  })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    await press($, 'project-remove', surface)
    const from = rec.runs.length
    await press($, 'project-remove-confirm', surface)
    expect(calls(rec, from)).toEqual(['project remove -- acme', 'project list --json'])
    expect((await footer($, surface)).note).toBe('✗ owl: tool lint runs in acme — change its cwd first')
    expect(await el($, 'project-row-acme', surface)).toBeDefined()
    await press($, 'project-remove', surface)
    await press($, 'tab-card', surface)
    await press($, 'tab-settings', surface)
    expect(await texts($, surface)).not.toContain('Remove project acme?')
  }
})

test('the hotkeys stay unique on the screen with projects and harnesses', async ($, on) => {
  world(on, { harnesses: [hz({ name: 'claude' })], projects: [pj('acme')] })
  await panel($)
  await press($, 'tab-settings')
  for (const surface of SURFACES) {
    const p = await pane($, surface)
    const hotkeys = (await p.findAll({ type: 'Button' })).map((b) => b.props.hotkey).filter(Boolean)
    await p.unmount()
    expect(new Set(hotkeys).size).toBe(hotkeys.length)
    for (const k of ['p', 'n', 'r']) expect(hotkeys).toContain(k)
  }
})
