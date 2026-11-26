//! A process's peak resident memory, for tests that read a large file in a child process so the
//! figure is the read's alone.
#![allow(dead_code)]

/// This process's peak resident set size in KiB (`VmHWM` in `/proc/self/status`), on Linux.
pub fn peak_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("VmHWM:")?;
        rest.trim().strip_suffix("kB")?.trim().parse().ok()
    })
}

/// Runs the ignored test `name` of this test binary alone, in a child process with `env` set, and
/// returns what it printed.
pub fn run_child(name: &str, env: &[(&str, &str)]) -> String {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            name,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
