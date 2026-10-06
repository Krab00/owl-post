// Card tab (`4`): this machine's card as a colleague's agent sees it (`owl card`), the peer
// file to hand over (`owl contact export`, `y` copies it), and a colleague's card fetched by
// name, e-mail or fingerprint (`owl card <peer>`; Contacts' `k` fills the field).
import { enter, reachable, s, type Api } from './lib'
import { Field, P, SIGN, clip, line, type Ctx, type View } from './ui'

// `owl card`'s A2A card as a JSON object. A colleague's is printed as their machine served
// it, so the tab checks the type of every field it reads: one of the wrong type is left out.
type Card = Record<string, unknown>

const obj = (v: unknown): Card | undefined => (v && typeof v === 'object' && !Array.isArray(v) ? (v as Card) : undefined)
const list = (v: unknown): unknown[] => (Array.isArray(v) ? v : [])
// A string, number or boolean as text; anything else ''.
const text = (v: unknown) => (['string', 'number', 'boolean'].includes(typeof v) ? String(v) : '')

let mine: Card | undefined
let theirs: Card | undefined
let raw = false
let peer = ''

const param = (c: Card, uri: string, key: string) => {
  const ext = list(obj(c.capabilities)?.extensions).map(obj).find((x) => x?.uri === `urn:owlpost:ext:${uri}:v1`)
  return obj(ext?.params)?.[key]
}

// As the mock: `iroh, pinned to this key` when the card lists an `owl-iroh://` interface whose
// endpoint id is the card's own public key (`iroh` alone when it is another); a card without
// one lists its interface URLs.
function transport(c: Card): string {
  const urls = list(c.supportedInterfaces).map((i) => text(obj(i)?.url)).filter(Boolean)
  const iroh = urls.find((u) => u.startsWith('owl-iroh://'))
  if (!iroh) return urls.join(', ')
  const key = text(param(c, 'identity', 'pubkey')).replace(/^ed25519:/, '')
  return key && iroh.slice('owl-iroh://'.length) === key ? 'iroh, pinned to this key' : 'iroh'
}

// Label and value of each row, as the mock lists them.
function rows(c: Card): [string, string][] {
  const projects = list(param(c, 'repo-question', 'projects')).map(text).filter(Boolean)
  return [
    ['Name', text(c.name)],
    ['E-mail', text(obj(c.provider)?.url).replace(/^mailto:/, '')],
    ['Fingerprint', text(param(c, 'identity', 'fingerprint'))],
    ['Public key', text(param(c, 'identity', 'pubkey'))],
    ['Transport', transport(c)],
    ['Projects', projects.length ? projects.join(', ') : 'none'],
    ['Answers', param(c, 'human-gate', 'responds') === true ? 'yes, a human approves every answer' : 'no'],
    ['Harness', text(param(c, 'human-gate', 'harness')) || '-'],
  ]
}

async function fetchPeer(api: Api, who: string) {
  peer = who.trim()
  if (!peer) return
  api.say(`… owl card ${peer}`)
  const r = await api.owl(['card', peer])
  let v: unknown
  try {
    v = r.ok ? JSON.parse(r.out) : undefined
  } catch {}
  theirs = obj(v)
  api.say(theirs ? `✓ card of ${text(theirs.name)}` : `✗ ${r.ok ? `owl card ${peer} printed no card` : r.out.split('\n')[0]}`)
}

enter.card = async (api) => {
  mine = obj(await api.json<unknown>(['card'], undefined))
  if (s.cardPeer) {
    const who = s.cardPeer
    s.cardPeer = ''
    await fetchPeer(api, who)
  }
}

// `e`: the peer file written next to the session (`owlpost-peer.json` in its directory).
const EXPORT = 'owlpost-peer.json'
async function exportPeerFile(api: Api) {
  const r = await api.owl(['contact', 'export'])
  if (!r.ok) return api.say(`✗ ${r.out.split('\n')[0]}`)
  const w = await api.writePeerFile(r.out + '\n')
  api.say(
    w === 'written' ? `✓ peer file written to ./${EXPORT}` : w === 'link' ? `✗ ./${EXPORT} is a symlink, not written` : `✗ could not write ./${EXPORT}`,
  )
}

