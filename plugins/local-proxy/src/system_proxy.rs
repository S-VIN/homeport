//! Temporarily point OS proxy settings at a listener. Restore each original
//! value only while it still equals the value written by homeport.
//!
//! What homeport changed is also kept in a journal file, so a crash, a logoff or
//! a killed process is undone at the next start.
#[cfg(unix)]
use std::process::Command;
use std::{net::SocketAddr, path::Path, sync::Mutex};

type Read = Box<dyn Fn() -> Result<Option<String>, String> + Send>;
type Write = Box<dyn Fn(Option<&str>) -> bool + Send>;
/// One system value. `key` names it the same way in every run (for the journal).
struct Setting {
    key: String,
    read: Read,
    write: Write,
    wanted: Option<String>,
}
struct Change {
    setting: Setting,
    before: Option<String>,
    ours: Option<String>,
}
static CHANGES: Mutex<Vec<Change>> = Mutex::new(Vec::new());

/// The journal: the address and, per setting key, the value before and the value homeport wrote.
#[derive(serde::Serialize, serde::Deserialize)]
struct Journal {
    address: SocketAddr,
    entries: Vec<(String, Option<String>, Option<String>)>,
}

#[cfg(unix)]
fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|_| format!("Cannot run {program}."))?;
    if !out.status.success() {
        return Err(format!(
            "{program} could not read the system proxy settings."
        ));
    }
    String::from_utf8(out.stdout)
        .map(|s| s.trim_end_matches(['\r', '\n']).to_owned())
        .map_err(|_| format!("Invalid response from {program}."))
}
#[cfg(unix)]
fn run(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn restore(changes: &mut Vec<Change>) -> Result<(), String> {
    let mut failed = Vec::new();
    while let Some(change) = changes.pop() {
        match (change.setting.read)() {
            Ok(current) if current != change.ours => {} // Changed outside homeport: respect the new value.
            Ok(_) if (change.setting.write)(change.before.as_deref()) => {}
            _ => failed.push(change),
        }
    }
    failed.reverse();
    *changes = failed;
    if changes.is_empty() {
        Ok(())
    } else {
        Err("Could not restore some previous system proxy settings. Check your system network settings.".into())
    }
}

/// `Some(address)` acquires the proxy settings; `None` restores the previous values.
/// A failed application rolls back the fields already written. `journal` is the
/// file that survives a crash; a journal left by an earlier run is restored first.
pub fn apply(journal: &Path, address: Option<SocketAddr>) -> Result<String, String> {
    if homeport_kernel::Platform::is_flatpak() || homeport_kernel::Platform::is_docker() {
        return Err("System proxy control is unavailable in Flatpak and Docker.".into());
    }
    let mut changes = CHANGES.lock().map_err(|_| "System proxy lock failed.")?;
    if changes.is_empty() {
        recover(journal, &mut changes);
    }
    let restored = restore(&mut changes);
    platform::notify();
    let saved = save(journal, address, &changes);
    restored?;
    saved?;
    let Some(address) = address else {
        return Ok("Previous system proxy settings restored".into());
    };
    let (label, settings) = platform::settings(address)?;
    let applied = acquire(&mut changes, settings);
    platform::notify();
    let saved = save(journal, Some(address), &changes);
    applied?;
    saved.map(|()| label)
}

/// Reopen what an earlier run left in the journal, so `restore` can undo it.
fn recover(journal: &Path, changes: &mut Vec<Change>) {
    let Some(old) = std::fs::read(journal)
        .ok()
        .and_then(|b| serde_json::from_slice::<Journal>(&b).ok())
    else {
        return;
    };
    let Ok((_, settings)) = platform::settings(old.address) else {
        return;
    };
    let mut settings: Vec<Option<Setting>> = settings.into_iter().map(Some).collect();
    for (key, before, ours) in old.entries {
        if let Some(setting) = settings
            .iter_mut()
            .find(|s| s.as_ref().is_some_and(|s| s.key == key))
            .and_then(Option::take)
        {
            changes.push(Change {
                setting,
                before,
                ours,
            });
        }
    }
}

/// Write the journal for `changes`, or delete it when nothing is left to restore.
fn save(journal: &Path, address: Option<SocketAddr>, changes: &[Change]) -> Result<(), String> {
    let address = address.or_else(|| {
        std::fs::read(journal)
            .ok()
            .and_then(|b| serde_json::from_slice::<Journal>(&b).ok())
            .map(|j| j.address)
    });
    let (Some(address), false) = (address, changes.is_empty()) else {
        return match std::fs::remove_file(journal) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("Cannot remove {}: {e}", journal.display()))
            }
            _ => Ok(()),
        };
    };
    let entries = changes
        .iter()
        .map(|c| (c.setting.key.clone(), c.before.clone(), c.ours.clone()))
        .collect();
    let bytes =
        serde_json::to_vec_pretty(&Journal { address, entries }).map_err(|e| e.to_string())?;
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(journal.parent().unwrap_or(Path::new(".")))?;
        let tmp = journal.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(tmp, journal)
    };
    write().map_err(|e| format!("Cannot save {}: {e}", journal.display()))
}

