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
                max_iter = Some(value.parse::<u32>().map_err(|_| FlagParseError::InvalidValue {
                    flag: key.to_string(),
                    value: value.to_string(),
                })?);
            }
            "budget" => {
                token_budget = Some(parse_budget(value).ok_or(FlagParseError::InvalidValue {
                    flag: key.to_string(),
                    value: value.to_string(),
                })?);
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
    let (num_part, mult) = match s.as_bytes().last()? {
        b'k' | b'K' => (&s[..s.len() - 1], 1_000u64),
        b'm' | b'M' => (&s[..s.len() - 1], 1_000_000u64),
        _ => (s, 1u64),
    };
    let n: u64 = num_part.parse().ok()?;
    n.checked_mul(mult)
}
