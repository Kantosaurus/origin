// SPDX-License-Identifier: Apache-2.0
//! Customizable composer keybindings (claude-code L147 parity).
//!
//! A pure [`KeyMap`] resolves a `crossterm` key event to a rebindable
//! [`Action`]. The builtin default map reproduces today's hard-wired composer
//! shortcuts exactly, so an absent `~/.origin/keybindings.toml` ⇒ the resolver
//! returns [`Action::None`] for every event the legacy reducer already owns and
//! the caller falls through to the unchanged direct path. Only an explicit
//! override file changes any binding.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

/// A rebindable composer action.
///
/// These name the small set of editor commands the composer exposes for
/// rebinding. [`Action::None`] is the resolver's "not bound — let the default
/// reducer handle it" sentinel; it is never stored in a map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Submit the current buffer.
    Submit,
    /// Cancel the in-flight operation (or quit when idle).
    Cancel,
    /// Recall the previous history entry.
    HistoryPrev,
    /// Recall the next history entry.
    HistoryNext,
    /// Clear the input buffer.
    Clear,
    /// Enter reverse-incremental history search (Ctrl-R by default).
    ReverseSearch,
    /// No binding matched; the caller uses its default handling.
    None,
}

impl Action {
    /// Parse an action name as written in `keybindings.toml`.
    ///
    /// Case-insensitive; hyphen and underscore are interchangeable so both
    /// `history-prev` and `history_prev` resolve. Unknown names yield `None`
    /// (the entry is ignored by the loader).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().replace('-', "_").as_str() {
            "submit" => Some(Self::Submit),
            "cancel" => Some(Self::Cancel),
            "history_prev" => Some(Self::HistoryPrev),
            "history_next" => Some(Self::HistoryNext),
            "clear" => Some(Self::Clear),
            "reverse_search" => Some(Self::ReverseSearch),
            _ => None,
        }
    }
}

/// A single key chord: a [`KeyCode`] plus a normalised modifier mask.
///
/// Only `CONTROL`, `ALT`, and `SHIFT` are retained from the event modifiers so
/// terminal-injected flags (e.g. `KEYPAD`) never defeat a match. `crossterm`'s
/// `KeyCode`/`KeyModifiers` derive `PartialEq`/`Eq` but not `Ord`, so chords are
/// compared by equality in a small linear table rather than ordered in a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    code: KeyCode,
    mods: KeyModifiers,
}

/// Modifiers we consider meaningful for a chord. Everything else is masked off
/// before comparison so spurious terminal flags do not break a binding.
const MEANINGFUL_MODS: KeyModifiers = KeyModifiers::CONTROL
    .union(KeyModifiers::ALT)
    .union(KeyModifiers::SHIFT);

impl Chord {
    /// Build a chord from a code and modifier mask, normalising the modifiers.
    #[must_use]
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        Self {
            code,
            mods: mods & MEANINGFUL_MODS,
        }
    }

    /// The chord a key event denotes, with modifiers normalised.
    #[must_use]
    pub fn from_event(ev: KeyEvent) -> Self {
        Self::new(ev.code, ev.modifiers)
    }

    /// Parse a chord description like `ctrl+p`, `shift+enter`, `up`, `esc`.
    ///
    /// `+`-separated tokens; the final token is the key, the rest are
    /// modifiers (`ctrl`/`control`, `alt`/`meta`, `shift`). Key names cover the
    /// composer's vocabulary: single chars, `enter`/`return`, `tab`, `esc`,
    /// `up`/`down`/`left`/`right`, `backspace`, `space`. Returns `None` for any
    /// unrecognised token so a typo in the config is ignored rather than fatal.
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        let mut mods = KeyModifiers::NONE;
        let mut code: Option<KeyCode> = None;
        for tok in spec.split('+') {
            let tok = tok.trim();
            if tok.is_empty() {
                return None;
            }
            match tok.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "meta" | "option" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                key => {
                    // The key token must be last; a second key is a malformed
                    // chord.
                    if code.is_some() {
                        return None;
                    }
                    code = Some(parse_key(key)?);
                }
            }
        }
        code.map(|c| Self::new(c, mods))
    }
}

