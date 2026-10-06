// Settings tab (`5`): the two inbox switches (live watch, band above the prompt), the
// harnesses that draft answers (`owl harness`: list, scan PATH, add, edit the command, remove
// with `x` while the ring is on its row, pick the drafting one), the daemon service (install,
// uninstall with `x` otherwise — it asks first) with its last pull, `owl doctor` row by row,
// and the version with an update. The switches live in plugin.json of the owlpost home
// (`setSwitch`).
import { enter, reachable, s, type Api, type Check, type Harness } from './lib'
import { Chosen, INPUT_MAX, P, SIGN, ago, clean, clip, line, pulse, type Ctx, type View } from './ui'

let checks: Check[] = []
let ranAt = '' // when doctor last ran, '' before it did
let version = ''
let confirming = false
let harnesses: Harness[] = []
let sel = '' // the name of the selected harness row
let editing = '' // the name whose command the edit field is open for ('' = closed)
let editCmd = '' // the edit field's text (kept over a failed save)
let adding = false // the add block (name and command fields) shows
let addName = ''
let addCmd = ''

// `owl harness list --json`; a failure leaves the list empty.
async function loadHarnesses(api: Api) {
  harnesses = await api.json<Harness[]>(['harness', 'list'], [])
}

// One harness command (`owl harness <args>`), then the list again: the note says what owl did.
async function harness(api: Api, args: string[]) {
  const ok = await api.act(['harness', ...args])
  await loadHarnesses(api)
  api.redraw()
  return ok
}

// The selected harness row: the one the focus ring is on, else the one selected before, else
// the first. The edit field closes when the selection moves to another row.
function chosenHarness(): Harness | undefined {
  const ringed = harnesses.find((h) => s.ring === `harness-${h.name}`)
  if (ringed) sel = ringed.name
  const chosen = harnesses.find((h) => h.name === sel) ?? harnesses[0]
  if (editing && chosen?.name !== editing) {
    editing = ''
    editCmd = ''
  }
  return chosen
}

// `e` puts the current command into the field on one line (Enter splits it at whitespace
// anyway); one the engine cannot hold is not opened.
function openEdit(c: Ctx, h: Harness) {
  const text = clean(h.cmd.join(' ')).replace(/[\t\n]/g, ' ')
  if (text.length > INPUT_MAX) return c.api.say(`✗ the command has ${text.length} characters, too many to edit here`)
  editing = h.name
  editCmd = text
  c.api.redraw()
  c.api.focus('harness-cmd-edit')
}

// A name owl would read as a flag (clap takes `<NAME> -- <CMD>...`) never runs.
function dashed(api: Api, name: string) {
  if (!name.startsWith('-')) return false
  api.say('✗ a harness name cannot start with -')
  return true
}

// Enter in the edit field saves the command (`owl harness edit <name> -- <words>`); a failure
// keeps the field and its text, a success closes it.
async function saveEdit(api: Api, name: string, v: string) {
  editCmd = v
  const words = v.trim().split(/\s+/).filter(Boolean)
  if (!words.length) return api.say('✗ type a command first')
  if (dashed(api, name)) return
  const ok = await harness(api, ['edit', name, '--', ...words])
  if (!ok) return
  editing = ''
  editCmd = ''
  api.redraw()
}

// Enter in the add block's command field (`owl harness add <name> -- <words>`); a failure
// keeps both fields, a success clears them and closes the block.
async function addHarness(api: Api, v: string) {
  addCmd = v
  const name = addName.trim()
  if (!name) return api.say('✗ type a name first')
  const words = v.trim().split(/\s+/).filter(Boolean)
  if (!words.length) return api.say('✗ type a command first')
  if (dashed(api, name)) return
  const ok = await harness(api, ['add', name, '--', ...words])
  if (!ok) return
  addName = addCmd = ''
  adding = false
  api.redraw()
}

