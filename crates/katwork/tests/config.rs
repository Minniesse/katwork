use katwork::config::Config;

#[test]
fn lock_timeout_must_be_a_positive_integer() {
    for (value, millis) in [(None, 5000), (Some("250"), 250)] {
        let config = Config::from_lookup(|name| {
            if name == "KATWORK_LOCK_TIMEOUT_MS" {
                value.map(str::to_owned)
            } else {
                lookup(name)
            }
        })
        .unwrap();
        assert_eq!(
            config.lock_timeout,
            std::time::Duration::from_millis(millis)
        );
    }
    for value in ["0", "x", "", "-1", "+5", "1.5"] {
        let error = Config::from_lookup(|name| {
            if name == "KATWORK_LOCK_TIMEOUT_MS" {
                Some(value.into())
            } else {
                lookup(name)
            }
        })
        .unwrap_err();
        assert_eq!(error.code(), "config_invalid");
    }
}

fn lookup(name: &str) -> Option<String> {
    match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some("test-only-secret-at-least-32-bytes".into()),
        _ => None,
    }
}

#[test]
fn config_from_lookup_validates_required_values() {
    for (database, secret, demo, expected, enabled) in [
        (None, Some("x".repeat(32)), None, "config_invalid", false),
        (
            Some(""),
            Some("x".repeat(32)),
            None,
            "config_invalid",
            false,
        ),
        (
            Some("   "),
            Some("x".repeat(32)),
            None,
            "config_invalid",
            false,
        ),
        (
            Some("postgres://localhost/katwork"),
            None,
            None,
            "config_invalid",
            false,
        ),
        (
            Some("postgres://localhost/katwork"),
            Some("x".repeat(31)),
            None,
            "config_invalid",
            false,
        ),
        (
            Some("postgres://localhost/katwork"),
            Some("x".repeat(32)),
            None,
            "ok",
            false,
        ),
        (
            Some("postgres://localhost/katwork"),
            Some("ก".repeat(11)),
            Some("1"),
            "ok",
            true,
        ),
        (
            Some("postgres://localhost/katwork"),
            Some("x".repeat(32)),
            Some("true"),
            "ok",
            false,
        ),
    ] {
        let result = Config::from_lookup(|name| match name {
            "DATABASE_URL" => database.map(str::to_owned),
            "KATWORK_SECRET" => secret.clone(),
            "KATWORK_DEMO" => demo.map(str::to_owned),
            _ => None,
        });
        match result {
            Ok(config) => {
                assert_eq!(expected, "ok");
                assert_eq!(config.database_url, database.unwrap());
                assert_eq!(config.secret, secret.unwrap());
                assert_eq!(config.demo, enabled);
            }
            Err(error) => {
                assert_eq!(error.code(), expected);
                assert_eq!(error.to_string(), expected);
            }
        }
    }
}

#[test]
fn optional_configuration_requires_host_or_api_key() {
    for empty in [None, Some(String::new())] {
        let config = Config::from_lookup(|name| match name {
            "KATWORK_LOCK_TIMEOUT_MS" => None,
            _ => lookup(name).or_else(|| empty.clone()),
        })
        .unwrap();
        assert!(config.public_url.is_none());
        assert!(config.demo_password.is_none());
        assert!(config.smtp.is_none());
        assert!(config.openai.is_none());
        assert!(config.anthropic.is_none());
    }
    let config = Config::from_lookup(|name| match name {
        "SMTP_PORT" => Some("not-a-port".into()),
        "SMTP_PASSWORD" => Some("smtp-test-password".into()),
        "OPENAI_BASE_URL" | "ANTHROPIC_BASE_URL" => Some("https://example.test".into()),
        _ => lookup(name),
    })
    .unwrap();
    assert!(config.smtp.is_none());
    assert!(config.openai.is_none());
    assert!(config.anthropic.is_none());
}

