use std::{collections::HashMap, hash::Hash, time::Duration};

use tokio::time::Instant;

/// At most one log line per key per window, so an unauthenticated flood cannot flood the log.
pub(crate) struct LogThrottle<K> {
    logged: HashMap<K, Instant>,
}

impl<K> Default for LogThrottle<K> {
    fn default() -> Self {
        Self {
            logged: HashMap::new(),
        }
    }
}

impl<K: Hash + Eq> LogThrottle<K> {
    pub(crate) const WINDOW: Duration = Duration::from_secs(60);
    /// Keys remembered at once. When every one logged within the window, a new key is not
    /// logged until old ones age out, so the map stays bounded.
    const KEYS: usize = 10_000;

    pub(crate) fn should_log(&mut self, key: K, now: Instant) -> bool {
        let recent = |at: &Instant| now.duration_since(*at) < Self::WINDOW;
        if self.logged.get(&key).is_some_and(recent) {
            return false;
        }
        if self.logged.len() >= Self::KEYS && !self.logged.contains_key(&key) {
            self.logged.retain(|_, at| recent(at));
            if self.logged.len() >= Self::KEYS {
                return false;
            }
        }
        self.logged.insert(key, now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_once_per_key_per_window() {
        let mut throttle = LogThrottle::default();
        let start = Instant::now();
        assert!(throttle.should_log("first", start));
        assert!(!throttle.should_log("first", start + Duration::from_secs(1)));
        assert!(throttle.should_log("second", start + Duration::from_secs(1)));
        assert!(!throttle.should_log("first", start + Duration::from_millis(59_999)));
        assert!(throttle.should_log("first", start + Duration::from_secs(60)));
        assert!(!throttle.should_log("second", start + Duration::from_secs(60)));
        assert!(throttle.should_log("second", start + Duration::from_secs(61)));
    }

    #[test]
    fn a_full_map_logs_new_keys_only_once_old_ones_age_out() {
        let mut throttle = LogThrottle::default();
        let start = Instant::now();
        for key in 0..LogThrottle::<u32>::KEYS as u32 {
            assert!(throttle.should_log(key, start));
        }
        assert!(!throttle.should_log(u32::MAX, start + Duration::from_secs(59)));
        assert!(throttle.should_log(u32::MAX, start + Duration::from_secs(60)));
        assert_eq!(throttle.logged.len(), 1);
    }
}