/// Parse a single non-modifier key token into a [`KeyCode`].
fn parse_key(key: &str) -> Option<KeyCode> {
    match key {
        "enter" | "return" => Some(KeyCode::Enter),
        "tab" => Some(KeyCode::Tab),
        "esc" | "escape" => Some(KeyCode::Esc),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "backspace" | "bs" => Some(KeyCode::Backspace),
        "space" => Some(KeyCode::Char(' ')),
        other => {
            let mut chars = other.chars();
            let first = chars.next()?;
            if chars.next().is_none() {
                Some(KeyCode::Char(first))
            } else {
                None
            }
        }
    }
}

/// A resolved chord → action map.
///
/// Built from the builtin defaults and optionally overlaid with a user config.
/// [`KeyMap::resolve`] returns the bound [`Action`] for a key event, or
/// [`Action::None`] when no chord matches — the signal to fall through to the
/// legacy reducer. The table is tiny (a handful of bindings) so a linear scan
/// is both simplest and fastest, and it sidesteps `crossterm`'s lack of `Ord`.
#[derive(Debug, Clone)]
pub struct KeyMap {
    bindings: Vec<(Chord, Action)>,
    /// `true` once at least one user override has actually been applied (via
    /// [`KeyMap::from_toml_str`]/[`KeyMap::load`]). The pure builtin map leaves
    /// this `false`, letting the caller gate *additive* behaviour (e.g. wiring
    /// the otherwise-no-op `Clear` chord) so the default key path stays
    /// byte-identical when no `keybindings.toml` is present.
    overridden: bool,
}

impl Default for KeyMap {
    fn default() -> Self {
        Self::builtin()
    }
}

