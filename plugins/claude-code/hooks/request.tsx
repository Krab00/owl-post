// The incoming-request screen (Chats → a thread whose request waits): the thread so far, the
// request with what happened to it, the unsent draft in a dashed box, and the actions its state
// allows — consent (allow once / always, deny; the labels name the fingerprint, because a peer
// is its key), draft / redraft, send, edit, reject, and an own answer for a question. A
// question is drafted by the model through `/owlpost:draft`; a file request and a tool call
// are drafted by `owl draft` itself, with no model turn: **Run (owl draft)** runs the tool.
import { VERIFIED, enter, groups, isUuid, mention, mustVerify, s, short, type Api, type Draft, type Ev, type Route } from './lib'
import { BackRow, Bubble, DateRule, Field, Hidden, INPUT_MAX, Message, P, Rule, clean, line, SIGN, hhmm, status, type Ctx, type Hint, type View } from './ui'

type Request = Extract<Route, { name: 'request' }>

let draft: Draft | undefined
let shownFor = '' // the ts of the record's newest event when `draft` was loaded
let loading = false
let answer = '' // the Own answer field
let verifying = '' // the fingerprint Allow always waits on "I verified this fingerprint" for

const OPEN = ['consent', 'pending', 'drafted']

// The newest event of the shown record.
const newest = (r: Request) => [...s.timeline].reverse().find((e) => e.record_id === r.record)

// The draft of the record when it has one, from `owl show <id> --json`.
async function load(api: Api) {
  const r = s.route
  if (r.name !== 'request') return
  const e = newest(r)
  draft = e?.state === 'drafted' ? (await api.json<{ draft?: Draft }>(['show', '--', r.record], {})).draft : undefined
  shownFor = e?.ts ?? ''
}

// The record moved on since its draft was loaded (a redraft landed): no box and no Send until
// the new draft is loaded.
const stale = (e: Ev) => e.state === 'drafted' && e.ts !== shownFor

enter.request = async (api) => {
  answer = ''
  verifying = ''
  await load(api)
}

// After an owl action of this screen: the draft again (it may have changed or gone).
async function reload(api: Api) {
  await load(api)
  api.redraw()
}

const run = (api: Api, args: string[]) => api.act(args).then(async (ok) => (await reload(api), ok))

// Send signs what the record holds now, so it runs only while that is the draft on screen: a
// redraft that landed since (the model writes it later) is shown first and needs another `s`.
async function send(api: Api, record: string) {
  const now = (await api.json<{ draft?: Draft }>(['show', '--', record], {})).draft
  if (now?.text !== draft?.text) {
    await reload(api)
    return api.say('✗ the draft changed: read the new one, then press s again')
  }
  await run(api, ['send', '--', record])
}

// One secondary line per later event of the record that the screen does not draw otherwise:
// `14:15 · allowed once by you`. The receipt is the message itself, the hold is the consent
// box, and a draft is its own bubble.
const SHOWN = ['received', 'held', 'drafted', 'state-seen']
function said(e: Ev) {
  const by = e.by === 'human' ? 'you' : e.by
  const what = e.kind === 'allowed' && e.detail?.scope ? `allowed ${String(e.detail.scope)}` : e.kind
  return `${hhmm(e.ts)} · ${what}${by ? ` by ${by}` : ''}`
}

// What the request asks for, after its kind in the header: `asks for a memory entry`,
// `asks for src/x.rs`, `asks to run tool {…}`.
function asks(e: Ev) {
  if (e.type === 'content') return e.detail?.memory || /^memory:/.test(e.text) ? 'asks for a memory entry' : `asks for ${e.text}`
  if (e.type === 'tool-call') return `asks to run ${e.text}`
  return ''
}

// The consent box (yellow border): held, why, and allow once / always / deny.
function Consent({ c, r, e }: { c: Ctx; r: Request; e: Ev }) {
  const { Box, Text, Button } = c.ui
  const memory = asks(e) === 'asks for a memory entry'
  return (
    <Box flexDirection="column" borderStyle="single" borderColor={P.wait} paddingX={1} paddingY={1}>
      <Text bold color={P.wait}>Held for consent</Text>
      <Text color={P.secondary}>
        {memory ? "A memory entry is held every time, whatever the contact's policy." : 'Your policy for this contact is manual: nothing runs until you allow it.'}
      </Text>
      <Box flexDirection="row" flexWrap="wrap" columnGap={2} marginTop={1}>
        <Button key="allow-once" plain hotkey="o" label="Allow once" onPress={() => void run(c.api, ['allow', r.peer, '--once'])} />
        <Button key="allow-always" plain hotkey="w" label="Allow always"
          onPress={() => {
            if (!mustVerify(r.peer)) return void run(c.api, ['allow', r.peer, '--always'])
            verifying = r.peer
            c.api.redraw()
          }} />
        <Button key="deny" plain hotkey="x" label="Deny" onPress={() => void run(c.api, ['deny', r.peer])} />
      </Box>
      {verifying === r.peer ? <Verify c={c} r={r} /> : null}
    </Box>
  )
}

