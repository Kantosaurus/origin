// SPDX-License-Identifier: Apache-2.0
//! Ponytail intensity modes and the `/ponytail` command grammar.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PonytailMode {
    Off,
    Lite,
    #[default]
    Full,
    Ultra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PonytailCmd {
    /// `/ponytail` with no argument — report the current mode.
    Report,
    /// `/ponytail <level>` with a valid level.
    Set(PonytailMode),
    /// `/ponytail <garbage>` — surface a usage error.
    Usage,
}

impl PonytailMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
        }
    }

    #[must_use]
    pub fn parse_level(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "lite" => Some(Self::Lite),
            "full" => Some(Self::Full),
            "ultra" => Some(Self::Ultra),
            _ => None,
        }
    }
}

/// Parse a `/ponytail [level]` line. `None` ⇒ not a ponytail command (fall through).
#[must_use]
pub fn parse_ponytail_command(line: &str) -> Option<PonytailCmd> {
    let rest = line.trim().strip_prefix("/ponytail")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None; // `/ponytailfoo`
    }
    let arg = rest.trim();
    if arg.is_empty() {
        return Some(PonytailCmd::Report);
    }
    Some(PonytailMode::parse_level(arg).map_or(PonytailCmd::Usage, PonytailCmd::Set))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_round_trip() {
        for m in [PonytailMode::Off, PonytailMode::Lite, PonytailMode::Full, PonytailMode::Ultra] {
            assert_eq!(PonytailMode::parse_level(m.as_str()), Some(m));
        }
    }

    #[test]
    fn command_grammar() {
        assert_eq!(parse_ponytail_command("/ponytail"), Some(PonytailCmd::Report));
        assert_eq!(parse_ponytail_command("/ponytail ultra"), Some(PonytailCmd::Set(PonytailMode::Ultra)));
        assert_eq!(parse_ponytail_command("/ponytail OFF"), Some(PonytailCmd::Set(PonytailMode::Off)));
        assert_eq!(parse_ponytail_command("/ponytail bogus"), Some(PonytailCmd::Usage));
        assert_eq!(parse_ponytail_command("/ponytailfoo"), None);
        assert_eq!(parse_ponytail_command("hello"), None);
    }

    #[test]
    fn default_is_full() {
        assert_eq!(PonytailMode::default(), PonytailMode::Full);
    }
}