// The version line comes from `owl --version` (`owl 0.3.0`); entering the tab also drops a
// pending uninstall question and reloads the harnesses.
enter.settings = async (api) => {
  confirming = false
  const r = await api.owl(['--version'])
  version = r.ok ? r.out : ''
  await loadHarnesses(api)
}

// `Live watch` flips the wake-on-message switch and says what changes when.
async function flipWatch(c: Ctx) {
  await c.api.setSwitch('watch', !s.watch)
  s.watch = !s.watch
  c.api.say(s.watch ? '✓ live watch on — from the next session start' : '✓ live watch off — new sessions do not wake')
}

// The band reads `s.band` while drawing, so it reacts at once.
async function flipBand(c: Ctx) {
  await c.api.setSwitch('band', !s.band)
  s.band = !s.band
  c.api.say(s.band ? '✓ band on' : '✓ band off')
}

// The service goes away only after the question was answered with Confirm.
async function uninstall(c: Ctx) {
  confirming = false
  await c.api.act(['uninstall'])
}

// `owl doctor --json` prints its rows on stdout even when a check fails (exit 1), so the
// rows come from `stdout`, never from `out` (stderr on a failure). An answer that is not
// rows — not JSON, or JSON that is not an array — clears the list and leaves the first
// output line in the note (the parse, or the `filter` on a non-array, throws into the catch).
async function doctor(c: Ctx) {
  c.api.say('… owl doctor')
  const r = await c.api.owl(['doctor', '--json'])
  ranAt = new Date().toISOString()
  try {
    checks = JSON.parse(r.stdout) as Check[]
    const failed = checks.filter((x) => x.status === 'fail').length
    c.api.say(failed ? `✗ doctor: ${failed} check(s) failed` : '✓ doctor: all checks ok')
  } catch {
    checks = []
    c.api.say(`✗ ${r.out.split('\n')[0]}`)
  }
}

// A check's status as the mock writes it: the word (`ok`, `warn`, `fail`) in its colour.
const mark = (status: string) => ({ word: status, color: status === 'ok' ? P.ok : status === 'warn' ? P.wait : P.bad })

// A settings row as the mock: a 2-wide indent, the label in a 24-wide column, the value.
function Row({ c, label, children }: { c: Ctx; label?: string; children: unknown }) {
  const { Box, Text } = c.ui
  return (
    <Box flexDirection="row">
      <Box width={2} flexShrink={0} />
      <Box width={24} flexShrink={0}>{label ? <Text>{label}</Text> : null}</Box>
      <Box flexDirection="row" flexShrink={1} columnGap={1}>{children as never}</Box>
    </Box>
  )
}

// An on/off switch as the mock: the current position drawn chosen, the other one a dim Button
// (keyed by the switch) that flips it.
function Switch({ c, k, on, flip }: { c: Ctx; k: string; on: boolean; flip: () => void }) {
  const { Box, Button } = c.ui
  const other = <Box paddingX={1}><Button key={k} plain dimColor label={on ? 'off' : 'on'} onPress={flip} /></Box>
  return (
    <Box flexDirection="row" columnGap={1}>
      {on ? <Chosen c={c} text="on" /> : other}
      {on ? other : <Chosen c={c} text="off" />}
    </Box>
  )
}

const Title = ({ c, text, sub }: { c: Ctx; text: string; sub?: string }) => (
  <c.ui.Box flexDirection="row" columnGap={1}>
    <c.ui.Text bold color={P.secondary}>{text}</c.ui.Text>
    {sub ? <c.ui.Text color={P.secondary}>{sub}</c.ui.Text> : null}
  </c.ui.Box>
)