// After consent: the draft as our bubble (dashed accent border, `not sent`) with Send / Edit /
// Redraft / Reject under it at the right, or, with no draft yet, Draft and Reject; then the Own
// answer field for a question.
function Actions({ c, r, e }: { c: Ctx; r: Request; e: Ev }) {
  const { Box, Text, Button } = c.ui
  const drafted = e.state === 'drafted' && !stale(e)
  // A file request and a tool call are answered by owl itself: no model draft, no own answer.
  const question = e.type !== 'tool-call' && e.type !== 'content'
  const draftButton =
    e.type === 'tool-call' ? (
      <Button key="draft" plain hotkey="d" label="Run (owl draft)" dimColor={drafted ? true : undefined} onPress={() => void c.api.act(['draft', '--', r.record]).then(() => reload(c.api))} />
    ) : e.type === 'content' ? (
      <Button key="draft" plain hotkey="d" label={drafted ? 'Redraft' : 'Draft'} dimColor={drafted ? true : undefined} onPress={() => void run(c.api, ['draft', '--', r.record])} />
    ) : (
      <Button key="draft" plain hotkey="d" label={drafted ? 'Redraft' : 'Draft with Claude'} dimColor={drafted ? true : undefined}
        onPress={() => void c.api.command('owlpost:draft', r.record).then(() => c.api.say('… drafting with Claude'))} />
    )
  const reject = <Button key="reject" plain hotkey="x" label="Reject" dimColor onPress={() => void run(c.api, ['reject', '--', r.record])} />
  return (
    <Box flexDirection="column" gap={1}>
      {drafted && draft ? (
        <Box flexDirection="column" alignItems="flex-end">
          <Text color={P.secondary}>
            {line(`${hhmm(draft.drafted_at || e.ts)} · draft · by ${draft.harness} · `)}
            <Text color={P.wait}>not sent</Text>
          </Text>
          <Bubble c={c} text={draft.text} mine dashed />
          <Box flexDirection="row" flexWrap="wrap" columnGap={2} justifyContent="flex-end">
            <Button key="send" plain hotkey="s" label="Send" onPress={() => void send(c.api, r.record)} />
            {question ? (
              <Button key="edit" plain hotkey="e" label="Edit"
                onPress={() => {
                  // The draft goes into the field, which refuses what the engine cannot draw there.
                  const text = draft?.text ?? ''
                  if (text.length > INPUT_MAX) return c.api.say(`✗ the draft has ${text.length} characters, too many to edit here`)
                  answer = clean(text)
                  c.api.redraw()
                  c.api.focus('answer')
                }} />
            ) : null}
            {draftButton}
            {reject}
          </Box>
        </Box>
      ) : (
        <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
          {draftButton}
          {reject}
        </Box>
      )}
      {question ? (
        <Field c={c} k="answer" label="Own answer" value={answer} submitLabel="Save draft"
          placeholder={drafted ? 'type your own answer — enter replaces the draft' : 'type your own answer — enter stores it as the draft'}
          set={(v) => { answer = v }}
          onSubmit={(v) => {
            if (!v.trim()) return c.api.say('✗ type an answer first')
            // `--text=`: an answer starting with `-` is text, not a flag; it stays in the field
            // until owl took it.
            void run(c.api, ['draft', `--text=${v}`, '--', r.record]).then((ok) => {
              if (ok) answer = ''
              c.api.redraw()
            })
          }} />
      ) : null}
      {question ? (
        <Hidden c={c} k="use-prompt" hotkey="u" onPress={() => void c.api.promptText().then((t) => { answer = t; c.api.redraw() })} />
      ) : null}
    </Box>
  )
}

