//! What bounds an upload: how many run at once, how slowly a body may arrive, and how much room
//! it may take.
use std::{
    collections::HashMap,
    pin::pin,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use tokio::time::Instant;

use super::Staging;
use crate::{error::KatError, ids::UserId};

/// Uploads the server runs at once.
pub const UPLOADS_AT_ONCE: usize = 8;
/// Uploads one person runs at once.
pub const UPLOADS_PER_PERSON: usize = 2;
/// An upload's body must arrive within this time.
pub const UPLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Uploads never take the disk below this much free space.
pub const FREE_SPACE_FLOOR: u64 = 1_073_741_824;

/// Counts uploads in flight, in total and per person. It never waits: a full limiter is
/// `TooManyUploads`.
pub struct UploadLimiter {
    total: usize,
    per_user: usize,
    slots: Arc<Mutex<Slots>>,
}

#[derive(Default)]
struct Slots {
    total: usize,
    users: HashMap<UserId, usize>,
}

/// A slot of the limiter; dropping it frees the slot.
pub struct UploadPermit {
    user: UserId,
    slots: Arc<Mutex<Slots>>,
}

impl UploadLimiter {
    pub fn new(total: usize, per_user: usize) -> Self {
        Self {
            total,
            per_user,
            slots: Arc::default(),
        }
    }

    pub fn try_acquire(&self, user: UserId) -> Result<UploadPermit, KatError> {
        // A panic while the lock was held leaves the counts consistent: every change is one step.
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let held = slots.users.get(&user).copied().unwrap_or(0);
        if slots.total >= self.total || held >= self.per_user {
            return Err(KatError::TooManyUploads);
        }
        slots.total += 1;
        *slots.users.entry(user).or_default() += 1;
        Ok(UploadPermit {
            user,
            slots: self.slots.clone(),
        })
    }
}

impl Drop for UploadPermit {
    fn drop(&mut self) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots.total = slots.total.saturating_sub(1);
        if let Some(held) = slots.users.get_mut(&self.user) {
            *held = held.saturating_sub(1);
            if *held == 0 {
                slots.users.remove(&self.user);
            }
        }
    }
}

/// The upload rate floor. The first `observe` starts the first window; from its end on, each
/// `observe` at or past the end of the current window checks that the bytes received since the
/// window began average at least `min_bytes_per_sec` over the time it lasted, then starts the
/// next window. A floor of 0 never refuses.
#[derive(Debug, Clone)]
pub struct RateCheck {
    min_bytes_per_sec: u64,
    window: Duration,
    /// When the current window began and the bytes received by then.
    start: Option<(Instant, u64)>,
}

impl RateCheck {
    pub fn new(min_bytes_per_sec: u64, window: Duration) -> Self {
        Self {
            min_bytes_per_sec,
            window,
            start: None,
        }
    }

    pub fn observe(&mut self, now: Instant, total_bytes: u64) -> Result<(), KatError> {
        let Some((start, at_start)) = self.start else {
            self.start = Some((now, total_bytes));
            return Ok(());
        };
        let elapsed = now.saturating_duration_since(start);
        if elapsed < self.window {
            return Ok(());
        }
        let received = u128::from(total_bytes.saturating_sub(at_start));
        let needed = u128::from(self.min_bytes_per_sec) * elapsed.as_nanos() / 1_000_000_000;
        if received < needed {
            return Err(KatError::UploadTooSlow);
        }
        self.start = Some((now, total_bytes));
        Ok(())
    }

    /// When the current window ends: the latest the next check is due. `None` before the first
    /// `observe`.
    pub fn deadline(&self) -> Option<Instant> {
        self.start.map(|(start, _)| start + self.window)
    }
}

