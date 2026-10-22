use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn server() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_katwork-server"));
    command
        .env_clear()
        .env("DATABASE_URL", "postgres://localhost/katwork")
        .env("KATWORK_SECRET", uuid::Uuid::now_v7().to_string());
    command
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
            .env("RUST_LOG", "trace")
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
        stderr.to_lowercase().contains("connection refused")
            || stderr.to_lowercase().contains("connection reset"),
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

#[test]
fn migrate_rejects_missing_configuration() {
    let output = server()
        .arg("migrate")
        .env_remove("DATABASE_URL")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "config_invalid\n");
}

#[sqlx::test(migrations = false)]
async fn migrate_command_applies_schema(pool: sqlx::PgPool) {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut options = pool.connect_options().as_ref().clone();
    options = options.database(&database);
    let url = sqlx::ConnectOptions::to_url_lossy(&options).to_string();
    for _ in 0..2 {
        let output = server()
            .arg("migrate")
            .env("DATABASE_URL", &url)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

fn database_url(pool: &sqlx::PgPool) -> String {
    sqlx::ConnectOptions::to_url_lossy(pool.connect_options().as_ref()).to_string()
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn seed_demo_creates_demo_user_and_workspace(pool: sqlx::PgPool) {
    let password = uuid::Uuid::now_v7().to_string();
    for _ in 0..2 {
        let output = server()
            .arg("seed-demo")
            .env("DATABASE_URL", database_url(&pool))
            .env("KATWORK_DEMO_PASSWORD", &password)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&password));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&password));
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM memberships m JOIN users u ON u.id = m.user_id JOIN workspaces w ON w.id = m.workspace_id WHERE u.email = 'demo@katwork.local' AND m.role = 'owner'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1);
    let hash: String =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE email = 'demo@katwork.local'")
            .fetch_one(&pool)
            .await
            .unwrap();
    use argon2::PasswordVerifier;
    assert!(
        argon2::Argon2::default()
            .verify_password(
                password.as_bytes(),
                &argon2::PasswordHash::new(&hash).unwrap()
            )
            .is_ok()
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn seed_demo_reset_password_allows_login(pool: sqlx::PgPool) {
    let first = server()
        .arg("seed-demo")
        .env("DATABASE_URL", database_url(&pool))
        .output()
        .unwrap();
    assert!(first.status.success());
    let first_password = String::from_utf8(first.stdout).unwrap().trim().to_owned();
    assert!(first_password.len() >= 32);
    let second = server()
        .arg("seed-demo")
        .env("DATABASE_URL", database_url(&pool))
        .output()
        .unwrap();
    assert!(second.status.success());
    assert!(second.stdout.is_empty());
    let password = uuid::Uuid::now_v7().to_string();
    let reset = server()
        .args(["seed-demo", "--reset-password"])
        .env("DATABASE_URL", database_url(&pool))
        .env("KATWORK_DEMO_PASSWORD", &password)
        .output()
        .unwrap();
    assert!(reset.status.success());
    assert_eq!(String::from_utf8(reset.stdout).unwrap().trim(), password);
    let hash: String =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE email = 'demo@katwork.local'")
            .fetch_one(&pool)
            .await
            .unwrap();
    use argon2::PasswordVerifier;
    assert!(
        argon2::Argon2::default()
            .verify_password(
                password.as_bytes(),
                &argon2::PasswordHash::new(&hash).unwrap()
            )
            .is_ok()
    );
    assert!(
        argon2::Argon2::default()
            .verify_password(
                first_password.as_bytes(),
                &argon2::PasswordHash::new(&hash).unwrap()
            )
            .is_err()
    );
}