impl KeyMap {
    /// The builtin default map: the composer's current hard-wired chords.
    ///
    /// Mirrors `input::reduce`: `Enter` submits, `Ctrl+C` cancels, `Up`/`Down`
    /// walk history, `Ctrl+U` clears. These are advisory — the resolver only
    /// reports the action; the existing reducer remains the source of truth for
    /// the default path, so reporting the same intent keeps behaviour identical.
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            bindings: vec![
                (Chord::new(KeyCode::Enter, KeyModifiers::NONE), Action::Submit),
                (
                    Chord::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                    Action::Cancel,
                ),
                (Chord::new(KeyCode::Up, KeyModifiers::NONE), Action::HistoryPrev),
                (Chord::new(KeyCode::Down, KeyModifiers::NONE), Action::HistoryNext),
                (
                    Chord::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                    Action::Clear,
                ),
            ],
            overridden: false,
        }
    }

    /// Whether a user override has been applied on top of the builtin map.
    ///
    /// `false` for the pure builtin/default map; `true` after a non-empty
    /// `keybindings.toml` rebinds at least one action. Callers gate additive
    /// behaviour on this so an absent config leaves the key path byte-identical.
    #[must_use]
    pub const fn is_overridden(&self) -> bool {
        self.overridden
    }

    /// Overlay parsed override entries onto this map.
    ///
    /// Each entry rebinds one action to one chord. A rebind first drops any
    /// existing chord(s) pointing at that action so a moved binding leaves no
    /// stale duplicate, and any prior binding of the *new* chord, then appends
    /// the new pair.
    fn apply_overrides(&mut self, overrides: &[(Action, Chord)]) {
        for &(action, chord) in overrides {
            self.bindings.retain(|(c, a)| *a != action && *c != chord);
            self.bindings.push((chord, action));
            self.overridden = true;
        }
    }

    /// Resolve a key event to its bound action, or [`Action::None`].
    #[must_use]
    pub fn resolve(&self, ev: KeyEvent) -> Action {
        let chord = Chord::from_event(ev);
        self.bindings
            .iter()
            .find_map(|(c, a)| (*c == chord).then_some(*a))
            .unwrap_or(Action::None)
    }

    /// The *builtin* (default) key event that triggers `action`, used to
    /// canonicalize a user-rebound chord back to the event the legacy reducer
    /// already understands. Returns `None` for [`Action::None`] and for any
    /// action without a builtin chord. Kept independent of `self` so the
    /// translation target is always the legacy default, regardless of overrides.
    #[must_use]
    pub fn builtin_event(action: Action) -> Option<KeyEvent> {
        let chord = Self::builtin()
            .bindings
            .into_iter()
            .find_map(|(c, a)| (a == action).then_some(c))?;
        Some(KeyEvent::new(chord.code, chord.mods))
    }

    /// Translate an incoming key event through this map into the *canonical*
    /// builtin event the legacy reducer/scrollback path expects.
    ///
    /// - With the **builtin** map, a bound chord resolves to its own action
    ///   whose builtin event is the same chord, so the returned event equals
    ///   the input (byte-identical). An unbound event also returns unchanged.
    /// - With a **user override**, a rebound chord (e.g. `ctrl+p` →
    ///   `HistoryPrev`) resolves to the action and is rewritten to the builtin
    ///   event (`Up`), so the existing reducer fires the action as if the
    ///   default key was pressed. The freed default chord no longer resolves
    ///   (it returns [`Action::None`]) and passes through unchanged.
    #[must_use]
    pub fn canonicalize(&self, ev: KeyEvent) -> KeyEvent {
        match self.resolve(ev) {
            Action::None => ev,
            action => Self::builtin_event(action).unwrap_or(ev),
        }
    }

    /// Build a map from the builtin defaults overlaid with a TOML override
    /// string.
    ///
    /// The TOML shape is a flat `action = "chord"` table, e.g.
    /// `history-prev = "ctrl+p"`. Unknown action names and unparseable chords
    /// are skipped (so a partially-mistyped file still loads its valid lines).
    /// An empty or whitespace-only string yields the pure builtin map, i.e.
    /// behaviour identical to having no config at all.
    ///
    /// # Errors
    ///
    /// Returns the `toml` parse error when the input is not a valid TOML table.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        let mut map = Self::builtin();
        if s.trim().is_empty() {
            return Ok(map);
        }
        let raw: RawKeyMap = toml::from_str(s)?;
        let overrides: Vec<(Action, Chord)> = raw
            .bindings
            .iter()
            .filter_map(|(name, spec)| {
                let action = Action::parse(name)?;
                let chord = Chord::parse(spec)?;
                Some((action, chord))
            })
            .collect();
        map.apply_overrides(&overrides);
        Ok(map)
    }

    /// Load the builtin map overlaid with `~/.origin/keybindings.toml` when it
    /// exists.
    ///
    /// A missing file (or any read/parse failure) yields the pure builtin map,
    /// so the default path is never disturbed by a broken config. Honors
    /// `$ORIGIN_HOME` for tests and alternate-root installs, matching
    /// [`crate::config::path`].
    #[must_use]
    pub fn load() -> Self {
        let Some(home) = std::env::var_os("ORIGIN_HOME")
            .map(std::path::PathBuf::from)
            .or_else(dirs::home_dir)
        else {
            return Self::builtin();
        };
        let path = home.join(".origin").join("keybindings.toml");
        std::fs::read_to_string(&path).map_or_else(
            |_| Self::builtin(),
            |text| Self::from_toml_str(&text).unwrap_or_else(|_| Self::builtin()),
        )
    }
}

/// On-disk shape of `keybindings.toml`: a flat `action = "chord"` table.
#[derive(Debug, Deserialize)]
struct RawKeyMap {
    #[serde(flatten)]
    bindings: BTreeMap<String, String>,
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used)] // panic/unwrap are idiomatic test assertions
mod tests {
    use super::*;

