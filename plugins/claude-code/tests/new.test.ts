// The New tab (hooks/new.tsx): kind, To, thread and the fields of a question, a file request
// and a tool call, sent as one `owl` command. `owl` is the world of tests/world.ts.
import { expect, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { Ev } from '../hooks/lib'
import { ALICE, BOB, SURFACES, calls, enter, field, footer, pane, panel, press, texts, type, world, type WorldOptions } from './world'

const ev = (o: Partial<Ev>): Ev => ({
  ts: '2026-10-01T07:15:00Z', kind: 'asked', dir: 'out', record_id: '', context_id: null, type: 'question',
  state: 'answered', project: null, path: '-', text: '', ...o,
})

// Two threads with Bob: one we asked in and he answered (continuable, `--reply-to` is his
// answer), one only he asked in (not offered).
const CTX = '01a106b9-52d7-7af3-80bc-0e6e800362f4'
const ASKED = '01a106b9-52d7-7af3-80bc-0e6e80aaaaaa'
const ANSWER = '01a106b9-52d7-7af3-80bc-0e6e80bbbbbb'
const THEIRS = '01a106b9-8e9e-7ad0-bf5b-410a7c351bd1'
const TIMELINE: Ev[] = [
  ev({ record_id: ASKED, context_id: CTX, text: 'What is your last commit?' }),
  ev({ ts: '2026-10-01T07:20:00Z', kind: 'answer-received', dir: 'in', record_id: ANSWER, context_id: CTX, type: 'answer', text: 'abc123' }),
  ev({ ts: '2026-10-02T09:00:00Z', kind: 'received', dir: 'in', record_id: THEIRS, context_id: THEIRS, type: 'question', state: 'pending', text: 'Which branch?' }),
]

const opts = (o: WorldOptions = {}): WorldOptions => ({ contacts: [ALICE, BOB], timelines: { [BOB.fingerprint]: TIMELINE }, ...o })

async function toNew($: Engine) {
  await panel($)
  await press($, 'tab-new')
}

// Picks Bob through the To filter.
async function pickBob($: Engine, surface: (typeof SURFACES)[number] = 'terminal') {
  await type($, 'to', 'b@x', 'change', surface)
  await press($, `to-${BOB.fingerprint}`, surface)
}

// The pane's Buttons: their keys, or their labels.
async function buttons($: Engine, surface: (typeof SURFACES)[number] = 'terminal') {
  const p = await pane($, surface)
  const all = await p.findAll({ type: 'Button' })
  await p.unmount()
  return all
}
const keys = async ($: Engine, surface: (typeof SURFACES)[number] = 'terminal') => (await buttons($, surface)).map((b) => b.key)
// The Kind row as drawn.
const kinds = async ($: Engine, surface: (typeof SURFACES)[number]) => {
  const k = await buttons($, surface)
  return k.filter((b) => b.key?.startsWith('kind-')).map((b) => b.text)
}
const labels = async ($: Engine, surface: (typeof SURFACES)[number] = 'terminal') => (await buttons($, surface)).map((b) => b.text)

test('New opens from the tab and from /owlpost:ask without arguments, with its note and keys', async ($, on) => {
  world(on, opts())
  await $.command.run({ command: 'owlpost:ask', args: '', origin: { kind: 'composer' }, presentation: { isFullscreen: true, columns: 200 } })
  expect(await texts($)).toContain('New message')
  await press($, 'tab-chats')
  await press($, 'tab-new')
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    expect(t).toContain('New message')
    expect(t).toContain('Question')
    // As the mock: the chosen kind drawn chosen (inverse), the others Buttons; Send with what
    // Enter does beside it; an empty note; the keys while typing, then after.
    expect(await kinds($, surface)).toEqual(['Request a file', 'Call a tool'])
    expect(t).toContain(' Ask a question ')
    expect(t).toContain('enter sends — the command is the approval')
    const { note, hints } = await footer($, surface)
    expect(note).toBe('')
    expect(hints).toBe('keystypingenter sendtab leave the fieldthenu use prompt textf attachesc prompt')
  }
})

