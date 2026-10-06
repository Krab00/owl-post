// The Card tab (hooks/card.tsx): the own card, the peer file on the clipboard, raw JSON and a
// colleague's card fetched by name. `owl` is the world of tests/world.ts.
import { expect, test } from 'claude-code/testing'
import { DRAW_MAX, P, cutNote } from '../hooks/ui'
import { BOB, SURFACES, calls, footer, pane, panel, press, texts, type, world } from './world'

// `owl card` as the daemon prints it, cut to what the tab reads.
const card = (name: string, email: string, fp: string, projects: string[], harness: string) =>
  JSON.stringify({
    name,
    version: '0.3.0',
    provider: { organization: name, url: `mailto:${email}` },
    supportedInterfaces: [{ protocolBinding: 'owlpost-v1', protocolVersion: '1', url: 'owl-iroh://iBPCQX7B' }],
    capabilities: {
      extensions: [
        { uri: 'urn:owlpost:ext:identity:v1', params: { fingerprint: fp, pubkey: 'ed25519:iBPCQX7B', relay: 'https://relay' } },
        { uri: 'urn:owlpost:ext:repo-question:v1', params: { projects } },
        { uri: 'urn:owlpost:ext:human-gate:v1', params: { harness, responds: true } },
      ],
    },
  })

const MINE = card('Alice', 'a@x.io', 'owl:pfbrpuq3pbfblrnd', ['demo'], 'claude')
const BOBS = card('Bob', 'b@x.io', 'owl:xri6rpdrer5rdmt4', [], 'codex')
const PEER_FILE = '{"name":"Alice","emails":["a@x.io"],"endpoints":[],"pubkey":"ed25519:iBPCQX7B"}'

test('the Card tab shows the own card row by row', async ($, on) => {
  const rec = world(on, { answers: { 'card --json': MINE } })
  await panel($)
  await press($, 'tab-card')
  expect(calls(rec)).toContain('card --json')
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    for (const [label, value] of [
      ['Name', 'Alice'],
      ['E-mail', 'a@x.io'],
      ['Fingerprint', 'owl:pfbrpuq3pbfblrnd'],
      ['Public key', 'ed25519:iBPCQX7B'],
      ['Transport', 'iroh, pinned to this key'],
      ['Projects', 'demo'],
      ['Answers', 'yes, a human approves every answer'],
      ['Harness', 'claude'],
      // As the mock: the daemon as this machine last found it (no daemon status in this world).
      ['Daemon', '✕ not reachable · owl: no daemon status yet (daemon.status missing) — is the daemon running? see owl install'],
    ]) {
      expect(t[t.indexOf(label) + 1]).toBe(value)
    }
    // The mock's rows: no Version row; `yes` of Answers in green, the name in bold.
    expect(t).not.toContain('Version')
    const p = await pane($, surface)
    const all = await p.findAll({ type: 'Text' })
    await p.unmount()
    expect(all.find((x) => x.text === 'yes')?.props.color).toBe(P.ok)
    expect(all.find((x) => x.text === 'Alice')?.props.bold).toBe(true)
    expect((await footer($, surface)).note).toBe('nothing here leaves the machine until you paste it')
  }
})

test('Daemon: reachable at the address of daemon.addr (0.0.0.0 as 127.0.0.1)', async ($, on) => {
  const presence = { last_pull_at: '2026-10-05T10:00:00Z', open_asks: 0, peers_probed: 0, peers: [] }
  world(on, { answers: { 'card --json': MINE }, presence, daemonAddr: '0.0.0.0:7411\n' })
  await panel($)
  await press($, 'tab-card')
  for (const surface of SURFACES) {
    const t = await texts($, surface)
    expect(t[t.indexOf('Daemon') + 1]).toBe('● reachable at 127.0.0.1:7411')
  }
})