#[test]
fn optional_configuration_reads_all_fields() {
    let config = Config::from_lookup(|name| match name {
        "KATWORK_PUBLIC_URL" => Some("https://katwork.example.test".into()),
        "KATWORK_DEMO_PASSWORD" => Some("demo-test-password".into()),
        "SMTP_HOST" => Some("smtp.example.test".into()),
        "SMTP_PORT" => Some("587".into()),
        "SMTP_USERNAME" => Some("mailer".into()),
        "SMTP_PASSWORD" => Some("smtp-test-password".into()),
        "SMTP_FROM" => Some("katwork@example.test".into()),
        "OPENAI_API_KEY" => Some("openai-test-key".into()),
        "OPENAI_BASE_URL" => Some("https://openai.example.test".into()),
        "ANTHROPIC_API_KEY" => Some("anthropic-test-key".into()),
        "ANTHROPIC_BASE_URL" => Some("https://anthropic.example.test".into()),
        _ => lookup(name),
    })
    .unwrap();
    assert_eq!(
        config.public_url.as_deref(),
        Some("https://katwork.example.test")
    );
    assert_eq!(config.demo_password.as_deref(), Some("demo-test-password"));
    let smtp = config.smtp.unwrap();
    assert_eq!(smtp.host, "smtp.example.test");
    assert_eq!(smtp.port, Some(587));
    assert_eq!(smtp.username.as_deref(), Some("mailer"));
    assert_eq!(smtp.password.as_deref(), Some("smtp-test-password"));
    assert_eq!(smtp.from.as_deref(), Some("katwork@example.test"));
    let openai = config.openai.unwrap();
    assert_eq!(openai.api_key, "openai-test-key");
    assert_eq!(
        openai.base_url.as_deref(),
        Some("https://openai.example.test")
    );
    let anthropic = config.anthropic.unwrap();
    assert_eq!(anthropic.api_key, "anthropic-test-key");
    assert_eq!(
        anthropic.base_url.as_deref(),
        Some("https://anthropic.example.test")
    );
}

#[test]
fn smtp_port_is_optional_and_must_be_a_valid_port_when_set() {
    for port in [
        None,
        Some(""),
        Some("465"),
        Some("bad"),
        Some("65536"),
        Some("0"),
    ] {
        let result = Config::from_lookup(|name| match name {
            "SMTP_HOST" => Some("smtp.example.test".into()),
            "SMTP_PORT" => port.map(str::to_owned),
            _ => lookup(name),
        });
        match port {
            Some("bad" | "65536" | "0") => {
                assert_eq!(result.err().unwrap().code(), "config_invalid")
            }
            _ => {
                let smtp = result.unwrap().smtp.unwrap();
                assert_eq!(
                    smtp.port,
                    if port == Some("465") { Some(465) } else { None }
                );
                assert!(smtp.username.is_none());
                assert!(smtp.password.is_none());
                assert!(smtp.from.is_none());
            }
        }
    }
}

#[test]
fn config_debug_redacts_secrets() {
    let config = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://katwork:db-test-password@localhost/katwork".into()),
        "KATWORK_DEMO_PASSWORD" => Some("demo-test-password".into()),
        "SMTP_HOST" => Some("smtp.example.test".into()),
        "SMTP_PASSWORD" => Some("smtp-test-password".into()),
        "OPENAI_API_KEY" => Some("openai-test-key".into()),
        "ANTHROPIC_API_KEY" => Some("anthropic-test-key".into()),
        _ => lookup(name),
    })
    .unwrap();
    let debug = format!("{config:?}");
    for secret in [
        "db-test-password",
        "test-only-secret-at-least-32-bytes",
        "demo-test-password",
        "smtp-test-password",
        "openai-test-key",
        "anthropic-test-key",
    ] {
        assert!(!debug.contains(secret), "leaked {secret}: {debug}");
    }
    assert!(debug.contains("database_url: \"postgres://localhost/katwork\""));
    assert!(debug.contains("secret: \"[redacted]\""));
    assert!(debug.contains("password: \"[redacted]\""));
    assert!(debug.contains("api_key: \"[redacted]\""));
    assert!(!format!("{:?}", config.smtp).contains("smtp-test-password"));
    assert!(!format!("{:?}", config.openai).contains("openai-test-key"));
}

