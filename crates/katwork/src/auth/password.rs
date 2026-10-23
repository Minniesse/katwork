use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

use tokio::sync::Semaphore;

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use rand_core::{OsRng, RngCore};

use crate::error::KatError;

fn work_slots() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}

static WORK: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(work_slots())));

/// Holds every Argon2 work slot of this process until the permit drops, so a test can show
/// what a caller does while it waits for one. Production code never calls it.
#[doc(hidden)]
pub async fn hold_every_work_slot() -> tokio::sync::OwnedSemaphorePermit {
    WORK.clone()
        .acquire_many_owned(work_slots() as u32)
        .await
        .expect("the work semaphore is never closed")
}

pub(super) async fn run<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, KatError> {
    run_with(WORK.clone(), work).await
}

async fn run_with<T: Send + 'static>(
    slots: Arc<Semaphore>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, KatError> {
    let permit = tokio::time::timeout(Duration::from_secs(5), slots.acquire_owned())
        .await
        .map_err(|_| KatError::AuthBusy)?
        .map_err(|_| KatError::AuthBusy)?;
    tokio::task::spawn_blocking(move || {
        // Keep capacity charged until the blocking work ends, even if its request is cancelled.
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| KatError::AuthFailed)
}

pub fn hash(password: &str) -> Result<String, KatError> {
    if password.chars().count() < 8 || password.len() > 1024 {
        return Err(KatError::PasswordInvalid);
    }
    let mut salt = [0u8; 16];
    OsRng
        .try_fill_bytes(&mut salt)
        .map_err(|_| KatError::AuthFailed)?;
    let salt = SaltString::encode_b64(&salt).map_err(|_| KatError::AuthFailed)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| KatError::AuthFailed)
}

pub fn verify(password: &str, hash: &str) -> bool {
    password.len() <= 1024
        && PasswordHash::new(hash).is_ok_and(|hash| {
            hash.algorithm.as_str() == "argon2id"
                && Argon2::default()
                    .verify_password(password.as_bytes(), &hash)
                    .is_ok()
        })
}

pub(super) fn verify_unknown(password: &str) {
    // Unknown accounts still pay the password verification cost.
    static DUMMY: LazyLock<Option<String>> =
        LazyLock::new(|| super::random_token().and_then(|value| hash(&value)).ok());
    if let Some(hash) = &*DUMMY {
        verify(password, hash);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn argon_work_is_cpu_bounded_and_waits_at_most_five_seconds() {
        let count = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        let work = Arc::new(Semaphore::new(count));
        assert_eq!(work.available_permits(), count);
        let held = work.clone().acquire_many_owned(count as u32).await.unwrap();
        let start = tokio::time::Instant::now();
        let result = run_with(work.clone(), || panic!("work ran without a permit")).await;
        assert_eq!(result.unwrap_err().code(), "auth_busy");
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(5));
        drop(held);
        assert_eq!(run_with(work.clone(), || 42).await.unwrap(), 42);

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let task = tokio::spawn(run_with(work.clone(), move || {
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
        }));
        started_rx.await.unwrap();
        task.abort();
        let _ = task.await;
        assert_eq!(
            work.available_permits(),
            count - 1,
            "cancelled request released running Argon work"
        );
        finish_tx.send(()).unwrap();
        let _all = work.clone().acquire_many_owned(count as u32).await.unwrap();
    }
}
