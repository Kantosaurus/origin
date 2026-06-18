# origin-skills

> Skills loader with embedding upsert and allowed-tools narrowing.

## Purpose

`origin-skills` owns the on-disk **skill model** and the in-memory **active-skill
stack**. A *skill* is a `SKILL.md` file — YAML frontmatter plus a Markdown body —
that extends the agent with a reusable workflow without anyone writing Rust. This
crate parses skills, ships a vendored catalog of "superpowers" skills embedded in
the binary, content-addresses each body, upserts skill text into the shared
embedding index for lazy per-turn injection, and computes the `allowed-tools`
intersection mask that narrows the permission surface while skills are active.

It sits between the filesystem / embedded assets and two consumers: the daemon
(which loads the catalog and injects active-skill bodies into the system prompt)
and the permission engine (which reads the active mask).

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `SkillFrontmatter` | struct | Typed frontmatter: `name`, `description`, `allowed_tools` (`#[serde(rename = "allowed-tools")]`, defaults empty). |
| `parse_frontmatter` | fn | Split a `SKILL.md` string into `ParsedSkill { front, body }`; BOM- and CRLF-tolerant, body normalized to LF. |
| `ParsedSkill` | struct | `{ front: SkillFrontmatter, body: String }`. |
| `FrontmatterError` | enum | `MissingOpen` / `MissingDelimiter` / `Yaml(String)`. |
| `Skill` | struct | A loaded skill: `front`, `body`, `body_hash: SkillHash`, `source: PathBuf`. |
| `SkillHash` | struct | `pub [u8; 32]` — blake3 hash of the body bytes (the CAS dedupe key). |
| `LoaderError` | enum | `Io { path, source }` / `Frontmatter { path, source }`. |
| `load_skills_dir` | fn | Walk one level into a dir, load every `<dir>/SKILL.md`. |
| `load_embedded` | fn | Return every vendored superpowers skill (walked from the embedded tree). |
| `load_all` | fn | `load_embedded()` merged with a user dir; **user entries override embedded by `name`**. |
| `ActiveSkill` | struct | One active-stack entry: `{ front, body }`. |
| `SkillRegistry` | struct | The per-connection active-skill stack + `allowed_tools()` mask. |
| `SkillEmbedder` | struct | Upserts skill text into `origin_mem::MemIndex` (kind = `Skill`). |
| `SkillEmbedError` | enum | `Index(..)` / `Embed(..)`. |

## Key types

The frontmatter schema is intentionally tiny — two required fields plus an
optional tool allow-list:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default, rename = "allowed-tools")]
    pub allowed_tools: Vec<String>,
}
```

A loaded skill carries its content hash and provenance:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SkillHash(pub [u8; 32]); // blake3 of the body bytes

#[derive(Debug, Clone)]
pub struct Skill {
    pub front: crate::frontmatter::SkillFrontmatter,
    pub body: String,
    pub body_hash: SkillHash,
    pub source: std::path::PathBuf,
}
```

The active-skill stack computes the **intersection** of every restricting skill's
`allowed-tools` — and deliberately treats a skill with no list as "imposes no
narrowing" rather than "deny everything":

```rust
pub fn allowed_tools(&self) -> Option<HashSet<String>> {
    // Only skills with a non-empty allowed-tools contribute. If none do,
    // return None so the permission engine falls through to the default tier.
    // Some(empty set) means *no tool is allowed*.
    let mut restricting = self.stack.iter().map(|s| &s.front)
        .filter(|s| !s.allowed_tools.is_empty());
    let first = restricting.next()?;
    let mut acc: HashSet<String> = first.allowed_tools.iter().cloned().collect();
    for skill in restricting {
        let cur: HashSet<String> = skill.allowed_tools.iter().cloned().collect();
        acc = acc.intersection(&cur).cloned().collect();
    }
    Some(acc)
}
```

## How it works

**Loading & precedence.** `load_embedded()` walks an `include_dir!`-embedded
`embedded/superpowers/` tree (so the binary ships with every superpowers skill —
no install step). `load_all(user_root)` then merges any `~/.origin/skills/`
overrides on top, replacing embedded entries with the same `name` and appending
new ones. `load_skills_dir` is *fail-fast*: a single malformed `SKILL.md` returns
`LoaderError`. The daemon's catalog wrapper degrades that to an empty catalog so a
corrupt skill can't deny service.

**Content addressing.** Each body is hashed with blake3 into `SkillHash`; two
skills with identical bodies dedupe in CAS regardless of path.

**Lazy injection.** `SkillEmbedder` embeds `(name + description + first line of
body)` into the same `origin_mem` HNSW index the conversation memory uses, tagged
kind `Skill`. The per-turn recall pass proposes the top-K skills; their bodies
materialize into the prompt-cache Sticky band. Session-start scan cost is
therefore zero even with hundreds of installed skills. In production the embedder
shares an `Arc<origin_mem::Embedder>` with the daemon so skills and memories land
in the same vector space; a deterministic `stub_for_tests()` arm avoids ONNX in
unit tests.

**Activation.** The daemon pushes onto a per-connection `SkillRegistry` via
`activate_with_body(front, body)`; `iter_active_entries()` feeds the
`<origin-active-skills>` system-prompt block, and `allowed_tools()` feeds the
permission engine.

```text
SKILL.md ──parse──▶ Skill{front,body,body_hash} ──┬─▶ SkillEmbedder.upsert ─▶ MemIndex (kind=Skill)
                                                   └─▶ SkillRegistry.activate_with_body
                                                          ├─▶ <origin-active-skills> prompt block
                                                          └─▶ allowed_tools() ▶ permission mask
```

## Dependencies & features

- `serde` + `serde_yaml` — frontmatter deserialization.
- `blake3` — body hashing (`SkillHash`).
- `include_dir` (workspace) — compile-time embedding of the superpowers catalog.
- `origin-mem` — the shared embedder + HNSW index for lazy injection.
- `thiserror` — error enums. Dev: `tempfile`.

No cargo features; `[lints] workspace = true` (so `unsafe` is forbidden,
`unwrap_used` denied).

## Used by

`Select-String -Path crates\*\Cargo.toml -Pattern "origin-skills"` →

- `origin-daemon` — loads the catalog, injects active-skill bodies, snapshots the registry.
- `origin-cli` — surfaces skills in autocomplete / palette.
- `origin-permission` — reads the active `allowed-tools` mask.
- `origin-migrate` — imports skills from other harnesses.

## Testing

`crates/origin-skills/tests/` holds `embedded_skills.rs` (asserts the embedded
catalog by name and exact count — currently **19** skills, a gate that must be
updated when adding embedded skills), plus `frontmatter.rs`, `loader.rs`,
`registry.rs`, `embed.rs`, and `import.rs`. Several modules also carry in-file
`#[cfg(test)]` units (e.g. the loader's merge/override semantics).

## See also

- [Skills, Hooks & Workflows](../subsystems/skills.md) — the subsystem deep-dive.
- [Authoring skills](../guides/authoring-skills.md) — how to write a `SKILL.md`.
- [Security model](../security/security-model.md) — how `allowed-tools` narrowing is enforced.
- [Crate index](README.md)

_Last reviewed against workspace version 0.9.8._