#[test]
fn config_debug_rebuilds_database_urls_without_credentials() {
    let password = ["hun", "ter2"].concat();
    let query_password = ["s3", "cret"].concat();
    let ambiguous_host = ["abc", "@h"].concat();
    for (url, expected) in [
        (
            format!("postgres://u:{}@h/db", "p%40ss;word"),
            "postgres://h/db",
        ),
        (
            format!("postgres://u:{}@h/db", "a%2Fb=c"),
            "postgres://h/db",
        ),
        (
            format!("postgres://u:{}@h/db", "päss%40rd"),
            "postgres://h/db",
        ),
        (
            format!("postgres://u:{}@h/db", "pass\tword"),
            "postgres://h/db",
        ),
        (format!("postgres:u:{password}@h/db"), "[redacted]"),
        (format!("postgres:/u:{password}@h/db"), "[redacted]"),
        (
            format!("postgres://u:12345/{ambiguous_host}/db"),
            "[redacted]",
        ),
        (
            format!("postgres://u:12345#{ambiguous_host}/db"),
            "[redacted]",
        ),
        (
            format!("postgres://h/db?sslpassword={query_password}"),
            "postgres://h/db",
        ),
        (
            format!("postgres://h/db?PASSWORD={query_password}"),
            "postgres://h/db",
        ),
        (format!("postgres://u:{}@h/db", "p"), "postgres://h/db"),
        (
            format!("postgres://h/db?password={ambiguous_host}"),
            "[redacted]",
        ),
        (
            format!("postgres://h/db#{query_password}"),
            "postgres://h/db",
        ),
        (format!("https://u:{}@h/db", "password"), "[redacted]"),
        ("not-a-url".into(), "[redacted]"),
        ("postgres:///db".into(), "[redacted]"),
        (
            format!("postgresql://u:{}@h:5433/db", "p"),
            "postgresql://h:5433/db",
        ),
        (
            format!("postgres://u:{}@[::1]:5433/db", "p"),
            "postgres://[::1]:5433/db",
        ),
    ] {
        let mut config = Config::from_lookup(lookup).unwrap();
        config.database_url = url.clone();
        let debug = format!("{config:?}");
        let database = debug
            .split_once("database_url: ")
            .unwrap()
            .1
            .split(',')
            .next()
            .unwrap();
        assert_eq!(database, format!("{expected:?}"), "URL: {url:?}");
    }
}

#[test]
fn config_and_provider_debug_show_only_url_origins() {
    for (url, expected) in [
        ("https://host/path?api-key=x", "https://host"),
        ("https://user:pass@host", "https://host"),
        (
            "https://user:pass@host:8443/path#secret",
            "https://host:8443",
        ),
        ("https://proxy.test/v1/sk-Qq9", "https://proxy.test"),
        (
            "https://proxy.test:8443/v1;api-key=sk-Qq9",
            "https://proxy.test:8443",
        ),
        ("https://host/path@secret", "[redacted]"),
        ("not-a-url", "[redacted]"),
    ] {
        let mut config = Config::from_lookup(|name| match name {
            "OPENAI_API_KEY" | "ANTHROPIC_API_KEY" => Some(uuid::Uuid::now_v7().to_string()),
            _ => lookup(name),
        })
        .unwrap();
        // Debug must redact even malformed URLs supplied through direct construction.
        config.public_url = Some(url.into());
        config.openai.as_mut().unwrap().base_url = Some(url.into());
        config.anthropic.as_mut().unwrap().base_url = Some(url.into());
        let debug = format!("{config:?}");
        assert!(
            debug.contains(&format!("public_url: Some({expected:?})")),
            "{debug}"
        );
        for provider in [config.openai.unwrap(), config.anthropic.unwrap()] {
            assert_eq!(
                format!("{provider:?}"),
                format!("ProviderEnv {{ api_key: \"[redacted]\", base_url: Some({expected:?}) }}")
            );
        }
    }
}

#[test]
fn smtp_debug_redacts_url_credentials() {
    for (host, expected) in [
        (
            "smtps://mailer:sm7p-pass@smtp.test:465",
            "smtps://smtp.test:465",
        ),
        ("smtp.test", "smtp.test"),
    ] {
        let mut config = Config::from_lookup(|name| match name {
            "SMTP_HOST" => Some("smtp.test".into()),
            _ => lookup(name),
        })
        .unwrap();
        config.smtp.as_mut().unwrap().host = host.into();
        let debug = format!("{:?}", config.smtp.unwrap());
        assert!(debug.contains(&format!("host: {expected:?}")), "{debug}");
        assert!(!debug.contains("sm7p-pass"), "{debug}");
        assert!(!debug.contains("mailer"), "{debug}");
    }
}

