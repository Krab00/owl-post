// Contacts tab (`2`): the contact book (`owl contact list --json`, sorted by name by the
// shell), adding a colleague's peer file (`owl add`), the per-contact policy
// (`owl allow` / `owl deny`) and removing a contact (`owl contact remove`), which asks first.
// Each row shows the contact's presence as the daemon last probed it (`owl presence`), and
// Ping now (`g`) probes the selected one at once (`owl ping`).
import { VERIFIED, mustVerify, s, type Api, type Contact } from './lib'
import { Chosen, Field, Head, Hidden, Legend, P, Rule, SIGN, Spacer, clip, line, presence, pulse, type Ctx, type View } from './ui'

let filter = ''
let peerFile = ''
let sel = '' // the fingerprint whose row is selected
let confirm = '' // the fingerprint whose removal was asked and waits for a confirm
let verifying = '' // the fingerprint whose auto policy waits for the human's word

// The colour of a policy mode, as the mock: auto green, manual yellow, never red; anything
// unwritten reads as none.
const POLICY: Record<string, string> = { auto: P.ok, manual: P.wait, never: P.bad }

// Contact's `p: Policy`: this tab with that contact selected and its policy choices focused.
export async function openPolicy(api: Api, fp: string) {
  sel = fp
  confirm = verifying = ''
  await api.go({ name: 'contacts' })
  const mode = s.contacts.find((x) => x.fingerprint === fp)?.policy?.mode
  api.focus(`policy-${['auto', 'manual', 'never'].find((m) => m !== mode)}`)
}

// Add: the field's text is the peer file JSON itself or a path to it. A failure keeps the
// text; the note shows what owl said.
async function add(api: Api, v: string) {
  const text = v.trim()
  if (!text) return api.say('✗ paste a peer file first')
  const ok = await api.act(['add', text])
  if (!ok) return
  peerFile = ''
  api.redraw()
}

// A policy choice runs its owl command; the choices and the verify ask close either way.
async function setPolicy(api: Api, args: string[]) {
  await api.act(args)
  verifying = ''
  api.redraw()
}

// `owl ping` exits 0 when the contact answered and 2 when not, printing why in `error`; the
// note (one line, as every note) says which. The rows keep what the daemon last wrote.
async function ping(api: Api, x: Contact) {
  api.say(`… owl ping ${x.name}`)
  const r = await api.owl(['ping', '--json', '--', x.fingerprint])
  let error: unknown
  try {
    error = JSON.parse(r.stdout)?.error
  } catch {}
  if (r.code === 0) return api.say(`✓ ${x.name} is online · probed just now`)
  api.say(r.code === 2 ? `✗ ${x.name} is offline · ${String(error ?? '')}` : `✗ ${r.out.split('\n')[0]}`)
}

// Confirming a removal runs it (a local contact needs `--local`); the ask closes either way.
async function removeContact(api: Api, x: Contact) {
  await api.act(['contact', 'remove', x.fingerprint, ...(x.source === 'local' ? ['--local'] : [])])
  confirm = ''
  api.redraw()
}

// A press on a row that is not the selected one selects it (the mouse's way to the actions);
// on the selected one it opens the chat.
function select(c: Ctx, fp: string) {
  sel = fp
  confirm = verifying = ''
  c.api.redraw()
}

// The selected contact's policy row: `p: Policy` and the three choices, the current one drawn
// chosen. `p` moves the ring onto the first choice that is not the current one.
function Policy({ c, x }: { c: Ctx; x: Contact }) {
  const { Box, Button } = c.ui
  const mode = x.policy?.mode ?? 'none'
  const choose: Record<string, () => void> = {
    // A hand-added contact needs the human's word first; the mod never passes it alone.
    auto: () => (mustVerify(x.fingerprint) ? ((verifying = x.fingerprint), c.api.redraw()) : void setPolicy(c.api, ['allow', x.fingerprint, '--always'])),
    manual: () => void setPolicy(c.api, ['allow', x.fingerprint]),
    never: () => void setPolicy(c.api, ['deny', x.fingerprint]),
  }
  return (
    <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
      <Button key="policy" plain hotkey="p" label="Policy"
        onPress={() => { verifying = ''; c.api.redraw(); c.api.focus(`policy-${['auto', 'manual', 'never'].find((m) => m !== mode)}`) }} />
      {['auto', 'manual', 'never'].map((m) =>
        m === mode ? <Box key={`policy-box-${m}`}><Chosen c={c} text={m} /></Box> : <Button key={`policy-${m}`} plain dimColor label={m} onPress={choose[m]!} />,
      )}
    </Box>
  )
}