fn acquire(changes: &mut Vec<Change>, settings: Vec<Setting>) -> Result<(), String> {
    // Read everything before changing anything, including PAC and bypass settings.
    let mut pending = Vec::new();
    for setting in settings {
        let before = (setting.read)()?;
        pending.push(Change {
            ours: setting.wanted.clone(),
            setting,
            before,
        });
    }
    for mut change in pending {
        if !(change.setting.write)(change.setting.wanted.as_deref()) {
            let undone = (change.setting.write)(change.before.as_deref());
            if !undone {
                if let Ok(actual) = (change.setting.read)() {
                    change.ours = actual;
                }
                changes.push(change);
            }
            let rollback = restore(changes);
            return Err(format!(
                "Could not apply the system proxy.{} {}",
                platform::WRITE_HINT,
                rollback
                    .err()
                    .unwrap_or_else(|| "Previous settings were restored.".into())
            ));
        }
        // Commands may normalize values (e.g. GVariant quotes or registry DWORDs).
        match (change.setting.read)() {
            Ok(actual) => change.ours = actual,
            Err(e) => {
                let undone = (change.setting.write)(change.before.as_deref());
                if !undone {
                    changes.push(change);
                }
                let rollback = restore(changes);
                return Err(format!("{e} {}", rollback.err().unwrap_or_else(|| if undone {
                    "Previous settings were restored.".into()
                } else { "Check your system network settings: the failed write could not be verified.".into() })));
            }
        }
        changes.push(change);
    }
    Ok(())
}