#[test]
fn redaction_handles_encoded_query_and_malformed_database_passwords() {
    for (url, message, expected) in [
        (
            "postgres://user:db%22pass%5Cword@localhost/db",
            r#"failed db\"pass\\word"#,
            "failed [redacted]",
        ),
        (
            "postgres://user:p%40ss%3Aword@localhost/db",
            "failed p%40ss%3Aword or p@ss:word",
            "failed [redacted] or [redacted]",
        ),
        (
            "postgres://user@localhost/db?password=query%2Bsecret",
            "failed query%2Bsecret or query+secret",
            "failed [redacted] or [redacted]",
        ),
        (
            "postgres://user:malformed-secret@localhost:bad/db",
            "failed malformed-secret",
            "[redacted]",
        ),
        (
            "postgres://localhost/db",
            "connection refused",
            "connection refused",
        ),
    ] {
        let config = Config::from_lookup(|name| match name {
            "DATABASE_URL" => Some(url.into()),
            _ => lookup(name),
        })
        .unwrap();
        assert_eq!(config.redact_secrets(message), expected);
        let debug = format!("{config:?}");
        for secret in [
            "p%40ss%3Aword",
            "p@ss:word",
            "query%2Bsecret",
            "query+secret",
            "malformed-secret",
        ] {
            assert!(!debug.contains(secret));
        }
    }
}

#[test]
fn error_redaction_scrubs_raw_encoded_and_decoded_database_parts() {
    let query_password = ["s3", "cret"].concat();
    let cases: &[(String, &[&str])] = &[
        (
            format!("postgres://u:{}@h/db", "p%40ss;word"),
            &["p%40ss;word", "p@ss;word", "p%40ss%3Bword", "u:p%40ss;word"],
        ),
        (
            format!("postgres://u:{}@h/db", "a%2Fb=c"),
            &["a%2Fb=c", "a/b=c", "a%2Fb%3Dc"],
        ),
        (
            format!("postgres://u:{}@h/db", "päss%40rd"),
            &["päss%40rd", "päss@rd", "p%C3%A4ss%40rd"],
        ),
        (
            format!("postgres://u:{}@h/db", "pass\tword"),
            &["pass\tword", "password", "pass\\tword", "u:pass\tword"],
        ),
        (
            format!("postgres://h/db?sslpassword={query_password}"),
            &[&query_password],
        ),
        (
            format!("postgres://h/db?PASSWORD={query_password}"),
            &[&query_password],
        ),
        (
            "postgres://h/db?sslmode=query%2Bsecret".into(),
            &["query%2Bsecret", "query+secret"],
        ),
        (
            "postgres://h/db?unknown=query+secret".into(),
            &["query+secret", "query secret"],
        ),
    ];
    for (url, secrets) in cases {
        let mut config = Config::from_lookup(lookup).unwrap();
        config.database_url = url.clone();
        for &secret in *secrets {
            let redacted = config.redact_secrets(&format!("failed {secret}"));
            assert!(redacted.contains("[redacted]"), "{redacted:?}");
            assert!(
                !redacted.contains(secret),
                "leaked {secret:?}: {redacted:?}"
            );
        }
        let redacted_url = config.redact_secrets(&format!("failed {url}"));
        assert!(!redacted_url.contains(url));
        for &secret in *secrets {
            assert!(!redacted_url.contains(secret), "{redacted_url:?}");
        }
    }
}

#[test]
fn error_redaction_fails_closed_for_ambiguous_database_urls() {
    let password = ["hun", "ter2"].concat();
    let ambiguous_host = ["abc", "@h"].concat();
    for url in [
        format!("postgres:u:{password}@h/db"),
        format!("postgres:/u:{password}@h/db"),
        format!("postgres://u:12345/{ambiguous_host}/db"),
        format!("postgres://u:12345#{ambiguous_host}/db"),
        format!("postgres://h/db?password={ambiguous_host}"),
        format!("https://u:{}@h/db", "password"),
    ] {
        let mut config = Config::from_lookup(lookup).unwrap();
        config.database_url = url;
        assert_eq!(
            config.redact_secrets(&format!("database u:{password}@h/db does not exist")),
            "[redacted]"
        );
    }
}

#[test]
fn error_redaction_rebuilds_embedded_urls() {
    let config = Config::from_lookup(lookup).unwrap();
    let redacted = config.redact_secrets(
        "failed (https://user:pass@host/path?api-key=x#hidden) url=postgres://other:other-secret@db/name https://proxy/path?api-key=key-secret",
    );
    for secret in [
        "user",
        "pass",
        "api-key",
        "hidden",
        "other-secret",
        "key-secret",
    ] {
        assert!(!redacted.contains(secret), "leaked {secret}: {redacted}");
    }
    assert!(redacted.contains("https://host/path"), "{redacted}");
    assert!(redacted.contains("postgres://db/name"), "{redacted}");
    assert!(redacted.contains("https://proxy/path"), "{redacted}");
}