async function copyPeerFile(api: Api) {
  const r = await api.owl(['contact', 'export'])
  const copied = r.ok && (await api.copy(r.out + '\n'))
  api.say(copied ? '✓ peer file copied' : `✗ ${r.ok ? 'the clipboard did not take it' : r.out.split('\n')[0]}`)
}

// The rows of a card, label column 14 wide as the mock; the name in bold, `yes` of Answers in
// green, and for this machine the daemon as `owl presence` last found it.
function Rows({ c, card, daemon }: { c: Ctx; card: Card; daemon?: boolean }) {
  const { Box, Text } = c.ui
  const all = rows(card)
  return (
    <Box flexDirection="column">
      {all.map(([label, value]) => (
        <Box key={label} flexDirection="row" height={1} overflow="hidden">
          <Box width={14} flexShrink={0}>
            <Text color={P.secondary}>{label}</Text>
          </Box>
          {label === 'Answers' && value.startsWith('yes') ? (
            <Text wrap="truncate"><Text color={P.ok}>yes</Text>{line(value.slice(3))}</Text>
          ) : (
            <Text bold={label === 'Name' ? true : undefined} wrap="truncate">{line(String(value ?? ''))}</Text>
          )}
        </Box>
      ))}
      {daemon ? (
        <Box key="Daemon" flexDirection="row" height={1} overflow="hidden">
          <Box width={14} flexShrink={0}><Text color={P.secondary}>Daemon</Text></Box>
          {s.presence ? (
            <Text wrap="truncate"><Text color={P.ok}>{SIGN.online}</Text>{line(reachable())}</Text>
          ) : (
            <Text wrap="truncate"><Text color={P.bad}>{SIGN.offline}</Text>{line(` not reachable · ${s.failed.presence}`)}</Text>
          )}
        </Box>
      ) : null}
    </Box>
  )
}

export const card: View = (c) => {
  const { Box, Text, Button } = c.ui
  return {
    body: (
      <Box flexDirection="column" gap={1}>
        <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
          <Text bold>My card</Text>
          <Text color={P.secondary}>what a colleague's agent learns about this machine</Text>
        </Box>
        {mine ? <Rows c={c} card={mine} daemon /> : <Text color={P.bad}>{`${SIGN.bad} no card: is owl set up? Run doctor in Settings.`}</Text>}
        {raw && mine ? <Text>{clip(JSON.stringify(mine, null, 2))}</Text> : null}
        <Box flexDirection="column">
          <Text bold color={P.secondary}>Share</Text>
          <Text color={P.secondary}>Send your peer file to a colleague; they add it with /owlpost:add.</Text>
          <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
            <Button key="copy" plain hotkey="y" label="Copy peer file" onPress={() => void copyPeerFile(c.api)} />
            <Button key="export" plain hotkey="e" label="Export to a file" onPress={() => void exportPeerFile(c.api)} />
            <Button key="raw" plain hotkey="j" label={raw ? 'Hide raw JSON' : 'Show raw JSON'}
              onPress={() => { raw = !raw; c.api.redraw() }} />
          </Box>
        </Box>
        <Box flexDirection="column">
          <Text bold color={P.secondary}>A colleague's card</Text>
          <Text color={P.secondary}>
            {'Open one from Contacts with '}
            <Text color={P.accent}>k</Text>
            {', or fetch it by name, e-mail or fingerprint:'}
          </Text>
          <Field c={c} k="peer" label="Peer" placeholder="name, e-mail or fingerprint" value={peer} submitLabel="Fetch"
            set={(v) => { peer = v }} onSubmit={(v) => void fetchPeer(c.api, v)} />
          {theirs ? <Rows c={c} card={theirs} /> : null}
        </Box>
      </Box>
    ),
    keys: [['y', 'copy peer file'], ['e', 'export'], ['j', 'raw JSON'], ['h', 'keys'], ['esc', 'prompt']],
    note: 'nothing here leaves the machine until you paste it',
  }
}
