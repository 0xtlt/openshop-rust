//! Typed errors of the engine. They travel inside `anyhow::Error` and are
//! recovered with `downcast_ref`.

use chrono::{DateTime, Utc};

use crate::runtime::time::to_iso;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlowCanceledError;

impl std::fmt::Display for FlowCanceledError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Flow was canceled")
    }
}

impl std::error::Error for FlowCanceledError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepTimeoutError {
    pub step_name: String,
    pub timeout_ms: u64,
}

impl std::fmt::Display for StepTimeoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Step \"{}\" timed out after {}ms", self.step_name, self.timeout_ms)
    }
}

impl std::error::Error for StepTimeoutError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowTimeoutError {
    pub flow_name: String,
    pub timeout_ms: u64,
}

impl std::fmt::Display for FlowTimeoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Flow \"{}\" timed out after {}ms", self.flow_name, self.timeout_ms)
    }
}

impl std::error::Error for FlowTimeoutError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowConcurrencyError {
    pub flow_name: String,
    pub shop: String,
    pub existing_run_id: String,
}

impl std::fmt::Display for FlowConcurrencyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Flow \"{}\" is already running for shop \"{}\" (run #{})",
            self.flow_name, self.shop, self.existing_run_id
        )
    }
}

impl std::error::Error for FlowConcurrencyError {}

/// A run was asked for a flow that is not registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownFlowError {
    pub flow_name: String,
    pub available: Vec<String>,
}

impl std::fmt::Display for UnknownFlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Flow \"{}\" not found. Available: {}",
            self.flow_name,
            self.available.join(", ")
        )
    }
}

impl std::error::Error for UnknownFlowError {}

/// The input of a run fails the input schema of its flow. A dispatch refuses
/// it before a run is queued; a run that holds it anyway fails with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidFlowInput {
    pub flow_name: String,
    pub summary: String,
}

impl std::fmt::Display for InvalidFlowInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid input for flow \"{}\": {}", self.flow_name, self.summary)
    }
}

impl std::error::Error for InvalidFlowInput {}

/// Raised by `step.sleep()` to suspend the run until `resume_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SleepSignal {
    pub resume_at: DateTime<Utc>,
}

impl std::fmt::Display for SleepSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Flow sleeping until {}", to_iso(&self.resume_at))
    }
}

impl std::error::Error for SleepSignal {}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn flow_canceled_error_has_a_fixed_message() {
        assert_eq!(FlowCanceledError.to_string(), "Flow was canceled");
    }

    #[test]
    fn step_timeout_error_exposes_step_and_timeout() {
        let error = StepTimeoutError {
            step_name: "fetch".into(),
            timeout_ms: 5_000,
        };
        assert_eq!(error.to_string(), "Step \"fetch\" timed out after 5000ms");
    }

    #[test]
    fn flow_timeout_error_exposes_flow_and_timeout() {
        let error = FlowTimeoutError {
            flow_name: "sync".into(),
            timeout_ms: 60_000,
        };
        assert_eq!(error.to_string(), "Flow \"sync\" timed out after 60000ms");
    }

    #[test]
    fn flow_concurrency_error_mentions_the_existing_run() {
        let error = FlowConcurrencyError {
            flow_name: "sync".into(),
            shop: "a.myshopify.com".into(),
            existing_run_id: "42".into(),
        };
        assert_eq!(
            error.to_string(),
            "Flow \"sync\" is already running for shop \"a.myshopify.com\" (run #42)"
        );
    }

    #[test]
    fn sleep_signal_formats_the_resume_date() {
        let resume_at = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(
            SleepSignal { resume_at }.to_string(),
            "Flow sleeping until 2026-01-02T03:04:05.000Z"
        );
    }

    #[test]
    fn typed_errors_survive_anyhow_context() {
        use anyhow::Context;
        let result: anyhow::Result<()> = Err(FlowCanceledError.into());
        let error = result.context("while syncing").unwrap_err();
        assert!(error.downcast_ref::<FlowCanceledError>().is_some());
    }
}
