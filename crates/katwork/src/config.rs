use std::{collections::BTreeSet, env, fmt, net::IpAddr, time::Duration};

use percent_encoding::percent_decode_str;
use url::Url;

use crate::error::KatError;

pub struct Config {
    pub database_url: String,
    pub secret: String,
    pub demo: bool,
    pub transport: TransportConfig,
    pub trusted_proxies: Vec<(IpAddr, u8)>,
    pub public_url: Option<String>,
    pub demo_password: Option<String>,
    pub smtp: Option<SmtpConfig>,
    pub openai: Option<ProviderEnv>,
    pub anthropic: Option<ProviderEnv>,
}

#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub header_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_connections: usize,
    pub max_connections_per_ip: usize,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            header_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(60),
            max_connections: 256,
            max_connections_per_ip: 32,
        }
    }
}

#[derive(Clone)]
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

fn parse_log_url(value: &str, database: bool) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    if !url.has_host()
        || (database && !matches!(url.scheme(), "postgres" | "postgresql"))
        || [Some(url.path()), url.query(), url.fragment()]
            .into_iter()
            .flatten()
            .any(|part| part.contains('@'))
    {
        return None;
    }
    Some(url)
}

fn redact_url(value: &str, database: bool) -> String {
    let Some(url) = parse_log_url(value, database) else {
        return "[redacted]".into();
    };
    format!("{}{}", url_origin(&url), url.path())
}

fn redact_origin(value: &str) -> String {
    parse_log_url(value, false)
        .map(|url| url_origin(&url))
        .unwrap_or_else(|| "[redacted]".into())
}

fn url_origin(url: &Url) -> String {
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    format!("{}://{}{port}", url.scheme(), url.host().unwrap())
}

fn url_redactions(message: &str) -> Vec<(usize, usize, String)> {
    let mut replacements = Vec::new();
    let mut offset = 0;
    for token in message.split_inclusive(char::is_whitespace) {
        let end = token
            .trim_end_matches(|c: char| {
                c.is_whitespace() || matches!(c, '\'' | '"' | ')' | '}' | ',' | ';' | '>')
            })
            .len();
        if let Some(separator) = token[..end].find("://") {
            let start = token[..separator]
                .trim_end_matches(|c: char| {
                    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
                })
                .len();
            replacements.push((
                offset + start,
                offset + end,
                redact_url(&token[start..end], false),
            ));
        }
        offset += token.len();
    }
    replacements
}

fn replace_secrets(message: &str, secrets: Vec<String>) -> String {
    let secrets: BTreeSet<_> = secrets
        .into_iter()
        .flat_map(|secret| {
            let decoded = percent_decode_str(&secret).decode_utf8_lossy().into_owned();
            [secret, decoded]
        })
        .filter(|secret| secret.chars().count() >= 4)
        .flat_map(|secret| {
            // SQLx quotes invalid options using Debug, escaping quotes and backslashes.
            let quoted = format!("{secret:?}");
            [secret, quoted[1..quoted.len() - 1].to_owned()]
        })
        .collect();
    let mut secrets: Vec<_> = secrets.into_iter().collect();
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    let mut replacements = url_redactions(message);
    let mut offset = 0;
    while offset < message.len() {
        let rest = &message[offset..];
        if let Some(secret) = secrets
            .iter()
            .find(|secret| rest.starts_with(secret.as_str()))
        {
            replacements.push((offset, offset + secret.len(), "[redacted]".into()));
            offset += secret.len();
        } else {
            offset += rest.chars().next().unwrap().len_utf8();
        }
    }
    replacements.sort_by_key(|&(start, _, _)| start);
    let mut replacements = replacements.into_iter().peekable();
    let mut redacted = String::with_capacity(message.len());
    let mut copied = 0;
    while let Some((start, mut end, mut replacement)) = replacements.next() {
        // Merge overlaps on the original text so neither pass can split a secret or URL.
        while replacements
            .peek()
            .is_some_and(|&(next_start, _, _)| next_start < end)
        {
            end = end.max(replacements.next().unwrap().1);
            replacement = "[redacted]".into();
        }
        redacted.push_str(&message[copied..start]);
        redacted.push_str(&replacement);
        copied = end;
    }
    redacted.push_str(&message[copied..]);
    redacted
}

