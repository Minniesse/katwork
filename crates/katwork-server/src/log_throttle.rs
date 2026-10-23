use std::{collections::HashMap, hash::Hash, time::Duration};

use tokio::time::Instant;

/// At most one log line per key per window, so an unauthenticated flood cannot flood the log.
pub(crate) struct LogThrottle<K> {
    logged: HashMap<K, Instant>,
    /// No entry is older than this, so a full map is swept only once its oldest key can go.
    oldest: Option<Instant>,
    #[cfg(test)]
    sweeps: usize,
}

impl<K> Default for LogThrottle<K> {
    fn default() -> Self {
        Self {
            logged: HashMap::new(),
            oldest: None,
            #[cfg(test)]
            sweeps: 0,
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
            // Every key is recent while the oldest is, so there is nothing to sweep yet.
            if self.oldest.is_some_and(|oldest| recent(&oldest)) {
                return false;
            }
            self.logged.retain(|_, at| recent(at));
            self.oldest = self.logged.values().min().copied();
            #[cfg(test)]
            {
                self.sweeps += 1;
            }
            if self.logged.len() >= Self::KEYS {
                return false;
            }
        }
        self.logged.insert(key, now);
        self.oldest = Some(self.oldest.map_or(now, |oldest| oldest.min(now)));
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

    #[test]
    fn a_full_map_of_recent_keys_is_not_scanned_for_each_new_key() {
        let mut throttle = LogThrottle::default();
        let start = Instant::now();
        let half = LogThrottle::<u32>::KEYS as u32 / 2;
        for key in 0..half {
            assert!(throttle.should_log(key, start));
        }
        for key in half..2 * half {
            assert!(throttle.should_log(key, start + Duration::from_secs(30)));
        }
        // Full of keys logged within the window: refusals cost no sweep.
        for key in 0..1_000 {
            assert!(!throttle.should_log(u32::MAX - key, start + Duration::from_secs(59)));
        }
        assert_eq!(throttle.sweeps, 0);
        // The first half has aged out: one sweep makes room.
        assert!(throttle.should_log(u32::MAX, start + Duration::from_secs(60)));
        assert_eq!(throttle.sweeps, 1);
        assert_eq!(throttle.logged.len(), half as usize + 1);
        for key in 0..half - 1 {
            assert!(throttle.should_log(3 * half + key, start + Duration::from_secs(60)));
        }
        // Full again, and its oldest key is from 30 s: no sweep until that one ages out.
        for key in 0..1_000 {
            assert!(!throttle.should_log(u32::MAX - 1 - key, start + Duration::from_secs(89)));
        }
        assert_eq!(throttle.sweeps, 1);
        assert!(throttle.should_log(u32::MAX - 1, start + Duration::from_secs(90)));
        assert_eq!(throttle.sweeps, 2);
    }
}
