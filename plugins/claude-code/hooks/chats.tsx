// Chats tab (`1`): one row per person (`owl thread --json`), then that person's threads
// (`owl thread <peer> --json` grouped by context_id), then one thread's messages with the
// reply block (`owl ask --reply-to`). A thread with a request waiting for the owner opens the
// incoming-request screen instead (the shell's `openThread`). Every answer has Cite and Apply,
// which put `@owl:msg://<id>` in the main prompt box.
// A person and a thread can be archived (`owl archive`, hidden from the list; `v` shows the
// archived ones, where `a` brings one back) or deleted (`owl delete`, to the trash, after a
// confirm); the footer's `u: Undo` takes the last of these back. The selected row is the one
// the focus ring was last on (the first row before that); its actions show under it.
import { enter, groups, isUuid, mention, replyTarget, s, short, type Api, type Ev } from './lib'
import { BackRow, DateRule, Field, Filled, Head, Legend, Message, P, Rule, Spacer, ago, clip, fit, line, presence, SIGN, status, when, whenShort, type Ctx, type Hint, type View } from './ui'
import { openPolicy } from './contacts'

let filter = ''
let reply = ''
let path = ''
let ctx = ''
let attach = false // the reply block shows Path and Context
let applied = '' // the answer whose Apply was pressed last on this screen (drawn pressed)
let replyNote = '' // what the last Send did, under the field
let draftFor = '' // the thread the reply, path and local file were typed in
let sending = false // an `owl ask` of Send is in flight: a second press would send the text again
// Replies `owl ask` did not deliver, by thread id: owl keeps no record of them (the ask it
// reserved in `asks/` is removed again), so the thread shows them from here, one per text.
const unsent: Record<string, Ev[]> = {}
let shelf = false // Chats lists the archived people
const sel: Record<string, string> = {} // by row key prefix: the selected person's fingerprint, thread's context
let confirm = '' // the fingerprint or context whose delete waits for a confirm

// The person's name: the contact book, else the thread list, else the fingerprint.
const nameOf = (peer: string) =>
  line(s.contacts.find((x) => x.fingerprint === peer)?.name ?? s.threads.find((t) => t.from === peer)?.from_name ?? peer)

async function newThread(api: Api, to: string) {
  s.compose = { to }
  await api.go({ name: 'new' })
}

// The selected row of a list keyed `<prefix><id>`: the one the ring is on, else the one
// selected before, else the first. A delete waiting for a confirm is dropped when the
// selection moves off its row.
function selected<T>(rows: T[], id: (x: T) => string, prefix: string): T | undefined {
  const ringed = rows.find((x) => s.ring === prefix + id(x))
  if (ringed) sel[prefix] = id(ringed)
  const chosen = rows.find((x) => id(x) === sel[prefix]) ?? rows[0]
  if (confirm && (!chosen || id(chosen) !== confirm)) confirm = ''
  return chosen
}

// Archive / unarchive / delete of a person (`target` [peer]) or of one thread of theirs
// ([`--context=<id>`, `--`, peer]); on success the footer's Undo gets the way back.
async function shelve(api: Api, verb: 'archive' | 'unarchive' | 'delete', target: string[]) {
  confirm = ''
  const ok = await api.act([verb, ...target])
  s.undo = !ok ? [] : verb === 'delete' ? ['undo'] : [verb === 'archive' ? 'unarchive' : 'archive', ...target]
  api.redraw()
}

