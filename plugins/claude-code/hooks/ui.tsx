// The panel's drawing side, shared by every view: the palette, the state signs, what a view
// returns, and the pieces every screen repeats (the Back row, a message, a date rule).
import type { ElementTable, RenderChildren } from 'claude-code'
import { fieldKey, s, short, type Api, type Ev } from './lib'

// The palette: theme keys only, so the
// colours follow the user's theme and a refused colour is found here. Never a hex value,
// an ansi form or a colour name anywhere in the mod.
export const P = {
  accent: 'claude', // hotkeys, the active tab, Send
  secondary: 'inactive', // secondary text
  rule: 'subtle', // rules, field borders
  ok: 'success', // answered, ok
  wait: 'warning', // needs your answer, waiting
  bad: 'error', // denied, never, failed
  mention: 'suggestion', // a mention in the prompt
  selected: 'userMessageBackground', // background of the selected row
  // The bubble fills (in the mock #3b2b25 ours, warm, and #2a2e37 theirs, cool grey): picked
  // per theme by `bubble`, so both keep a real contrast on the background of the pane.
  oursWarm: 'diffRemovedDimmed',
  oursCool: 'selectionBg',
  theirs: 'userMessageBackground',
} as const

// The fill of a message bubble on the user's theme. Ours is warm as in the mock where the
// theme has a warm fill that stands out (`dark`, `light`); the daltonized and ansi themes get
// the selection blue instead (their warm fill is too close to the pane or is red). Theirs is
// the grey of a user message, except on `light`, where that grey is the pane's own (240 on 245).
export const bubble = (mine: boolean) =>
  mine ? (/daltonized|ansi/.test(s.theme) ? P.oursCool : P.oursWarm) : s.theme === 'light' ? P.oursCool : P.theirs

// The signs that carry a state without colour (`NO_COLOR`, 16 colours). A contact's presence
// has signs of its own (`online`, `offline`, `unknown`), which no status uses, so the two
// never read as each other.
export const SIGN = { selected: '▸', open: '!', ok: '✓', wait: '…', bad: '-', none: '○', online: '●', offline: '✕', unknown: '?' } as const

// `label` is the words of a header line, `short` those of a table cell.
export type Status = { sign: string; color: string; label: string; short: string }

// What one event's record is waiting for, as sign, colour and words.
export function status(e: Ev): Status {
  const st = (sign: string, color: string, label: string, short = label): Status => ({ sign, color, label, short })
  if (e.dir === 'in' && (e.type.endsWith('reply') || e.type === 'answer') && !BAD.includes(e.state)) return st(SIGN.ok, P.ok, 'answer received', 'answered')
  switch (e.state) {
    case 'consent':
      return e.dir === 'in' ? st(SIGN.open, P.wait, 'needs your answer', 'consent') : st(SIGN.wait, P.wait, 'waiting for consent', 'consent')
    case 'pending':
      return st(SIGN.open, P.wait, 'needs your answer', 'needs answer')
    case 'drafted':
      return st(SIGN.open, P.wait, 'needs your answer', 'needs answer')
    case 'waiting':
      return st(SIGN.wait, P.wait, 'waiting')
    case 'answered':
    case 'sent':
    case 'acked':
      return st(SIGN.ok, P.ok, 'answered')
    case 'denied':
    case 'declined':
    case 'rejected':
    case 'expired':
      return st(SIGN.bad, P.bad, e.state)
    case 'undelivered': // a reply `owl ask` could not deliver (chats.tsx keeps it)
      return st(SIGN.bad, P.bad, `not delivered · ${e.detail?.why ?? 'not sent'}`, 'not delivered')
    default:
      return st(SIGN.none, P.secondary, e.state)
  }
}

const BAD = ['denied', 'declined', 'rejected', 'expired']

// The elements every surface with a pane has (terminal, desktop, vscode).
export type UI = Pick<ElementTable<'terminal'>, 'Box' | 'Text' | 'Button' | 'Input' | 'Select'>