test('Daemon: a reachable daemon without daemon.addr says reachable alone', async ($, on) => {
  const presence = { last_pull_at: '2026-10-05T10:00:00Z', open_asks: 0, peers_probed: 0, peers: [] }
  world(on, { answers: { 'card --json': MINE }, presence })
  await panel($)
  await press($, 'tab-card')
  const t = await texts($)
  expect(t[t.indexOf('Daemon') + 1]).toBe('● reachable')
})

test('Transport: iroh of another key is iroh alone; a card without iroh lists its URLs', async ($, on) => {
  const other = JSON.parse(MINE)
  other.supportedInterfaces = [{ url: 'https://b.example.org:7411/' }, { url: 'owl-iroh://ZZZ' }]
  const https = JSON.parse(MINE)
  https.supportedInterfaces = [{ url: 'https://b.example.org:7411/' }]
  const answers: Record<string, string> = { 'card --json': JSON.stringify(other) }
  world(on, { answers })
  await panel($)
  await press($, 'tab-card')
  let t = await texts($)
  expect(t[t.indexOf('Transport') + 1]).toBe('iroh')
  answers['card --json'] = JSON.stringify(https)
  await press($, 'tab-settings')
  await press($, 'tab-card')
  t = await texts($)
  expect(t[t.indexOf('Transport') + 1]).toBe('https://b.example.org:7411/')
})

test('a card without projects says none, and no card at all says so', async ($, on) => {
  const answers: Record<string, string | { exitCode: number; stderr: string }> = { 'card --json': BOBS }
  world(on, { answers })
  await panel($)
  await press($, 'tab-card')
  let t = await texts($)
  expect(t[t.indexOf('Projects') + 1]).toBe('none')
  answers['card --json'] = { exitCode: 2, stderr: 'owl: no key' }
  await press($, 'tab-chats')
  await press($, 'tab-card')
  t = await texts($)
  expect(t).toContain('- no card: is owl set up? Run doctor in Settings.')
  expect(t).not.toContain('Name')
})

test('y copies the peer file and says so; a failed export says why', async ($, on) => {
  const answers: Record<string, string | { exitCode: number; stderr: string }> = { 'card --json': MINE, 'contact export': PEER_FILE }
  const rec = world(on, { answers })
  await panel($)
  await press($, 'tab-card')
  for (const surface of SURFACES) {
    await press($, 'copy', surface)
    expect(rec.copied.at(-1)).toBe(PEER_FILE + '\n')
    expect((await footer($, surface)).note).toBe('✓ peer file copied')
  }
  answers['contact export'] = { exitCode: 1, stderr: 'owl: no key in /scratch/owlpost' }
  await press($, 'copy')
  expect(rec.copied).toHaveLength(2)
  expect((await footer($)).note).toBe('✗ owl: no key in /scratch/owlpost')
})

test('e writes the peer file next to the session', async ($, on) => {
  const rec = world(on, { answers: { 'card --json': MINE, 'contact export': PEER_FILE } })
  await panel($)
  await press($, 'tab-card')
  await press($, 'export')
  expect(rec.writes).toHaveLength(1)
  expect(rec.writes[0]!.path).toMatch(/(^|\/)owlpost-peer\.json$/)
  expect(rec.writes[0]!.text).toBe(PEER_FILE + '\n')
  expect((await footer($)).note).toBe('✓ peer file written to ./owlpost-peer.json')
})

test('e refuses ./owlpost-peer.json when it is a symlink', async ($, on) => {
  const rec = world(on, { answers: { 'card --json': MINE, 'contact export': PEER_FILE }, links: ['owlpost-peer.json'] })
  await panel($)
  await press($, 'tab-card')
  await press($, 'export')
  expect(rec.writes).toEqual([])
  expect((await footer($)).note).toBe('✗ ./owlpost-peer.json is a symlink, not written')
})

test('e refuses a symlink whose name differs only in case (APFS is case-insensitive)', async ($, on) => {
  const rec = world(on, { answers: { 'card --json': MINE, 'contact export': PEER_FILE }, links: ['OWLPOST-Peer.json'] })
  await panel($)
  await press($, 'tab-card')
  await press($, 'export')
  expect(rec.writes).toEqual([])
  expect((await footer($)).note).toBe('✗ ./owlpost-peer.json is a symlink, not written')
})

