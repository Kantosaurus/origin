---
name: clear
description: Invoked by /clear. Resets the current conversation by discarding the in-session context (message history, loaded files, accumulated tool results, transient working assumptions) and restarting from a clean slate. Project files on disk, user settings, and persistent cross-session memory are NOT affected. Use whenever the user types /clear, says 'clear the conversation', 'start fresh', 'wipe context', or otherwise asks for a context reset.
---

# Clear Conversation

## Overview

`/clear` wipes the **in-session context** so you continue as if origin had just been launched. It does **not** touch anything outside the current conversation.

## What gets wiped

- Prior message history in this conversation
- Files read or loaded into context this session
- Tool call results accumulated this session
- Transient assumptions, plans, or todos built up during the session
- Active skill activations from earlier in the session (other than `clear` itself)

## What is preserved

- Project files on disk (no edits are reverted)
- User settings, configuration, and permissions
- Persistent cross-session memory (`MEMORY.md` and linked memory files)
- The current working directory and git state
- Authenticated provider sessions and credentials

## How to respond after `/clear`

Treat the next user turn as the first turn of a new conversation:

1. Do not reference earlier messages, decisions, files, or tool results from this session — you no longer have them.
2. Do not assume continuity of in-progress work. If the user resumes a prior task, ask them to restate what they need.
3. Re-read any files you need; do not rely on a remembered version.
4. Persistent memory is still available — consult it as you normally would at the start of a conversation.

## When to use

- The user explicitly types `/clear`.
- The user asks to "start fresh", "wipe context", "clear the chat", "reset the conversation", or equivalent.
- The user signals a hard topic change and wants no carry-over from prior context.

## When NOT to use

- The user only wants to undo a *file* change — that is a git/edit operation, not a context reset.
- The user wants to clear *persistent memory* — that is a memory-file edit, not `/clear`.
- The user just wants you to stop a specific behavior — adjust behavior in-place rather than wiping the session.