// The panel's Input. The engine empties a field on Enter and puts `value` back only when it
// differs from the value it drew there last, so a field drawn again with the text it held
// (a failed send, a reply kept for Send) would stay empty and the next Enter would submit ''.
// A field under a new key is drawn afresh from `value`: every Enter gives the field the next
// key (`fieldKey`; the engine keeps the focus on it), so after Enter a field shows what the mod
// holds.
export function fields(ui: UI, api: Api): UI {
  const Input: UI['Input'] = (p) =>
    ui.Input({
      ...p,
      key: fieldKey(p.key),
      onSubmit: (v, e) => {
        s.entered[p.key] = (s.entered[p.key] ?? 0) + 1
        p.onSubmit(v, e)
        api.redraw()
      },
    })
  return { ...ui, Input }
}

// What a view gets while drawing.
export type Ctx = { api: Api; ui: UI }

// A key hint of the footer: the key and what it does (`['enter', 'open']`), and a colour of
// the key's own for a word that is no key (New's `typing`, `then`).
export type Hint = [string, string, string?]

// What a view returns: its body, its footer hints, and the note shown when no action has
// left one (every screen has a note line).
export type Screen = { body: RenderChildren; keys: Hint[]; note: string }

export type View = (c: Ctx) => Screen

// The engine refuses a Text child longer than 10000 characters or holding a control character
// other than tab and newline, and a Button label holding any control character; either draws
// the whole pane blank. So text a peer controls (a message, a name, a path, a card) is drawn
// through `clip`: control characters become spaces, and it is cut to DRAW_MAX characters with
// the cut said; a label through `line`, the same on one line. Cite still attaches the whole
// message (the prompt hook has its own cap).
export const DRAW_MAX = 4000
// The engine's own limit on an Input's value; a value holding a control character is refused too.
export const INPUT_MAX = 10_000
export const cutNote = (n: number) => `… cut, ${n} characters in all`
export const clean = (text: string) => text.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, ' ')
export const clip = (text: string) => (text.length > DRAW_MAX ? `${clean(text.slice(0, DRAW_MAX))}\n${cutNote(text.length)}` : clean(text))
export const line = (text: string) => clip(text.replace(/[\t\n]/g, ' ')).replace('\n', ' ')
// A label cut to `n` cells with an ellipsis: a Button's label cannot truncate itself (it wraps),
// so a name in a fixed column is cut before it is drawn.
export const fit = (text: string, n: number) => {
  const l = line(text)
  return [...l].length > n ? `${[...l].slice(0, Math.max(1, n - 1)).join('')}…` : l
}

// `HH:MM` in local time.
export const hhmm = (ts: string) => {
  const d = new Date(ts)
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}

const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

// `Sun 4 Oct`, the date rule between days of a thread.
export const day = (ts: string) => {
  const d = new Date(ts)
  return `${DAYS[d.getDay()]} ${d.getDate()} ${MONTHS[d.getMonth()]}`
}

// A list row's time: `HH:MM` today, `yesterday`, the weekday within a week, else `4 Oct`.
export function when(ts: string, now = new Date()) {
  const d = new Date(ts)
  const days = Math.round((new Date(now.toDateString()).getTime() - new Date(d.toDateString()).getTime()) / 86_400_000)
  if (days <= 0) return hhmm(ts)
  if (days === 1) return 'yesterday'
  if (days < 7) return DAYS[d.getDay()]
  return `${d.getDate()} ${MONTHS[d.getMonth()]}`
}

// The Last cell of a thread row (5 wide in the mock): `14:14` today, the weekday within a
// week (yesterday too), else `4 Oct`.
export const whenShort = (ts: string, now = new Date()) => {
  const w = when(ts, now) ?? ''
  return w === 'yesterday' ? DAYS[new Date(ts).getDay()] ?? w : w
}

