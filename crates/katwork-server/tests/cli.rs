use std::process::Command;

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
    for url in [
        "postgres://katwork:db-test-password@localhost/katwork?sslmode=db-test-password",
        "postgres://katwork:db%22pass%5Cword@localhost/katwork?sslmode=db%22pass%5Cword",
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
    }
}