/// What the GNOME proxy settings reach. Outside GNOME-family desktops (sway,
/// Hyprland, XFCE…) only GTK apps and Firefox read them.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
fn gnome_label(current_desktop: &str) -> &'static str {
    let family = ["gnome", "unity", "budgie", "cinnamon", "pantheon"];
    let gnome = current_desktop
        .split(':')
        .any(|d| family.iter().any(|f| d.to_ascii_lowercase().contains(f)));
    if gnome {
        "GNOME"
    } else {
        "GNOME settings (GTK apps, Firefox; terminal apps need http_proxy)"
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::*;
    pub const WRITE_HINT: &str = "";
    pub fn settings(address: SocketAddr) -> Result<(String, Vec<Setting>), String> {
        let port = address.port();
        let host = address.ip().to_string();
        let mut settings = Vec::new();
        let mut desktops = Vec::new();
        if output("gsettings", &["list-keys", "org.gnome.system.proxy"]).is_ok() {
            desktops.push(gnome_label(
                &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
            ));
            let mut field = |schema: &'static str, key: &'static str, wanted: String| {
                settings.push(Setting {
                    key: format!("gsettings:{schema}:{key}"),
                    read: Box::new(move || output("gsettings", &["get", schema, key]).map(Some)),
                    write: Box::new(move |v| {
                        v.is_some_and(|v| run("gsettings", &["set", schema, key, v]))
                    }),
                    wanted: Some(wanted),
                })
            };
            for schema in [
                "org.gnome.system.proxy.http",
                "org.gnome.system.proxy.https",
                "org.gnome.system.proxy.socks",
            ] {
                field(schema, "host", format!("'{host}'"));
                field(schema, "port", port.to_string());
            }
            field(
                "org.gnome.system.proxy",
                "ignore-hosts",
                "['localhost', '127.0.0.0/8', '::1']".into(),
            );
            // Disabling HTTP authentication is temporary too; credentials are untouched.
            field(
                "org.gnome.system.proxy.http",
                "use-authentication",
                "false".into(),
            );
            field("org.gnome.system.proxy", "mode", "'manual'".into());
        }
        if let Some((read, write)) = [
            ("kreadconfig6", "kwriteconfig6"),
            ("kreadconfig5", "kwriteconfig5"),
        ]
        .into_iter()
        .find(|(r, w)| run(r, &["--help"]) && run(w, &["--help"]))
        {
            desktops.push("KDE");
            let host = if address.is_ipv6() {
                format!("[{host}]")
            } else {
                host.clone()
            };
            for (key, wanted) in [
                ("httpProxy", format!("http://{host} {port}")),
                ("httpsProxy", format!("http://{host} {port}")),
                ("socksProxy", format!("socks://{host} {port}")),
                ("NoProxyFor", "localhost,127.0.0.1,::1".into()),
                ("ProxyType", "1".into()),
            ] {
                settings.push(Setting {
                    key: format!("kioslaverc:{key}"),
                    read: Box::new(move || {
                        output(
                            read,
                            &[
                                "--file",
                                "kioslaverc",
                                "--group",
                                "Proxy Settings",
                                "--key",
                                key,
                                "--default",
                                "__homeport_missing__",
                            ],
                        )
                        .map(|v| (v != "__homeport_missing__").then_some(v))
                    }),
                    write: Box::new(move |v| {
                        let mut args = vec![
                            "--file",
                            "kioslaverc",
                            "--group",
                            "Proxy Settings",
                            "--key",
                            key,
                        ];
                        if let Some(v) = v {
                            args.push(v);
                        } else {
                            args.push("--delete");
                        }
                        run(write, &args)
                    }),
                    wanted: Some(wanted),
                });
            }
        }
        if settings.is_empty() {
            return Err("No supported desktop proxy settings (GNOME or KDE) were found.".into());
        }
        Ok((desktops.join(" and "), settings))
    }
    pub fn notify() {
        let _ = run(
            "dbus-send",
            &[
                "--session",
                "--type=signal",
                "/KIO/Scheduler",
                "org.kde.KIO.Scheduler.reparseSlaveConfiguration",
                "string:",
            ],
        );
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    pub const WRITE_HINT: &str = " Changing network settings needs an administrator account.";
    fn value<'a>(text: &'a str, key: &str) -> &'a str {
        text.lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or("")
            .trim()
    }
    /// One kind of proxy of a network service as `server\tport\ton|off`.
    fn read_proxy(get: &str, service: &str) -> Result<Option<String>, String> {
        let raw = output("networksetup", &[get, service])?;
        if value(&raw, "Authenticated Proxy Enabled:") == "1" {
            return Err("An authenticated system proxy is configured. Configure your apps to use an homeport proxy port directly to preserve its credentials.".into());
        }
        let on = if value(&raw, "Enabled:") == "Yes" {
            "on"
        } else {
            "off"
        };
        Ok(Some(format!(
            "{}\t{}\t{on}",
            value(&raw, "Server:"),
            value(&raw, "Port:")
        )))
    }
    /// Write what `read_proxy` returned.
    fn write_proxy(set: &str, state: &str, service: &str, value: Option<&str>) -> bool {
        let Some(value) = value else { return false };
        let [server, port, on] = value.split('\t').collect::<Vec<_>>()[..] else {
            return false;
        };
        // No server before: networksetup cannot write an empty one, only switch it off.
        (server.is_empty() || run("networksetup", &[set, service, server, port, "off"]))
            && run("networksetup", &[state, service, on])
    }
    pub fn settings(address: SocketAddr) -> Result<(String, Vec<Setting>), String> {
        let port = address.port();
        let host = address.ip().to_string();
        let list = output("networksetup", &["-listallnetworkservices"])?;
        let services: Vec<_> = list
            .lines()
            .skip(1)
            .filter(|s| !s.starts_with('*') && !s.is_empty())
            .collect();
        let mut settings = Vec::new();
        for service in &services {
            for (get, set, state) in [
                ("-getwebproxy", "-setwebproxy", "-setwebproxystate"),
                (
                    "-getsecurewebproxy",
                    "-setsecurewebproxy",
                    "-setsecurewebproxystate",
                ),
                (
                    "-getsocksfirewallproxy",
                    "-setsocksfirewallproxy",
                    "-setsocksfirewallproxystate",
                ),
            ] {
                let read_service = service.to_string();
                let write_service = read_service.clone();
                settings.push(Setting {
                    key: format!("networksetup:{service}:{get}"),
                    read: Box::new(move || read_proxy(get, &read_service)),
                    write: Box::new(move |v| write_proxy(set, state, &write_service, v)),
                    wanted: Some(format!("{host}\t{port}\ton")),
                });
            }
            for (get, set, key) in [
                ("-getautoproxyurl", "-setautoproxystate", "Enabled:"),
                (
                    "-getproxyautodiscovery",
                    "-setproxyautodiscovery",
                    "Auto Proxy Discovery:",
                ),
            ] {
                let read_service = service.to_string();
                let write_service = read_service.clone();
                settings.push(Setting {
                    key: format!("networksetup:{service}:{get}"),
                    read: Box::new(move || {
                        output("networksetup", &[get, &read_service]).map(|raw| {
                            Some(
                                if matches!(value(&raw, key), "Yes" | "On") {
                                    "on"
                                } else {
                                    "off"
                                }
                                .into(),
                            )
                        })
                    }),
                    write: Box::new(move |v| {
                        v.is_some_and(|v| run("networksetup", &[set, &write_service, v]))
                    }),
                    wanted: Some("off".into()),
                });
            }
        }
        if settings.is_empty() {
            return Err("No enabled network services were found.".into());
        }
        Ok((format!("{} network services", services.len()), settings))
    }
    pub fn notify() {}
}