#[test]
fn short_database_password_does_not_prevent_other_url_redaction() {
    let mut config = Config::from_lookup(lookup).unwrap();
    config.database_url = "postgres://u:s@h/db".into();
    let redacted = config.redact_secrets("failed https://proxy/path?api-key=unrelated-key");
    assert!(!redacted.contains("unrelated-key"), "{redacted}");
}

#[test]
fn secret_matching_url_syntax_does_not_hide_other_url_credentials() {
    for password in ["s%3A%2F%2F", "%2F%2Fus"] {
        let mut config = Config::from_lookup(lookup).unwrap();
        config.database_url = format!("postgres://u:{password}@h/db");
        let redacted = config
            .redact_secrets("failed https://user:unrelated-pass@proxy/path?api-key=unrelated-key");
        for secret in ["unrelated-pass", "unrelated-key"] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
        assert!(redacted.starts_with("failed "));
    }
}

#[test]
fn error_redaction_preserves_common_query_values() {
    for query in [
        "sslmode=require",
        "sslmode=disable",
        "sslmode=r%65quire",
        "connect_timeout=5",
        "port=5432",
    ] {
        let mut config = Config::from_lookup(lookup).unwrap();
        config.database_url = format!("postgres://u:pw@h/db?{query}");
        let message = "TLS required; disable retries; port 5432";
        assert_eq!(config.redact_secrets(message), message, "{query}");
    }
    for password in ["require", "disable", "5432"] {
        let mut config = Config::from_lookup(lookup).unwrap();
        for url in [
            format!("postgres://u:{password}@h/db"),
            format!("postgres://h/db?PASSWORD={password}"),
        ] {
            config.database_url = url;
            assert_eq!(config.redact_secrets(password), "[redacted]");
        }
    }
}

#[test]
fn error_redaction_matches_longest_secret_without_rescanning_markers() {
    let mut config = Config::from_lookup(lookup).unwrap();
    config.database_url = "postgres://u:redacted-long@h/db?options=redacted".into();
    config.demo_password = Some("acted".into());
    assert_eq!(
        config.redact_secrets("failed redacted-long redacted acted"),
        "failed [redacted] [redacted] [redacted]"
    );
}

#[test]
fn generated_database_urls_have_bounded_redaction_output() {
    let values = [
        "redacted",
        "e",
        "d",
        "require",
        "5",
        "%72edacted",
        "5432",
        "",
    ];
    for i in 0..200 {
        let password = format!("fixture-{i:03}-redacted");
        let value = values[i % values.len()];
        let mut config = Config::from_lookup(lookup).unwrap();
        config.database_url =
            format!("postgres://u:{password}@h/db?options={value}&x={value}&sslmode={value}");
        config.demo_password = Some("acted".into());
        let decoded = percent_encoding::percent_decode_str(value).decode_utf8_lossy();
        let message = format!("failed {password} / {decoded} / {password} ภาษาไทย");
        // The original message has at most three non-overlapping secret matches.
        let redacted = config.redact_secrets(&message);
        assert!(redacted.len() <= message.len() + 10 * 3, "case {i}");
        assert!(!redacted.contains(&password), "case {i}");
        assert!(redacted.ends_with("ภาษาไทย"));
    }
}

#[test]
fn error_redaction_removes_decoded_options_before_rebuilding_urls() {
    let password = ["pw", "9x"].concat();
    let mut config = Config::from_lookup(lookup).unwrap();
    config.database_url = format!("postgres://h/db?options=-c%20password%3D{password}");
    let message = format!("failed postgres://h/db?options=-c password={password}");
    for redacted in [
        config.redact_secrets(&message),
        katwork::config::redact_database_secrets(&message, &config.database_url),
    ] {
        assert!(!redacted.contains(&password), "{redacted}");
        assert!(redacted.starts_with("failed "), "{redacted}");
    }
}

#[test]
fn error_redaction_preserves_dns_and_tls_diagnostics() {
    let mut config = Config::from_lookup(lookup).unwrap();
    config.database_url = "postgres://u:pw@h/db?sslmode=require".into();
    for message in [
        "error communicating with database: failed to lookup address information: nodename nor servname provided, or not known",
        "received fatal alert: HandshakeFailure",
    ] {
        assert_eq!(config.redact_secrets(message), message);
        let report = config.redact_secrets(&format!("{message} ({})", config.database_url));
        assert!(report.starts_with(message), "{report}");
        assert!(!report.contains("pw"), "{report}");
    }
}

#[test]
fn error_redaction_limits_malformed_url_replacement_to_the_url() {
    let config = Config::from_lookup(lookup).unwrap();
    assert_eq!(
        config.redact_secrets("failed (https://u:pass@host:bad/path) then retry"),
        "failed ([redacted]) then retry"
    );
}

