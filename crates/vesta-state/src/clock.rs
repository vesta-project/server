use std::time::{SystemTime, UNIX_EPOCH};

/// Injectable wall clock; timestamps are Java-compatible signed epoch milliseconds.
pub trait Clock: Send + Sync {
    fn millis(&self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn millis(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
            Err(error) => {
                // Floor rather than truncate a fractional millisecond before the epoch.
                let nanos = error.duration().as_nanos();
                let millis = nanos.div_ceil(1_000_000);
                if millis > i64::MAX as u128 {
                    i64::MIN
                } else {
                    -(millis as i64)
                }
            }
        }
    }
}
