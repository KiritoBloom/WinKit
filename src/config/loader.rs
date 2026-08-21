//! Configuration loading.
//!
//! Resolution order:
//! 1. `--config <path>` command-line flag
//! 2. `WIN_KIT_CONFIG` environment variable
//! 3. `./winkit.toml` or `./config/winkit.toml` in the working directory
//! 4. Built-in defaults (no file needed)
//!
//! A file that exists but cannot be parsed is a hard error; a file that
//! does not exist is simply skipped.

use crate::config::Config;
use crate::errors::{ErrorKind, WinkitError};
use std::path::{Path, PathBuf};

/// Try to load configuration from `explicit` (from the CLI), then the
/// environment, then well-known locations, then defaults.
pub fn load(explicit: Option<PathBuf>) -> Result<Config, WinkitError> {
    if let Some(path) = explicit {
        return load_from_file(&path);
    }
    if let Some(path) = std::env::var_os("WIN_KIT_CONFIG") {
        return load_from_file(Path::new(&path));
    }
    for candidate in [Path::new("winkit.toml"), Path::new("config/winkit.toml")] {
        if candidate.exists() {
            return load_from_file(candidate);
        }
    }
    Ok(Config::default())
}

fn load_from_file(path: &Path) -> Result<Config, WinkitError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        WinkitError::new(
            ErrorKind::InvalidArgument,
            format!("cannot read config {path:?}: {e}"),
        )
    })?;
    toml::from_str(&text).map_err(|e| {
        WinkitError::new(
            ErrorKind::InvalidArgument,
            format!("invalid config {path:?}: {e}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_example() {
        let text = r#"
[server]
log_level = "debug"

[permissions]
mode = "read_only"

[providers]
enabled = ["windows"]

[tools]
disabled = []

[limits]
max_processes = 100
max_events = 50

[diagnostics]
high_cpu_percent = 40.0
"#;
        let c: Config = toml::from_str(text).unwrap();
        assert_eq!(c.server.log_level, "debug");
        assert_eq!(c.limits.max_processes, 100);
        assert_eq!(c.diagnostics.high_cpu_percent, 40.0);
        // Unspecified fields keep defaults.
        assert_eq!(c.limits.max_events, 50);
        assert_eq!(c.limits.max_services, 500);
    }

    #[test]
    fn missing_file_falls_back_to_defaults() {
        let c = load(Some(PathBuf::from("does-not-exist-winkit.toml")));
        assert!(c.is_err());
        let c = load(None).unwrap();
        assert_eq!(c.permissions.mode, "read_only");
    }
}