#[test]
fn rejects_smtp_host_with_embedded_credentials() {
    let password = uuid::Uuid::now_v7().to_string();
    for host in [
        format!("mailer:{password}@smtp.test:465"),
        format!("smtps://mailer:{password}@smtp.test:465"),
    ] {
        let result = Config::from_lookup(|name| match name {
            "SMTP_HOST" => Some(host.clone()),
            _ => lookup(name),
        });
        assert_eq!(result.unwrap_err().code(), "config_invalid");
    }
}

#[test]
fn rejects_short_smtp_and_demo_passwords() {
    for variable in ["SMTP_PASSWORD", "KATWORK_DEMO_PASSWORD"] {
        for size in 1..8 {
            let value: String = uuid::Uuid::now_v7()
                .to_string()
                .chars()
                .take(size)
                .collect();
            let result = Config::from_lookup(|name| {
                if name == variable {
                    Some(value.clone())
                } else {
                    lookup(name)
                }
            });
            assert_eq!(result.unwrap_err().code(), "config_invalid");
        }
        let value = "ก".repeat(7);
        let result = Config::from_lookup(|name| {
            if name == variable {
                Some(value.clone())
            } else {
                lookup(name)
            }
        });
        assert_eq!(result.unwrap_err().code(), "config_invalid");
    }
}

#[test]
fn rejects_public_urls_that_cannot_safely_define_an_origin() {
    for url in [
        "not-a-url",
        "ftp://host",
        "https://user@host",
        "https://host/path",
        "https://host/?x=y",
        "https://host/#fragment",
    ] {
        let result = Config::from_lookup(|name| match name {
            "KATWORK_PUBLIC_URL" => Some(url.into()),
            _ => lookup(name),
        });
        assert_eq!(result.unwrap_err().code(), "config_invalid");
    }
}

#[test]
fn https_scheme_is_canonicalized_before_cookie_security() {
    let config = Config::from_lookup(|name| match name {
        "KATWORK_PUBLIC_URL" => Some("HTTPS://KATWORK.EXAMPLE:443/".into()),
        _ => lookup(name),
    })
    .unwrap();
    assert!(config.cookie_secure());
    assert_eq!(config.public_url_or_default(), "https://katwork.example");
}

#[test]
fn demo_on_https_requires_explicit_override() {
    for allow in [
        None,
        Some(""),
        Some("0"),
        Some("false"),
        Some(" 1"),
        Some("1"),
    ] {
        let result = Config::from_lookup(|name| match name {
            "KATWORK_DEMO" => Some("1".into()),
            "KATWORK_PUBLIC_URL" => Some("HTTPS://katwork.example".into()),
            "KATWORK_ALLOW_DEMO_ON_HTTPS" => allow.map(str::to_owned),
            _ => lookup(name),
        });
        if allow == Some("1") {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.unwrap_err().code(), "config_invalid");
        }
    }
}

#[test]
fn trusted_proxy_config_rejects_invalid_cidrs() {
    for value in [
        "127.0.0.1",
        "127.0.0.1/33",
        "::1/129",
        "evil/8",
        "10.0.0.0/8,,::1/128",
    ] {
        let result = Config::from_lookup(|name| match name {
            "KATWORK_TRUSTED_PROXIES" => Some(value.into()),
            _ => lookup(name),
        });
        assert_eq!(result.unwrap_err().code(), "config_invalid", "{value}");
    }
}

#[test]
fn trusted_proxy_cidrs_match_only_the_configured_networks() {
    let empty = Config::from_lookup(lookup).unwrap();
    assert!(!empty.trusts_proxy("127.0.0.1".parse().unwrap()));
    for (networks, yes, no) in [
        (
            "192.0.2.0/24, 2001:db8::/32",
            vec!["192.0.2.0", "192.0.2.255", "2001:db8::1"],
            vec!["192.0.3.0", "2001:db9::1", "::ffff:192.0.2.1"],
        ),
        (
            "127.0.0.1/32, ::1/128",
            vec!["127.0.0.1", "::1"],
            vec!["127.0.0.2", "::2"],
        ),
        ("0.0.0.0/0", vec!["192.0.2.1"], vec!["::1"]),
        ("::/0", vec!["2001:db8::1"], vec!["192.0.2.1"]),
    ] {
        let config = Config::from_lookup(|name| match name {
            "KATWORK_TRUSTED_PROXIES" => Some(networks.into()),
            _ => lookup(name),
        })
        .unwrap();
        for ip in yes {
            assert!(config.trusts_proxy(ip.parse().unwrap()), "{ip}");
        }
        for ip in no {
            assert!(!config.trusts_proxy(ip.parse().unwrap()), "{ip}");
        }
    }
}

