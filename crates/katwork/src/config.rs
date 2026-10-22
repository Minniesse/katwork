use std::{env, fmt};

use percent_encoding::percent_decode_str;
use url::Url;

use crate::error::KatError;

pub struct Config {
    pub database_url: String,
    pub secret: String,
    pub demo: bool,
    pub public_url: Option<String>,
    pub demo_password: Option<String>,
    pub smtp: Option<SmtpConfig>,
    pub openai: Option<ProviderEnv>,
    pub anthropic: Option<ProviderEnv>,
}

pub struct SmtpConfig {
    pub host: String,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: Option<String>,
}

pub struct ProviderEnv {
    pub api_key: String,
    pub base_url: Option<String>,
}

impl Config {
    pub fn redact_secrets(&self, message: &str) -> String {
        let Ok(url) = Url::parse(&self.database_url) else {
            // Fail closed when malformed credentials cannot be parsed safely.
            return "[redacted]".into();
        };
        let mut secrets: Vec<String> = [
            Some(self.secret.as_str()),
            self.demo_password.as_deref(),
            self.smtp.as_ref().and_then(|smtp| smtp.password.as_deref()),
            self.openai
                .as_ref()
                .map(|provider| provider.api_key.as_str()),
            self.anthropic
                .as_ref()
                .map(|provider| provider.api_key.as_str()),
            url.password(),
        ]
        .into_iter()
        .flatten()
        .map(str::to_owned)
        .collect();
        if let Some(password) = url.password() {
            secrets.push(
                percent_decode_str(password)
                    .decode_utf8_lossy()
                    .into_owned(),
            );
        }
        for pair in url.query().into_iter().flat_map(|query| query.split('&')) {
            if let Some((key, value)) = url::form_urlencoded::parse(pair.as_bytes()).next() {
                if key == "password" {
                    secrets.push(value.into_owned());
                    if let Some((_, encoded)) = pair.split_once('=') {
                        secrets.push(encoded.to_owned());
                    }
                }
            }
        }
        // SQLx quotes invalid options using Debug, escaping quotes and backslashes.
        let escaped: Vec<_> = secrets
            .iter()
            .map(|secret| {
                let quoted = format!("{secret:?}");
                quoted[1..quoted.len() - 1].to_owned()
            })
            .collect();
        secrets.extend(escaped);
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        let mut redacted = message.to_owned();
        for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
            redacted = redacted.replace(secret, "[redacted]");
        }
        redacted
    }

    pub fn from_env() -> Result<Self, KatError> {
        Self::from_lookup(|name| env::var(name).ok())
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, KatError> {
        let database_url = lookup("DATABASE_URL").ok_or(KatError::ConfigInvalid)?;
        let secret = lookup("KATWORK_SECRET").ok_or(KatError::ConfigInvalid)?;
        if database_url.trim().is_empty() || secret.len() < 32 {
            return Err(KatError::ConfigInvalid);
        }
        let optional = |name: &str| lookup(name).filter(|value| !value.is_empty());
        let smtp = if let Some(host) = optional("SMTP_HOST") {
            let port = optional("SMTP_PORT")
                .map(|value| value.parse::<u16>().map_err(|_| KatError::ConfigInvalid))
                .transpose()?;
            if port == Some(0) {
                return Err(KatError::ConfigInvalid);
            }
            Some(SmtpConfig {
                host,
                port,
                username: optional("SMTP_USERNAME"),
                password: optional("SMTP_PASSWORD"),
                from: optional("SMTP_FROM"),
            })
        } else {
            None
        };
        Ok(Self {
            database_url,
            secret,
            demo: lookup("KATWORK_DEMO").is_some_and(|value| value == "1"),
            public_url: optional("KATWORK_PUBLIC_URL"),
            demo_password: optional("KATWORK_DEMO_PASSWORD"),
            smtp,
            openai: optional("OPENAI_API_KEY").map(|api_key| ProviderEnv {
                api_key,
                base_url: optional("OPENAI_BASE_URL"),
            }),
            anthropic: optional("ANTHROPIC_API_KEY").map(|api_key| ProviderEnv {
                api_key,
                base_url: optional("ANTHROPIC_BASE_URL"),
            }),
        })
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("database_url", &self.redact_secrets(&self.database_url))
            .field("secret", &"[redacted]")
            .field("demo", &self.demo)
            .field("public_url", &self.public_url)
            .field("demo_password", &"[redacted]")
            .field("smtp", &self.smtp)
            .field("openai", &self.openai)
            .field("anthropic", &self.anthropic)
            .finish()
    }
}

impl fmt::Debug for SmtpConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmtpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .field("from", &self.from)
            .finish()
    }
}

impl fmt::Debug for ProviderEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderEnv")
            .field("api_key", &"[redacted]")
            .field("base_url", &self.base_url)
            .finish()
    }
}