export const contacts: View = (c) => {
  const { Box, Text, Button } = c.ui
  const f = filter.trim().toLowerCase()
  const rows = s.contacts.filter(
    (x) =>
      x.name.toLowerCase().includes(f) ||
      x.emails.some((e) => e.toLowerCase().includes(f)) ||
      x.fingerprint.toLowerCase().includes(f),
  )
  const ringed = rows.find((x) => s.ring === `row-${x.fingerprint}`)
  if (ringed && ringed.fingerprint !== sel) {
    sel = ringed.fingerprint
    confirm = verifying = ''
  }
  const chosen = rows.find((x) => x.fingerprint === sel) ?? rows[0]
  const addFocus = () => c.api.focus('add')
  return {
    body: (
      <Box flexDirection="column" gap={1} flexGrow={1}>
        <Box flexDirection="row" columnGap={1}>
          <Text bold>Contacts</Text>
          <Text color={P.secondary}>{String(s.contacts.length)}</Text>
          <Spacer c={c} />
          <Box flexShrink={0}><Button key="add-top" plain hotkey="n" label="Add contact" onPress={addFocus} /></Box>
        </Box>
        <Field c={c} k="filter" label="Filter" hotkey="f" labelDim placeholder="name, e-mail or fingerprint" value={filter}
          set={(v) => { filter = v; confirm = ''; verifying = ''; c.api.redraw() }} onSubmit={() => c.api.redraw()} />
        {s.contacts.length === 0 ? (
          s.failed.contacts ? (
            <Text color={P.bad}>{clip(`${SIGN.bad} ${s.failed.contacts}`)}</Text>
          ) : (
            <Text color={P.secondary}>No contacts yet. n: Add contact takes a colleague's peer file.</Text>
          )
        ) : rows.length === 0 ? (
          <Text color={P.secondary}>{`No contact matches "${filter.trim()}".`}</Text>
        ) : (
          // A table as the mock: `▸`, presence and name, seen, policy, source (2 * 16 7 6); the
          // selected row shows its e-mails and fingerprint, its actions and its policy under it.
          <Box flexDirection="column">
            <Box flexDirection="row" gap={1}>
              <Box width={2} flexShrink={0} />
              <Box flexGrow={1} flexShrink={1}><Head c={c} text="Name" /></Box>
              <Box width={16} flexShrink={0}><Head c={c} text="Seen" /></Box>
              <Box width={7} flexShrink={0}><Head c={c} text="Policy" /></Box>
              <Box width={6} flexShrink={0}><Head c={c} text="Src" /></Box>
            </Box>
            <Rule c={c} />
            <Legend c={c} />
            {rows.map((x, i) => {
              const selected = chosen?.fingerprint === x.fingerprint
              const mode = x.policy?.mode ?? 'none'
              const p = presence(x.fingerprint)
              return (
                <Box key={x.fingerprint} flexDirection="column">
                  {i > 0 ? <Rule c={c} /> : null}
                  <Box flexDirection="column" backgroundColor={selected ? P.selected : undefined}>
                    <Box flexDirection="row" gap={1}>
                      <Box width={2} flexShrink={0}><Text color={P.accent}>{selected ? SIGN.selected : ' '}</Text></Box>
                      <Box flexGrow={1} flexShrink={1} height={1} gap={1} overflow="hidden">
                        <Box flexShrink={0}><Text color={p.color}>{p.sign}</Text></Box>
                        <Button key={`row-${x.fingerprint}`} plain label={line(x.name)}
                          onPress={() => (selected ? void c.api.go({ name: 'contact', peer: x.fingerprint }) : select(c, x.fingerprint))} />
                      </Box>
                      <Box width={16} flexShrink={0} height={1} overflow="hidden">
                        <Text color={P.secondary} wrap="truncate">
                          <Text color={p.color}>{p.sign}</Text>
                          {line(` ${p.short}`)}
                        </Text>
                      </Box>
                      <Box width={7} flexShrink={0}><Text color={POLICY[mode] ?? P.secondary}>{line(mode)}</Text></Box>
                      <Box width={6} flexShrink={0}><Text color={P.secondary}>{line(x.source)}</Text></Box>
                    </Box>
                    {selected ? (
                      <Box flexDirection="column" paddingLeft={3} paddingRight={1}>
                        <Text color={P.secondary} wrap="truncate">{clip(`${x.emails.join(', ')} · ${x.fingerprint}`)}</Text>
                        <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
                          <Box flexDirection="row">
                            <Text color={P.accent}>enter: </Text>
                            <Button key="open" plain label="Open chat" onPress={() => void c.api.go({ name: 'contact', peer: x.fingerprint })} />
                          </Box>
                          <Button key="card" plain hotkey="k" label="Card"
                            onPress={() => { s.cardPeer = x.fingerprint; void c.api.go({ name: 'card' }) }} />
                          <Button key="ping" plain hotkey="g" label="Ping now" onPress={() => void ping(c.api, x)} />
                          {confirm === x.fingerprint ? (
                            <Button key="remove-confirm" plain hotkey="x" label={line(`Confirm remove ${x.name}`)} onPress={() => void removeContact(c.api, x)} />
                          ) : (
                            <Button key="remove" plain hotkey="x" label="Remove" onPress={() => { confirm = x.fingerprint; c.api.redraw() }} />
                          )}
                          {confirm === x.fingerprint ? (
                            <Button key="remove-cancel" plain label="Cancel" onPress={() => { confirm = ''; c.api.redraw() }} />
                          ) : null}
                        </Box>
                        <Policy c={c} x={x} />
                        {verifying === x.fingerprint ? (
                          <Box flexDirection="column">
                            <Text>{`Policy auto answers ${line(x.name)} without asking you. Compare this fingerprint with ${line(x.name)} out of band (a call, a chat) first:`}</Text>
                            <Text bold>{line(x.fingerprint)}</Text>
                            <Box flexDirection="row" gap={2}>
                              <Button key="verify-confirm" plain label="I verified this fingerprint"
                                onPress={() => void setPolicy(c.api, ['allow', x.fingerprint, '--always', VERIFIED])} />
                              <Button key="verify-cancel" plain label="Cancel"
                                onPress={() => { verifying = ''; c.api.redraw() }} />
                            </Box>
                          </Box>
                        ) : null}
                      </Box>
                    ) : null}
                  </Box>
                </Box>
              )
            })}
          </Box>
        )}
        <Box flexGrow={1} />
        <Box flexDirection="column">
          <Field c={c} k="add" label="Add contact" hotkey="n" placeholder="paste a peer file (JSON) or a path to one" value={peerFile} submitLabel="Add"
            set={(v) => { peerFile = v }} onSubmit={(v) => void add(c.api, v)} />
          <Text color={P.secondary}>A colleague gets yours from the Card tab.</Text>
          {/* `u`, not drawn in the mock: the prompt box's text into the field (a peer file is
              several lines, which a one-line field does not take as typed). */}
          <Hidden c={c} k="use-prompt" hotkey="u" onPress={async () => { peerFile = await c.api.promptText(); c.api.redraw(); addFocus() }} />
        </Box>
      </Box>
    ),
    keys: [['tab', 'move'], ['↑↓', 'scroll'], ['enter', 'open chat'], ['k', 'card'], ['p', 'policy'], ['g', 'ping'], ['x', 'remove'], ['n', 'add'], ['f', 'filter']],
    note: s.presence ? `last pull ${pulse()}` : s.failed.presence ? `✗ ${s.failed.presence}` : 'enter on a contact opens the chat',
  }
}
