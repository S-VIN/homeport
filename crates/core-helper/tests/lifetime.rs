//! Exercise the real worker protocol without requesting administrator rights.
#![cfg(unix)]
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    process::{Child, Command},
    time::{Duration, Instant},
};

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn closing_the_lease_reaps_the_worker_core_and_removes_its_cache() {
    let dir = tempfile::Builder::new()
        .prefix("rproxy-test-")
        .tempdir()
        .unwrap();
    let ticket = dir.path().join("ticket");
    let secret = [42; 32];
    fs::write(&ticket, secret).unwrap();
    let pid_file = dir.path().join("pid");
    let core = dir.path().join("core with ' quotes");
    fs::write(
        &core,
        format!(
            "#!/bin/sh\necho $$ > {}\nexec sleep 60\n",
            rproxy_core_helper::shell_quote(&pid_file.to_string_lossy())
        ),
    )
    .unwrap();
    fs::set_permissions(&core, fs::Permissions::from_mode(0o700)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut worker = Worker(
        Command::new(env!("CARGO_BIN_EXE_rproxy-core-helper"))
            .arg("--serve")
            .arg(listener.local_addr().unwrap().port().to_string())
            .arg(ticket)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut lease = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        assert!(worker.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    };
    lease
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut received = [0; 32];
    lease.read_exact(&mut received).unwrap();
    assert_eq!(received, secret);
    let request = rproxy_core_helper::Launch {
        core,
        config: dir.path().join("config.json"),
    };
    let mut body = serde_json::to_vec(&request).unwrap();
    body.push(b'\n');
    lease.write_all(&body).unwrap();
    let pid: i32 = loop {
        if let Ok(pid) = fs::read_to_string(&pid_file)
            && let Ok(pid) = pid.trim().parse()
        {
            break pid;
        }
        assert!(worker.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(lease); // Includes the application's crash/kill case: the OS closes its socket.
    loop {
        if let Some(status) = worker.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "core survived its owner");
    assert!(!dir.path().join("core-data").exists());
}

#[test]
fn invalid_ticket_cannot_start_a_worker() {
    let dir = tempfile::tempdir().unwrap();
    let ticket = dir.path().join("ticket");
    fs::write(&ticket, b"short").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_rproxy-core-helper"))
        .args(["--serve", "1"])
        .arg(ticket)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Invalid tunnel ticket"));
}