// An age as `owl presence` writes it: `19s ago`, `5m ago`, `3h ago`, `2d ago`; a time that
// does not parse is shown as it is.
export function ago(ts: string, now = Date.now()) {
  const n = Math.max(0, Math.floor((now - Date.parse(ts)) / 1000))
  if (Number.isNaN(n)) return ts
  return n < 60 ? `${n}s ago` : n < 3_600 ? `${Math.floor(n / 60)}m ago` : n < 86_400 ? `${Math.floor(n / 3_600)}h ago` : `${Math.floor(n / 86_400)}d ago`
}

// One contact's presence from `owl presence` as sign, colour and words: online, last seen,
// offline, or not probed (also when there is no presence at all).
export function presence(fp: string): Status {
  const p = s.presence?.peers.find((x) => x.fingerprint === fp)
  if (p?.online) return { sign: SIGN.online, color: P.ok, label: 'online', short: 'online' }
  if (p?.online === false)
    return p.last_seen
      ? { sign: SIGN.offline, color: P.bad, label: `last seen ${seen(p.last_seen)}`, short: seen(p.last_seen) }
      : { sign: SIGN.offline, color: P.bad, label: 'offline', short: 'offline' }
  return { sign: SIGN.unknown, color: P.secondary, label: 'not probed', short: 'not probed' }
}

// When a contact was last seen, as the Seen column writes it: `16:02` today, else `Fri 16:02`
// within a week, else `4 Oct`.
export function seen(ts: string, now = new Date()): string {
  const w = when(ts, now) ?? ''
  return w === hhmm(ts) || /\d/.test(w) ? w : `${w === 'yesterday' ? DAYS[new Date(ts).getDay()] ?? '' : w} ${hhmm(ts)}`
}

// What the presence signs mean, under the header of a table that draws them (indented to
// the name column, as the mock).
export const Legend = ({ c }: { c: Ctx }) => (
  <c.ui.Box paddingLeft={3}>
    <c.ui.Text color={P.secondary}>
      <c.ui.Text color={P.ok}>{SIGN.online}</c.ui.Text>
      {' online  '}
      <c.ui.Text color={P.bad}>{SIGN.offline}</c.ui.Text>
      {' offline  '}
      <c.ui.Text color={P.secondary}>{SIGN.unknown}</c.ui.Text>
      {' unknown'}
    </c.ui.Text>
  </c.ui.Box>
)

// The daemon summary (`19s ago · 2 open asks · 1 peer probed`), or why there is none.
export function pulse() {
  const p = s.presence
  if (!p) return s.failed.presence
  const n = (k: number, one: string) => `${k} ${one}${k === 1 ? '' : 's'}`
  return `${ago(p.last_pull_at)} · ${n(p.open_asks, 'open ask')} · ${n(p.peers_probed, 'peer')} probed`
}

// `── Sun 4 Oct ──`, centred, in secondary text as the mock draws it.
export const DateRule = ({ c, ts }: { c: Ctx; ts: string }) => (
  <c.ui.Box flexDirection="row" justifyContent="center">
    <c.ui.Text color={P.secondary}>{`── ${day(ts)} ──`}</c.ui.Text>
  </c.ui.Box>
)

// The thin rule between two rows of a list or two messages, and under the tab row and above
// the keys: a run of `─` as wide as its box, the rows it wraps onto hidden.
export const Rule = ({ c }: { c: Ctx }) => (
  <c.ui.Box height={1} overflow="hidden">
    <c.ui.Text color={P.rule}>{'─'.repeat(400)}</c.ui.Text>
  </c.ui.Box>
)

// Takes the room left in a row, pushing what follows to the right edge.
export const Spacer = ({ c }: { c: Ctx }) => <c.ui.Box flexGrow={1} />

// A list table's header cell, in secondary bold.
export const Head = ({ c, text }: { c: Ctx; text: string }) => (
  <c.ui.Text bold color={P.secondary}>{text}</c.ui.Text>
)