fn database_secrets(database_url: &str) -> Option<Vec<String>> {
    let url = parse_log_url(database_url, true)?;
    let mut secrets = vec![database_url.to_owned()];
    if let Some((_, authority)) = database_url.split_once("://")
        && let Some((userinfo, _)) = authority.rsplit_once('@')
    {
        secrets.push(userinfo.to_owned());
        if let Some((_, password)) = userinfo.split_once(':') {
            secrets.push(password.to_owned());
        }
    }
    if let Some(password) = url.password() {
        secrets.push(format!("{}:{password}", url.username()));
        secrets.push(password.to_owned());
    }
    let raw_query = database_url
        .split_once('?')
        .map(|(_, rest)| rest.split('#').next().unwrap());
    for query in [raw_query, url.query()].into_iter().flatten() {
        for pair in query.split('&') {
            if let Some((key, value)) = pair.split_once('=')
                && secret_query_value(key, value)
            {
                secrets.push(value.to_owned());
            }
        }
        secrets.extend(
            url::form_urlencoded::parse(query.as_bytes())
                .filter(|(key, value)| secret_query_value(key, value))
                .map(|(_, value)| value.into_owned()),
        );
    }
    Some(secrets)
}

fn secret_query_value(key: &str, value: &str) -> bool {
    let key = percent_decode_str(key).decode_utf8_lossy();
    let value = percent_decode_str(value).decode_utf8_lossy();
    key.to_ascii_lowercase().contains("password")
        || !(value.bytes().all(|byte| byte.is_ascii_digit())
            || matches!(
                value.to_ascii_lowercase().as_str(),
                "disable"
                    | "allow"
                    | "prefer"
                    | "require"
                    | "verify-ca"
                    | "verify-full"
                    | "true"
                    | "false"
            ))
}

pub fn redact_database_secrets(message: &str, database_url: &str) -> String {
    let Some(secrets) = database_secrets(database_url) else {
        // Fail closed when malformed credentials cannot be parsed safely.
        return "[redacted]".into();
    };
    replace_secrets(message, secrets)
}

impl Config {
    pub fn public_url_or_default(&self) -> &str {
        self.public_url
            .as_deref()
            .unwrap_or("http://localhost:3000")
    }

    pub fn trusts_proxy(&self, ip: IpAddr) -> bool {
        self.trusted_proxies
            .iter()
            .any(|(network, prefix)| match (network, ip) {
                (IpAddr::V4(network), IpAddr::V4(ip)) => {
                    let shift = 32 - u32::from(*prefix);
                    u32::from(*network).checked_shr(shift) == u32::from(ip).checked_shr(shift)
                }
                (IpAddr::V6(network), IpAddr::V6(ip)) => {
                    let shift = 128 - u32::from(*prefix);
                    u128::from(*network).checked_shr(shift) == u128::from(ip).checked_shr(shift)
                }
                _ => false,
            })
    }

    pub fn cookie_secure(&self) -> bool {
        self.public_url_or_default().starts_with("https://")
    }

