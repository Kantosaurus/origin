# Migration

`origin import` brings your existing sessions and skills over from Claude
Code, `jcode`, and `opencode`. Imports are idempotent: every artifact is
hashed and compared against the local CAS, so running the importer twice
adds nothing the second time.

## Always dry-run first

Every importer accepts `--dry-run`. Use it. The output lists exactly what
would be created, deduped, or skipped:

```bash
origin import claude-code --from ~/.claude --dry-run
origin import jcode       --from ~/.jcode  --dry-run
origin import opencode    --from ~/.opencode --dry-run
```

A dry-run produces no SQLite writes and no CAS shards. It still hashes every
source artifact, so on a large `~/.claude` it can take a minute.

## Importing Claude Code

```bash
origin import claude-code --from ~/.claude
```

What travels:

- **Sessions.** `~/.claude/projects/*/sessions/*.jsonl` transcripts become
  `origin` sessions. Each message is rehydrated into `origin-core`'s IR;
  tool inputs/outputs land in CAS, with one `Handle` per message — the
  same shape as a session born inside `origin`.
- **Skills.** `~/.claude/skills/*/SKILL.md` and any sibling assets dedupe
  by content hash against the local CAS. The TUI shows a one-screen
  confirmation listing only genuinely new skills.
- **Settings hints.** Selected `~/.claude/settings.json` fields (allowed
  hooks, permission rules) are surfaced as suggested `config.toml` edits;
  nothing is written until you confirm.

What does not travel yet:

- **Memories.** Memory import requires the `origin-mem` HNSW + sidecar
  verifier pipeline to be available on the target machine; this is gated
  on Phase 6 of the project plan and ships incrementally.
- **OAuth tokens.** Auth is always re-established through `origin keyring
  login` — never copied from another harness's storage.

## Importing jcode and opencode

The flags are symmetric:

```bash
origin import jcode    --from ~/.jcode
origin import opencode --from ~/.opencode
```

Each importer knows the source's session format (`jcode` stores per-project
SQLite blobs, `opencode` uses NDJSON event logs) and projects them through
the same `origin-core` IR landing path. The dedupe story is identical to
Claude Code: content-hashed comparison against the local CAS, with new
skills surfaced in a TUI confirmation screen.

## Idempotent content-hash dedupe

Every artifact — a session message, a tool result, a skill body — is FastCDC-
chunked and content-addressed before insertion. The importer queries
`origin-cas` for each chunk hash; matches refcount up, misses become new
shards. The same source file imported twice from two machines produces zero
new bytes the second time.

This also means you can incrementally re-run an import after a Claude Code
update without worrying about duplicate sessions:

```bash
# Initial import
origin import claude-code --from ~/.claude

# Three weeks later, after using Claude Code in parallel
origin import claude-code --from ~/.claude
# → reports: "147 sessions already present, 8 new, 0 skills changed"
```

## Verifying an import

After the import completes, sanity-check the result:

```bash
# Sessions show up in origin's listing
origin sessions ls --imported-from claude-code | head

# Resume one
origin sessions resume <session-id>

# Confirm CAS GC sees the new refcounts
origin admin gc-cas --report
```

If an import surfaces an error (typically a malformed transcript line in the
source), the importer logs the offending record to the trace parquet ring and
continues with the next one — partial imports are always safe to re-run.
