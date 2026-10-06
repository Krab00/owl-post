// Keys (`h` from every screen, no tab): every key the panel binds, in sections, and what
// stays with Claude Code, as the mock lists them. A static list; `b` returns to where the list
// was opened from.
import { BackRow, P, type Ctx, type View } from './ui'

type Rows = [string, string, string?][] // key, what it does, the key's own colour

const PROMPT: Rows = [
  ['/owlpost:panel', 'open or focus the panel, again to close'],
  ['ctrl+x tab', 'move the keys into the panel'],
  ['click', 'a click in the panel does the same'],
]
const EVERYWHERE: Rows = [
  ['1–5', 'tabs'],
  ['tab', 'move the focus ring'],
  ['↑ ↓', 'scroll the panel'],
  ['enter', 'press the focused control'],
  ['esc', 'keys back to the prompt, panel stays'],
  ['h q', 'this list, close the panel'],
  ['in a text field', 'letters type; tab leaves the field', P.wait],
]
const LISTS: Rows = [
  ['f', 'filter'],
  ['n', 'new thread, add contact'],
  ['b', 'back'],
  ['k p', 'card, policy'],
  ['a v', 'archive, show archived'],
]
const THREAD: Rows = [
  ['r', 'reply'],
  ['c', 'cite the message in the prompt'],
  ['a', 'apply it in the prompt'],
  ['t u', 'cite, apply the whole thread'],
  ['y', 'copy'],
]
const REQUESTS: Rows = [
  ['o w', 'allow once, allow always'],
  ['d', 'draft with Claude'],
  ['e s', 'edit, send'],
  ['x', 'reject, deny, remove, delete'],
]

// One section: its title in bold secondary, then key (body text) and what it does (secondary).
function Section({ c, title, rows, width }: { c: Ctx; title: string; rows: Rows; width: number }) {
  const { Box, Text } = c.ui
  return (
    <Box flexDirection="column">
      <Text bold color={P.secondary}>{title}</Text>
      {rows.map(([k, what, color]) => (
        <Box key={k} flexDirection="row">
          <Box width={width} flexShrink={0}>
            {color ? <Text color={color}>{k}</Text> : <Text>{k}</Text>}
          </Box>
          <Text color={P.secondary}>{what}</Text>
        </Box>
      ))}
    </Box>
  )
}

export const keys: View = (c) => {
  const { Box, Text } = c.ui
  return {
    body: (
      <Box flexDirection="column" gap={1}>
        <BackRow c={c} title="Keys" />
        <Section c={c} title="From the prompt" rows={PROMPT} width={16} />
        <Section c={c} title="Everywhere in the panel" rows={EVERYWHERE} width={16} />
        <Box flexDirection="row" columnGap={2} flexWrap="wrap">
          <Box width={28} flexShrink={0}><Section c={c} title="Lists" rows={LISTS} width={4} /></Box>
          <Box flexGrow={1} flexShrink={1}><Section c={c} title="Thread" rows={THREAD} width={4} /></Box>
        </Box>
        <Section c={c} title="Requests" rows={REQUESTS} width={16} />
        <Box flexDirection="column">
          <Text bold color={P.secondary}>Left to Claude Code</Text>
          <Text color={P.secondary}>
            The panel binds bare letters and digits only. They work while the panel holds the keyboard and the focus is on a
            row or button, never while you type in a field. Arrows, tab, enter, esc and ctrl chords keep their Claude Code
            meaning.
          </Text>
        </Box>
      </Box>
    ),
    keys: [['b', 'back'], ['esc', 'prompt']],
    note: '',
  }
}