/// Refuses an upload of `incoming` bytes that would take the disk below the free-space floor
/// (`StorageFull`, checked first) or the workspace past its quota (`StorageQuotaExceeded`). A sum
/// that overflows is refused, never wrapped.
pub fn check_space(
    free_bytes: u64,
    used_bytes: u64,
    incoming: u64,
    quota: u64,
) -> Result<(), KatError> {
    if FREE_SPACE_FLOOR
        .checked_add(incoming)
        .is_none_or(|floor| free_bytes < floor)
    {
        return Err(KatError::StorageFull);
    }
    if used_bytes
        .checked_add(incoming)
        .is_none_or(|after| after > quota)
    {
        return Err(KatError::StorageQuotaExceeded);
    }
    Ok(())
}

/// Streams an upload's body into `staging` under `rate`, within `total`.
///
/// The rate is checked as chunks arrive and, when none does, at the end of each window, so a
/// stalled body is refused too. A body error (a reset, a cut-off message) is `RequestInvalid`;
/// running past `total` is `UploadTooSlow`. Each chunk is written in the select's arm, never as
/// one of its branches: `write_all` is not cancel-safe.
pub async fn stream_body<S, E>(
    body: S,
    staging: &mut Staging,
    rate: &mut RateCheck,
    total: Duration,
) -> Result<(), KatError>
where
    S: Stream<Item = Result<Bytes, E>>,
{
    let streaming = async {
        let mut body = pin!(body);
        let mut received: u64 = 0;
        let start = Instant::now();
        rate.observe(start, received)?;
        let mut timer = pin!(tokio::time::sleep_until(
            rate.deadline().unwrap_or(start + total)
        ));
        loop {
            tokio::select! {
                biased;
                next = body.next() => match next {
                    None => return Ok(()),
                    Some(Err(_)) => return Err(KatError::RequestInvalid),
                    Some(Ok(chunk)) => {
                        staging.write(&chunk).await?;
                        received = received.saturating_add(chunk.len() as u64);
                        rate.observe(Instant::now(), received)?;
                    }
                },
                () = &mut timer => rate.observe(Instant::now(), received)?,
            }
            if let Some(deadline) = rate.deadline() {
                timer.as_mut().reset(deadline);
            }
        }
    };
    tokio::time::timeout(total, streaming)
        .await
        .map_err(|_| KatError::UploadTooSlow)?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_use(limiter: &UploadLimiter) -> (usize, usize) {
        let slots = limiter.slots.lock().unwrap();
        (slots.total, slots.users.len())
    }

    #[test]
    fn limiter_table() {
        let limiter = UploadLimiter::new(3, 2);
        let (a, b, c) = (UserId::new(), UserId::new(), UserId::new());
        let a1 = limiter.try_acquire(a).unwrap();
        let a2 = limiter.try_acquire(a).unwrap();
        // A third for one person is over the per-person limit.
        assert!(matches!(
            limiter.try_acquire(a),
            Err(KatError::TooManyUploads)
        ));
        let b1 = limiter.try_acquire(b).unwrap();
        // A fourth in all is over the total, for a person with none.
        assert!(matches!(
            limiter.try_acquire(c),
            Err(KatError::TooManyUploads)
        ));
        assert_eq!(in_use(&limiter), (3, 2));
        drop(a1);
        let c1 = limiter.try_acquire(c).unwrap();
        assert!(matches!(
            limiter.try_acquire(b),
            Err(KatError::TooManyUploads)
        ));
        drop((a2, b1, c1));
        // Freed slots leave no empty entry behind.
        assert_eq!(in_use(&limiter), (0, 0));
        let again: Vec<_> = (0..2).map(|_| limiter.try_acquire(a).unwrap()).collect();
        assert_eq!(in_use(&limiter), (2, 1));
        drop(again);
        assert_eq!(in_use(&limiter), (0, 0));
    }

    #[test]
    fn a_poisoned_limiter_still_counts() {
        let limiter = Arc::new(UploadLimiter::new(1, 1));
        let user = UserId::new();
        let held = limiter.try_acquire(user).unwrap();
        let poisoner = limiter.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.slots.lock().unwrap();
            panic!("poison the lock");
        })
        .join();
        assert!(limiter.slots.is_poisoned());
        assert!(matches!(
            limiter.try_acquire(user),
            Err(KatError::TooManyUploads)
        ));
        drop(held);
        limiter.try_acquire(user).unwrap();
    }
}
