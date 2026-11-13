//! Pointing the operating system's proxy settings at a listener, and back.
//!
//! - Linux: GNOME (`gsettings`) and KDE (`kwriteconfig6`/`kwriteconfig5`),
//!   whichever is present;
//! - macOS: every enabled network service (`networksetup`);
//! - Windows: the per-user WinINet settings (registry), then a settings-changed
//!   notification so running apps pick them up.
//!
//! Only commands of the platform itself are used; a missing tool is skipped.
use std::process::Command;

fn run(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Use `127.0.0.1:port` (HTTP and SOCKS5) as the system proxy, or turn it off.
/// Returns what was changed, or why nothing could be.
pub fn apply(port: Option<u16>) -> Result<String, String> {
    platform::apply(port)
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::run;
    fn gnome(port: Option<u16>) -> bool {
        if !run("gsettings", &["list-keys", "org.gnome.system.proxy"]) {
            return false;
        }
        let Some(port) = port else {
            return run(
                "gsettings",
                &["set", "org.gnome.system.proxy", "mode", "none"],
            );
        };
        let port = port.to_string();
        let mut ok = true;
        for schema in [
            "org.gnome.system.proxy.http",
            "org.gnome.system.proxy.https",
            "org.gnome.system.proxy.socks",
        ] {
            ok &= run("gsettings", &["set", schema, "host", "127.0.0.1"])
                && run("gsettings", &["set", schema, "port", &port]);
        }
        ok &= run(
            "gsettings",
            &[
                "set",
                "org.gnome.system.proxy",
                "ignore-hosts",
                "['localhost', '127.0.0.0/8', '::1']",
            ],
        );
        ok && run(
            "gsettings",
            &["set", "org.gnome.system.proxy", "mode", "manual"],
        )
    }
    fn kde(port: Option<u16>) -> bool {
        let Some(tool) = ["kwriteconfig6", "kwriteconfig5"]
            .into_iter()
            .find(|t| run(t, &["--help"]))
        else {
            return false;
        };
        let set = |key: &str, value: &str| {
            run(
                tool,
                &[
                    "--file",
                    "kioslaverc",
                    "--group",
                    "Proxy Settings",
                    "--key",
                    key,
                    value,
                ],
            )
        };
        let ok = match port {
            Some(p) => {
                set("httpProxy", &format!("http://127.0.0.1 {p}"))
                    && set("httpsProxy", &format!("http://127.0.0.1 {p}"))
                    && set("socksProxy", &format!("socks://127.0.0.1 {p}"))
                    && set("NoProxyFor", "localhost,127.0.0.1,::1")
                    && set("ProxyType", "1")
            }
            None => set("ProxyType", "0"),
        };
        // Running KDE apps re-read the settings on this signal.
        let _ = run(
            "dbus-send",
            &[
                "--type=signal",
                "/KIO/Scheduler",
                "org.kde.KIO.Scheduler.reparseSlaveConfiguration",
                "string:",
            ],
        );
        ok
    }
    pub fn apply(port: Option<u16>) -> Result<String, String> {
        let done: Vec<&str> = [("GNOME", gnome(port)), ("KDE", kde(port))]
            .into_iter()
            .filter(|(_, ok)| *ok)
            .map(|(n, _)| n)
            .collect();
        if done.is_empty() {
            return Err("No supported desktop proxy settings (GNOME or KDE) were found.".into());
        }
        Ok(done.join(" and "))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::run;
    use std::process::Command;
    pub fn apply(port: Option<u16>) -> Result<String, String> {
        let out = Command::new("networksetup")
            .arg("-listallnetworkservices")
            .output()
            .map_err(|e| e.to_string())?;
        // The first line is a note; disabled services start with '*'.
        let services: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .skip(1)
            .filter(|l| !l.starts_with('*') && !l.is_empty())
            .map(str::to_owned)
            .collect();
        let mut ok = 0;
        for s in &services {
            let done = match port {
                Some(p) => {
                    let p = p.to_string();
                    run("networksetup", &["-setwebproxy", s, "127.0.0.1", &p])
                        && run("networksetup", &["-setsecurewebproxy", s, "127.0.0.1", &p])
                        && run(
                            "networksetup",
                            &["-setsocksfirewallproxy", s, "127.0.0.1", &p],
                        )
                }
                None => {
                    run("networksetup", &["-setwebproxystate", s, "off"])
                        && run("networksetup", &["-setsecurewebproxystate", s, "off"])
                        && run("networksetup", &["-setsocksfirewallproxystate", s, "off"])
                }
            };
            ok += usize::from(done);
        }
        if ok == 0 {
            return Err("Could not change the proxy of any network service.".into());
        }
        Ok(format!("{ok} network services"))
    }
}

#[cfg(windows)]
mod platform {
    use super::run;
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    pub fn apply(port: Option<u16>) -> Result<String, String> {
        let ok = match port {
            Some(p) => {
                run(
                    "reg",
                    &[
                        "add",
                        KEY,
                        "/v",
                        "ProxyServer",
                        "/t",
                        "REG_SZ",
                        "/d",
                        &format!("127.0.0.1:{p}"),
                        "/f",
                    ],
                ) && run(
                    "reg",
                    &[
                        "add",
                        KEY,
                        "/v",
                        "ProxyOverride",
                        "/t",
                        "REG_SZ",
                        "/d",
                        "<local>",
                        "/f",
                    ],
                ) && run(
                    "reg",
                    &[
                        "add",
                        KEY,
                        "/v",
                        "ProxyEnable",
                        "/t",
                        "REG_DWORD",
                        "/d",
                        "1",
                        "/f",
                    ],
                )
            }
            None => run(
                "reg",
                &[
                    "add",
                    KEY,
                    "/v",
                    "ProxyEnable",
                    "/t",
                    "REG_DWORD",
                    "/d",
                    "0",
                    "/f",
                ],
            ),
        };
        if !ok {
            return Err("Could not change the Windows proxy settings.".into());
        }
        // Tell running apps (browsers, WinINet users) to re-read the settings.
        unsafe {
            use windows_sys::Win32::Networking::WinInet::{
                InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
            };
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_SETTINGS_CHANGED,
                std::ptr::null_mut(),
                0,
            );
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_REFRESH,
                std::ptr::null_mut(),
                0,
            );
        }
        Ok("Windows".into())
    }
}