test('the To filter matches by name, e-mail and fingerprint; a pick shows the policy; Change starts over', async ($, on) => {
  world(on, opts())
  await toNew($)
  for (const surface of SURFACES) {
    const shown = async () => (await keys($, surface)).filter((k) => k?.startsWith('to-'))
    expect(await shown()).toEqual([`to-${ALICE.fingerprint}`, `to-${BOB.fingerprint}`])
    await type($, 'to', 'ALI', 'change', surface)
    expect(await shown()).toEqual([`to-${ALICE.fingerprint}`])
    await type($, 'to', 'b@x', 'change', surface)
    expect(await shown()).toEqual([`to-${BOB.fingerprint}`])
    await type($, 'to', 'xri6', 'change', surface)
    expect(await shown()).toEqual([`to-${BOB.fingerprint}`])
    await type($, 'to', 'nobody', 'change', surface)
    expect(await shown()).toEqual([])
    expect(await texts($, surface)).toContain('no contact matches')
    await type($, 'to', 'Ali', 'change', surface)
    await press($, `to-${ALICE.fingerprint}`, surface)
    let t = await texts($, surface)
    expect(t).toContain('Alice')
    expect(t).toContain(`${ALICE.fingerprint} · policy manual`)
    expect(await shown()).toEqual([])
    await press($, 'change', surface)
    await pickBob($, surface)
    t = await texts($, surface)
    expect(t).toContain(`${BOB.fingerprint} · policy none`)
    await press($, 'change', surface)
    expect(await keys($, surface)).not.toContain('thread-new')
  }
})

test('ask: the exact argv with and without each optional part; fields are cleared after a send', async ($, on) => {
  const rec = world(on, opts())
  await toNew($)
  for (const surface of SURFACES) {
    await pickBob($, surface)
    let from = rec.runs.length
    await type($, 'text', 'Which branch?', 'submit', surface)
    expect(calls(rec, from)).toEqual(['ask --peer owl:xri6rpdrer5rdmt4 -- Which branch?'])
    expect(rec.runs.find((r) => r.args.startsWith('ask'))?.argv.at(-1)).toBe('Which branch?')
    from = rec.runs.length
    await type($, 'path', 'src/lib.rs', 'change', surface)
    await press($, 'context-focus', surface) // f: the local-file field shows
    await type($, 'context', '/tmp/err.txt', 'change', surface)
    await type($, 'text', 'Why?', 'change', surface)
    await press($, 'send', surface)
    expect(rec.runs.slice(from).find((r) => r.args.startsWith('ask'))?.argv).toEqual(
      ['ask', '--peer', BOB.fingerprint, '--file', 'src/lib.rs', '--context', '/tmp/err.txt', '--', 'Why?'])
    // Cleared: the next send has neither path nor context, To and Kind stay.
    from = rec.runs.length
    await type($, 'path', 'a.rs', 'change', surface)
    await type($, 'text', 'Again', 'submit', surface)
    expect(calls(rec, from)).toEqual(['ask --peer owl:xri6rpdrer5rdmt4 --file a.rs -- Again'])
    from = rec.runs.length
    await press($, 'context-focus', surface)
    await type($, 'context', 'ctx.txt', 'change', surface)
    await type($, 'text', 'Once more', 'submit', surface)
    expect(calls(rec, from)).toEqual(['ask --peer owl:xri6rpdrer5rdmt4 --context ctx.txt -- Once more'])
    from = rec.runs.length
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ missing: Question')
    await type($, 'text', '   ', 'submit', surface)
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ missing: Question')
    await type($, 'text', '', 'change', surface)
    await press($, 'change', surface)
  }
})

test('request: the exact argv with and without Ref, and what is missing', async ($, on) => {
  const rec = world(on, opts())
  await toNew($)
  for (const surface of SURFACES) {
    await press($, 'kind-request', surface)
    expect(await kinds($, surface)).toEqual(['Ask a question', 'Call a tool'])
    const t = await texts($, surface)
    expect(t).toContain('Project')
    expect(t).not.toContain('Question')
    expect((await footer($, surface)).hints).toBe('keystypingenter sendtab leave the fieldthenesc prompt')
    let from = rec.runs.length
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ missing: To, Project, Path')
    await pickBob($, surface)
    await type($, 'project', 'demo', 'change', surface)
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual([])
    expect((await footer($, surface)).note).toBe('✗ missing: Path')
    await type($, 'path', 'README.md', 'change', surface)
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual(['request -- owl:xri6rpdrer5rdmt4 demo README.md'])
    from = rec.runs.length
    await type($, 'project', 'demo', 'change', surface)
    await type($, 'path', 'src/a.rs', 'change', surface)
    await type($, 'ref', 'v1.0', 'submit', surface)
    expect(calls(rec, from)).toEqual([])
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual(['request --ref v1.0 -- owl:xri6rpdrer5rdmt4 demo src/a.rs'])
    // Only Path given: Project is named.
    from = rec.runs.length
    await type($, 'path', 'x', 'change', surface)
    await press($, 'send', surface)
    expect((await footer($, surface)).note).toBe('✗ missing: Project')
    await type($, 'path', '', 'change', surface)
    await press($, 'change', surface)
    await press($, 'kind-ask', surface)
  }
})

