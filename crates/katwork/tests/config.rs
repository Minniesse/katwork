use katwork::config::Config;

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
        let config = Config::from_lookup(|name| lookup(name).or_else(|| empty.clone())).unwrap();
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
    assert!(debug.contains("postgres://katwork:[redacted]@localhost/katwork"));
    assert!(debug.contains("secret: \"[redacted]\""));
    assert!(debug.contains("password: \"[redacted]\""));
    assert!(debug.contains("api_key: \"[redacted]\""));
    assert!(!format!("{:?}", config.smtp).contains("smtp-test-password"));
    assert!(!format!("{:?}", config.openai).contains("openai-test-key"));
}

#[test]
fn redaction_handles_encoded_query_and_malformed_database_passwords() {
    for (url, message, expected) in [
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
