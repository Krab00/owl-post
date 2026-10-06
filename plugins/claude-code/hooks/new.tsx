// New tab (`3`): a question (`owl ask`), a file request (`owl request`) or a tool call
// (`owl call`) to one contact, in a new thread or continuing one of theirs. Other views hand
// over the addressee in `s.compose`.
import { enter, groups, replyTarget, s, type Api, type Ev } from './lib'
import { Chosen, Field, P, Spacer, hhmm, line, when, type Ctx, type Hint, type View } from './ui'

type Kind = 'ask' | 'request' | 'call'

const KINDS: [Kind, string][] = [
  ['ask', 'Ask a question'],
  ['request', 'Request a file'],
  ['call', 'Call a tool'],
]

let kind: Kind = 'ask'
let to = '' // the picked contact's fingerprint
let filter = '' // the To field while nobody is picked
let events: Ev[] = [] // the picked person's events
let thread = '' // the context id of the thread to continue; '' starts a new one
let attachFile = false // `f` opened the local-file field of Context
const f = { path: '', context: '', text: '', project: '', ref: '', tool: '' }

// The picked person's threads that can be continued (`--reply-to` needs a record of ours or
// an answer we got).
const continuable = () => groups(events).filter((g) => replyTarget(g.events) !== '')

async function pick(api: Api, fp: string) {
  to = fp
  filter = ''
  events = await api.json<Ev[]>(['thread', fp], [])
  thread = ''
  api.redraw()
}

enter.new = async (api) => {
  if (!s.compose.to) return
  const fp = s.compose.to
  s.compose = { to: '' }
  await pick(api, fp)
}

// `--reply-to` of the chosen thread, '' for a new one.
function replyTo() {
  const g = continuable().find((x) => x.context === thread)
  return g ? replyTarget(g.events) : ''
}

const opt = (flag: string, v: string) => (v ? [flag, v] : [])

async function send(api: Api) {
  const v = { path: f.path.trim(), context: f.context.trim(), text: f.text.trim(), project: f.project.trim(), ref: f.ref.trim(), tool: f.tool.trim() }
  const need: [string, string][] =
    kind === 'ask' ? [['Question', v.text]] : kind === 'request' ? [['Project', v.project], ['Path', v.path]] : [['Tool', v.tool], ['Input JSON', v.text]]
  const missing = [...(to ? [] : ['To']), ...need.filter(([, x]) => !x).map(([label]) => label)]
  if (missing.length) return api.say(`✗ missing: ${missing.join(', ')}`)
  const reply = opt('--reply-to', replyTo())
  // The typed words go after `--`, so one starting with `-` is text, not a flag.
  const ok =
    kind === 'ask'
      ? await api.act(['ask', '--peer', to, ...opt('--file', v.path), ...opt('--context', v.context), ...reply, '--', v.text])
      : kind === 'request'
        ? await api.act(['request', ...opt('--ref', v.ref), ...reply, '--', to, v.project, v.path])
        : await api.act(['call', '--input', '-', ...reply, '--', to, v.tool], { stdin: v.text })
  if (ok) Object.assign(f, { path: '', context: '', text: '', project: '', ref: '', tool: '' })
  api.redraw()
}

// A Continue row's time: `today 07:15`, else the day and the time.
const at = (ts: string) => `${when(ts).match(/\d\d:\d\d/) ? 'today' : when(ts)} ${hhmm(ts)}`

function To({ c }: { c: Ctx }) {
  const { Box, Text, Button } = c.ui
  if (to) {
    const who = s.contacts.find((x) => x.fingerprint === to)
    return (
      <Box flexDirection="column">
        <Text color={P.secondary}>To</Text>
        <Box flexDirection="row" gap={2}>
          <Box flexGrow={1} flexShrink={1}><Text bold wrap="truncate">{line(who?.name ?? to)}</Text></Box>
          <Box flexShrink={0}><Button key="change" label="Change" onPress={() => { to = ''; c.api.redraw() }} /></Box>
        </Box>
        <Text color={P.secondary}>{line(`${to} · policy ${who?.policy?.mode ?? 'none'}`)}</Text>
      </Box>
    )
  }
  const q = filter.trim().toLowerCase()
  const hits = s.contacts.filter((x) => [x.name, ...x.emails, x.fingerprint].some((t) => t.toLowerCase().includes(q)))
  return (
    <Box flexDirection="column">
      <Field c={c} k="to" label="To" placeholder="name, e-mail or owl:…" value={filter}
        set={(v) => { filter = v; c.api.redraw() }} onSubmit={() => c.api.redraw()} />
      {hits.map((x) => (
        <Button key={`to-${x.fingerprint}`} plain label={line(`${x.name} · ${x.emails.join(', ') || x.fingerprint}`)}
          onPress={() => void pick(c.api, x.fingerprint)} />
      ))}
      {hits.length ? null : <Text color={P.secondary}>no contact matches</Text>}
    </Box>
  )
}

