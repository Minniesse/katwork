use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn server() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_katwork-server"));
    command
        .env_clear()
        .env("DATABASE_URL", "postgres://localhost/katwork")
        .env("KATWORK_SECRET", "test-only-secret-at-least-32-bytes");
    command
}

#[test]
fn maintenance_commands_are_stubs() {
    for command in ["migrate", "seed-demo"] {
        assert!(server().arg(command).status().unwrap().success());
    }
}

#[test]
fn rejects_unknown_commands_and_extra_arguments() {
    for args in [vec!["unknown"], vec!["migrate", "unexpected"]] {
        let output = server().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("command_invalid"));
    }
}

#[test]
fn serve_rejects_missing_configuration() {
    let output = server()
        .arg("serve")
        .env_remove("KATWORK_SECRET")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("config_invalid"));
}

#[test]
fn serve_reports_database_cause_without_password() {
    let query_password = ["s3", "cret"].concat();
    for url in [
        "postgres://katwork:db-test-password@localhost/katwork?sslmode=db-test-password".to_owned(),
        "postgres://katwork:db%22pass%5Cword@localhost/katwork?sslmode=db%22pass%5Cword".to_owned(),
        format!(
            "postgres://u:{}@localhost/db?sslmode={}",
            "p%40ss;word", "p%40ss;word"
        ),
        format!(
            "postgres://u:{}@localhost/db?sslmode={}",
            "a%2Fb=c", "a%2Fb=c"
        ),
        format!(
            "postgres://u:{}@localhost/db?sslmode={}",
            "päss%40rd", "invalid-secret"
        ),
        format!(
            "postgres://u:{}@localhost/db?sslmode={}",
            "pass\tword", "invalid-secret"
        ),
        format!("postgres://localhost/db?sslpassword={query_password}&sslmode={query_password}"),
        format!("postgres://localhost/db?PASSWORD={query_password}&sslmode={query_password}"),
    ] {
        let output = server()
            .arg("serve")
            .env("DATABASE_URL", url)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(stderr.contains("database_unavailable"));
        assert!(stderr.contains("caused by:"), "{stderr}");
        assert!(stderr.contains("ssl_mode"), "{stderr}");
        assert!(stderr.contains("[redacted]"), "{stderr}");
        assert!(!stderr.contains("db-test-password"));
        assert!(!stderr.contains(r#"db\"pass\\word"#));
        for secret in [
            "p%40ss;word",
            "p@ss;word",
            "a%2Fb=c",
            "a/b=c",
            "päss",
            "pass\\tword",
            "invalid-secret",
            &query_password,
        ] {
            assert!(!stderr.contains(secret), "{stderr}");
        }
    }
}

#[test]
fn serve_reports_connection_refused_without_password() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let output = server()
        .arg("serve")
        .env(
            "DATABASE_URL",
            format!("postgres://u:{}@{address}/db", "p%40ss;word"),
        )
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("database_unavailable"), "{stderr}");
    assert!(
        stderr.to_lowercase().contains("connection refused"),
        "{stderr}"
    );
    assert!(!stderr.contains("pool timed out"), "{stderr}");
    assert!(!stderr.contains("p%40ss;word"));
    assert!(!stderr.contains("p@ss;word"));
}

#[test]
fn serve_config_error_does_not_print_database_password() {
    let query_password = ["s3", "cret"].concat();
    let output = server()
        .arg("serve")
        .env_remove("KATWORK_SECRET")
        .env(
            "DATABASE_URL",
            format!(
                "postgres://u:{}@h/db?PASSWORD={query_password}",
                "p%40ss;word"
            ),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "config_invalid\n");
}

#[test]
fn serve_short_query_values_finish_without_password() {
    let mut child = server()
        .arg("serve")
        .env("DATABASE_URL", "postgres://u:pw@h/db?sslmode=e&x=d")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(2) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("startup report did not finish within two seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("ssl_mode"), "{stderr}");
    assert!(!stderr.contains("pw"), "{stderr}");
    assert!(stderr.len() < 1024);
}
