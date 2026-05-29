// SPDX-License-Identifier: Apache-2.0
//! Parse the argument string passed alongside `/goal`.
//!
//! Grammar: `(--key=value )* <condition...>`. The first token that doesn't
//! start with `--` begins the condition; everything from there to end-of-line
//! is the condition verbatim (so a condition like `rewrite the --foo flag`
//! is preserved).

use thiserror::Error;

const MAX_CONDITION_LEN: usize = 4_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalArgs {
    pub condition: String,
    pub max_iter: Option<u32>,
    pub token_budget: Option<u64>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FlagParseError {
    #[error("unknown flag: {0}")]
    UnknownFlag(String),
    #[error("invalid value for {flag}: {value}")]
    InvalidValue { flag: String, value: String },
    #[error("--{0} requires =<value>")]
    MissingValue(String),
    #[error("goal condition is empty")]
    EmptyCondition,
    #[error("goal condition exceeds 4000 characters")]
    ConditionTooLong,
    #[error("duplicate flag: --{0}")]
    DuplicateFlag(String),
}

/// Parse the args portion of `/goal <args>`. The leading `/goal` token must
/// already be stripped by the caller.
///
/// # Errors
/// See [`FlagParseError`] variants.
pub fn parse_goal_args(raw: &str) -> Result<GoalArgs, FlagParseError> {
    let mut max_iter: Option<u32> = None;
    let mut token_budget: Option<u64> = None;

    let trimmed = raw.trim_start();
    let mut rest = trimmed;

    loop {
        let stripped = rest.trim_start();
        if !stripped.starts_with("--") {
            rest = stripped;
            break;
        }
        // Find end of this flag token (first whitespace).
        let token_end = stripped.find(char::is_whitespace).unwrap_or(stripped.len());
        let token = &stripped[..token_end];
        let after = &stripped[token_end..];
        let inner = &token[2..]; // strip leading "--"
        let (key, value) = inner
            .split_once('=')
            .ok_or_else(|| FlagParseError::MissingValue(inner.to_string()))?;
        match key {
            "max-iter" => {
                if max_iter.is_some() {
                    return Err(FlagParseError::DuplicateFlag(key.to_string()));
                }
                let parsed: u32 = value.parse().map_err(|_| FlagParseError::InvalidValue {
                    flag: key.to_string(),
                    value: value.to_string(),
                })?;
                if parsed == 0 {
                    return Err(FlagParseError::InvalidValue {
                        flag: key.to_string(),
                        value: value.to_string(),
                    });
                }
                max_iter = Some(parsed);
            }
            "budget" => {
                if token_budget.is_some() {
                    return Err(FlagParseError::DuplicateFlag(key.to_string()));
                }
                let parsed = parse_budget(value).ok_or_else(|| FlagParseError::InvalidValue {
                    flag: key.to_string(),
                    value: value.to_string(),
                })?;
                if parsed == 0 {
                    return Err(FlagParseError::InvalidValue {
                        flag: key.to_string(),
                        value: value.to_string(),
                    });
                }
                token_budget = Some(parsed);
            }
            other => return Err(FlagParseError::UnknownFlag(other.to_string())),
        }
        rest = after;
    }

    let condition = rest.trim().to_string();
    if condition.is_empty() {
        return Err(FlagParseError::EmptyCondition);
    }
    if condition.len() > MAX_CONDITION_LEN {
        return Err(FlagParseError::ConditionTooLong);
    }
    Ok(GoalArgs { condition, max_iter, token_budget })
}

fn parse_budget(s: &str) -> Option<u64> {
    // Split on the trailing *char* (not byte) so we never slice mid-codepoint.
    // In valid UTF-8 the original byte-level check happened to be safe
    // (continuation bytes are 0x80..=0xBF, never collide with ASCII k/K/m/M)
    // but the char-level form is obviously correct and surfaces non-ASCII
    // suffixes as a clean None → InvalidValue at the caller.
    let last_char = s.chars().last()?;
    let (num_part, mult): (&str, u64) = match last_char {
        'k' | 'K' => (&s[..s.len() - last_char.len_utf8()], 1_000),
        'm' | 'M' => (&s[..s.len() - last_char.len_utf8()], 1_000_000),
        _ => (s, 1),
    };
    let n: u64 = num_part.parse().ok()?;
    n.checked_mul(mult)
}
