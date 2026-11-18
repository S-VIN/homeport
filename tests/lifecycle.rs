//! Actual application lifecycle, using isolated profiles and the bundled core.
#![cfg(target_os = "linux")]
use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};
struct App(Child);
impl Drop for App {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn children(pid: u32) -> Vec<u32> {
    let mut result = vec![];
    if let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) {
        for task in tasks.flatten() {
            if let Ok(text) = fs::read_to_string(task.path().join("children")) {
                result.extend(
                    text.split_whitespace()
                        .filter_map(|s| s.parse::<u32>().ok()),
                );
            }
        }
    }
    result
}
fn alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        s.rsplit_once(") ")
            .is_some_and(|(_, s)| !s.starts_with('Z'))
    })
}
fn app(dir: &Path) -> App {
    App(Command::new(env!("CARGO_BIN_EXE_homeport"))
        .arg("web")
        .env("HOMEPORT_LISTEN", "127.0.0.1:0")
        .env("HOMEPORT_DATA_DIR", dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap())
}
fn core(app: &mut App) -> (u32, u32) {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(app.0.try_wait().unwrap().is_none());
        for supervisor in children(app.0.id()) {
            if let Some(core) = children(supervisor).first() {
                return (supervisor, *core);
            }
        }
        assert!(Instant::now() < until, "supervised core did not start");
        sleep(Duration::from_millis(20));
    }
}
#[test]
fn killing_application_reaps_core_and_allows_a_clean_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..4 {
        let mut owner = app(dir.path());
        let (supervisor, core) = core(&mut owner);
        // A duplicate succeeds through activation, without another core.
        let mut duplicate = app(dir.path());
        assert!(duplicate.0.wait().unwrap().success());
        assert_eq!(children(supervisor), vec![core]);
        owner.0.kill().unwrap();
        owner.0.wait().unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while alive(core) || alive(supervisor) {
            assert!(
                Instant::now() < until,
                "application left a running core or guard"
            );
            sleep(Duration::from_millis(20));
        }
    }
}

/// The tunnel supervisor is the same code started by the system's
/// administrator prompt; without the prompt it runs with ordinary rights.
#[test]
fn closing_the_lease_stops_the_tunnel_supervisor_and_reaps_its_core() {
    use std::{io::Read, net::TcpListener, os::unix::fs::PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let (ticket, pid_file) = (dir.path().join("lease"), dir.path().join("pid"));
    fs::write(&ticket, "ticket").unwrap();
    let core = dir.path().join("core with ' quotes");
    fs::write(&core, "#!/bin/sh\necho $$ > \"$2/pid\"\nexec sleep 60\n").unwrap();
    fs::set_permissions(&core, fs::Permissions::from_mode(0o700)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut supervisor = App(Command::new(env!("CARGO_BIN_EXE_homeport"))
        .args(["--core-supervisor", "--lease"])
        .arg(listener.local_addr().unwrap().port().to_string())
        .args([&ticket, &core])
        .arg("-d")
        .arg(dir.path())
        .spawn()
        .unwrap());
    let (mut lease, _) = listener.accept().unwrap();
    let mut received = [0; 6];
    lease.read_exact(&mut received).unwrap();
    assert_eq!(&received, b"ticket");
    let until = Instant::now() + Duration::from_secs(10);
    let pid: u32 = loop {
        if let Ok(pid) = fs::read_to_string(&pid_file)
            .unwrap_or_default()
            .trim()
            .parse()
        {
            break pid;
        }
        assert!(Instant::now() < until, "core did not start");
        sleep(Duration::from_millis(10));
    };
    assert!(alive(pid));
    drop(lease); // Also what a crash of the application does.
    assert!(supervisor.0.wait().unwrap().success());
    assert!(!alive(pid), "core survived its owner");
}
