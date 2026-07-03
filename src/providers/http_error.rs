use crate::providers::types::ProviderErrorKind;

pub(crate) struct HttpProviderErrorClassifier;

impl HttpProviderErrorClassifier {
    pub(crate) fn classify_status(status: u16) -> ProviderErrorKind {
        match status {
            429 | 500..=599 => ProviderErrorKind::Retryable,
            _ => ProviderErrorKind::Terminal,
        }
    }

    pub(crate) fn classify_transport(err: &ureq::Transport) -> ProviderErrorKind {
        if Self::is_timeout_source(std::error::Error::source(err)) {
            ProviderErrorKind::Timeout
        } else {
            ProviderErrorKind::Retryable
        }
    }

    pub(crate) fn is_timeout_source(source: Option<&(dyn std::error::Error + 'static)>) -> bool {
        source
            .and_then(|e| e.downcast_ref::<std::io::Error>())
            .map(|e| e.kind() == std::io::ErrorKind::TimedOut)
            .unwrap_or(false)
    }
}