// A key the mock binds without drawing a control for it (the current tab's digit, `h` on a
// screen whose hints do not list it): a Button with no label in a box that takes no room and
// leaves the flow, so its hotkey presses it and nothing shows.
export const Hidden = ({ c, k, hotkey, onPress }: { c: Ctx; k: string; hotkey: string; onPress: () => void }) => (
  <c.ui.Box position="absolute" width={0} height={1} overflow="hidden">
    <c.ui.Button key={k} plain hotkey={hotkey} label="" onPress={onPress} />
  </c.ui.Box>
)

// A control drawn pressed or chosen, as the mock's filled blocks: the label in an inverse block
// (`[manual]`, `[on]`, the chosen kind). It is not a Button: pressing the current choice does nothing.
export const Chosen = ({ c, text }: { c: Ctx; text: string }) => <c.ui.Text inverse>{` ${text} `}</c.ui.Text>

// A Button on the accent fill, as the mock's Send and a pressed Apply: a Button's label takes no
// colour of its own (ButtonProps has none), so the fill is a Box behind it.
export const Filled = ({ c, k, hotkey, label, onPress, rows = 1 }: { c: Ctx; k: string; hotkey?: string; label: string; onPress: () => void; rows?: number }) => (
  <c.ui.Box flexShrink={0} backgroundColor={P.accent} paddingX={hotkey ? 1 : 2} height={rows} flexDirection="column" justifyContent="center">
    <c.ui.Button key={k} plain hotkey={hotkey} label={label} onPress={onPress} />
  </c.ui.Box>
)

// The row on top of a screen below a tab: `b: Back`, the crumb (`Name ›`, a name already made
// one line by `line`), the title in bold and the short id at the right edge.
export const BackRow = ({ c, title, crumb, id, dim }: { c: Ctx; title: string; crumb?: string; id?: string; dim?: boolean }) => (
  <c.ui.Box flexDirection="row" gap={2} height={1} overflow="hidden">
    <c.ui.Box flexShrink={0}>
      <c.ui.Button key="back" plain hotkey="b" label="Back" dimColor={dim ? true : undefined} onPress={() => void c.api.back()} />
    </c.ui.Box>
    <c.ui.Box flexDirection="row" gap={1} flexGrow={1} flexShrink={1} overflow="hidden">
      {crumb ? <c.ui.Box flexShrink={0}><c.ui.Text color={P.secondary}>{`${crumb} ›`}</c.ui.Text></c.ui.Box> : null}
      <c.ui.Text bold wrap="truncate">{line(title)}</c.ui.Text>
    </c.ui.Box>
    {id ? <c.ui.Box flexShrink={0}><c.ui.Text color={P.secondary}>{line(short(id))}</c.ui.Text></c.ui.Box> : null}
  </c.ui.Box>
)

// ------------------------------------------------------------------ fields and the @ picker

