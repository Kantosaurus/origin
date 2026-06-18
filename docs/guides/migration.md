# Migration

Switching to `origin` from another coding-agent harness? `origin import` brings
your **sessions**, **skills**, and **memories** across into `origin`'s own
store. The import is **idempotent** — it dedupes by content hash, so re-running
it never creates duplicates. You can preview exactly what would be imported
before you commit to anything.

`origin` can import from **Claude Code**, **jcode**, **opencode**, and **Codex
CLI**.

> Want to keep *talking* to a transcript from another tool, not just archive it?
> See `origin resume-foreign` at the end of this guide — it reconstructs a
> foreign transcript into a brand-new resumable `origin` session.

---

## The `origin import` command

```sh
origin import <source> --from <path> [--apply] [--json] [--db <path>]
```

| Argument / flag | Meaning |
| --- | --- |
| `<source>` | One of `claude-code`, `jcode`, `opencode`, `codex`. |
| `--from <path>` | Path to the external session file or the harness's root directory. **Required.** |
| `--apply` | Persist the bundle to the store. **Omit it for a preview** (see below). |
| `--json` | Emit the report as JSON instead of human-readable text. |
| `--db <path>` | Override the SQLite store path. Defaults to `ORIGIN_DB`, then a temp-dir fallback. |

### Preview first (the default is a dry run)

By default — that is, **without `--apply`** — `import` scans the source and
prints a summary of what *would* be imported without writing anything. This is
the dry-run path; it has no side effects.

```sh
# Preview what an import would bring in (writes nothing):
origin import claude-code --from ~/.claude

# Same preview, machine-readable:
origin import claude-code --from ~/.claude --json
```

> **Flag note.** `origin` models the dry run as the *default* and uses `--apply`
> to opt into persistence. There is no separate `--dry-run` flag — running
> `import` without `--apply` *is* the dry run.

### Commit the import

Add `--apply` to actually persist the scanned bundle through the same SQLite
store the daemon uses:

```sh
origin import claude-code --from ~/.claude --apply
origin import jcode      --from ~/.jcode  --apply
origin import opencode   --from ~/.config/opencode --apply
origin import codex      --from ~/.codex --apply
```

---

## What travels

Each source adapter scans a harness root into a normalized bundle with three
artifact kinds:

| Artifact | What it is |
| --- | --- |
| **Sessions** | Conversation transcripts (a `source_id` plus an ordered list of role/body messages). |
| **Skills** | `SKILL.md`-style capabilities (name + body). |
| **Memories** | Saved memory entries (kind + body + tags). |

The summary / apply report counts each kind separately, with both *inserted* and
*skipped-duplicate* tallies:

```text
sessions_inserted          12
sessions_skipped_duplicate  0
skills_inserted             4
skills_skipped_duplicate    0
memories_inserted           7
memories_skipped_duplicate  0
```

After a real `--apply`, the reported insert counts match exactly what is then
queryable in the store.

---

## Idempotency: content-hash dedupe

Re-running `origin import` is safe. Every artifact is keyed by a **blake3**
content hash of its meaningful fields, and the store skips any artifact whose
hash it has already seen:

- **Sessions** hash `(source_id, [each message's role + body])`.
- **Skills** hash `(name, body)` — so two skills with identical bodies but
  different names are kept distinct.
- **Memories** hash `(kind, body, tags)`.

Each variable-length field is **length-framed** before hashing, so the encoding
is injective — no boundary between two fields can be confused for field content,
and two genuinely distinct artifacts can never collide into a "duplicate." The
practical upshot:

```sh
# Run it twice — the second run inserts nothing new:
origin import opencode --from ~/.config/opencode --apply
origin import opencode --from ~/.config/opencode --apply   # all skipped_duplicate
```

This means you can safely:

- re-import after adding more sessions in the old tool (only the new ones land);
- import the same root from a script on a schedule;
- import overlapping roots (e.g. a project dir and a parent dir) without fear of
  doubling anything.

---

## A typical migration

```sh
# 1. Preview — see the counts, confirm the path is right.
origin import claude-code --from ~/.claude

# 2. Commit.
origin import claude-code --from ~/.claude --apply

# 3. Confirm in origin.
origin sessions ls

# 4. (Optional) bring skills from a second tool too — still idempotent.
origin import opencode --from ~/.config/opencode --apply
```

Imported **skills** then participate in `origin`'s embedding-indexed injection
just like native ones — see [Authoring skills](authoring-skills.md). Imported
**sessions** show up in `origin sessions ls` and can be resumed with
`origin sessions resume <id>`.

---

## Live resume from a foreign harness

`origin import` *archives* history. If instead you want to **continue** a
conversation that started in another tool, use `origin resume-foreign`, which
reconstructs the foreign transcript into `origin`'s native message model as a
brand-new, resumable session:

```sh
origin resume-foreign claude-code ~/.claude/projects/acme/session-42.jsonl
# then keep talking to it:
origin sessions resume <new-id>
```

`resume-foreign` accepts `claude-code`, `jcode`, `opencode`, `codex`, and `pi`
(plus the aliases `claude`/`cc`/`oc`/`cx`/`π`). It also picks a sensible origin
model for the reconstructed session, mapping the external model id onto an
`origin` catalog id where it can.

---

## Where imported data lives

Imported artifacts are written to the daemon's **SQLite store**, selected (in
order) by the `--db` flag, the `ORIGIN_DB` environment variable, or a temp-dir
fallback (`<temp>/origin.db`). They do **not** live under `~/.origin/`. Point
`--db`/`ORIGIN_DB` at the same database the daemon uses so an import is visible
in your sessions list.

---

See the [`origin-migrate` crate reference](../crates/origin-migrate.md) for the
adapter, bundle, and sink internals (the `Source` trait, `MigrateBundle`,
`apply_with_store`, and the per-harness scanners).

_Last reviewed against workspace version 0.9.8._
