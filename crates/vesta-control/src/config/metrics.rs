//! Bounded local telemetry settings. This table deliberately uses snake_case keys.
use super::{ConfigError, Result};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsSettings {
    pub enabled: bool,
    pub sample_seconds: u64,
    pub raw_retention_hours: u64,
    pub five_minute_retention_days: u64,
    pub daily_retention_days: u64,
    pub max_storage_bytes: u64,
}
impl Default for MetricsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            sample_seconds: 10,
            raw_retention_hours: 24,
            five_minute_retention_days: 30,
            daily_retention_days: 365,
            max_storage_bytes: 256 * 1024 * 1024,
        }
    }
}
impl MetricsSettings {
    pub(super) fn validated(self) -> Result<Self> {
        if !(5..=60).contains(&self.sample_seconds)
            || !(1..=168).contains(&self.raw_retention_hours)
            || !(1..=90).contains(&self.five_minute_retention_days)
            || !(1..=730).contains(&self.daily_retention_days)
            || !(1_048_576..=1_073_741_824).contains(&self.max_storage_bytes)
        {
            return Err(ConfigError::InvalidDocument);
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metrics_configuration_is_closed_and_bounded() {
        let default: MetricsSettings = toml::from_str("").unwrap();
        assert_eq!(default, MetricsSettings::default());
        for input in [
            "sample_seconds=4",
            "sample_seconds=61",
            "raw_retention_hours=0",
            "daily_retention_days=731",
            "max_storage_bytes=0",
        ] {
            let settings: MetricsSettings = toml::from_str(input).unwrap();
            assert_eq!(settings.validated(), Err(ConfigError::InvalidDocument));
        }
        assert!(
            toml::from_str::<MetricsSettings>("sample_seconds=5\nenabled=false")
                .unwrap()
                .validated()
                .is_ok()
        );
        assert!(toml::from_str::<MetricsSettings>("extra=true").is_err());
    }
}
