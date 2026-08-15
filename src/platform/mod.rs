//! Platform abstraction layer.
//!
//! WinKit targets Windows. The `windows` module holds the real Win32
//! implementations; everything above it (providers, tools, server) depends
//! only on the `WindowsBackend` trait so it can be mocked in tests.

pub mod windows;
