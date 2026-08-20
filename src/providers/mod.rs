//! Provider architecture.
//!
//! Providers are the capability-bearing backends of WinKit. A provider
//! declares its identity and capabilities; the MCP tool layer dispatches
//! into providers behind the [`WindowsBackend`] / application-provider
//! abstractions so everything is mockable.

pub mod applications;
pub mod mock;
pub mod windows;

use crate::permissions::Capability;
use serde::Serialize;
use std::collections::HashMap;

/// Static availability of a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAvailability {
    /// Provider is compiled in and always usable.
    Ready,
    /// Provider is compiled in but requires the host to be in a certain
    /// state (e.g. an application running with inspection enabled).
    Conditional,
}

/// Metadata about a provider, surfaced to MCP clients.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub availability: ProviderAvailability,
    pub capabilities: Vec<String>,
}

/// Every provider in WinKit implements this trait.
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn availability(&self) -> ProviderAvailability;
    /// Capabilities this provider can satisfy.
    fn capabilities(&self) -> Vec<Capability>;
    /// Metadata for `list_applications`/`system_info`.
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id().to_string(),
            name: self.name().to_string(),
            version: self.version().to_string(),
            availability: self.availability(),
            capabilities: self
                .capabilities()
                .iter()
                .map(|c| c.as_str().to_string())
                .collect(),
        }
    }
}

/// Registry of all active providers.
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, ProviderInfo>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    pub fn register(&mut self, provider: &dyn Provider) {
        self.providers
            .insert(provider.id().to_string(), provider.info());
    }

    pub fn get(&self, id: &str) -> Option<&ProviderInfo> {
        self.providers.get(id)
    }

    pub fn all(&self) -> Vec<&ProviderInfo> {
        let mut v: Vec<&ProviderInfo> = self.providers.values().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.providers.contains_key(id)
    }
}

/// Boxed future alias used by async provider methods.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
