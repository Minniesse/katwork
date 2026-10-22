use std::env;

use katwork::error::KatError;

pub struct Config {
    pub database_url: String,
    pub secret: String,
    pub demo: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, KatError> {
        let database_url = env::var("DATABASE_URL").map_err(|_| KatError::ConfigInvalid)?;
        let secret = env::var("KATWORK_SECRET").map_err(|_| KatError::ConfigInvalid)?;
        if database_url.trim().is_empty() || secret.len() < 32 {
            return Err(KatError::ConfigInvalid);
        }
        Ok(Self {
            database_url,
            secret,
            demo: env::var("KATWORK_DEMO").is_ok_and(|value| value == "1"),
        })
    }
}
