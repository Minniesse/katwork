#[test]
fn init_env_is_private_random_validated_and_never_overwrites_implicitly() {
    let output = std::process::Command::new("sh")
        .arg("scripts/tests/init-env.sh")
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn backup_skips_fresh_database_publishes_only_complete_unique_archives_and_fails_closed() {
    let output = std::process::Command::new("sh")
        .arg("scripts/tests/backup.sh")
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn files_backup_links_unchanged_files_checks_new_ones_and_restores_them() {
    let output = std::process::Command::new("sh")
        .arg("scripts/tests/backup-files.sh")
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn demo_keeps_initial_credentials_private_and_preserves_them_on_rerun() {
    let output = std::process::Command::new("sh")
        .arg("scripts/tests/demo.sh")
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