/// The LAN settings through WinINet's per-connection options. Unlike writing
/// `ProxyEnable`/`ProxyServer` in the registry, this also updates
/// `Connections\DefaultConnectionSettings`, which Windows 10/11 read first.
#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        ffi::c_void,
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{Foundation::GlobalFree, Networking::WinInet::*};
    pub const WRITE_HINT: &str = "";

    fn list(item: &mut INTERNET_PER_CONN_OPTIONW) -> INTERNET_PER_CONN_OPTION_LISTW {
        // A null connection name is the LAN (default) connection.
        INTERNET_PER_CONN_OPTION_LISTW {
            dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
            dwOptionCount: 1,
            pOptions: item,
            ..Default::default()
        }
    }
    fn query(option: u32) -> Result<Option<String>, String> {
        let mut item = INTERNET_PER_CONN_OPTIONW {
            dwOption: option,
            ..Default::default()
        };
        let mut list = list(&mut item);
        let mut size = list.dwSize;
        // SAFETY: the list and its one option outlive the call; WinINet allocates
        // the returned string with GlobalAlloc, freed below.
        unsafe {
            if InternetQueryOptionW(
                null(),
                INTERNET_OPTION_PER_CONNECTION_OPTION,
                &mut list as *mut _ as *mut c_void,
                &mut size,
            ) == 0
            {
                return Err("Cannot read the Windows proxy settings.".into());
            }
            if option == INTERNET_PER_CONN_FLAGS {
                return Ok(Some(item.Value.dwValue.to_string()));
            }
            let text = item.Value.pszValue;
            if text.is_null() {
                return Ok(None);
            }
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            GlobalFree(text as *mut c_void);
            Ok(Some(value))
        }
    }
    fn set(option: u32, value: Option<&str>) -> bool {
        let mut item = INTERNET_PER_CONN_OPTIONW {
            dwOption: option,
            ..Default::default()
        };
        let mut wide: Option<Vec<u16>> = value.map(|v| v.encode_utf16().chain([0]).collect());
        if option == INTERNET_PER_CONN_FLAGS {
            let Some(Ok(flags)) = value.map(str::parse::<u32>) else {
                return false;
            };
            item.Value.dwValue = flags;
        } else {
            item.Value.pszValue = wide.as_mut().map_or(null_mut(), |w| w.as_mut_ptr());
        }
        let list = list(&mut item);
        // SAFETY: the list, its option and the string buffer outlive the call.
        unsafe {
            InternetSetOptionW(
                null(),
                INTERNET_OPTION_PER_CONNECTION_OPTION,
                &list as *const _ as *const c_void,
                list.dwSize,
            ) != 0
        }
    }
    pub fn settings(address: SocketAddr) -> Result<(String, Vec<Setting>), String> {
        // Direct + proxy; turns off automatic detection and the PAC script until restored.
        let flags = (PROXY_TYPE_DIRECT | PROXY_TYPE_PROXY).to_string();
        let settings = [
            (
                "proxy-server",
                INTERNET_PER_CONN_PROXY_SERVER,
                address.to_string(),
            ),
            (
                "proxy-bypass",
                INTERNET_PER_CONN_PROXY_BYPASS,
                "<local>".into(),
            ),
            ("flags", INTERNET_PER_CONN_FLAGS, flags),
        ]
        .into_iter()
        .map(|(key, option, wanted)| Setting {
            key: format!("wininet:{key}"),
            read: Box::new(move || query(option)),
            write: Box::new(move |v| set(option, v)),
            wanted: Some(wanted),
        })
        .collect();
        Ok(("Windows".into(), settings))
    }
    pub fn notify() {
        unsafe {
            for option in [INTERNET_OPTION_SETTINGS_CHANGED, INTERNET_OPTION_REFRESH] {
                InternetSetOptionW(null(), option, null(), 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn field(value: Arc<Mutex<Option<String>>>, wanted: &str, fail: bool) -> Setting {
        let read = value.clone();
        Setting {
            key: wanted.into(),
            read: Box::new(move || Ok(read.lock().unwrap().clone())),
            write: Box::new(move |v| {
                if fail && v == Some("new") {
                    return false;
                }
                *value.lock().unwrap() = v.map(str::to_owned);
                true
            }),
            wanted: Some(wanted.into()),
        }
    }
    #[test]
    fn temporary_settings_restore_original_and_preserve_external_edits() {
        let mode = Arc::new(Mutex::new(Some("PAC".into())));
        let address = Arc::new(Mutex::new(Some("corporate-proxy".into())));
        let absent = Arc::new(Mutex::new(None));
        let mut changes = vec![];
        acquire(
            &mut changes,
            vec![
                field(mode.clone(), "manual", false),
                field(address.clone(), "localhost", false),
                field(absent.clone(), "new", false),
            ],
        )
        .unwrap();
        *address.lock().unwrap() = Some("user-selected-proxy".into());
        restore(&mut changes).unwrap();
        assert_eq!(*mode.lock().unwrap(), Some("PAC".into()));
        assert_eq!(*address.lock().unwrap(), Some("user-selected-proxy".into()));
        assert_eq!(*absent.lock().unwrap(), None);
    }
    #[test]
    fn partial_application_rolls_back_before_returning_an_error() {
        let first = Arc::new(Mutex::new(Some("original".into())));
        let second = Arc::new(Mutex::new(Some("bypass".into())));
        let mut changes = vec![];
        assert!(
            acquire(
                &mut changes,
                vec![
                    field(first.clone(), "new", false),
                    field(second.clone(), "new", true)
                ]
            )
            .is_err()
        );
        assert_eq!(*first.lock().unwrap(), Some("original".into()));
        assert_eq!(*second.lock().unwrap(), Some("bypass".into()));
        assert!(changes.is_empty());
    }
    #[test]
    fn the_journal_keeps_what_is_left_to_restore() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("plugins/local-proxy/system-proxy.json");
        let address: SocketAddr = "127.0.0.1:20808".parse().unwrap();
        let mode = Arc::new(Mutex::new(Some("none".into())));
        let mut changes = vec![];
        acquire(&mut changes, vec![field(mode.clone(), "manual", false)]).unwrap();
        save(&journal, Some(address), &changes).unwrap();
        let saved: Journal = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
        assert_eq!(saved.address, address);
        assert_eq!(
            saved.entries,
            vec![(
                "manual".to_owned(),
                Some("none".to_owned()),
                Some("manual".to_owned())
            )]
        );
        // Restoring with no address left keeps the journal's address until nothing remains.
        restore(&mut changes).unwrap();
        save(&journal, None, &changes).unwrap();
        assert!(!journal.exists());
        save(&journal, None, &[]).unwrap();
    }
    #[test]
    fn gnome_settings_are_named_for_what_reads_them() {
        assert_eq!(gnome_label("ubuntu:GNOME"), "GNOME");
        assert_eq!(gnome_label("X-Cinnamon"), "GNOME");
        assert!(gnome_label("sway").contains("GTK apps, Firefox"));
        assert!(gnome_label("").contains("http_proxy"));
        assert!(gnome_label("Hyprland").starts_with("GNOME settings"));
    }
}