// `enter: Open`, `a: Archive` and `x: Delete` under the selected row, on its fill; a delete asks
// first, as the mock: `Delete this … from this machine?  y: Delete  n: Keep` and that the other
// side keeps theirs. `what` is `conversation` or `thread`, `who` the other side.
function RowActions({ c, id, what, who, archived, target, open }: { c: Ctx; id: string; what: string; who: string; archived: boolean; target: string[]; open: () => void }) {
  const { Box, Text, Button } = c.ui
  if (confirm === id) {
    return (
      <Box flexDirection="column" paddingLeft={3}>
        <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
          <Text color={P.wait}>{`Delete this ${what} from this machine?`}</Text>
          <Button key="delete-confirm" plain hotkey="y" label="Delete" onPress={() => void shelve(c.api, 'delete', target)} />
          <Button key="delete-cancel" plain hotkey="n" label="Keep" onPress={() => { confirm = ''; c.api.redraw() }} />
        </Box>
        <Text color={P.secondary}>{line(`${who.split(/\s+/)[0]} keeps their copy.`)}</Text>
      </Box>
    )
  }
  return (
    <Box flexDirection="row" flexWrap="wrap" columnGap={3} paddingLeft={3}>
      <Box flexDirection="row">
        <Text color={P.accent}>enter: </Text>
        <Button key="open" plain label="Open" onPress={open} />
      </Box>
      <Button key="archive" plain hotkey="a" label={archived ? 'Unarchive' : 'Archive'}
        onPress={() => void shelve(c.api, archived ? 'unarchive' : 'archive', target)} />
      <Button key="delete" plain hotkey="x" label="Delete" onPress={() => { confirm = id; c.api.redraw() }} />
    </Box>
  )
}

// `v: Show archived` and how many there are, under a rule that closes the table.
function Shelf({ c, on, count, flip }: { c: Ctx; on: boolean; count: number; flip: () => void }) {
  const { Box, Text, Button } = c.ui
  return (
    <Box flexDirection="column">
      <Rule c={c} />
      <Box flexDirection="row" gap={1} paddingLeft={3}>
        <Button key="archived" plain hotkey="v" dimColor label={on ? 'Hide archived' : 'Show archived'} onPress={flip} />
        {on ? null : <Box key="archived-count"><Text color={P.secondary}>{String(count)}</Text></Box>}
      </Box>
    </Box>
  )
}

// The sign column of a person's row: `!` while something waits for us, else the state of
// their newest record (`✓` answered, `…` waiting, `-` denied), blank before it is known.
function rowSign(t: { from: string; open: number }) {
  if (t.open > 0) return { sign: SIGN.open, color: P.wait, label: 'needs your answer', waits: true }
  const e = s.newest[t.from]
  if (!e) return undefined
  const st = status(e)
  return { ...st, sign: st.sign === SIGN.none ? ' ' : st.sign, waits: st.sign === SIGN.wait }
}

let asked = false // the newest events were asked for once by the view (after a hot reload)

