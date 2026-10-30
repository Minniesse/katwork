use std::{
    collections::BTreeSet, env, fmt, net::IpAddr, ops::RangeInclusive, path::PathBuf,
    time::Duration,
};

use percent_encoding::percent_decode_str;
use url::Url;

use crate::error::KatError;

/// Where uploaded and written files live when `KATWORK_ARTIFACT_DIR` is unset: the Compose
/// volume's mount point.
pub const DEFAULT_ARTIFACT_DIR: &str = "/var/lib/katwork/artifacts";

pub struct Config {
    pub lock_timeout: Duration,
    /// `KATWORK_DOC_WORKERS`: the document pool's threads, 1 to 16.
    pub doc_workers: usize,
    /// `KATWORK_DOC_MAX_XML_ELEMENTS`: the XML elements one document may hold across its parts
    /// (`DocLimits::max_xml_elements`), 1,000 to 20,000,000.
    pub doc_max_xml_elements: u64,
    /// `KATWORK_DOC_MAX_READ_BYTES`: the bytes one document read may hold at once
    /// (`DocLimits::max_read_bytes`), 64 MiB to 64 GiB.
    pub doc_max_read_bytes: u64,
    /// `KATWORK_ARTIFACT_DIR`: the blob store's root, an absolute path.
    pub artifact_dir: PathBuf,
    /// `KATWORK_UPLOAD_MAX_BYTES`: the upload cap, 1 to 26,214,400 (the readers' input cap,
    /// `DocLimits::max_input_bytes`).
    pub upload_max_bytes: u64,
    /// `KATWORK_WORKSPACE_STORAGE_BYTES`: one workspace's storage quota, at least 1.
    pub workspace_storage_bytes: u64,
    /// `KATWORK_UPLOAD_MIN_BYTES_PER_SEC`: the upload rate floor; 0 turns it off.
    pub upload_min_bytes_per_sec: u64,
    /// `KATWORK_UPLOAD_RATE_WINDOW_MS`: the window the rate floor is checked over, 100 ms to
    /// 300 s (the upload timeout).
    pub upload_rate_window: Duration,
    pub database_url: String,
    pub secret: String,
    pub demo: bool,
    pub transport: TransportConfig,
    pub public_url: Option<String>,
    pub demo_password: Option<String>,
    pub smtp: Option<SmtpConfig>,
    pub openai: Option<ProviderEnv>,
    pub anthropic: Option<ProviderEnv>,
    pub local: Option<LocalEnv>,
}

#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub header_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_connections: usize,
    pub max_connections_per_ip: usize,
    /// `KATWORK_TRUSTED_PROXIES` as (network, prefix) pairs. A trusted proxy may supply
    /// `X-Forwarded-For` and is exempt from the per-peer connection cap.
    pub trusted_proxies: Vec<(IpAddr, u8)>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            header_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(60),
            max_connections: 256,
            max_connections_per_ip: 32,
            trusted_proxies: Vec::new(),
        }
    }
}

impl TransportConfig {
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
}

#[derive(Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: Option<String>,
}

