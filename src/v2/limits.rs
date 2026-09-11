use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub artifact_max_bytes: u64,
    pub artifact_store_max_bytes: u64,
    pub capture_concurrency: usize,
    pub capture_timeout_seconds: u64,
    pub download_concurrency: usize,
    pub download_timeout_seconds: u64,
    pub artifact_retention_seconds: u64,
    pub lease_max_lifetime_seconds: u64,
    pub output_max_bytes: u64,
    pub output_store_max_bytes: u64,
    pub output_retention_seconds: u64,
    pub execution_input_max_bytes: u64,
    pub execution_input_store_max_bytes: u64,
    pub unresolved_input_retention_seconds: u64,
    pub disk_free_floor_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            artifact_max_bytes: 268435456,
            artifact_store_max_bytes: 8589934592,
            capture_concurrency: 2,
            capture_timeout_seconds: 120,
            download_concurrency: 4,
            download_timeout_seconds: 300,
            artifact_retention_seconds: 604800,
            lease_max_lifetime_seconds: 2592000,
            output_max_bytes: 8388608,
            output_store_max_bytes: 1073741824,
            output_retention_seconds: 604800,
            execution_input_max_bytes: 16777216,
            execution_input_store_max_bytes: 268435456,
            unresolved_input_retention_seconds: 86400,
            disk_free_floor_bytes: 2147483648,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> super::Result<()> {
        if self.capture_concurrency == 0
            || self.capture_concurrency > 64
            || self.download_concurrency == 0
            || self.download_concurrency > 256
            || self.artifact_max_bytes == 0
            || self.artifact_max_bytes > self.artifact_store_max_bytes
            || self.output_max_bytes == 0
            || self.output_max_bytes > self.output_store_max_bytes
            || self.execution_input_max_bytes == 0
            || self.execution_input_max_bytes > self.execution_input_store_max_bytes
        {
            return Err(super::Error::code(400, "invalid_v2_limits"));
        }
        for seconds in [
            self.capture_timeout_seconds,
            self.download_timeout_seconds,
            self.artifact_retention_seconds,
            self.lease_max_lifetime_seconds,
            self.output_retention_seconds,
            self.unresolved_input_retention_seconds,
        ] {
            if seconds == 0 || seconds > 315360000 {
                return Err(super::Error::code(400, "invalid_v2_limits"));
            }
        }
        if self.lease_max_lifetime_seconds < self.artifact_retention_seconds
            || self.lease_max_lifetime_seconds < self.output_retention_seconds
        {
            return Err(super::Error::code(400, "invalid_v2_limits"));
        }
        Ok(())
    }
}