export const settings: View = (c) => {
  const { Box, Text, Button, Input } = c.ui
  const chosen = chosenHarness()
  // One `x` on the screen (two on one hotkey: the later wins): the selected harness row's
  // Remove holds it while the ring is on that row or its actions, the daemon's Uninstall
  // (which still asks first) holds it otherwise.
  const xRemoves = !!chosen && ['harness-use', 'harness-edit', 'harness-remove', 'harness-cmd-edit', `harness-${chosen.name}`].includes(s.ring)
  const v = /\d+\.\d+\.\d+\S*/.exec(version)?.[0] ?? version
  return {
    body: (
      <Box flexDirection="column" gap={1}>
        <Box flexDirection="column">
          <Title c={c} text="Inbox" />
          <Row c={c} label="Live watch"><Switch c={c} k="watch" on={s.watch} flip={() => void flipWatch(c)} /></Row>
          <Row c={c}><Text color={P.secondary}>wake new sessions when a message arrives</Text></Row>
          <Row c={c} label="Band above the prompt"><Switch c={c} k="band" on={s.band} flip={() => void flipBand(c)} /></Row>
          <Row c={c}><Text color={P.secondary}>shows the unseen count and opens this panel</Text></Row>
        </Box>
        <Box flexDirection="column">
          <Title c={c} text="Harnesses" sub="who writes the draft you approve" />
          {harnesses.length === 0 ? (
            <Box paddingLeft={2}><Text color={P.secondary}>No harnesses. s: Scan PATH for harnesses adds the known ones found.</Text></Box>
          ) : (
            harnesses.map((x) => {
              const on = x === chosen
              return (
                // The row as the mock draws it: the name, then the program only, cut with an
                // ellipsis, then the status and the drafts badge, which never shrink or wrap.
                // Edit still gets the WHOLE command.
                <Box key={x.name} flexDirection="column" backgroundColor={on ? P.selected : undefined}>
                  <Box flexDirection="row" height={1} overflow="hidden">
                    <Box width={2} flexShrink={0}><Text color={P.accent}>{on ? SIGN.selected : ' '}</Text></Box>
                    <Box width={10} flexShrink={0} overflow="hidden">
                      <Button key={`harness-${x.name}`} plain label={line(x.name)}
                        onPress={() => void harness(c.api, ['use', '--', x.name])} />
                    </Box>
                    <Box flexGrow={1} flexShrink={1}>
                      <Text color={P.secondary} wrap="truncate">{line(x.cmd[0] ?? '')}</Text>
                    </Box>
                    <Box flexDirection="row" columnGap={1} flexShrink={0}>
                      {!x.enabled ? (
                        <Text color={P.bad}>{`${SIGN.bad} disabled`}</Text>
                      ) : x.found ? (
                        <Text color={P.ok}>{`${SIGN.ok} found`}</Text>
                      ) : (
                        <Text color={P.wait}>warn not found</Text>
                      )}
                      {x.drafting ? <Chosen c={c} text="drafts" /> : null}
                    </Box>
                  </Box>
                  {on ? (
                    <Box flexDirection="row" flexWrap="wrap" columnGap={2} paddingLeft={2}>
                      <Box flexDirection="row">
                        <Text color={P.accent}>enter: </Text>
                        <Button key="harness-use" plain label="Use for drafts" onPress={() => void harness(c.api, ['use', '--', x.name])} />
                      </Box>
                      <Button key="harness-edit" plain hotkey="e" label="Edit command" onPress={() => openEdit(c, x)} />
                      <Button key="harness-remove" plain hotkey={xRemoves ? 'x' : undefined} label="Remove"
                        onPress={() => void harness(c.api, ['remove', '--', x.name])} />
                    </Box>
                  ) : null}
                  {on && editing === x.name ? (
                    <Box paddingLeft={2} borderStyle="single" borderColor={P.rule}>
                      <Input key="harness-cmd-edit" label="Command" value={editCmd}
                        onInput={(v) => { editCmd = v }} onSubmit={(v) => void saveEdit(c.api, x.name, v)} />
                    </Box>
                  ) : null}
                </Box>
              )
            })
          )}
          <Box flexDirection="row" flexWrap="wrap" columnGap={2} paddingLeft={2}>
            <Button key="harness-scan" plain hotkey="s" label="Scan PATH for harnesses" onPress={() => void harness(c.api, ['scan'])} />
            <Button key="harness-add" plain hotkey="a" label="Add harness"
              onPress={() => { adding = !adding; if (adding) c.api.focus('harness-name'); c.api.redraw() }} />
          </Box>
          {adding ? (
            <Box flexDirection="column" paddingLeft={2} borderStyle="single" borderColor={P.rule}>
              <Input key="harness-name" label="Name" value={addName}
                onInput={(v) => { addName = v }} onSubmit={(v) => { addName = v; c.api.focus('harness-cmd') }} />
              <Input key="harness-cmd" label="Command" value={addCmd} submitLabel="Add"
                onInput={(v) => { addCmd = v }} onSubmit={(v) => void addHarness(c.api, v)} />
            </Box>
          ) : null}
        </Box>
        <Box flexDirection="column">
          <Title c={c} text="Daemon" />
          <Row c={c} label="Service">
            {s.presence ? (
              <Text wrap="truncate"><Text color={P.ok}>{SIGN.online}</Text>{line(reachable())}</Text>
            ) : (
              <Text color={P.secondary} wrap="truncate"><Text color={P.bad}>{SIGN.offline}</Text>{line(` not reachable · ${s.failed.presence}`)}</Text>
            )}
          </Row>
          <Row c={c}>
            <Box flexDirection="row" columnGap={2}>
              <Button key="install" plain hotkey="i" label={s.presence ? 'Reinstall' : 'Install'} onPress={() => void c.api.act(['install'])} />
              {confirming ? (
                <Box flexDirection="row" columnGap={2}>
                  <Text color={P.wait}>Uninstall the daemon service?</Text>
                  <Button key="uninstall-confirm" plain label="Confirm uninstall" onPress={() => void uninstall(c)} />
                  <Button key="uninstall-cancel" plain label="Cancel" onPress={() => { confirming = false; c.api.redraw() }} />
                </Box>
              ) : (
                <Button key="uninstall" plain hotkey={xRemoves ? undefined : 'x'} label="Uninstall" onPress={() => { confirming = true; c.api.redraw() }} />
              )}
            </Box>
          </Row>
          {s.presence ? <Row c={c} label="Last pull"><Text>{line(pulse())}</Text></Row> : null}
        </Box>
        <Box flexDirection="column">
          <Title c={c} text="Doctor" />
          <Box flexDirection="row">
            <Box width={2} flexShrink={0} />
            <Box width={24} flexShrink={0}><Button key="doctor" plain hotkey="d" label="Run doctor" onPress={() => void doctor(c)} /></Box>
            <Text color={P.secondary}>{ranAt ? `last run ${ago(ranAt).replace(/^\d+s ago$/, 'just now')}` : 'not run yet'}</Text>
          </Box>
          {checks.map((x, i) => {
            const m = mark(x.status)
            return (
              // `owl doctor` repeats check names (one row per harness), so the key is the index.
              <Box key={`check-${i}`} flexDirection="row" height={1} overflow="hidden">
                <Box width={2} flexShrink={0} />
                <Box width={5} flexShrink={0}><Text color={m.color}>{line(m.word)}</Text></Box>
                <Box width={10} flexShrink={0}><Text wrap="truncate">{line(x.check)}</Text></Box>
                <Text color={P.secondary} wrap="truncate">{clip(x.detail)}</Text>
              </Box>
            )
          })}
        </Box>
        <Box flexDirection="column">
          <Title c={c} text="Version" />
          <Row c={c} label="owlpost">{v ? <Text>{line(v)}</Text> : <Text color={P.bad}>{`${SIGN.bad} version unknown`}</Text>}</Row>
          <Row c={c}><Button key="update" plain hotkey="u" label="Update" onPress={() => void c.api.act(['update'], { timeoutMs: 600_000 })} /></Row>
        </Box>
      </Box>
    ),
    keys: [['tab', 'move'], ['↑↓', 'scroll'], ['enter', 'use for drafts'], ['e', 'edit'], ['s', 'scan'], ['a', 'add'], ['d', 'doctor'], ['u', 'update']],
    note: 'switches are stored in plugin.json of the owlpost home',
  }
}