test('j shows the raw JSON and hides it again', async ($, on) => {
  world(on, { answers: { 'card --json': MINE } })
  await panel($)
  await press($, 'tab-card')
  const raw = JSON.stringify(JSON.parse(MINE), null, 2)
  for (const surface of SURFACES) {
    expect(await texts($, surface)).not.toContain(raw)
    await press($, 'raw', surface)
    expect(await texts($, surface)).toContain(raw)
    await press($, 'raw', surface)
    expect(await texts($, surface)).not.toContain(raw)
  }
})

test("a colleague's card handed over by k is fetched once; the Card tab opened later fetches only the own", async ($, on) => {
  const rec = world(on, { contacts: [BOB], answers: { 'card --json': MINE, [`card ${BOB.fingerprint}`]: BOBS } })
  await panel($)
  await press($, 'tab-contacts')
  for (const surface of SURFACES) {
    let from = rec.runs.length
    await press($, 'card', surface)
    expect(calls(rec, from)).toEqual(['card --json', `card ${BOB.fingerprint}`])
    await press($, 'tab-settings', surface)
    from = rec.runs.length
    await press($, 'tab-card', surface)
    expect(calls(rec, from)).toEqual(['card --json'])
    await press($, 'tab-contacts', surface)
  }
})

test("Peer fetches a colleague's card by name; an unreachable one leaves the error in the note", async ($, on) => {
  const answers: Record<string, string | { exitCode: number; stderr: string }> = { 'card --json': MINE, 'card Bob': BOBS }
  const rec = world(on, { answers })
  await panel($)
  await press($, 'tab-card')
  for (const surface of SURFACES) {
    const from = rec.runs.length
    await type($, 'peer', 'Bob', 'submit', surface)
    expect(calls(rec, from)).toEqual(['card Bob'])
    const t = await texts($, surface)
    expect(t.filter((x) => x === 'Name')).toHaveLength(2)
    expect(t).toContain('owl:xri6rpdrer5rdmt4')
    expect((await footer($, surface)).note).toBe('✓ card of Bob')
  }
  answers['card Bob'] = { exitCode: 2, stderr: 'owl: offline: no endpoint of Bob reachable' }
  await type($, 'peer', 'Bob')
  expect(await texts($)).not.toContain('owl:xri6rpdrer5rdmt4')
  expect((await footer($)).note).toBe('✗ owl: offline: no endpoint of Bob reachable')
  // An empty field fetches nothing.
  const from = rec.runs.length
  await type($, 'peer', '  ')
  expect(calls(rec, from)).toEqual([])
})

test("a colleague's card with 20 000-character fields draws cut, its rows and the note too", async ($, on) => {
  const long = 'N'.repeat(20_000) + 'TAIL'
  world(on, { answers: { 'card --json': MINE, 'card Bob': card(long, `${'e'.repeat(20_000)}@x.io`, 'owl:xri6rpdrer5rdmt4', ['p'.repeat(20_000)], 'h'.repeat(20_000)) } })
  await panel($)
  await press($, 'tab-card')
  for (const surface of SURFACES) {
    await type($, 'peer', 'Bob', 'submit', surface)
    const t = await texts($, surface)
    expect(t).toContain('owl:xri6rpdrer5rdmt4')
    expect(t.filter((x) => x.includes(cutNote(long.length)))).toHaveLength(1) // Name
    expect(t.filter((x) => x.includes('… cut, '))).toHaveLength(5) // Name, E-mail, Projects, Harness, the note
    expect(t.some((x) => x.includes('TAIL') || x.length > 2 * DRAW_MAX)).toBe(false)
    const note = (await footer($, surface)).note
    expect(note.startsWith(`✓ card of ${'N'.repeat(100)}`)).toBe(true)
    expect(note.includes('TAIL') || note.length > 2 * DRAW_MAX).toBe(false)
  }
})