export const chats: View = (c) => {
  const { Box, Text, Button } = c.ui
  if (!asked && s.threads.length && !Object.keys(s.newest).length) {
    asked = true
    void c.api.loadNewest()
  }
  const unseen = s.threads.reduce((n, t) => n + t.unseen, 0)
  const waiting = s.threads.reduce((n, t) => n + t.open, 0)
  const f = filter.trim().toLowerCase()
  const list = shelf ? s.archivedChats : s.threads
  const failed = shelf ? s.failed.archived : s.failed.threads
  const rows = list.filter((t) => t.from_name.toLowerCase().includes(f) || t.from.toLowerCase().includes(f) || t.last_summary.toLowerCase().includes(f)
    || !!s.contacts.find((x) => x.fingerprint === t.from)?.emails.some((m) => m.toLowerCase().includes(f)))
  const chosen = selected(rows, (t) => t.from, 'chat-')
  return {
    body: (
      <Box flexDirection="column" gap={1}>
        <Box flexDirection="row" columnGap={2}>
          <Text bold>{shelf ? 'Archived' : 'Chats'}</Text>
          <Text color={P.secondary} wrap="truncate">{`${unseen} unseen · ${waiting} waiting for you`}</Text>
          <Spacer c={c} />
          <Box flexShrink={0}><Button key="new" plain hotkey="n" label="New thread" onPress={() => void newThread(c.api, '')} /></Box>
        </Box>
        <Field c={c} k="filter" label="Filter" hotkey="f" labelDim placeholder="name, e-mail, fingerprint or text" value={filter}
          set={(v) => { filter = v; confirm = ''; c.api.redraw() }} onSubmit={() => c.api.redraw()} />
        {list.length === 0 && failed ? (
          <Text color={P.bad}>{clip(`${SIGN.bad} ${failed}`)}</Text>
        ) : list.length === 0 ? (
          <Text color={P.secondary}>{shelf ? 'No archived conversations.' : 'No conversations yet. n: New thread asks a colleague; their questions land here.'}</Text>
        ) : rows.length === 0 ? (
          <Text color={P.secondary}>{`No conversation matches "${filter.trim()}".`}</Text>
        ) : (
          // A table as the mock: `▸`, name, sign, last message, when, unseen count (2 18 1 * 9 3).
          <Box flexDirection="column">
            <Box flexDirection="row" gap={1}>
              <Box width={2} flexShrink={0} />
              <Box width={18} flexShrink={0}><Head c={c} text="Name" /></Box>
              <Box width={1} flexShrink={0} />
              <Box flexGrow={1} flexShrink={1}><Head c={c} text="Last message" /></Box>
              <Box width={9} flexShrink={0} justifyContent="flex-end"><Head c={c} text="When" /></Box>
              <Box width={3} flexShrink={0}><Head c={c} text="New" /></Box>
            </Box>
            <Rule c={c} />
            <Legend c={c} />
            {rows.map((t, i) => {
              const on = t === chosen
              const p = presence(t.from)
              const sg = rowSign(t)
              return (
                <Box key={`sep-${t.from}`} flexDirection="column">
                  {i > 0 ? <Rule c={c} /> : null}
                  <Box key={t.from} flexDirection="column" backgroundColor={on ? P.selected : undefined}>
                    <Box flexDirection="row" gap={1}>
                      <Box width={2} flexShrink={0}><Text color={P.accent}>{on ? SIGN.selected : ' '}</Text></Box>
                      <Box width={18} height={1} flexShrink={0} gap={1} overflow="hidden">
                        <Box flexShrink={0}><Text color={p.color}>{p.sign}</Text></Box>
                        <Button key={`chat-${t.from}`} plain label={fit(t.from_name, 16)} onPress={() => void c.api.go({ name: 'contact', peer: t.from })} />
                      </Box>
                      <Box width={1} flexShrink={0}><Text bold={t.open > 0 ? true : undefined} color={sg?.color ?? P.secondary}>{sg?.sign ?? ' '}</Text></Box>
                      <Box flexGrow={1} flexShrink={1} height={1} overflow="hidden">
                        <Text color={P.secondary} wrap="truncate">
                          {sg?.waits ? <Text color={t.open > 0 ? P.wait : P.secondary}>{`${sg.label} · `}</Text> : null}
                          {line(t.last_summary)}
                        </Text>
                      </Box>
                      <Box width={9} flexShrink={0} justifyContent="flex-end"><Text color={P.secondary}>{when(t.last_ts)}</Text></Box>
                      <Box width={3} flexShrink={0} justifyContent="center">
                        {t.unseen > 0 ? <Text inverse bold color={P.accent}>{String(t.unseen).padStart(2).padEnd(3)}</Text> : null}
                      </Box>
                    </Box>
                    {on ? <RowActions c={c} id={t.from} what="conversation" who={t.from_name} archived={shelf} target={['--', t.from]}
                      open={() => void c.api.go({ name: 'contact', peer: t.from })} /> : null}
                  </Box>
                </Box>
              )
            })}
            <Shelf c={c} on={shelf} count={s.archivedChats.length} flip={() => { shelf = !shelf; c.api.redraw() }} />
          </Box>
        )}
        {rows.length === 0 ? <Shelf c={c} on={shelf} count={s.archivedChats.length} flip={() => { shelf = !shelf; c.api.redraw() }} /> : null}
      </Box>
    ),
    keys: [['tab', 'move'], ['↑↓', 'scroll'], ['enter', 'open'], ['a', shelf ? 'unarchive' : 'archive'], ['x', 'delete'], ['f', 'filter'], ['n', 'new thread'], ['h', 'keys']],
    note: s.presence ? `last pull ${line(ago(s.presence.last_pull_at))}` : 'enter opens a conversation',
  }
}

