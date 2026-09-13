use super::types::{ProviderStatus, ProviderUsage};
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct BackoffState {
    failures: u32,
}

impl BackoffState {
    pub fn record_success(&mut self) {
        self.failures = 0;
    }
    pub fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }
    pub fn delay(&self, base: Duration) -> Duration {
        if self.failures == 0 {
            return base;
        }
        let multiplier = 1u32
            .checked_shl(self.failures.saturating_sub(1).min(3))
            .unwrap_or(8);
        (base * multiplier).min(Duration::from_secs(30 * 60))
    }
}

pub fn merge_with_previous(
    previous: Option<&ProviderUsage>,
    mut current: ProviderUsage,
) -> ProviderUsage {
    if current.status == ProviderStatus::Fresh {
        return current;
    }
    if let Some(old) = previous.filter(|p| !p.windows.is_empty()) {
        current.windows = old.windows.clone();
        current.last_updated = old.last_updated;
        current.status = ProviderStatus::Stale;
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{ProviderId, UsageWindow};

    #[test]
    fn backoff_is_bounded() {
        let mut b = BackoffState::default();
        let base = Duration::from_secs(180);
        assert_eq!(b.delay(base), base);
        for _ in 0..10 {
            b.record_failure();
        }
        assert_eq!(b.delay(base), Duration::from_secs(1800));
        b.record_success();
        assert_eq!(b.delay(base), base);
    }

    #[test]
    fn preserves_last_known_values_as_stale() {
        let old = ProviderUsage {
            provider: ProviderId::Openai,
            windows: vec![UsageWindow {
                id: "p".into(),
                label: "5 hours".into(),
                used_percent: Some(20.0),
                remaining_percent: Some(80.0),
                reset_at: None,
            }],
            last_updated: Some(1),
            source: "x".into(),
            status: ProviderStatus::Fresh,
            error: None,
        };
        let failed = ProviderUsage {
            status: ProviderStatus::Unavailable,
            error: Some("failed".into()),
            ..ProviderUsage::empty(ProviderId::Openai, "x")
        };
        let merged = merge_with_previous(Some(&old), failed);
        assert_eq!(merged.status, ProviderStatus::Stale);
        assert_eq!(merged.windows[0].remaining_percent, Some(80.0));
    }
}
