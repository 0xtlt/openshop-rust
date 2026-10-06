/// Badge shown for a provider connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionStatus {
    pub tone: &'static str,
    pub label: &'static str,
}

pub fn provider_connection_status(configured: bool, last_check_ok: Option<bool>) -> ConnectionStatus {
    let (tone, label) = match (configured, last_check_ok) {
        (false, _) => ("warning", "Not configured"),
        (true, Some(true)) => ("success", "Connected"),
        (true, Some(false)) => ("critical", "Error"),
        (true, None) => ("info", "Configured"),
    };
    ConnectionStatus { tone, label }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(tone: &'static str, label: &'static str) -> ConnectionStatus {
        ConnectionStatus { tone, label }
    }

    #[test]
    fn keeps_an_unsaved_provider_separate_from_a_saved_one_that_has_not_been_checked() {
        assert_eq!(
            provider_connection_status(false, None),
            status("warning", "Not configured")
        );
        assert_eq!(provider_connection_status(true, None), status("info", "Configured"));
    }

    #[test]
    fn uses_the_last_health_check_when_the_provider_is_configured() {
        assert_eq!(
            provider_connection_status(true, Some(true)),
            status("success", "Connected")
        );
        assert_eq!(
            provider_connection_status(true, Some(false)),
            status("critical", "Error")
        );
    }

    #[test]
    fn does_not_report_a_connection_for_missing_required_config() {
        assert_eq!(
            provider_connection_status(false, Some(true)),
            status("warning", "Not configured")
        );
    }
}
