//! User-configured permission rules (P10.12).

#[derive(Debug, Clone)]
pub struct Rule {
    pub tool_name: String,
    pub scope: String,
    pub allow: bool,
}

impl Rule {
    /// Canonical bloom-filter key: `"{tool_name}@{scope}"`.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}@{}", self.tool_name, self.scope)
    }
}