    pub fn redact_secrets(&self, message: &str) -> String {
        let Some(mut secrets) = database_secrets(&self.database_url) else {
            return "[redacted]".into();
        };
        secrets.extend(
            [
                Some(self.secret.as_str()),
                self.demo_password.as_deref(),
                self.smtp.as_ref().and_then(|smtp| smtp.password.as_deref()),
                self.openai
                    .as_ref()
                    .map(|provider| provider.api_key.as_str()),
                self.anthropic
                    .as_ref()
                    .map(|provider| provider.api_key.as_str()),
            ]
            .into_iter()
            .flatten()
            .map(str::to_owned),
        );
        replace_secrets(message, secrets)
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
        for key in ["OPENAI_BASE_URL", "ANTHROPIC_BASE_URL"] {
            if let Some(value) = optional(key) {
                let url = Url::parse(&value).map_err(|_| KatError::ConfigInvalid)?;
                if !matches!(url.scheme(), "http" | "https") || !url.has_host() {
                    return Err(KatError::ConfigInvalid);
                }
            }
        }
        for name in ["SMTP_PASSWORD", "KATWORK_DEMO_PASSWORD"] {
            if optional(name).is_some_and(|value| value.chars().count() < 8) {
                return Err(KatError::ConfigInvalid);
            }
        }
        let mut public_url = optional("KATWORK_PUBLIC_URL");
        if let Some(value) = &public_url {
            let url = Url::parse(value).map_err(|_| KatError::ConfigInvalid)?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.has_host()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(KatError::ConfigInvalid);
            }
            public_url = Some(url.origin().ascii_serialization());
        }
        let demo = lookup("KATWORK_DEMO").is_some_and(|value| value == "1");
        if demo
            && public_url
                .as_deref()
                .is_some_and(|url| url.starts_with("https://"))
            && lookup("KATWORK_ALLOW_DEMO_ON_HTTPS").as_deref() != Some("1")
        {
            return Err(KatError::ConfigInvalid);
        }
        let trusted_proxies = optional("KATWORK_TRUSTED_PROXIES")
            .map(|value| {
                value
                    .split(',')
                    .map(|cidr| {
                        let (ip, prefix) =
                            cidr.trim().split_once('/').ok_or(KatError::ConfigInvalid)?;
                        let ip: IpAddr = ip.parse().map_err(|_| KatError::ConfigInvalid)?;
                        let prefix: u8 = prefix.parse().map_err(|_| KatError::ConfigInvalid)?;
                        if prefix > if ip.is_ipv4() { 32 } else { 128 } {
                            return Err(KatError::ConfigInvalid);
                        }
                        Ok((ip, prefix))
                    })
                    .collect::<Result<Vec<_>, KatError>>()
            })
            .transpose()?
            .unwrap_or_default();
        let positive = |name: &str, default: u32| -> Result<u32, KatError> {
            let value = optional(name)
                .map(|value| value.parse::<u32>())
                .transpose()
                .map_err(|_| KatError::ConfigInvalid)?
                .unwrap_or(default);
            if value == 0 {
                return Err(KatError::ConfigInvalid);
            }
            Ok(value)
        };
        let transport = TransportConfig {
            header_timeout: Duration::from_secs(
                positive("KATWORK_HEADER_TIMEOUT_SECONDS", 10)?.into(),
            ),
            idle_timeout: Duration::from_secs(positive("KATWORK_IDLE_TIMEOUT_SECONDS", 60)?.into()),
            max_connections: positive("KATWORK_MAX_CONNECTIONS", 256)? as usize,
            max_connections_per_ip: positive("KATWORK_MAX_CONNECTIONS_PER_IP", 32)? as usize,
        };
        let smtp = if let Some(host) = optional("SMTP_HOST") {
            if host.contains('@') {
                return Err(KatError::ConfigInvalid);
            }
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
            demo,
            transport,
            trusted_proxies,
            public_url,
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
            .field("database_url", &redact_url(&self.database_url, true))
            .field("secret", &"[redacted]")
            .field("demo", &self.demo)
            .field("public_url", &self.public_url.as_deref().map(redact_origin))
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
            .field(
                "host",
                &if self.host.contains("://") {
                    redact_origin(&self.host)
                } else {
                    self.host.clone()
                },
            )
            .field("port", &self.port)
            .field("username", &self.username.as_ref().map(|_| "[redacted]"))
            .field("password", &"[redacted]")
            .field("from", &self.from)
            .finish()
    }
}

impl fmt::Debug for ProviderEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderEnv")
            .field("api_key", &"[redacted]")
            .field("base_url", &self.base_url.as_deref().map(redact_origin))
            .finish()
    }
}
