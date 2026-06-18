# origin-i18n

> Lightweight std-only UI string catalog with locale fallback and placeholder substitution.

## Purpose

`origin-i18n` gives `origin`'s terminal chrome the multi-locale reach competitor
harnesses ship (kilocode carries ~21 UI locales; opencode bundles an i18n layer)
without dragging in a runtime catalog loader. Every translation is a literal arm
in a `match` over `&'static str`, so lookup never allocates, nothing has to be
warmed at startup, and the whole catalog is baked into the binary. Missing keys
fall back to English; keys absent everywhere echo a stable sentinel so the UI
never renders a blank slot.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Lang` | enum | The six shipped locales: `En`, `Es`, `Fr`, `De`, `Ja`, `ZhCn`. |
| `Lang::from_code` | fn | Parse a BCP-47-ish tag (`"fr-FR"`, `"zh-Hans"`); tolerant of region + case. |
| `Lang::code` | fn | Render a `Lang` back to its canonical code (`"en"`, `"zh-CN"`, …). |
| `available` | fn | `&'static [Lang]` of every locale this build can render, in canonical order. |
| `t` | fn | Localized string for `(lang, key)`; English fallback, then key-echo. |
| `tf` | fn | Like `t`, plus `{name}` placeholder substitution from `&[(&str, &str)]`. |

## Key types

```rust
pub enum Lang { En, Es, Fr, De, Ja, ZhCn }

#[must_use]
pub fn t(lang: Lang, key: &str) -> &'static str { /* lang → English → key */ }

#[must_use]
pub fn tf(lang: Lang, key: &str, args: &[(&str, &str)]) -> String { /* substitute {name} */ }
```

The catalog key set is enumerated once in a private `KEYS` table and includes
both top-level chrome (`welcome`, `thinking`, `permission.ask`, `cost.turn`) and
in-session command feedback routed from `origin-cli` (`cmd.model.set`,
`cmd.effort.set`, `cmd.outputstyle.set`, `cmd.queue.added`, `resume.foreign.ok`, …).

## How it works

`t` resolves in three tiers, and `tf` layers placeholder substitution on top:

```
t(lang, key)
   │  lookup(lang, key)         ── locale-specific arm
   ├─ Some(s) ───────────────►  return s
   │  lookup(En, key)           ── English fallback
   ├─ Some(s) ───────────────►  return s
   └─ static_key(key)           ── echo key from KEYS, else "?"

tf(lang, key, args)  =  t(lang, key) then replace each {name} ⇒ value
```

Per-locale `en`/`es`/`fr`/`de`/`ja`/`zh_cn` functions each return
`Option<&'static str>`, so a locale that omits a key simply returns `None` and
falls through to English. The English literals for keys routed from live CLI
call sites are deliberately reconciled to be **byte-identical** to the text those
sites emitted before routing — so default-English output (no `--lang`, no
`$LANG`) is unchanged. `tf` leaves placeholders without a matching arg verbatim
(`[{tool}]` with no `tool` stays `[{tool}]`) and tolerates an unbalanced `{`.

## Dependencies & features

Zero external dependencies — pure `std`, `#![forbid(unsafe_code)]`. No async, no
I/O, no `lazy_static`/`OnceLock`. No Cargo features.

## Used by

`Grep "origin-i18n" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-i18n/Cargo.toml` (self)

## Testing

Inline `#[cfg(test)]` unit tests cover: `from_code`/`code` round-trips; region +
case tolerance; English fallback when a locale lookup is `None`; unknown-key
sentinel (`"?"`); placeholder substitution and verbatim pass-through of unmatched
placeholders; `available()` listing all six locales uniquely; that every locale
translates every `KEYS` entry; and two drift guards — that the newly-routed keys
keep their byte-identical English literals and still localize away from English
in at least one non-English locale.

## See also

- [tui-and-cli subsystem](../subsystems/tui-and-cli.md)
- [crates index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
