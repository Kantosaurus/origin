// SPDX-License-Identifier: Apache-2.0
//! Parse the YAML frontmatter at the head of a `SKILL.md`.

use serde::Deserialize;
use thiserror::Error;

/// Required + optional frontmatter fields shipped at P10.1.
#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
#[derive(Debug, Clone, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default, rename = "allowed-tools")]
    pub allowed_tools: Vec<String>,
}

/// A parsed `SKILL.md` split into the frontmatter struct and the body string.
#[derive(Debug, Clone)]
pub struct ParsedSkill {
    pub front: SkillFrontmatter,
    pub body: String,
}

#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
#[derive(Debug, Error)]
pub enum FrontmatterError {
    #[error("frontmatter missing opening `---` delimiter")]
    MissingOpen,
    #[error("frontmatter missing closing `---` delimiter")]
    MissingDelimiter,
    #[error("yaml: {0}")]
    Yaml(String),
}

/// Split `source` into a frontmatter block + body, then deserialize the block.
///
/// Accepts either Unix (`\n`) or Windows (`\r\n`) line endings and strips an
/// optional leading UTF-8 BOM, so files checked out by `git config
/// core.autocrlf=true` or saved by BOM-emitting editors round-trip cleanly.
/// The returned `body` is normalized to LF.
///
/// # Errors
/// Returns [`FrontmatterError`] for missing delimiters or invalid YAML.
#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
pub fn parse_frontmatter(source: &str) -> Result<ParsedSkill, FrontmatterError> {
    let stripped = source.strip_prefix('\u{FEFF}').unwrap_or(source);
    // Lazy allocation: only normalize when CRLF is actually present.
    let normalized;
    let source: &str = if stripped.contains('\r') {
        normalized = stripped.replace("\r\n", "\n");
        &normalized
    } else {
        stripped
    };
    let rest = source
        .strip_prefix("---\n")
        .ok_or(FrontmatterError::MissingOpen)?;
    let (yaml, body) = rest
        .split_once("\n---\n")
        .ok_or(FrontmatterError::MissingDelimiter)?;
    let front: SkillFrontmatter =
        serde_yaml::from_str(yaml).map_err(|e| FrontmatterError::Yaml(e.to_string()))?;
    Ok(ParsedSkill {
        front,
        body: body.to_string(),
    })
}
