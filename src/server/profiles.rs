//! Progressive tool profiles.
//!
//! A coding agent receives a focused `tools/list` instead of every low-level
//! capability by default. `core` is the safe low-latency essentials,
//! `developer` (the default) adds workspace/server/webapp diagnosis and
//! bounded waiting, `browser` adds the managed-Chrome workflow, and `full`
//! exposes everything. A profile never bypasses permission checks — it only
//! filters which tools are advertised and dispatcheable.

use std::str::FromStr;

/// Logical tool profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolProfile {
    /// Safe, low-latency essentials.
    Core,
    /// Recommended default for coding agents.
    Developer,
    /// Browser discovery + managed-Chrome workflow.
    Browser,
    /// All safe read-only tools plus explicitly enabled managed actions.
    Full,
}

impl ToolProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Developer => "developer",
            Self::Browser => "browser",
            Self::Full => "full",
        }
    }

    /// The default profile for a developer-focused installation.
    pub fn default_profile() -> Self {
        Self::Developer
    }
}

impl FromStr for ToolProfile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "core" => Ok(Self::Core),
            "developer" => Ok(Self::Developer),
            "browser" => Ok(Self::Browser),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "unknown tool profile '{other}' (expected core, developer, browser, or full)"
            )),
        }
    }
}

impl std::fmt::Display for ToolProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names_round_trip() {
        for p in [
            ToolProfile::Core,
            ToolProfile::Developer,
            ToolProfile::Browser,
            ToolProfile::Full,
        ] {
            assert_eq!(p.to_string().parse::<ToolProfile>().unwrap(), p);
            assert_eq!(p.as_str(), p.to_string());
        }
        assert!("nonsense".parse::<ToolProfile>().is_err());
    }

    #[test]
    fn developer_is_the_default() {
        assert_eq!(ToolProfile::default_profile(), ToolProfile::Developer);
    }
}