export const contact: View = (c) => {
  const { Box, Text, Button } = c.ui
  const peer = s.route.name === 'contact' ? s.route.peer : ''
  const mode = s.contacts.find((x) => x.fingerprint === peer)?.policy?.mode ?? 'none'
  const threads = groups(s.timeline)
  const chosen = selected(threads, (g) => g.context, 'thread-')
  const p = presence(peer)
  const probed = s.presence?.peers.find((x) => x.fingerprint === peer)?.probed_at
  const who = nameOf(peer)
  // The threads are a table: `▸`, short id, first message, status, message count, newest time.
  return {
    body: (
      <Box flexDirection="column" gap={1}>
        <Box flexDirection="column">
          <BackRow c={c} title={who} dim />
          <Text color={P.secondary} wrap="truncate">
            <Text color={p.color}>{line(`${p.sign} ${p.label}`)}</Text>
            {line(`${probed ? ` · probed ${ago(probed)}` : ''} · ${peer} · policy ${mode}`)}
          </Text>
        </Box>
        <Box flexDirection="row" flexWrap="wrap" columnGap={3}>
          <Button key="new" plain hotkey="n" label="New thread" onPress={() => void newThread(c.api, peer)} />
          <Button key="card" plain hotkey="k" label="Card" dimColor
            onPress={() => { s.cardPeer = peer; void c.api.go({ name: 'card' }) }} />
          <Button key="policy" plain hotkey="p" label="Policy" dimColor onPress={() => void openPolicy(c.api, peer)} />
        </Box>
        <Box flexDirection="column">
          <Box flexDirection="row" gap={1}>
            <Text bold color={P.secondary}>{s.archived ? 'Archived threads' : 'Threads'}</Text>
            <Text color={P.secondary}>{String(threads.length)}</Text>
          </Box>
          <Box flexDirection="row" gap={1}>
            <Box width={2} flexShrink={0} />
            <Box width={6} flexShrink={0}><Head c={c} text="ID" /></Box>
            <Box flexGrow={1} flexShrink={1}><Head c={c} text="First message" /></Box>
            <Box width={14} flexShrink={0}><Head c={c} text="Status" /></Box>
            <Box width={4} flexShrink={0} justifyContent="flex-end"><Head c={c} text="Msg" /></Box>
            <Box width={6} flexShrink={0} justifyContent="flex-end"><Head c={c} text="Last" /></Box>
          </Box>
          <Rule c={c} />
          {threads.map((g, i) => {
            const st = status(g.last)
            const n = new Set(g.events.map((e) => e.record_id)).size
            const on = g === chosen
            // Only a thread with a context id is one that owl archive --context can name.
            return (
              <Box key={`sep-${g.context}`} flexDirection="column">
                {i > 0 ? <Rule c={c} /> : null}
                <Box key={g.context} flexDirection="column" backgroundColor={on ? P.selected : undefined}>
                  <Box flexDirection="row" gap={1}>
                    <Box width={2} flexShrink={0}><Text color={P.accent}>{on ? SIGN.selected : ' '}</Text></Box>
                    <Box width={6} flexShrink={0}><Text color={P.secondary}>{line(short(g.context))}</Text></Box>
                    <Box flexGrow={1} flexShrink={1} height={1} overflow="hidden">
                      <Button key={`thread-${g.context}`} plain label={line(g.first.text)}
                        onPress={() => void c.api.openThread(peer, g.context)} />
                    </Box>
                    <Box width={14} flexShrink={0}><Text color={st.color} wrap="truncate">{line(`${st.sign} ${st.short}`)}</Text></Box>
                    <Box width={4} flexShrink={0} justifyContent="flex-end"><Text color={P.secondary}>{String(n)}</Text></Box>
                    <Box width={6} flexShrink={0} justifyContent="flex-end"><Text color={P.secondary}>{whenShort(g.last.ts)}</Text></Box>
                  </Box>
                  {on && g.first.context_id === g.context ? (
                    <RowActions c={c} id={g.context} what="thread" who={who} archived={s.archived} target={[`--context=${g.context}`, '--', peer]}
                      open={() => void c.api.openThread(peer, g.context)} />
                  ) : null}
                </Box>
              </Box>
            )
          })}
          <Shelf c={c} on={s.archived} count={s.archivedThreads} flip={() => { s.archived = !s.archived; void c.api.go(s.route) }} />
        </Box>
      </Box>
    ),
    keys: [['tab', 'move'], ['↑↓', 'scroll'], ['enter', 'open'], ['a', s.archived ? 'unarchive' : 'archive'], ['x', 'delete'], ['v', 'archived'], ['n', 'new thread'], ['b', 'back']],
    note: 'enter opens a thread',
  }
}