// Allow always for a contact added by hand: owl needs the human's word that they compared the
// fingerprint out of band (`VERIFIED`), so the full fingerprint is shown and nothing runs
// before "I verified this fingerprint".
function Verify({ c, r }: { c: Ctx; r: Request }) {
  const { Box, Text, Button } = c.ui
  const who = line(s.contacts.find((x) => x.fingerprint === r.peer)?.name ?? r.peer)
  return (
    <Box flexDirection="column" marginTop={1}>
      <Text>{`Allow always answers ${who} without asking you. Compare this fingerprint with ${who} out of band (a call, a chat) first:`}</Text>
      <Text bold>{line(r.peer)}</Text>
      <Box flexDirection="row" gap={2}>
        <Button key="verify-confirm" plain label="I verified this fingerprint"
          onPress={() => { verifying = ''; void run(c.api, ['allow', r.peer, '--always', VERIFIED]) }} />
        <Button key="verify-cancel" plain label="Cancel" onPress={() => { verifying = ''; c.api.redraw() }} />
      </Box>
    </Box>
  )
}

// Footer hints as the mock lists them: one line for every state of the request.
const HINTS: Hint[] = [
  ['tab', 'move'], ['s', 'send'], ['e', 'edit'], ['d', 'draft'], ['x', 'reject'],
  ['o', 'allow once'], ['w', 'allow always'], ['c', 'cite'], ['b', 'back'],
]

// Whether message `i` of a list starts a day: the first, or another day than the one before.
const newDay = (list: Ev[], i: number) => i === 0 || new Date(list[i - 1]!.ts).toDateString() !== new Date(list[i]!.ts).toDateString()

export const request: View = (c) => {
  const { Box, Text } = c.ui
  const r = s.route as Request
  const evs = s.timeline.filter((e) => (e.context_id ?? e.record_id) === r.context)
  const who = line(s.threads.find((t) => t.from === r.peer)?.from_name || s.contacts.find((x) => x.fingerprint === r.peer)?.name || r.peer)
  const mine = evs.filter((e) => e.record_id === r.record)
  const e = newest(r)
  const first = groups(evs)[0]?.first
  // The other records of the thread, each drawn once: its first event (time, author, text)
  // with its newest state, as a thread draws them.
  const newestOf = new Map(evs.map((x) => [x.record_id, x]))
  const others = [...new Map(evs.filter((x) => x.record_id !== r.record).reverse().map((x) => [x.record_id, x])).values()]
    .reverse()
    .map((x) => ({ ...x, state: newestOf.get(x.record_id)!.state }))
  const st = e ? status(e) : undefined
  if (e && stale(e) && !loading) {
    loading = true
    void reload(c.api).finally(() => { loading = false })
  }
  const later = mine.slice(1).filter((x) => !SHOWN.includes(x.kind))
  const head = mine[0] ? { ...mine[0], state: e?.state ?? mine[0].state } : undefined
  // `c` cites the request itself into the prompt box, as an answer is cited in a thread.
  const cite = async () => {
    const ok = isUuid(r.record) && (await c.api.fill(`${mention(r.record)} `, 'insert'))
    c.api.say(ok ? `✓ "${mention(short(r.record))}" is in the prompt` : '✗ the prompt box did not take it')
  }
  return {
    body: (
      <Box flexDirection="column" gap={1}>
        <BackRow c={c} crumb={who} title={first?.text ?? ''} id={r.context} />
        {others.map((x, i) => (
          <Box key={`msg-${x.record_id}`} flexDirection="column">
            {newDay(others, i) ? <Box flexDirection="column" marginBottom={1}><DateRule c={c} ts={x.ts} /></Box> : <Rule c={c} />}
            <Message c={c} e={x} who={who} />
          </Box>
        ))}
        {e && st && head ? (
          <Box flexDirection="column" gap={1}>
            {newDay([...others, head], others.length) ? <DateRule c={c} ts={head.ts} /> : <Rule c={c} />}
            <Message c={c} e={head} who={who} right={st} extra={asks(e)} />
            {e.state === 'consent' ? <Consent c={c} r={r} e={e} /> : null}
            {e.state !== 'consent' && later.length ? (
              <Box flexDirection="column">
                <Rule c={c} />
                {later.map((x, i) => (
                  <Text key={`ev-${i}`} color={P.secondary} wrap="truncate">{line(said(x))}</Text>
                ))}
              </Box>
            ) : null}
            {OPEN.includes(e.state) && e.state !== 'consent' ? <Actions c={c} r={r} e={e} /> : null}
            {OPEN.includes(e.state) ? null : <Text color={st.color}>{line(`${st.sign} ${e.kind}`)}</Text>}
            <Hidden c={c} k="cite" hotkey="c" onPress={() => void cite()} />
          </Box>
        ) : (
          <Text color={P.secondary}>{`${SIGN.none} this request is not here any more`}</Text>
        )}
      </Box>
    ),
    keys: HINTS,
    note: 'nothing leaves this machine until you press s',
  }
}
