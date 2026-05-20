# Skills

A skill is a markdown file with a YAML frontmatter header and an instructions
body. Skills extend `origin` with reusable workflows — refactoring routines,
project-specific conventions, codebase tours — without requiring users to
write Rust. They live in `~/.origin/skills/` (or wherever `paths.skills`
points in [Configuration](configuration.md)).

## Frontmatter format

`origin-skills` parses the frontmatter into a typed schema. Anything past the
closing `---` is the body, which is what materializes into the model's
context when the skill activates.

```yaml
---
name: refactor-rust-module
description: |
  Refactor a single Rust module in the current workspace. Reads the module,
  proposes a plan, applies edits, and runs `cargo check` + `cargo test`
  before reporting back.
allowed-tools:
  - Read
  - Glob
  - Grep
  - Edit
  - Bash(cargo check:*)
  - Bash(cargo test:*)
required-capabilities:
  - code_graph
---

# Body

When invoked, first call `graph_query` to fetch the module's neighbors and
god-nodes. Then read the file, propose the smallest diff that achieves the
user's stated goal, and ...
```

Three fields are load-bearing:

- **`allowed-tools`** narrows the session's permission set for the duration
  of the skill. The pattern syntax supports per-tool argument wildcards
  (`Bash(cargo check:*)` permits any `cargo check ...` invocation but not
  `cargo run`). `origin-permission` enforces this at the tier-engine layer,
  so a skill that omits `Bash` cannot shell out even if it tries.
- **`required-capabilities`** lets a skill refuse to inject when its
  prerequisites aren't met. `code_graph`, `memory`, `mcp:<server>`,
  `sandbox:<profile>` are the standard capabilities.
- **`name`** is the skill's stable identifier. It is also used as the cache
  key for the embedding index.

## Embedding-indexed lazy injection

`origin` does not load every skill on session start — that would balloon the
system prompt. Instead, at skill install time, `origin-skills` embeds the
tuple `(name + description + first-line-of-body)` into the same HNSW index
that `origin-mem` uses, tagged with kind `Skill`. The per-turn recall pass
that finds memories also proposes skills; the top-K bodies materialize into
the `CachePlanner`'s Sticky band so they stay cache-resident across the rest
of the session.

Session-start scan cost is therefore zero. A user can have 500 skills
installed and the cold-start path still touches none of them.

When a skill activates you'll see a `skill_injected` event in the side panel,
and the `?metrics` panel reports how many skill tokens landed in the Sticky
band.

## Importing skills from other harnesses

The first time `origin` starts, it offers to import skills from any
detectable Claude Code installation:

```bash
origin import claude-code --from ~/.claude
```

The importer:

1. Walks `~/.claude/skills/`.
2. Hashes each `SKILL.md` body.
3. Compares against the local CAS — identical content dedupes silently.
4. Shows a one-screen TUI confirmation listing only the new skills.

Dry-run first if you want to see what would happen without writing anything:

```bash
origin import claude-code --from ~/.claude --dry-run
```

See [Migration](migration.md) for the equivalent commands for `jcode` and
`opencode`, and for what travels alongside the skills (sessions, eventually
memories).

## Authoring tips

- Keep the body under ~2KB. Larger bodies cost cache tokens every turn the
  skill is active. Move long examples into a referenced file the skill
  reads with `Read` at invocation time.
- Set `required-capabilities` explicitly. A skill that needs the code graph
  but doesn't declare it will be injected on machines without one, and the
  user will get confusing failures.
- Use the `allowed-tools` narrowing aggressively. The narrower the surface,
  the less the permission engine has to ask the user.