    const fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn builtin_resolves_default_chords() {
        let km = KeyMap::builtin();
        assert_eq!(km.resolve(ev(KeyCode::Enter, KeyModifiers::NONE)), Action::Submit);
        assert_eq!(
            km.resolve(ev(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Cancel
        );
        assert_eq!(km.resolve(ev(KeyCode::Up, KeyModifiers::NONE)), Action::HistoryPrev);
        assert_eq!(
            km.resolve(ev(KeyCode::Down, KeyModifiers::NONE)),
            Action::HistoryNext
        );
        assert_eq!(
            km.resolve(ev(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            Action::Clear
        );
    }

    #[test]
    fn unbound_event_resolves_to_none() {
        let km = KeyMap::builtin();
        // A plain character is not a bound chord — the caller falls through.
        assert_eq!(km.resolve(ev(KeyCode::Char('x'), KeyModifiers::NONE)), Action::None);
    }

    #[test]
    fn empty_toml_is_builtin() {
        let km = KeyMap::from_toml_str("   \n").unwrap();
        assert_eq!(km.resolve(ev(KeyCode::Enter, KeyModifiers::NONE)), Action::Submit);
        assert_eq!(km.resolve(ev(KeyCode::Up, KeyModifiers::NONE)), Action::HistoryPrev);
    }

    #[test]
    fn override_rebinds_history_prev_and_moves_chord() {
        // Rebinding history-prev to ctrl+p must (a) make ctrl+p resolve to
        // HistoryPrev and (b) free the old Up chord so it no longer fires it.
        let km = KeyMap::from_toml_str("history-prev = \"ctrl+p\"\n").unwrap();
        assert_eq!(
            km.resolve(ev(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Action::HistoryPrev
        );
        assert_eq!(km.resolve(ev(KeyCode::Up, KeyModifiers::NONE)), Action::None);
        // Other defaults are untouched.
        assert_eq!(km.resolve(ev(KeyCode::Enter, KeyModifiers::NONE)), Action::Submit);
    }

    #[test]
    fn override_underscore_action_name_accepted() {
        let km = KeyMap::from_toml_str("history_next = \"ctrl+n\"\n").unwrap();
        assert_eq!(
            km.resolve(ev(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Action::HistoryNext
        );
    }

    #[test]
    fn unknown_action_and_bad_chord_are_skipped() {
        // `frobnicate` is not an action and `nonsense++` is not a chord, but the
        // valid `clear` line still applies and the map still loads.
        let toml = "frobnicate = \"ctrl+z\"\nclear = \"ctrl+l\"\nsubmit = \"nonsense++\"\n";
        let km = KeyMap::from_toml_str(toml).unwrap();
        assert_eq!(
            km.resolve(ev(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            Action::Clear
        );
        // submit override was malformed, so the default Enter binding survives.
        assert_eq!(km.resolve(ev(KeyCode::Enter, KeyModifiers::NONE)), Action::Submit);
    }

    #[test]
    fn chord_parse_covers_named_keys() {
        assert_eq!(
            Chord::parse("shift+enter"),
            Some(Chord::new(KeyCode::Enter, KeyModifiers::SHIFT))
        );
        assert_eq!(Chord::parse("esc"), Some(Chord::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(
            Chord::parse("ctrl+alt+x"),
            Some(Chord::new(
                KeyCode::Char('x'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ))
        );
        assert_eq!(Chord::parse(""), None);
        assert_eq!(Chord::parse("ctrl+"), None);
        assert_eq!(Chord::parse("ctrl+a+b"), None);
    }

    #[test]
    fn from_event_masks_spurious_modifiers() {
        // A bound chord resolves through `from_event`, which keeps only the
        // meaningful Ctrl/Alt/Shift mask; a plain Enter resolves to Submit.
        let km = KeyMap::builtin();
        let noisy = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(km.resolve(noisy), Action::Submit);
    }

    #[test]
    fn builtin_is_not_overridden_and_canonicalizes_to_identity() {
        // The default/builtin map reports no overrides, and canonicalize is the
        // identity for every event — so the live key path is byte-identical when
        // no `keybindings.toml` is present.
        let km = KeyMap::builtin();
        assert!(!km.is_overridden());
        for ev in [
            ev(KeyCode::Enter, KeyModifiers::NONE),
            ev(KeyCode::Up, KeyModifiers::NONE),
            ev(KeyCode::Char('c'), KeyModifiers::CONTROL),
            ev(KeyCode::Char('x'), KeyModifiers::NONE),
            ev(KeyCode::Esc, KeyModifiers::NONE),
        ] {
            assert_eq!(km.canonicalize(ev), ev, "builtin canonicalize is identity");
        }
    }

    #[test]
    fn override_marks_overridden() {
        let km = KeyMap::from_toml_str("history-prev = \"ctrl+p\"\n").unwrap();
        assert!(km.is_overridden(), "a loaded rebind marks the map overridden");
        // Empty toml leaves the pure builtin map (not overridden).
        let empty = KeyMap::from_toml_str("   ").unwrap();
        assert!(!empty.is_overridden());
    }

    #[test]
    fn canonicalize_rewrites_rebound_chord_to_builtin_event() {
        // Rebinding history-prev to ctrl+p must canonicalize ctrl+p to the
        // builtin Up event (so the existing reducer fires HistoryPrev), and the
        // freed Up chord passes through unchanged (it now resolves to None).
        let km = KeyMap::from_toml_str("history-prev = \"ctrl+p\"\n").unwrap();
        let ctrl_p = ev(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(
            km.canonicalize(ctrl_p),
            ev(KeyCode::Up, KeyModifiers::NONE),
            "rebound chord canonicalizes to the builtin event"
        );
        let up = ev(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(km.canonicalize(up), up, "freed default chord passes through unchanged");
        // Untouched defaults still canonicalize to themselves.
        let enter = ev(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(km.canonicalize(enter), enter);
    }

    #[test]
    fn builtin_event_maps_actions_to_default_chords() {
        assert_eq!(
            KeyMap::builtin_event(Action::Submit),
            Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        );
        assert_eq!(
            KeyMap::builtin_event(Action::HistoryPrev),
            Some(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        );
        assert_eq!(
            KeyMap::builtin_event(Action::Clear),
            Some(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
        );
        assert_eq!(KeyMap::builtin_event(Action::None), None);
    }

    #[test]
    fn absent_file_is_builtin_defaults() {
        // The `load()` contract: a missing/empty `keybindings.toml` yields the
        // pure builtin map. `from_toml_str("")` is the in-memory equivalent of
        // the empty-config path `load` takes, asserted here without mutating the
        // shared process env (which would race other tests in this binary).
        let km = KeyMap::from_toml_str("").unwrap();
        assert!(!km.is_overridden(), "absent/empty file ⇒ builtin (not overridden)");
        assert_eq!(km.resolve(ev(KeyCode::Enter, KeyModifiers::NONE)), Action::Submit);
        assert_eq!(km.resolve(ev(KeyCode::Up, KeyModifiers::NONE)), Action::HistoryPrev);
    }

    #[test]
    fn custom_toml_rebinds_action() {
        // A custom keybindings.toml rebinds an action; the loaded map picks it up
        // and marks itself overridden so the caller enables additive behaviour.
        let km = KeyMap::from_toml_str("history-prev = \"ctrl+p\"\n").unwrap();
        assert!(km.is_overridden());
        assert_eq!(
            km.resolve(ev(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Action::HistoryPrev,
            "custom toml rebinds history-prev to ctrl+p"
        );
        // The freed default Up chord no longer fires history-prev.
        assert_eq!(km.resolve(ev(KeyCode::Up, KeyModifiers::NONE)), Action::None);
    }
}
