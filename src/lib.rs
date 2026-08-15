//! WinKit: a local-first MCP observability and diagnostics platform for
//! Windows.
//!
//! The crate is split into layers so the MCP surface never touches Win32
//! directly:
//!
//! ```text
//! server (MCP protocol, stdio transport, tool registry)
//!   ├── tools (tool definitions + argument handling)
//!   │     ├── providers (WindowsBackend / ApplicationProvider traits)
//!   │     └── platform::windows (real Win32 implementations)
//!   ├── permissions, config, models, diagnostics
//! ```
//!
//! The library target exists so the server, tools, and providers can be
//! exercised by unit and integration tests without launching the binary.

pub mod cli;
pub mod config;
pub mod diagnostics;
pub mod errors;
pub mod models;
pub mod permissions;
pub mod platform;
pub mod providers;
pub mod server;
pub mod tools;
pub mod utils;