// The `@query` a field's text ends a word with (`src/@lib`, `@"a b`): the last `@` word, with
// where it starts; undefined when there is none (an `@owl:` mention is no file).
export function atToken(value: string): { at: number; query: string } | undefined {
  const m = [...value.matchAll(/(^|\s)@([^\s]*)/g)].pop()
  if (!m || m[2]!.startsWith('owl:')) return undefined
  return { at: (m.index ?? 0) + m[1]!.length, query: m[2]!.replace(/^"/, '') }
}

// Up to eight files of the checkout matching the query: a name starting with it first, then
// any path holding it, shorter first.
export function matches(files: string[], query: string): string[] {
  const q = query.toLowerCase()
  const base = (f: string) => f.slice(f.lastIndexOf('/') + 1).toLowerCase()
  return files
    .filter((f) => f.toLowerCase().includes(q))
    .sort((a, b) => Number(!base(a).startsWith(q)) - Number(!base(b).startsWith(q)) || a.length - b.length || a.localeCompare(b))
    .slice(0, 8)
}

// The value with its `@query` word replaced by the picked path.
export const pickInto = (value: string, at: number, path: string) => {
  const end = value.slice(at).search(/\s/)
  return value.slice(0, at) + path + (end < 0 ? '' : value.slice(at + end))
}

// Under a field whose text has an `@query`: the matching files of the session's checkout as
// plain Buttons (`pick-<field>-<i>`, in the Box `picks-<field>`, scrolled into view as the
// `@query` changes); a press puts the path in place of the word and the ring back on the field,
// drawn afresh under its next key (`fieldKey`), so the cursor is not left where the `@` was
// (InputProps has no cursor). The engine's Input has no completion of its own (InputProps).
export function Picker({ c, k, value, set, note }: { c: Ctx; k: string; value: string; set: (v: string) => void; note?: string }) {
  const { Box, Text, Button } = c.ui
  const t = atToken(value)
  if (!t) return null
  if (!s.files) {
    void c.api.files().then(() => c.api.reveal(`picks-${k}`))
    return <Text color={P.secondary}>… listing the files</Text>
  }
  const hits = matches(s.files, t.query)
  return (
    <Box key={`picks-${k}`} flexDirection="column" paddingLeft={1}>
      {hits.length ? (
        hits.map((f, i) => (
          <Button key={`pick-${k}-${i}`} plain label={line(f)}
            onPress={() => { set(pickInto(value, t.at, f)); s.entered[k] = (s.entered[k] ?? 0) + 1; c.api.reveal(k, true) }} />
        ))
      ) : (
        <Text color={P.secondary}>{line(`no file matches @${t.query}`)}</Text>
      )}
      {note ? <Text color={P.secondary}>{note}</Text> : null}
    </Box>
  )
}

// A labelled field as the mock draws it: the label line (a hotkey Button when `hotkey`, then a
// secondary `hint`), and under it a one-line Input in a bordered box (the accent border on the
// screen's main field). `picker` lists the checkout's files under it while its text has an `@`.
export function Field(p: {
  c: Ctx
  k: string
  label?: string
  hotkey?: string
  onLabel?: () => void
  hint?: string
  placeholder?: string
  value: string
  set: (v: string) => void
  onSubmit: (v: string) => void
  submitLabel?: string
  main?: boolean
  picker?: string | true
  labelDim?: boolean
}) {
  const { Box, Text, Button, Input } = p.c.ui
  return (
    <Box flexDirection="column">
      {p.label ? (
        <Box flexDirection="row" gap={1}>
          {p.hotkey ? (
            <Button key={`${p.k}-focus`} plain hotkey={p.hotkey} label={p.label} dimColor={p.labelDim ? true : undefined}
              onPress={p.onLabel ?? (() => p.c.api.focus(p.k))} />
          ) : (
            <Text color={P.secondary}>{p.label}</Text>
          )}
          {p.hint ? <Text color={P.secondary}>{p.hint}</Text> : null}
        </Box>
      ) : null}
      <Box borderStyle="single" borderColor={p.main ? P.accent : P.rule} paddingX={1} flexGrow={1}>
        <Input key={p.k} placeholder={p.placeholder} value={p.value} submitLabel={p.submitLabel}
          onInput={(v) => { const had = !!atToken(p.value); p.set(v); if (p.picker && atToken(v)) p.c.api.reveal(`picks-${p.k}`); else if (p.picker && had) p.c.api.redraw() }}
          onSubmit={(v) => { p.set(v); p.onSubmit(v) }} />
      </Box>
      {p.picker ? <Picker c={p.c} k={p.k} value={p.value} set={p.set} note={typeof p.picker === 'string' ? p.picker : undefined} /> : null}
    </Box>
  )
}

// ------------------------------------------------------------------ messages

const firstName = (who: string) => who.split(/\s+/)[0] ?? who

// What kind of message a record is, as its header names it.
const KIND: Record<string, string> = { question: 'question', answer: 'answer', content: 'file request', 'content-reply': 'file', 'tool-call': 'tool call', 'tool-reply': 'tool result' }

// The header line of a message: ours `07:08 · you · ✓ answered`, theirs
// `07:08 · answer · drafted by claude · approved by Alex`.
export function header(e: Ev, who: string, extra = ''): string {
  const about = e.type === 'question' && e.path && e.path !== '-' ? `about ${e.path}` : ''
  if (e.dir === 'out') return [hhmm(e.ts), 'you', about].filter(Boolean).join(' · ')
  const answer = e.type === 'answer' || e.type.endsWith('reply')
  const kind = e.type === 'content' && /memory entry/.test(extra) ? 'question' : KIND[e.type] ?? e.type
  return [hhmm(e.ts), kind, extra, about, answer && e.harness ? `drafted by ${e.harness}` : '', answer ? `approved by ${e.by && e.by !== 'human' ? e.by : firstName(who)}` : '']
    .filter(Boolean)
    .join(' · ')
}

// The widest a bubble gets (the mock's 52ch of text), and its width for a text: its longest
// line plus the padding, never wider than the pane.
export const BUBBLE_MAX = 52
export const bubbleWidth = (text: string) =>
  Math.max(4, Math.min(Math.max(...text.split('\n').map((l) => l.length)), BUBBLE_MAX, s.cols - 4) + 2)

// A message bubble: the text on its fill, ours at the right edge and theirs at the left.
export function Bubble({ c, text, mine, dashed }: { c: Ctx; text: string; mine: boolean; dashed?: boolean }) {
  const { Box, Text } = c.ui
  const shown = clean(text.slice(0, DRAW_MAX))
  return (
    <Box alignSelf={mine ? 'flex-end' : 'flex-start'} width={bubbleWidth(shown) + (dashed ? 2 : 0)} backgroundColor={bubble(mine)} paddingX={1}
      flexDirection="column" borderStyle={dashed ? 'dashed' : undefined} borderColor={dashed ? P.accent : undefined}>
      <Text>{shown}</Text>
      {text.length > DRAW_MAX ? <Text color={P.secondary}>{cutNote(text.length)}</Text> : null}
    </Box>
  )
}

// One message of a thread: the header line and the bubble. Ours is aligned right on the warm
// fill, theirs left on the grey one, so the direction reads from the side, the fill and the
// header (`you`). Ours carries its state in the header; theirs may put one at the right end
// (`right`, the request screen), and `selected` marks the answer the letters act on.
export function Message({ c, e, who, right, selected, extra }: { c: Ctx; e: Ev; who: string; right?: Status; selected?: boolean; extra?: string }) {
  const { Box, Text } = c.ui
  const mine = e.dir === 'out'
  const st = status(e)
  // A reply owl did not deliver is drawn as an unsent draft (request.tsx): `not sent` in the
  // header, why after it, and the bubble in the dashed box.
  const unsent = mine && e.state === 'undelivered'
  return (
    <Box flexDirection="column" alignItems={right ? 'stretch' : mine ? 'flex-end' : 'flex-start'}>
      <Box flexDirection="row" justifyContent="space-between" gap={2}>
        <Text color={P.secondary} wrap="truncate">
          {selected ? <Text color={P.accent}>{`${SIGN.selected} `}</Text> : null}
          {line(header(e, who, extra))}
          {mine ? ' · ' : ''}
          {unsent ? <Text color={P.wait}>not sent</Text> : mine ? <Text color={st.color}>{line(`${st.sign} ${st.label}`)}</Text> : null}
          {unsent && e.detail?.why ? line(` · ${e.detail.why}`) : null}
        </Text>
        {right ? <Box flexShrink={0}><Text color={right.color}>{line(`${right.sign} ${right.label}`)}</Text></Box> : null}
      </Box>
      <Bubble c={c} text={e.text} mine={mine} dashed={unsent} />
    </Box>
  )
}