#[test]
fn transport_settings_reject_zero_and_invalid_values() {
    for setting in [
        "KATWORK_HEADER_TIMEOUT_SECONDS",
        "KATWORK_IDLE_TIMEOUT_SECONDS",
        "KATWORK_MAX_CONNECTIONS",
        "KATWORK_MAX_CONNECTIONS_PER_IP",
    ] {
        for value in ["0", "-1", "false", "18446744073709551615"] {
            let result = Config::from_lookup(|name| {
                if name == setting {
                    Some(value.into())
                } else {
                    lookup(name)
                }
            });
            assert_eq!(
                result.unwrap_err().code(),
                "config_invalid",
                "{setting}={value}"
            );
        }
    }
}

#[test]
fn transport_defaults_and_overrides() {
    let config = Config::from_lookup(lookup).unwrap();
    assert_eq!(config.transport.header_timeout.as_secs(), 10);
    assert_eq!(config.transport.idle_timeout.as_secs(), 60);
    assert_eq!(config.transport.max_connections, 256);
    assert_eq!(config.transport.max_connections_per_ip, 32);
    let config = Config::from_lookup(|name| match name {
        "KATWORK_HEADER_TIMEOUT_SECONDS" => Some("3".into()),
        "KATWORK_IDLE_TIMEOUT_SECONDS" => Some("4".into()),
        "KATWORK_MAX_CONNECTIONS" => Some("5".into()),
        "KATWORK_MAX_CONNECTIONS_PER_IP" => Some("2".into()),
        _ => lookup(name),
    })
    .unwrap();
    assert_eq!(config.transport.header_timeout.as_secs(), 3);
    assert_eq!(config.transport.idle_timeout.as_secs(), 4);
    assert_eq!(config.transport.max_connections, 5);
    assert_eq!(config.transport.max_connections_per_ip, 2);
}

#[test]
fn smtp_and_enclosing_config_debug_redact_username_tokens() {
    let token = uuid::Uuid::now_v7().to_string();
    let config = Config::from_lookup(|name| match name {
        "SMTP_HOST" => Some("smtp.example.test".into()),
        "SMTP_USERNAME" | "SMTP_PASSWORD" => Some(token.clone()),
        _ => lookup(name),
    })
    .unwrap();
    for debug in [
        format!("{:?}", config.smtp.as_ref().unwrap()),
        format!("{config:?}"),
    ] {
        assert!(!debug.contains(&token), "SMTP credential leaked in Debug");
        assert!(debug.contains("username: Some(\"[redacted]\")"));
        assert!(debug.contains("password: \"[redacted]\""));
    }
    let mut smtp = config.smtp.unwrap();
    smtp.username = None;
    assert!(format!("{smtp:?}").contains("username: None"));
}

#[test]
fn error_redaction_scrubs_all_configured_credentials() {
    let names = [
        "KATWORK_SECRET",
        "KATWORK_DEMO_PASSWORD",
        "SMTP_PASSWORD",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
    ];
    let secrets: Vec<_> = names
        .iter()
        .map(|_| uuid::Uuid::now_v7().to_string())
        .collect();
    let config = Config::from_lookup(|name| {
        names
            .iter()
            .position(|key| *key == name)
            .map(|i| secrets[i].clone())
            .or_else(|| match name {
                "SMTP_HOST" => Some("smtp.example.invalid".into()),
                _ => lookup(name),
            })
    })
    .unwrap();
    let diagnostic = format!("provider diagnostic {} end", secrets.join(" "));
    let redacted = config.redact_secrets(&diagnostic);
    assert!(redacted.starts_with("provider diagnostic "));
    assert!(redacted.ends_with(" end"));
    for secret in secrets {
        assert!(!redacted.contains(&secret));
    }
    assert_eq!(redacted.matches("[redacted]").count(), names.len());
}

