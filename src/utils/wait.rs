//! Bounded, condition-based polling for the wait tools (`wait_for_port`,
//! `wait_for_http`, `wait_for_process`). Waits never busy-loop and never run
//! forever: every wait has an absolute deadline, a clamped interval, and a
//! bounded number of attempts.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};

/// Polling bounds derived from caller input and configuration.
#[derive(Debug, Clone, Copy)]
pub struct WaitConfig {
    /// Absolute deadline for the whole wait (ms).
    pub timeout_ms: u64,
    /// Pause between attempts (ms).
    pub interval_ms: u64,
}

impl WaitConfig {
    /// Clamp caller-requested bounds into safe ranges. `max_timeout_ms` is
    /// the configured operation timeout, so a caller can never wait longer
    /// than the server's own absolute deadline.
    pub fn bounded(timeout_ms: Option<u64>, interval_ms: Option<u64>, max_timeout_ms: u64) -> Self {
        Self {
            timeout_ms: timeout_ms
                .unwrap_or(10_000)
                .clamp(100, max_timeout_ms.max(100)),
            interval_ms: interval_ms.unwrap_or(250).clamp(50, 5_000),
        }
    }
}

/// Result of a bounded wait.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitOutcome {
    /// `true` when the condition became true before the deadline.
    pub completed: bool,
    /// Number of condition evaluations performed.
    pub attempts: usize,
    pub elapsed_ms: u64,
    /// Observation from the final evaluation (bounded; `null` when the
    /// check produced nothing).
    pub last_observation: Value,
}

/// Poll `check` until it returns `(true, observation)`, the deadline passes,
/// or the attempt cap is reached. `check` must be cheap or internally
/// bounded — the wait helper never adds its own sleeps to a long-running
/// probe.
pub async fn wait_for_condition<F, Fut>(mut check: F, cfg: WaitConfig) -> WaitOutcome
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = (bool, Value)> + Send,
{
    let started = Instant::now();
    let deadline = started + Duration::from_millis(cfg.timeout_ms.max(1));
    let interval = Duration::from_millis(cfg.interval_ms.max(50));
    let max_attempts = (cfg.timeout_ms.max(1) / cfg.interval_ms.max(50) + 2) as usize;
    let mut attempts = 0usize;
    let mut last_observation: Value;

    loop {
        attempts += 1;
        let (ready, observation) = check().await;
        last_observation = observation;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if ready {
            return WaitOutcome {
                completed: true,
                attempts,
                elapsed_ms,
                last_observation,
            };
        }
        if attempts >= max_attempts || Instant::now() >= deadline {
            return WaitOutcome {
                completed: false,
                attempts,
                elapsed_ms,
                last_observation,
            };
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_clamps_ranges() {
        let cfg = WaitConfig::bounded(Some(30_000), Some(1), 10_000);
        assert_eq!(cfg.timeout_ms, 10_000);
        assert_eq!(cfg.interval_ms, 50);
        let cfg2 = WaitConfig::bounded(None, None, 5_000);
        assert_eq!(cfg2.timeout_ms, 5_000);
        assert_eq!(cfg2.interval_ms, 250);
    }

    #[tokio::test]
    async fn completes_when_condition_becomes_true() {
        let cfg = WaitConfig {
            timeout_ms: 2_000,
            interval_ms: 20,
        };
        let mut count = 0;
        let outcome = wait_for_condition(
            move || {
                count += 1;
                async move {
                    let ready = count >= 3;
                    (ready, serde_json::json!({ "attempt": count }))
                }
            },
            cfg,
        )
        .await;
        assert!(outcome.completed);
        assert_eq!(outcome.attempts, 3);
        assert_eq!(outcome.last_observation["attempt"], 3);
    }

    #[tokio::test]
    async fn times_out_without_looping_forever() {
        let cfg = WaitConfig {
            timeout_ms: 150,
            interval_ms: 50,
        };
        let outcome =
            wait_for_condition(|| async move { (false, serde_json::Value::Null) }, cfg).await;
        assert!(!outcome.completed);
        assert!(outcome.attempts >= 2);
        assert!(outcome.elapsed_ms < 2_000);
    }
}
