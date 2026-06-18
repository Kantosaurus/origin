# origin-outputstyle

> Output styles (Explanatory/Learning/Concise) plus a transform-or-hide MessageDisplay hook.

## Purpose

`origin-outputstyle` carries two orthogonal text concerns. A [`Style`] picks a
claude-code-style output persona — Explanatory, Learning, or Concise — and
contributes a system-prompt suffix that shapes the assistant's prose without
touching tool behaviour. Separately, a `MessageDisplay` hook can rewrite or
suppress a rendered message via a [`DisplayAction`]. The crate is a pure text
transform — no I/O, no async — so every path is offline and trivially testable.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Style` | enum | `Default` / `Explanatory` / `Learning` / `Concise`. |
| `Style::from_str_opt` | fn | Case/whitespace-insensitive label parse, `None` if unknown. |
| `Style::label` | fn | Canonical lowercase label. |
| `Style::system_suffix` | fn | Extra system-prompt guidance (`""` for `Default`). |
| `Style::display_transform` | fn | Per-style display rewrite seam (identity today). |
| `DisplayAction` | enum | `Show` / `Hide` / `Replace(String)`. |
| `DisplayHookResult` | struct | `{ action: DisplayAction }` with `new`. |
| `apply_display` | fn | Apply an action to text → `Option<String>` (`None` = hide). |
| `resolve_display` | fn | Hook-first composition over an optional `Style`. |
| `parse_display_hook` | fn | Decode a hook's JSON verdict; `OutputStyleError::Parse` on bad input. |

## Key types

```rust
#[derive(Default, Serialize, Deserialize)]
pub enum Style { #[default] Default, Explanatory, Learning, Concise }

#[serde(rename_all = "lowercase", tag = "action", content = "text")]
pub enum DisplayAction { Show, Hide, Replace(String) }
```

## How it works

The two concerns compose hook-first in `resolve_display`:

```
resolve_display(text, style, action)
   │  action.is_some()?
   ├─ yes → apply_display(text, action)   Show→text · Hide→None · Replace→sub
   └─ no  → style.unwrap_or_default().display_transform(text)
```

`system_suffix` is the prompt-side lever: the daemon appends a non-empty
instruction for each non-`Default` style (e.g. *"Output style: Explanatory. As
you work, explain the reasoning behind your choices…"*) and nothing for
`Default`, keeping the default wire byte-identical. `display_transform` is the
output-side seam: every built-in style is the identity transform today (styles
shape the *prompt*, not the *rendered* text), but it exists so a future style or
a downstream embedder can rewrite or hide displayed text. `parse_display_hook`
reads a hook's `{ "action": "show"|"hide"|"replace", "text"?: … }` object;
the action match is case-insensitive and whitespace-tolerant, `replace` without a
`text` field defaults to the empty string, and any other action string is an
`OutputStyleError::Parse`. This is the seam the `MessageDisplay` lifecycle event
(see `origin-hooks`) feeds: a hook inspects an about-to-be-displayed message and
returns a verdict that `apply_display`/`resolve_display` then enact, so a hook can
redact secrets or suppress noise from the rendered transcript without ever
touching the model wire.

## Dependencies & features

`serde` + `serde_json` (hook verdict de/serialization) and `thiserror`.
`#![forbid(unsafe_code)]`. No Cargo features.

## Used by

`Grep "origin-outputstyle" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-outputstyle/Cargo.toml` (self)

## Testing

Inline unit tests cover: label round-trips through `from_str_opt` (with
case/whitespace tolerance); non-empty `system_suffix` for non-default styles and
empty for `Default`; each `apply_display` arm (Show/Hide/Replace); parsing all
three actions (including `"HIDE"` case-insensitivity and `replace`-without-text
defaulting to empty); rejection of malformed JSON, non-objects, and unknown
actions; parse→apply round-trip; that `display_transform` is the identity for
every style; and that a hook decision wins over the active style in
`resolve_display`.

## See also

- [tui-and-cli subsystem](../subsystems/tui-and-cli.md)
- [skills subsystem](../subsystems/skills.md)
- [crates index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