#[test]
fn local_base_url_and_model_must_be_set_together() {
    for (present, value, missing) in [
        (
            "KATWORK_LOCAL_BASE_URL",
            "http://localhost:8000/v1",
            "KATWORK_LOCAL_MODEL",
        ),
        (
            "KATWORK_LOCAL_MODEL",
            "local-model",
            "KATWORK_LOCAL_BASE_URL",
        ),
    ] {
        let error = Config::from_lookup(|name| {
            if name == present {
                Some(value.into())
            } else {
                lookup(name)
            }
        })
        .unwrap_err();
        assert_eq!(error.code(), "config_invalid");
        assert!(format!("{error:?}").contains(missing));
        assert!(!format!("{error:?}").contains(value));
    }
}

#[test]
fn local_base_url_must_be_http_or_https() {
    for bad in [
        "garbage",
        "file:///tmp",
        "ftp://host",
        "https://",
        "mailto:user@example.test",
    ] {
        let error = Config::from_lookup(|name| match name {
            "KATWORK_LOCAL_BASE_URL" => Some(bad.into()),
            "KATWORK_LOCAL_MODEL" => Some("local-model".into()),
            _ => lookup(name),
        })
        .unwrap_err();
        assert_eq!(error.code(), "config_invalid");
        assert!(format!("{error:?}").contains("KATWORK_LOCAL_BASE_URL"));
        assert!(!format!("{error:?}").contains(bad));
    }
}

#[test]
fn local_base_url_rejects_query() {
    rejects_local_base_url(&format!(
        "http://localhost:8000/v1?token={}",
        uuid::Uuid::now_v7()
    ));
}

#[test]
fn local_base_url_rejects_fragment() {
    rejects_local_base_url("http://localhost:8000/v1#fragment");
}

#[test]
fn local_base_url_rejects_username() {
    rejects_local_base_url("http://operator@localhost:8000/v1");
}

#[test]
fn local_base_url_rejects_password() {
    rejects_local_base_url(&format!(
        "http://:{}@localhost:8000/v1",
        uuid::Uuid::now_v7()
    ));
}

fn rejects_local_base_url(value: &str) {
    let error = Config::from_lookup(|name| match name {
        "KATWORK_LOCAL_BASE_URL" => Some(value.into()),
        "KATWORK_LOCAL_MODEL" => Some("local-model".into()),
        _ => lookup(name),
    })
    .unwrap_err();
    assert!(matches!(
        error,
        katwork::error::KatError::ConfigInvalid {
            var: "KATWORK_LOCAL_BASE_URL"
        }
    ));
    assert_eq!(error.to_string(), "config_invalid");
    assert!(!format!("{error:?}").contains(value));
}

#[test]
fn local_api_key_is_redacted_in_debug() {
    let key = uuid::Uuid::now_v7().to_string();
    let password = uuid::Uuid::now_v7().to_string();
    let query = uuid::Uuid::now_v7().to_string();
    let url = format!("https://user:{password}@local.test:8443/private?token={query}");
    let mut cfg = Config::from_lookup(|name| match name {
        "KATWORK_LOCAL_BASE_URL" => Some("https://local.test:8443/v1".into()),
        "KATWORK_LOCAL_MODEL" => Some("local-model".into()),
        "KATWORK_LOCAL_API_KEY" => Some(key.clone()),
        _ => lookup(name),
    })
    .unwrap();
    // Debug must redact even manually constructed configuration.
    cfg.local.as_mut().unwrap().base_url = url.clone();
    let local = cfg.local.as_ref().unwrap();
    assert_eq!(local.base_url, url);
    assert_eq!(local.model, "local-model");
    assert_eq!(local.api_key.as_ref(), Some(&key));
    for debug in [format!("{cfg:?}"), format!("{local:?}")] {
        assert!(debug.contains("https://local.test:8443"));
        for secret in [&key, &password, &query, "/private", "user:"] {
            assert!(!debug.contains(secret), "secret leaked");
        }
    }
    let redacted = cfg.redact_secrets(&format!("key={key} url={url}"));
    for secret in [&key, &password, &query] {
        assert!(!redacted.contains(secret));
    }
}

#[test]
fn config_invalid_names_variable_without_value() {
    for var in [
        "KATWORK_SECRET",
        "DATABASE_URL",
        "SMTP_PORT",
        "KATWORK_HEADER_TIMEOUT_SECONDS",
        "KATWORK_TRUSTED_PROXIES",
    ] {
        let error = Config::from_lookup(|name| match name {
            "SMTP_HOST" => Some("localhost".into()),
            n if n == var => Some(" ".into()),
            _ => lookup(name),
        })
        .unwrap_err();
        assert!(
            matches!(error, katwork::error::KatError::ConfigInvalid { var: actual } if actual == var)
        );
        assert_eq!(error.to_string(), "config_invalid");
    }
}
