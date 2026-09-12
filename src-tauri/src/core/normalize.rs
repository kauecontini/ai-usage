use super::types::UsageWindow;

pub fn clamp_percent(value: f64) -> Option<f64> {
    if !value.is_finite() {
        return None;
    }
    Some(value.clamp(0.0, 100.0))
}

pub fn normalize_window(id: impl Into<String>, label: impl Into<String>, used: f64, reset_at: Option<i64>) -> UsageWindow {
    let used_percent = clamp_percent(used);
    let remaining_percent = used_percent.map(|v| (100.0 - v).clamp(0.0, 100.0));
    UsageWindow {
        id: id.into(),
        label: label.into(),
        used_percent,
        remaining_percent,
        reset_at: reset_at.filter(|v| *v > 0),
    }
}

pub fn label_for_minutes(minutes: Option<i64>, fallback: &str) -> String {
    match minutes {
        Some(300) => "5 hours".to_string(),
        Some(10_080) => "Weekly".to_string(),
        Some(m) if m > 0 && m % 1440 == 0 => format!("{} days", m / 1440),
        Some(m) if m > 0 && m % 60 == 0 => format!("{} hours", m / 60),
        _ => fallback.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_used_to_remaining() {
        let w = normalize_window("p", "5 hours", 28.0, None);
        assert_eq!(w.used_percent, Some(28.0));
        assert_eq!(w.remaining_percent, Some(72.0));
    }

    #[test]
    fn clamps_percentages() {
        assert_eq!(normalize_window("p", "x", -3.0, None).remaining_percent, Some(100.0));
        assert_eq!(normalize_window("p", "x", 130.0, None).remaining_percent, Some(0.0));
        assert_eq!(clamp_percent(f64::NAN), None);
    }

    #[test]
    fn recognizes_standard_windows() {
        assert_eq!(label_for_minutes(Some(300), "Primary"), "5 hours");
        assert_eq!(label_for_minutes(Some(10_080), "Secondary"), "Weekly");
    }
}
