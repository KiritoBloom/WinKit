//! Blocking helper — run CPU/IO-bound work off the async runtime with optional timeout.

use crate::errors::WinkitError;
use std::time::Duration;

/// Run `f` on the blocking thread-pool and await its result.
/// Propagates `WinkitError` directly; join failures become `internal`.
pub async fn run_blocking<T>(
    f: impl FnOnce() -> Result<T, WinkitError> + Send + 'static,
) -> Result<T, WinkitError>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| WinkitError::internal(format!("background task failed: {e}")))?
}

/// Run `f` on the blocking pool with a timeout budget.
/// `budget_ms` is clamped to >=1.  On timeout returns `WinkitError::timeout`.
pub async fn run_blocking_with_timeout<T>(
    budget_ms: u64,
    f: impl FnOnce() -> Result<T, WinkitError> + Send + 'static,
) -> Result<T, WinkitError>
where
    T: Send + 'static,
{
    let budget = Duration::from_millis(budget_ms.max(1));
    let task = tokio::task::spawn_blocking(f);
    tokio::time::timeout(budget, task)
        .await
        .map_err(|_| WinkitError::timeout(format!("operation exceeded the {budget_ms} ms budget")))?
        .map_err(|e| WinkitError::internal(format!("background task failed: {e}")))?
}