// One entry per record, oldest first: its first event's text with its newest event's state.
function messages(events: Ev[]): Ev[] {
  const ids = [...new Set(events.map((e) => e.record_id))]
  return ids.map((id) => {
    const evs = events.filter((e) => e.record_id === id)
    return { ...evs[0], state: evs[evs.length - 1].state }
  })
}

// The panel's fields have no `@` file completion (the engine's Input has none), so a file is
// picked in the prompt box, which has it: the newest `@file` there (`@"a b.rs"` too; not an
// `@owl:` mention), '' when there is none.
export async function promptFile(api: Api): Promise<string> {
  const found = [...(await api.promptText()).matchAll(/(?:^|\s)@(?:"([^"]+)"|(\S+))/g)]
    .map((m) => m[1] ?? m[2])
    .filter((f) => !f.startsWith('owl:'))
  return found[found.length - 1] ?? ''
}

// `p` / `l`: the field takes the prompt box's newest `@file` when there is one, and the ring
// goes to the field either way.
async function pick(api: Api, key: 'path' | 'context') {
  const file = await promptFile(api)
  if (file) {
    if (key === 'path') path = file
    else ctx = file
    api.say(`✓ ${key === 'path' ? 'path' : 'local file'} ${file} from the prompt box`)
  }
  api.focus(key)
}

async function send(api: Api, peer: string, target: string) {
  // A path typed as in the prompt box (`@src/x.rs`) is the path without the `@`.
  const [text, file, snippet] = [reply.trim(), path.trim().replace(/^@/, ''), ctx.trim().replace(/^@/, '')]
  if (!text) return api.say('✗ type a reply first')
  if (sending) return api.say('… still sending the reply')
  sending = true
  replyNote = ''
  const drawn = s.entered.reply
  const ok = await api.act([
    'ask', '--peer', peer, '--reply-to', target,
    ...(file ? ['--file', file] : []),
    ...(snippet ? ['--context', snippet] : []),
    '--', // a reply starting with `-` is text, not a flag
    text,
  ]).finally(() => { sending = false })
  // The block says what happened under the field, as the mock: the note line has owl's words.
  const said = s.note.replace(/^[✓✗] /, '')
  const id = /\b[0-9a-f]{6}\b/.exec(said)?.[0]
  // Then where the sent record stands, from its state in the reloaded timeline (as the mock:
  // `✓ sent q8d2e4 · waiting for Alex's consent`).
  const sent = id ? s.timeline.find((e) => short(e.record_id) === id) : undefined
  const where = sent && (sent.state === 'consent' ? `waiting for ${nameOf(peer).split(' ')[0]}'s consent` : status(sent).label)
  replyNote = ok ? `✓ sent${id ? ` ${id}` : ''}${where ? ` · ${where}` : ''}` : `× not sent: ${said} · your text stays above`
  s.note = '' // one note per send: the block's, not owl's words again above the footer
  // A failed send stays in the thread as not delivered (a second failure of the same text
  // replaces the first); a send that went out is the record's own bubble from now on.
  const context = s.timeline.find((e) => e.record_id === target)?.context_id ?? target
  const kept = (unsent[context] ?? []).filter((e) => e.text !== text)
  unsent[context] = ok ? kept : [...kept, undelivered(context, text, said)]
  if (!ok) return api.redraw()
  reply = path = ctx = ''
  attach = false
  // Typing draws nothing, so the field was last drawn with the value from before the text was
  // typed (often ''); drawn again with '' the engine would keep the typed text. Under a new key
  // it is drawn afresh, empty (Enter already gave it one, see `fields` in ui.tsx).
  if (s.entered.reply === drawn) s.entered.reply = (drawn ?? 0) + 1
  api.redraw()
}

// The bubble of a reply owl did not deliver: ours, in thread `context`, now, with why.
function undelivered(context: string, text: string, said: string): Ev {
  const why = /\boffline\b/.test(said) ? 'peer offline' : said.replace(/^owl: /, '')
  return {
    ts: new Date().toISOString(), kind: 'asked', dir: 'out', record_id: `unsent:${context}:${text}`, context_id: context,
    type: 'question', state: 'undelivered', project: null, path: '-', text, detail: { why },
  }
}

// Cite writes the mentions at the cursor of the prompt box, into what is typed there; Apply
// puts `Apply <mentions>` in front of what is typed, for the person's own words to finish.
// Nothing is submitted. The note names the message by its short id.
async function bridge(api: Api, cite: boolean, ids: string[]) {
  const m = ids.map(mention).join(' ')
  const ok = cite ? await api.fill(`${m} `, 'insert') : await api.fill(`Apply ${m} ${await api.promptText()}`, 'replace')
  const shown = ids.map((id) => mention(short(id))).join(' ')
  // After Apply the keys are back in the prompt (the mock's `· keys returned`).
  api.say(!ok ? '✗ the prompt box did not take it' : cite ? `✓ "${shown}" is in the prompt` : `✓ "Apply ${shown}" is in the prompt · keys returned`)
}

// An answer from the peer whose id is a UUID: an id the peer chose otherwise is never put in
// the prompt box.
const isAnswer = (e: Ev) => e.dir === 'in' && e.type === 'answer' && isUuid(e.record_id)

// Cite and Apply of one answer are keyed by its full id: two answers may share a short id.
const citeKey = (e: Ev) => `cite-${e.record_id}`
const applyKey = (e: Ev) => `apply-${e.record_id}`

// Entering a thread puts the ring on the newest answer's Cite, where the letters are, so the
// ringed control, Enter, `c` and `a` all act on the same answer.
enter.thread = async (api) => {
  const r = s.route.name === 'thread' ? s.route.context : ''
  // What was typed in another thread is no draft of this one. The engine keeps a field's typed
  // text when it is drawn again with the same key, so a field that held text comes under a new
  // key, empty.
  if (r !== draftFor) {
    for (const [k, v] of [['reply', reply], ['path', path], ['context', ctx]] as const) if (v) s.entered[k] = (s.entered[k] ?? 0) + 1
    reply = path = ctx = ''
    draftFor = r
  }
  attach = !!(path || ctx) // what is attached shows
  applied = ''
  replyNote = ''
  const answers = messages(s.timeline.filter((e) => (e.context_id ?? e.record_id) === r)).filter(isAnswer)
  if (answers.length) api.focus(citeKey(answers[answers.length - 1]!))
}

// The answer's controls under its bubble, as the mock: `r: Reply  y: Copy`, then
// `to main agent  c: Cite  a: Apply`. Only the lettered answer's carry the letters (of two
// buttons on one letter the later would win); a pressed Apply is drawn on the accent fill.
function AnswerControls({ c, e, lettered, newest }: { c: Ctx; e: Ev; lettered: boolean; newest: boolean }) {
  const { Box, Text, Button } = c.ui
  const k = (h: string) => (lettered ? h : undefined)
  return (
    <Box flexDirection="column">
      <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
        <Button key={`reply-${e.record_id}`} plain hotkey={k('r')} label="Reply" onPress={() => c.api.focus('reply')} />
        <Button key={`copy-${e.record_id}`} plain hotkey={k('y')} label="Copy" dimColor
          onPress={async () => c.api.say((await c.api.copy(e.text)) ? `✓ answer ${short(e.record_id)} copied` : '✗ the clipboard did not take it')} />
      </Box>
      <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
        <Text color={P.secondary}>to main agent</Text>
        <Button key={citeKey(e)} plain hotkey={k('c')} label="Cite" autoFocus={newest ? true : undefined}
          onPress={() => void bridge(c.api, true, [e.record_id])} />
        {applied === e.record_id ? (
          <Filled c={c} k={applyKey(e)} hotkey={k('a')} label="Apply" onPress={() => void bridge(c.api, false, [e.record_id])} />
        ) : (
          <Button key={applyKey(e)} plain hotkey={k('a')} label="Apply"
            onPress={() => { applied = e.record_id; void bridge(c.api, false, [e.record_id]) }} />
        )}
      </Box>
    </Box>
  )
}

export const thread: View = (c) => {
  const { Box, Text, Button } = c.ui
  const r = s.route.name === 'thread' ? s.route : { peer: '', context: '' }
  const events = s.timeline.filter((e) => (e.context_id ?? e.record_id) === r.context)
  const who = nameOf(r.peer)
  const target = replyTarget(events)
  const msgs = [...messages(events), ...(unsent[r.context] ?? [])]
  // `c` / `a` press the answer the focus ring is on, else the newest.
  const answers = msgs.filter(isAnswer)
  const lettered = answers.find((e) => [citeKey(e), applyKey(e)].includes(s.ring)) ?? answers[answers.length - 1]
  const all = answers.map((e) => e.record_id)
  const whole = (cite: boolean) => () => (all.length ? void bridge(c.api, cite, all) : c.api.say('✗ no answer in this thread yet'))
  const keys: Hint[] = target
    ? [['tab', 'move'], ['↑↓', 'scroll'], ['r', 'reply'], ['enter', 'send'], ['v', 'paste prompt'], ['f', 'attach'], ['p', 'path'], ['l', 'local file'],
      ...(answers.length ? ([['y', 'copy'], ['c', 'cite'], ['a', 'apply']] as Hint[]) : []), ['t', 'cite thread'], ['b', 'back']]
    : [['tab', 'move'], ['↑↓', 'scroll'], ...(answers.length ? ([['y', 'copy'], ['c', 'cite'], ['a', 'apply']] as Hint[]) : []), ['t', 'cite thread'], ['n', 'new thread'], ['b', 'back']]
  return {
    body: (
      <Box flexDirection="column" gap={1} flexGrow={1}>
        <Box flexDirection="column">
          <BackRow c={c} crumb={who} title={events[0]?.text ?? ''} id={r.context} />
          <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
            <Text color={P.secondary}>to main agent</Text>
            <Button key="whole-cite" plain hotkey="t" label="Cite thread" dimColor onPress={whole(true)} />
            <Button key="whole-apply" plain hotkey="u" label="Apply thread" dimColor onPress={whole(false)} />
            <Spacer c={c} />
            <Button key="new" plain hotkey="n" label="New thread" dimColor onPress={() => void newThread(c.api, r.peer)} />
          </Box>
        </Box>
        <Box flexDirection="column" flexGrow={1} justifyContent="flex-end">
          {msgs.map((e, i) => {
            const newDay = i === 0 || new Date(msgs[i - 1]!.ts).toDateString() !== new Date(e.ts).toDateString()
            return (
              <Box key={e.record_id} flexDirection="column">
                {newDay ? <Box flexDirection="column" marginBottom={1} marginTop={i ? 1 : 0}><DateRule c={c} ts={e.ts} /></Box> : <Rule c={c} />}
                <Message c={c} e={e} who={who} selected={isAnswer(e) && e === lettered} />
                {isAnswer(e) ? <AnswerControls c={c} e={e} lettered={e === lettered} newest={e === answers[answers.length - 1]} /> : null}
              </Box>
            )
          })}
        </Box>
        {target ? (
          // The reply block as the mock: under a rule, the bold title with the thread's id at the
          // right; the bordered field with Send on the accent fill beside it (Enter sends too);
          // `enter sends` and `v: paste from prompt box`; then the optional attachments behind `f`.
          <Box flexDirection="column">
            <Rule c={c} />
            <Box flexDirection="row" gap={2} height={1} overflow="hidden">
              <Box flexDirection="row" flexGrow={1} flexShrink={1} overflow="hidden">
                <Button key="reply-focus" plain hotkey="r" label="" onPress={() => c.api.focus('reply')} />
                <Text bold wrap="truncate">{line(`Reply to ${who}`)}</Text>
              </Box>
              <Box flexShrink={0}><Text color={P.secondary}>{line(`in ${short(r.context)}`)}</Text></Box>
            </Box>
            <Box flexDirection="row" gap={1}>
              <Box flexDirection="column" flexGrow={1} flexShrink={1}>
                <Field c={c} k="reply" main placeholder="Type a follow-up question…" value={reply} submitLabel="send"
                  set={(v) => { reply = v }} onSubmit={() => void send(c.api, r.peer, target)} />
              </Box>
              <Filled c={c} k="send" label="Send" rows={3} onPress={() => void send(c.api, r.peer, target)} />
            </Box>
            <Box flexDirection="row" gap={2}>
              <Text color={P.secondary}>enter sends</Text>
              <Spacer c={c} />
              <Button key="use-prompt" plain hotkey="v" label="paste from prompt box" dimColor
                onPress={async () => { reply = await c.api.promptText(); c.api.redraw() }} />
            </Box>
            {replyNote ? <Text color={replyNote.startsWith('✓') ? P.ok : P.bad} wrap="truncate">{line(replyNote)}</Text> : null}
            <Box flexDirection="row" flexWrap="wrap" columnGap={2}>
              <Button key="attach" plain hotkey="f" label={attach ? '− Attach' : '+ Attach'}
                onPress={() => { attach = !attach; if (!attach) path = ctx = ''; c.api.redraw() }} />
              <Text color={P.secondary}>{attach ? 'optional · both can stay empty' : 'optional · a file of theirs or of yours'}</Text>
            </Box>
            {attach ? (
              <Box flexDirection="column" paddingLeft={3}>
                <Field c={c} k="path" label="Ask about a file in their repo" hotkey="p" hint="(path)" onLabel={() => void pick(c.api, 'path')}
                  placeholder="@ picks a file · e.g. src/main.rs" value={path} set={(v) => { path = v }} onSubmit={() => c.api.redraw()}
                  picker="from your checkout: a hint, their repo may differ" />
                <Field c={c} k="context" label="Attach a local file" hotkey="l" hint="(≤ 8 KiB)" onLabel={() => void pick(c.api, 'context')}
                  placeholder="@ picks a file · its text goes with the question" value={ctx} set={(v) => { ctx = v }} onSubmit={() => c.api.redraw()}
                  picker />
              </Box>
            ) : null}
          </Box>
        ) : (
          <Text color={P.secondary}>{`This exchange is from before threads, so it cannot be continued; n: New thread asks ${who}.`}</Text>
        )}
      </Box>
    ),
    keys,
    note: `thread ${short(r.context)} with ${who}`,
  }
}