test('call: the tool and the JSON go on stdin; what is missing is named', async ($, on) => {
  const rec = world(on, opts())
  await toNew($)
  for (const surface of SURFACES) {
    await press($, 'kind-call', surface)
    expect((await footer($, surface)).hints).toBe('keystypingenter sendtab leave the fieldthenu use prompt textesc prompt')
    await pickBob($, surface)
    let from = rec.runs.length
    await press($, 'send', surface)
    expect((await footer($, surface)).note).toBe('✗ missing: Tool, Input JSON')
    await type($, 'tool', 'lint', 'change', surface)
    await press($, 'send', surface)
    expect((await footer($, surface)).note).toBe('✗ missing: Input JSON')
    await type($, 'tool', '', 'change', surface)
    await type($, 'text', '{"a":1}', 'submit', surface)
    expect((await footer($, surface)).note).toBe('✗ missing: Tool')
    expect(calls(rec, from)).toEqual([])
    await type($, 'tool', 'lint', 'change', surface)
    await type($, 'text', '{"a":1}', 'submit', surface)
    expect(calls(rec, from)).toEqual(['call --input - -- owl:xri6rpdrer5rdmt4 lint'])
    expect(rec.runs.filter((r) => r.args.startsWith('call')).at(-1)?.stdin).toBe('{"a":1}')
    await press($, 'change', surface)
    await press($, 'kind-ask', surface)
  }
})

test('a thread is continued with --reply-to of its newest record; one only they asked in is offered too', async ($, on) => {
  const rec = world(on, opts())
  await toNew($)
  for (const surface of SURFACES) {
    await pickBob($, surface)
    const k = await keys($, surface)
    expect(k).toContain('thread-new')
    expect(k).toContain(`thread-${CTX}`)
    expect(k).toContain(`thread-${THEIRS}`)
    const l = await labels($, surface)
    expect(l).toContain('(•) New thread')
    expect(l.some((x) => x.startsWith('( ) Continue · What is your last commit?'))).toBe(true)
    await press($, `thread-${CTX}`, surface)
    expect(await labels($, surface)).toContain('( ) New thread')
    expect((await labels($, surface)).some((x) => x.startsWith('(•) Continue · What is your last commit?'))).toBe(true)
    let from = rec.runs.length
    await type($, 'text', 'And now?', 'submit', surface)
    expect(calls(rec, from)).toEqual([`ask --peer owl:xri6rpdrer5rdmt4 --reply-to ${ANSWER} -- And now?`])
    from = rec.runs.length
    await press($, 'kind-request', surface)
    await type($, 'project', 'demo', 'change', surface)
    await type($, 'path', 'a', 'change', surface)
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual([`request --reply-to ${ANSWER} -- owl:xri6rpdrer5rdmt4 demo a`])
    from = rec.runs.length
    await press($, 'kind-call', surface)
    await type($, 'tool', 'lint', 'change', surface)
    await type($, 'text', '{}', 'submit', surface)
    expect(calls(rec, from)).toEqual([`call --input - --reply-to ${ANSWER} -- owl:xri6rpdrer5rdmt4 lint`])
    // Back to a new thread.
    await press($, 'kind-ask', surface)
    await press($, 'thread-new', surface)
    from = rec.runs.length
    await type($, 'text', 'Fresh', 'submit', surface)
    expect(calls(rec, from)).toEqual(['ask --peer owl:xri6rpdrer5rdmt4 -- Fresh'])
    // Their thread goes on from their question.
    await press($, `thread-${THEIRS}`, surface)
    from = rec.runs.length
    await type($, 'text', 'main', 'submit', surface)
    expect(calls(rec, from)).toEqual([`ask --peer owl:xri6rpdrer5rdmt4 --reply-to ${THEIRS} -- main`])
    await press($, 'thread-new', surface)
    await press($, 'change', surface)
  }
})

