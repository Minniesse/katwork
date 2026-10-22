use std::sync::LazyLock;

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use rand_core::{OsRng, RngCore};

use crate::error::KatError;

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
