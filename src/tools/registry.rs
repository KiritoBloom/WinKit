//! Registry diagnostics: allowlist-only reads.

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{clamp_limit, optional_bool, optional_usize, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

const MAX_SOFTWARE: usize = 200;

pub async fn registry_diagnostics_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let include_software = optional_bool(&args, "include_software").unwrap_or(true);
    let max_software = clamp_limit(optional_usize(&args, "max_software"), MAX_SOFTWARE);
    let diag = state
        .windows
        .registry_diagnostics(include_software, max_software)?;
    Ok(json!(diag))
}

pub fn registry_diagnostics_definition() -> ToolDefinition {
    ToolDefinition {
        name: "registry_diagnostics",
        description: "Read-only registry diagnostics from a fixed allowlist of keys: OS identity (Windows NT\\CurrentVersion), startup programs (Run/RunOnce under HKLM and HKCU with enabled/disabled state), and installed software (Uninstall keys). Arbitrary keys are never read.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "include_software": { "type": "boolean", "description": "Include the installed-software enumeration (default true)." },
                "max_software": { "type": "integer", "minimum": 1, "description": "Cap on installed-software entries (default 200)." }
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::RegistryRead),
        timeout_ms: None,
        handler: wrap(registry_diagnostics_handler),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::providers::mock::MockWindowsBackend;
    use crate::providers::windows::WindowsBackend;
    use crate::server::AppState;
    use serde_json::json;
    use std::sync::Arc;

    fn state() -> Arc<AppState> {
        let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
        let mut config = Config::default();
        config.permissions.mode = "read_only".to_string();
        config.providers.enabled = vec!["windows".to_string()];
        AppState::with_backend(config, backend).unwrap()
    }

    #[tokio::test]
    async fn registry_diagnostics_returns_fixture_view() {
        let out = registry_diagnostics_handler(state(), json!({}))
            .await
            .unwrap();
        assert_eq!(out["system_identity"]["product_name"], "Windows 11 Pro");
        assert_eq!(out["counts"]["startup_programs"], 3);
        assert_eq!(out["counts"]["installed_software"], 2);
        let startup = out["startup_programs"].as_array().unwrap();
        assert!(startup
            .iter()
            .any(|s| s["name"] == "OneDrive" && s["enabled"] == true));
        assert!(startup
            .iter()
            .any(|s| s["name"] == "OldTool" && s["enabled"] == false));
        // Enrichment fields flow through registry_diagnostics too.
        assert!(startup.iter().any(|s| s["hidden"] == true
            && s["entry_type"] == "run_once"
            && s["impact"].is_string()));
    }

    #[tokio::test]
    async fn registry_diagnostics_skips_software_when_requested() {
        let out = registry_diagnostics_handler(state(), json!({ "include_software": false }))
            .await
            .unwrap();
        assert!(out["installed_software"].as_array().unwrap().is_empty());
        assert_eq!(out["counts"]["installed_software"], 0);
    }

    #[tokio::test]
    async fn registry_diagnostics_caps_software() {
        let out = registry_diagnostics_handler(state(), json!({ "max_software": 1 }))
            .await
            .unwrap();
        assert_eq!(out["installed_software"].as_array().unwrap().len(), 1);
        assert_eq!(out["counts"]["installed_software"], 1);
    }
}
