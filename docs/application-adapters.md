# Application Adapters

Application adapters give WinKit deep, structured visibility into user-facing
applications. The first adapter is Chrome (deep tab inspection over CDP).
This document describes the adapter contract and how to add a new one.

## The contract

An adapter implements the `ApplicationProvider` trait
(`src/providers/applications/mod.rs`). Every method returns a boxed future so
implementors can drive WebSocket clients (Chrome's CDP) or other async
transport.

```rust
pub trait ApplicationProvider: Send + Sync {
    fn id(&self) -> &'static str;          // e.g. "chrome"
    fn display_name(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn capabilities(&self) -> Vec<Capability>;
    fn state(&self) -> ApplicationState;   // cheap, re-evaluated availability
    fn info(&self) -> BoxFuture<Result<ApplicationInfo, WinkitError>>;

    // Capability dispatch - default implementations return
    // `UnsupportedCapability`; override what you implement:
    fn list_tabs(&self) -> BoxFuture<Result<Vec<TabInfo>, WinkitError>>;
    fn get_tab(&self, tab_id: &str) -> BoxFuture<Result<TabInfo, WinkitError>>;
    fn get_active_tab(&self) -> BoxFuture<Result<TabInfo, WinkitError>>;
    fn tab_performance(&self, tab_id: &str) -> BoxFuture<Result<PerformanceMetrics, WinkitError>>;
    fn tab_memory(&self, tab_id: &str) -> BoxFuture<Result<MemoryInfo, WinkitError>>;
    fn tab_network(&self, tab_id: &str) -> BoxFuture<Result<NetworkSummary, WinkitError>>;
    fn tab_runtime(&self, tab_id: &str) -> BoxFuture<Result<RuntimeInfo, WinkitError>>;
    fn tab_diagnostics(&self, tab_id: &str, windows: &dyn WindowsBackend)
        -> BoxFuture<Result<TabDiagnostics, WinkitError>>;
    fn browser_info(&self) -> BoxFuture<Result<BrowserInfo, WinkitError>>;
}
```

Key design rules:

- **Honest defaults.** If an adapter doesn't implement a method, it inherits
  the default that returns `UnsupportedCapability`. The adapter's
  `capabilities()` must report only what it actually implements, and
  `state()` must report availability truthfully.
- **Availability is a lifecycle, not a boolean.** Chrome distinguishes
  `not_installed`, `installed`, `running`, `endpoint_unavailable`,
  `endpoint_available`, and `connected`. `list_applications`/`get_application`
  surface this so agents never assume a browser is inspectable.
- **Cross-layer diagnostics get the Windows backend.** `tab_diagnostics`
  receives `&dyn WindowsBackend` so an adapter can correlate application
  state with OS-level evidence (e.g. Chrome process memory) - always through
  the trait, never Win32 directly.

## Registration

Adapters register in two registries built by `AppState::build`
(`src/server/mod.rs`):

```rust
providers.register(&chrome);      // ProviderRegistry - metadata for system_info
applications.register(chrome);    // ApplicationRegistry - capability-bearing adapter
```

Provider activation is config-driven: `[providers] enabled = ["chrome"]`.
An empty list means "all built-in providers". Tools that require an adapter
look it up through `state.applications.get(id)` and return a
`provider_unavailable` error when the adapter is not enabled.

## Adding a new adapter (e.g. Edge)

1. Create `src/providers/applications/<name>/` with the adapter
   implementation and its transport.
2. Implement `ApplicationProvider`; inherit the defaults for anything not yet
   supported and keep `capabilities()` honest.
3. Implement `Provider` via the blanket impl for
   `Box<dyn ApplicationProvider>` (or adapt as needed for a standalone
   provider).
4. Register it in `AppState::build` and add its id to the default
   `providers.enabled` list.
5. Add adapter-specific tools in `src/tools/` (or reuse the generic
   `list_applications`/`get_application`).
6. Add fixture-backed tests in `tests/`; keep the mock backend covering the
   new adapter's contract.

## Tool mapping

Generic application tools (`list_applications`, `get_application`) work for
every adapter automatically. Adapter-specific tools (the `chrome_*` family)
dispatch to the adapter by id. The permission system gates each tool by the
capability the underlying adapter method maps to, so even an adapter that
implements everything is constrained by the configured mode.