test('u takes the prompt text into the Question and the Input JSON; f is drawn for a question only', async ($, on) => {
  const rec = world(on, opts({ prompt: 'from the prompt' }))
  await toNew($)
  for (const surface of SURFACES) {
    await pickBob($, surface)
    await press($, 'use-prompt', surface)
    await press($, 'send', surface)
    expect(calls(rec).at(-1)).toBe('ask --peer owl:xri6rpdrer5rdmt4 -- from the prompt')
    // f: the press itself cannot run here (the test engine has no ui.focus), see the report.
    expect(await labels($, surface)).toContain('Attach a file (diff, error, excerpt · ≤ 8 KiB)')
    await press($, 'kind-call', surface)
    await type($, 'tool', 'lint', 'change', surface)
    await press($, 'use-prompt', surface)
    await press($, 'send', surface)
    expect(rec.runs.filter((r) => r.args.startsWith('call')).at(-1)?.stdin).toBe('from the prompt')
    expect(await keys($, surface)).not.toContain('context-focus')
    await press($, 'kind-request', surface)
    expect(await keys($, surface)).not.toContain('use-prompt')
    await press($, 'kind-ask', surface)
    await press($, 'change', surface)
  }
})

test("the note shows owl's answer on success and its error on failure; a failed send keeps the fields", async ($, on) => {
  const answers: Record<string, string | { exitCode: number; stderr: string }> = {
    'ask --peer owl:xri6rpdrer5rdmt4 -- Hi': `accepted ${ASKED}`,
    'ask --peer owl:xri6rpdrer5rdmt4 -- Bye': { exitCode: 1, stderr: 'owl: offline: no endpoint of Bob reachable' },
  }
  const rec = world(on, opts({ answers }))
  await toNew($)
  for (const surface of SURFACES) {
    await pickBob($, surface)
    await type($, 'text', 'Hi', 'submit', surface)
    expect((await footer($, surface)).note).toBe('✓ accepted aaaaaa')
    await type($, 'text', 'Bye', 'submit', surface)
    expect((await footer($, surface)).note).toBe('✗ owl: offline: no endpoint of Bob reachable')
    const from = rec.runs.length
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual(['ask --peer owl:xri6rpdrer5rdmt4 -- Bye'])
    await type($, 'text', '', 'change', surface)
    await press($, 'change', surface)
  }
})

test('text starting with - goes after --: a question, a file request and a tool call', async ($, on) => {
  const rec = world(on, opts({ answers: (a) => (/^(ask|request|call) /.test(a) ? `accepted ${ASKED}` : undefined) }))
  await toNew($)
  for (const surface of SURFACES) {
    await pickBob($, surface)
    let from = rec.runs.length
    await type($, 'text', '-v or --verbose?', 'submit', surface)
    expect(rec.runs.slice(from).find((r) => r.argv[0] === 'ask')?.argv).toEqual(['ask', '--peer', BOB.fingerprint, '--', '-v or --verbose?'])
    expect((await footer($, surface)).note).toBe('✓ accepted aaaaaa')
    from = rec.runs.length
    await press($, 'kind-request', surface)
    await type($, 'project', '-demo', 'change', surface)
    await type($, 'path', '-a.rs', 'change', surface)
    await press($, 'send', surface)
    expect(rec.runs.slice(from).find((r) => r.argv[0] === 'request')?.argv).toEqual(['request', '--', BOB.fingerprint, '-demo', '-a.rs'])
    expect((await footer($, surface)).note).toBe('✓ accepted aaaaaa')
    from = rec.runs.length
    await press($, 'kind-call', surface)
    await type($, 'tool', '-lint', 'change', surface)
    await type($, 'text', '{}', 'submit', surface)
    expect(rec.runs.slice(from).find((r) => r.argv[0] === 'call')?.argv).toEqual(['call', '--input', '-', '--', BOB.fingerprint, '-lint'])
    expect((await footer($, surface)).note).toBe('✓ accepted aaaaaa')
    await press($, 'kind-ask', surface)
    await press($, 'change', surface)
  }
})


