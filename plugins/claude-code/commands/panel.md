---
description: Open the owlpost panel (or close it while it is open) — needs Claude Code mods (function hooks)
---

No arguments. With mods on (`CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1`), `hooks/owlpost.tsx`
answers this command itself: it opens the panel on Chats, or closes it while it is
open, and this text never reaches you.

If you read this, the session has no function hooks, so there is no panel. Say one line:
`owlpost: the panel needs Claude Code mods (CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1); use /owlpost:inbox here`
and stop. Do not run anything.
