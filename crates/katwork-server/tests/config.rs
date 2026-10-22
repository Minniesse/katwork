use std::process::Command;

use katwork_server::config::Config;

// Child processes keep environment changes out of the parallel test runner.
#[test]
fn config_from_env() {
    if let Ok(expected) = std::env::var("KATWORK_TEST_EXPECTED") {
        match Config::from_env() {
            Ok(config) => {
                assert_eq!(expected, "ok");
                assert_eq!(config.database_url, "postgres://localhost/katwork");
                assert_eq!(config.secret, std::env::var("KATWORK_SECRET").unwrap());
                assert_eq!(
                    config.demo,
                    std::env::var("KATWORK_TEST_DEMO").unwrap() == "1"
                );
            }
            Err(error) => {
                assert_eq!(error.code(), expected);
                assert_eq!(error.to_string(), expected);
            }
        }
        return;
    }

    for (database, secret, demo, expected, enabled) in [
        (None, Some("x".repeat(32)), None, "config_invalid", "0"),
        (Some(""), Some("x".repeat(32)), None, "config_invalid", "0"),
        (
            Some("   "),
            Some("x".repeat(32)),
            None,
            "config_invalid",
            "0",
        ),
        (
            Some("postgres://localhost/katwork"),
            None,
            None,
            "config_invalid",
            "0",
        ),
        (
            Some("postgres://localhost/katwork"),
            Some("x".repeat(31)),
            None,
            "config_invalid",
            "0",
        ),
        (
            Some("postgres://localhost/katwork"),
            Some("x".repeat(32)),
            None,
            "ok",
            "0",
        ),
        (
            Some("postgres://localhost/katwork"),
            Some("ก".repeat(11)),
            Some("1"),
            "ok",
            "1",
        ),
        (
            Some("postgres://localhost/katwork"),
            Some("x".repeat(32)),
            Some("true"),
            "ok",
            "0",
        ),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "config_from_env", "--nocapture"])
            .env_clear()
            .env("KATWORK_TEST_EXPECTED", expected)
            .env("KATWORK_TEST_DEMO", enabled);
        if let Some(value) = database {
            child.env("DATABASE_URL", value);
        }
        if let Some(value) = secret {
            child.env("KATWORK_SECRET", value);
        }
        if let Some(value) = demo {
            child.env("KATWORK_DEMO", value);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