// The thread choice as the mock: `(•) New thread` with what it does, then one `( ) Continue ·`
// row per thread that can go on, with its time at the right; the chosen one in body text.
function Thread({ c }: { c: Ctx }) {
  const { Box, Text, Button } = c.ui
  const choose = (sid: string) => () => { thread = sid; c.api.redraw() }
  return (
    <Box flexDirection="column">
      <Box flexDirection="row" gap={2} height={1} overflow="hidden">
        <Box flexShrink={0}><Text color={P.secondary}>Thread</Text></Box>
        <Text color={P.secondary} wrap="truncate">continuing lets their agent see the earlier messages</Text>
      </Box>
      <Box flexDirection="row" gap={1} height={1} overflow="hidden">
        <Box flexShrink={0}><Button key="thread-new" plain dimColor={thread ? true : undefined} label={`${thread ? '( )' : '(•)'} New thread`} onPress={choose('')} /></Box>
        <Text color={P.secondary} wrap="truncate">starts a separate conversation</Text>
      </Box>
      {continuable().map((g) => (
        <Box key={`row-${g.context}`} flexDirection="row" gap={1} height={1} overflow="hidden">
          <Box flexShrink={1} overflow="hidden">
            <Button key={`thread-${g.context}`} plain dimColor={thread === g.context ? undefined : true}
              label={`${thread === g.context ? '(•)' : '( )'} Continue · ${line(g.first.text)}`} onPress={choose(g.context)} />
          </Box>
          <Spacer c={c} />
          <Box flexShrink={0}><Text color={P.secondary}>{at(g.last.ts)}</Text></Box>
        </Box>
      ))}
    </Box>
  )
}

// One field of the form: its label in secondary text and the bordered one-line Input; Enter
// only keeps the text unless `submit` says otherwise. `picker` lists the checkout's files on `@`.
function FormField({ c, k, label, placeholder, submit, picker, main }: { c: Ctx; k: keyof typeof f; label: string; placeholder?: string; submit?: () => void; picker?: string | true; main?: boolean }) {
  return (
    <Field c={c} k={k} label={label} placeholder={placeholder} value={f[k]} submitLabel={submit ? 'send' : undefined} picker={picker} main={main}
      set={(v) => { f[k] = v }} onSubmit={() => (submit ? submit() : c.api.redraw())} />
  )
}

export const compose: View = (c) => {
  const { Box, Text, Button } = c.ui
  const sendNow = () => void send(c.api)
  const usePrompt = (
    <Button key="use-prompt" plain hotkey="u" label="Use the text in the prompt box"
      onPress={async () => { f.text = await c.api.promptText(); c.api.redraw() }} />
  )
  return {
    body: (
      <Box flexDirection="column" gap={1}>
        <Text bold>New message</Text>
        <Box flexDirection="column">
          <Text color={P.secondary}>Kind</Text>
          <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
            {KINDS.map(([k, label]) =>
              kind === k ? <Box key={`kind-box-${k}`}><Chosen c={c} text={label} /></Box>
                : <Button key={`kind-${k}`} plain dimColor label={label} onPress={() => { kind = k; c.api.redraw() }} />,
            )}
          </Box>
        </Box>
        <To c={c} />
        {to ? <Thread c={c} /> : null}
        {kind === 'ask' ? (
          <Box flexDirection="column" gap={1}>
            <FormField c={c} k="path" label="Path (optional)" placeholder="src/lib.rs" picker="from your checkout: a hint, their repo may differ" />
            <Box flexDirection="column">
              <Text color={P.secondary}>Context (optional)</Text>
              <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
                <Button key="context-focus" plain hotkey="f" label="Attach a file (diff, error, excerpt · ≤ 8 KiB)"
                  onPress={() => { attachFile = true; c.api.redraw(); c.api.focus('context') }} />
                {usePrompt}
              </Box>
              {attachFile || f.context ? (
                <FormField c={c} k="context" label="Local file" placeholder="@ picks a file · its text goes with the question" picker />
              ) : null}
            </Box>
            <FormField c={c} k="text" label="Question" placeholder="what do you want to ask?" submit={sendNow} main />
          </Box>
        ) : kind === 'request' ? (
          <Box flexDirection="column" gap={1}>
            <FormField c={c} k="project" label="Project" />
            <FormField c={c} k="path" label="Path" picker="from your checkout: a hint, their repo may differ" />
            <FormField c={c} k="ref" label="Ref (optional)" placeholder="the owner's current branch" />
          </Box>
        ) : (
          <Box flexDirection="column" gap={1}>
            <FormField c={c} k="tool" label="Tool" />
            <Box flexDirection="column">
              <FormField c={c} k="text" label="Input JSON" placeholder='{"key": "value"}' submit={sendNow} main />
              {usePrompt}
            </Box>
          </Box>
        )}
        <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
          <Button key="send" variant="primary" label="Send" onPress={sendNow} />
          <Text color={P.secondary}>enter sends — the command is the approval</Text>
        </Box>
      </Box>
    ),
    keys: NEW_KEYS.filter(([k]) => (k !== 'u' || kind !== 'request') && (k !== 'f' || kind === 'ask')),
    note: '',
  }
}

// The footer as the mock: what the keys do while typing, then after leaving the field (`u` and
// `f` where the kind has them).
const NEW_KEYS: Hint[] = [['typing', '', P.wait], ['enter', 'send'], ['tab', 'leave the field'], ['then', '', P.secondary], ['u', 'use prompt text'], ['f', 'attach'], ['esc', 'prompt']]