pub struct LocalEnv {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
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
        self.transport.trusts_proxy(ip)
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
                self.local
                    .as_ref()
                    .and_then(|local| local.api_key.as_deref()),
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
        // Every failure names its variable and drops the parse error, which may quote the value.
        let invalid = |var: &'static str| KatError::ConfigInvalid { var };
        let database_url = lookup("DATABASE_URL")
            .filter(|value| !value.trim().is_empty())
            .ok_or(invalid("DATABASE_URL"))?;
        let secret = lookup("KATWORK_SECRET")
            .filter(|value| value.len() >= 32)
            .ok_or(invalid("KATWORK_SECRET"))?;
        let optional = |name: &str| lookup(name).filter(|value| !value.is_empty());
        for key in [
            "OPENAI_BASE_URL",
            "ANTHROPIC_BASE_URL",
            "KATWORK_LOCAL_BASE_URL",
        ] {
            if let Some(value) = optional(key) {
                let url = Url::parse(&value).map_err(|_| invalid(key))?;
                if !matches!(url.scheme(), "http" | "https") || !url.has_host() {
                    return Err(invalid(key));
                }
                if key == "KATWORK_LOCAL_BASE_URL"
                    && (!url.username().is_empty()
                        || url.password().is_some()
                        || url.query().is_some()
                        || url.fragment().is_some())
                {
                    return Err(invalid(key));
                }
            }
        }
        for name in ["SMTP_PASSWORD", "KATWORK_DEMO_PASSWORD"] {
            if optional(name).is_some_and(|value| value.chars().count() < 8) {
                return Err(invalid(name));
            }
        }
        let mut public_url = optional("KATWORK_PUBLIC_URL");
        if let Some(value) = &public_url {
            let url = Url::parse(value).map_err(|_| invalid("KATWORK_PUBLIC_URL"))?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.has_host()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(invalid("KATWORK_PUBLIC_URL"));
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
            return Err(invalid("KATWORK_ALLOW_DEMO_ON_HTTPS"));
        }
        // Each entry is a CIDR or a bare IP, which means that one address.
        let trusted_proxies = optional("KATWORK_TRUSTED_PROXIES")
            .map(|value| {
                value
                    .split(',')
                    .map(|entry| {
                        let (ip, prefix) = match entry.trim().split_once('/') {
                            Some((ip, prefix)) => (ip, Some(prefix)),
                            None => (entry.trim(), None),
                        };
                        let ip: IpAddr =
                            ip.parse().map_err(|_| invalid("KATWORK_TRUSTED_PROXIES"))?;
                        let bits = if ip.is_ipv4() { 32 } else { 128 };
                        let prefix: u8 = prefix
                            .map(str::parse)
                            .transpose()
                            .map_err(|_| invalid("KATWORK_TRUSTED_PROXIES"))?
                            .unwrap_or(bits);
                        if prefix > bits {
                            return Err(invalid("KATWORK_TRUSTED_PROXIES"));
                        }
                        Ok((ip, prefix))
                    })
                    .collect::<Result<Vec<_>, KatError>>()
            })
            .transpose()?
            .unwrap_or_default();
        let positive = |name: &'static str, default: u32| -> Result<u32, KatError> {
            let value = optional(name)
                .map(|value| value.parse::<u32>())
                .transpose()
                .map_err(|_| invalid(name))?
                .unwrap_or(default);
            if value == 0 {
                return Err(invalid(name));
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
            trusted_proxies,
        };
        let lock_timeout = lock_timeout(optional("KATWORK_LOCK_TIMEOUT_MS"))?;
        let doc_workers = match optional("KATWORK_DOC_WORKERS") {
            None => 2,
            Some(value) => value
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                .then(|| value.parse::<usize>().ok())
                .flatten()
                .filter(|workers| (1..=16).contains(workers))
                .ok_or(invalid("KATWORK_DOC_WORKERS"))?,
        };
        let doc_max_xml_elements = match optional("KATWORK_DOC_MAX_XML_ELEMENTS") {
            None => crate::docs::DocLimits::default().max_xml_elements,
            Some(value) => value
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                .then(|| value.parse::<u64>().ok())
                .flatten()
                .filter(|elements| (1_000..=20_000_000).contains(elements))
                .ok_or(invalid("KATWORK_DOC_MAX_XML_ELEMENTS"))?,
        };
        let doc_max_read_bytes = match optional("KATWORK_DOC_MAX_READ_BYTES") {
            None => crate::docs::DocLimits::default().max_read_bytes,
            Some(value) => value
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                .then(|| value.parse::<u64>().ok())
                .flatten()
                .filter(|bytes| (67_108_864..=68_719_476_736).contains(bytes))
                .ok_or(invalid("KATWORK_DOC_MAX_READ_BYTES"))?,
        };
        let digits = |name: &'static str,
                      default: u64,
                      range: RangeInclusive<u64>|
         -> Result<u64, KatError> {
            match optional(name) {
                None => Ok(default),
                Some(value) => value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
                    .then(|| value.parse::<u64>().ok())
                    .flatten()
                    .filter(|value| range.contains(value))
                    .ok_or(invalid(name)),
            }
        };
        let artifact_dir = optional("KATWORK_ARTIFACT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_ARTIFACT_DIR));
        if !artifact_dir.is_absolute() {
            return Err(invalid("KATWORK_ARTIFACT_DIR"));
        }
        // A larger upload would be one no reader opens.
        let max_input_bytes = crate::docs::DocLimits::default().max_input_bytes;
        let upload_max_bytes = digits(
            "KATWORK_UPLOAD_MAX_BYTES",
            max_input_bytes,
            1..=max_input_bytes,
        )?;
        let workspace_storage_bytes = digits(
            "KATWORK_WORKSPACE_STORAGE_BYTES",
            10_737_418_240,
            1..=u64::MAX,
        )?;
        let upload_min_bytes_per_sec =
            digits("KATWORK_UPLOAD_MIN_BYTES_PER_SEC", 32_768, 0..=u64::MAX)?;
        let upload_rate_window = Duration::from_millis(digits(
            "KATWORK_UPLOAD_RATE_WINDOW_MS",
            10_000,
            100..=300_000,
        )?);
        let smtp = if let Some(host) = optional("SMTP_HOST") {
            if host.contains('@') {
                return Err(invalid("SMTP_HOST"));
            }
            let port = optional("SMTP_PORT")
                .map(|value| value.parse::<u16>().map_err(|_| invalid("SMTP_PORT")))
                .transpose()?;
            if port == Some(0) {
                return Err(invalid("SMTP_PORT"));
            }
            // A mailed link must never point at the localhost default.
            if public_url.is_none() {
                return Err(invalid("KATWORK_PUBLIC_URL"));
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
        let local = match (
            optional("KATWORK_LOCAL_BASE_URL"),
            optional("KATWORK_LOCAL_MODEL"),
        ) {
            (None, None) => None,
            (Some(_), None) => return Err(invalid("KATWORK_LOCAL_MODEL")),
            (None, Some(_)) => return Err(invalid("KATWORK_LOCAL_BASE_URL")),
            (Some(base_url), Some(model)) => Some(LocalEnv {
                base_url,
                model,
                api_key: optional("KATWORK_LOCAL_API_KEY"),
            }),
        };
        Ok(Self {
            lock_timeout,
            doc_workers,
            doc_max_xml_elements,
            doc_max_read_bytes,
            artifact_dir,
            upload_max_bytes,
            workspace_storage_bytes,
            upload_min_bytes_per_sec,
            upload_rate_window,
            local,
            database_url,
            secret,
            demo,
            transport,
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
            .field("local", &self.local)
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

impl fmt::Debug for LocalEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalEnv")
            .field("base_url", &redact_origin(&self.base_url))
            .field("model", &self.model)
            .field("api_key", &"[redacted]")
            .finish()
    }
}

pub(crate) fn lock_timeout(value: Option<String>) -> Result<Duration, KatError> {
    match value.filter(|value| !value.is_empty()) {
        None => Ok(Duration::from_millis(5000)),
        Some(value) => {
            if !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(KatError::ConfigInvalid {
                    var: "KATWORK_LOCK_TIMEOUT_MS",
                });
            }
            let millis: u32 = value.parse().map_err(|_| KatError::ConfigInvalid {
                var: "KATWORK_LOCK_TIMEOUT_MS",
            })?;
            if millis == 0 || millis > i32::MAX as u32 {
                return Err(KatError::ConfigInvalid {
                    var: "KATWORK_LOCK_TIMEOUT_MS",
                });
            }
            Ok(Duration::from_millis(millis.into()))
        }
    }
}