// The engine empties a field on Enter (tests/world.ts): after a failed send each field still
// shows what was typed, and Enter with nothing typed sends that text again, for every kind.
// The text is drawn by the mod before Enter (a redraw such as the 15 s poll), the case where
// a field drawn again with the same value would stay empty.
test('a failed send leaves the typed text in the fields; the next Enter or Send resends it', async ($, on) => {
  const fail = { on: true }
  const rec = world(on, opts({ answers: (a) => (fail.on && /^(ask|request|call) /.test(a) ? { exitCode: 1, stderr: 'owl: peer answered 400' } : undefined) }))
  await toNew($)
  for (const surface of SURFACES) {
    fail.on = true
    await pickBob($, surface)
    // ask
    await type($, 'text', 'Which branch?', 'change', surface)
    await texts($, surface)
    let from = rec.runs.length
    await enter($, 'text', surface)
    await enter($, 'text', surface)
    expect(calls(rec, from)).toEqual(['ask --peer owl:xri6rpdrer5rdmt4 -- Which branch?', 'ask --peer owl:xri6rpdrer5rdmt4 -- Which branch?'])
    expect((await footer($, surface)).note).toBe('✗ owl: peer answered 400')
    expect(await field($, 'text', surface)).toBe('Which branch?')
    fail.on = false
    from = rec.runs.length
    await enter($, 'text', surface)
    expect(calls(rec, from)).toEqual(['ask --peer owl:xri6rpdrer5rdmt4 -- Which branch?'])
    expect(await field($, 'text', surface)).toBe('')
    // call
    fail.on = true
    await press($, 'kind-call', surface)
    await type($, 'tool', 'lint', 'submit', surface)
    await type($, 'text', '{"a":1}', 'change', surface)
    await texts($, surface)
    from = rec.runs.length
    await enter($, 'text', surface)
    await enter($, 'text', surface)
    expect(calls(rec, from)).toEqual(['call --input - -- owl:xri6rpdrer5rdmt4 lint', 'call --input - -- owl:xri6rpdrer5rdmt4 lint'])
    expect(rec.runs.slice(from).filter((r) => r.args.startsWith('call')).map((r) => r.stdin)).toEqual(['{"a":1}', '{"a":1}'])
    expect(await field($, 'tool', surface)).toBe('lint')
    expect(await field($, 'text', surface)).toBe('{"a":1}')
    fail.on = false
    from = rec.runs.length
    await enter($, 'text', surface)
    expect(calls(rec, from)).toEqual(['call --input - -- owl:xri6rpdrer5rdmt4 lint'])
    expect(await field($, 'tool', surface)).toBe('')
    expect(await field($, 'text', surface)).toBe('')
    // request: Enter only keeps a field's text, Send sends
    fail.on = true
    await press($, 'kind-request', surface)
    await type($, 'project', 'demo', 'submit', surface)
    await type($, 'path', 'README.md', 'submit', surface)
    await enter($, 'path', surface)
    expect(await field($, 'path', surface)).toBe('README.md')
    from = rec.runs.length
    await press($, 'send', surface)
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual(['request -- owl:xri6rpdrer5rdmt4 demo README.md', 'request -- owl:xri6rpdrer5rdmt4 demo README.md'])
    expect(await field($, 'project', surface)).toBe('demo')
    expect(await field($, 'path', surface)).toBe('README.md')
    await enter($, 'project', surface)
    await enter($, 'path', surface)
    fail.on = false
    from = rec.runs.length
    await press($, 'send', surface)
    expect(calls(rec, from)).toEqual(['request -- owl:xri6rpdrer5rdmt4 demo README.md'])
    expect(await field($, 'path', surface)).toBe('')
    await press($, 'change', surface)
    await press($, 'kind-ask', surface)
  }
})

test('two threads whose ids end in the same six hex are two choices, and each continues its own', async ($, on) => {
  const [C1, C2] = ['11111111-1111-7111-8111-aaaaaa000001', '22222222-2222-7222-8222-bbbbbb000001']
  const [Q1, Q2] = ['11111111-1111-7111-8111-00000000000a', '22222222-2222-7222-8222-00000000000b']
  const timeline = [
    ev({ record_id: Q1, context_id: C1, state: 'waiting', text: 'first' }),
    ev({ ts: '2026-10-01T08:00:00Z', record_id: Q2, context_id: C2, state: 'waiting', text: 'second' }),
  ]
  const rec = world(on, opts({ timelines: { [BOB.fingerprint]: timeline } }))
  await toNew($)
  for (const surface of SURFACES) {
    await pickBob($, surface)
    expect((await keys($, surface)).filter((k) => k?.startsWith('thread-'))).toEqual(['thread-new', `thread-${C2}`, `thread-${C1}`]) // newest first
    await press($, `thread-${C2}`, surface)
    const l = await labels($, surface)
    expect(l.some((x) => x.startsWith('( ) Continue · first'))).toBe(true)
    expect(l.some((x) => x.startsWith('(•) Continue · second'))).toBe(true)
    let from = rec.runs.length
    await type($, 'text', 'Next?', 'submit', surface)
    expect(calls(rec, from)).toEqual([`ask --peer ${BOB.fingerprint} --reply-to ${Q2} -- Next?`])
    await press($, `thread-${C1}`, surface)
    from = rec.runs.length
    await type($, 'text', 'A tu?', 'submit', surface)
    expect(calls(rec, from)).toEqual([`ask --peer ${BOB.fingerprint} --reply-to ${Q1} -- A tu?`])
    await press($, 'change', surface)
  }
})
