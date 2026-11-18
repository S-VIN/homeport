//! The tunnel core's whole path — administrator prompt, lease, controller,
//! stop — with a stand-in `pkexec`: only root can create the tunnel itself.
#![cfg(target_os = "linux")]
use homeport_kernel::mihomo::{Mihomo, supervisor};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

/// Run the ignored test `child` in its own process (PATH and the configured
/// supervisor are process-wide) with `pkexec` first in PATH. Returns its
/// output and how often the prompt was opened.
fn with_pkexec(answer: &str, child: &str) -> (String, usize) {
    let dir = tempfile::tempdir().unwrap();
    let (pkexec, opened) = (dir.path().join("pkexec"), dir.path().join("opened"));
    fs::write(
        &pkexec,
        format!("#!/bin/sh\necho >> {}\n{answer}\n", opened.display()),
    )
    .unwrap();
    fs::set_permissions(&pkexec, fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::var("PATH").unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", child, "--ignored", "--nocapture"])
        .env("PATH", format!("{}:{path}", dir.path().display()))
        .env("TUNNEL_TEST_HOME", dir.path().join("core"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}");
    (text, fs::read_to_string(opened).unwrap().lines().count())
}
fn home() -> PathBuf {
    supervisor::configure(env!("CARGO_BIN_EXE_homeport").into());
    std::env::var_os("TUNNEL_TEST_HOME").unwrap().into()
}

#[test]
fn an_authorized_core_runs_under_its_lease_and_stops_with_it() {
    // Drop pkexec's own option, then run the supervisor like root would.
    with_pkexec("shift\nexec \"$@\"", "authorized_child");
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "child process of the test above"]
async fn authorized_child() {
    let home = home();
    let mut core = Mihomo::start(&[], &home, true).await.unwrap();
    assert!(core.elevated() && core.alive());
    assert!(core.version().await.is_ok(), "controller answers");
    core.stop().await;
    assert!(!core.alive());
    assert!(core.version().await.is_err(), "the core is gone");
    drop(core);
    let sessions = fs::read_dir(&home).unwrap().flatten();
    assert_eq!(
        sessions
            .filter(|e| e.file_name().to_string_lossy().starts_with("homeport-"))
            .count(),
        0,
        "session folder removed"
    );
}

#[test]
fn a_dismissed_prompt_is_reported_and_not_repeated() {
    let (text, opened) = with_pkexec("exit 126", "dismissed_child");
    assert!(text.contains("Tunnel access was not granted"), "{text}");
    assert_eq!(opened, 1);
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "child process of the test above"]
async fn dismissed_child() {
    let error = Mihomo::start(&[], &home(), true).await.err().unwrap();
    println!("{error}");
}
