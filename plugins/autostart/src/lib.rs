//! # autostart
//! Start homeport in the background when the user logs in: an XDG autostart
//! entry on Linux, the Background portal in Flatpak, the `Run` registry key on
//! Windows and a LaunchAgent on macOS.
use homeport_kernel::{kit, prelude::*};
use std::io;

pub static PLUGIN: Plugin = Plugin {
    id: "autostart",
    name: "Start at login",
    description: "Start Homeport in the background when you log in.",
    platforms: Platform::DESKTOP,
    setup,
    ..Plugin::DEFAULT
};

/// Whether homeport starts at login, as the system has it (re-read after every change).
pub static ENABLED: State<bool> = State::new("autostart.enabled");
/// What the user chose. A login entry that disappeared (an app reinstall, a
/// cleanup tool) is created again at the next start.
static WANTED: State<bool> = State::new("autostart.wanted");

pub struct SetAutostart(pub bool);
impl Op for SetAutostart {
    type Reply = ();
    const SAVES: bool = true;
}

/// The login entry for this executable, as the system stores it.
pub fn expected() -> io::Result<String> {
    // An AppImage is mounted at a new path on every start: use the image itself.
    let exe = Platform::appimage()
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)?;
    entry::content(&exe)
}

/// Create or remove the login entry.
pub fn set(enable: bool) -> io::Result<()> {
    entry::write(if enable { Some(expected()?) } else { None }.as_deref())
}

/// The entry the Background portal writes for a Flatpak, read through the
/// manifest's `xdg-config/autostart:ro` (the sandbox's own config is elsewhere).
fn portal_entry() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let app = std::env::var("FLATPAK_ID").ok()?;
    Some(std::path::PathBuf::from(home).join(format!(".config/autostart/{app}.desktop")))
}

/// Whether a login entry exists now.
fn present() -> bool {
    if Platform::is_flatpak() {
        portal_entry().is_some_and(|p| p.is_file())
    } else {
        entry::read().is_ok_and(|e| e.is_some())
    }
}

/// Make the system match the user's choice.
async fn apply(on: bool) -> Result<()> {
    if Platform::is_flatpak() {
        portal::request(on).await.map(drop).map_err(Error::msg)
    } else {
        set(on).map_err(|e| Error::msg(format!("Cannot update the login entry: {e}")))
    }
}

/// At start: an entry that is gone while wanted comes back, and an entry from
/// another copy of homeport (moved, updated or reinstalled) now starts this one.
/// Development builds leave the user's entry alone.
async fn repair(cx: Cx, _: Started) -> Result<()> {
    if cfg!(debug_assertions) {
        return Ok(());
    }
    let wanted = *cx.get(&WANTED);
    let stale = !Platform::is_flatpak()
        && matches!((entry::read(), expected()), (Ok(Some(e)), Ok(x)) if e != x);
    if wanted && (!present() || stale) {
        apply(true).await?;
        info!("Login entry restored");
    }
    cx.set_live(&ENABLED, present());
    Ok(())
}

async fn set_autostart(cx: Cx, SetAutostart(on): SetAutostart) -> Result<()> {
    apply(on).await?;
    info!("Start at login turned {}", if on { "on" } else { "off" });
    cx.write(move |w| {
        w.set(&WANTED, on);
        Ok(())
    })
    .await?;
    cx.set_live(&ENABLED, present());
    Ok(())
}

fn setup(p: &mut Setup) -> Result<()> {
    p.live(&ENABLED, present);
    p.setting(&WANTED);
    p.on(repair);
    p.handle(set_autostart);
    p.fill(&SETTINGS_SECTIONS, "toggle", 0, |ui, cx, _| {
        let mut on = *cx.live(&ENABLED);
        kit::settings_row(ui, "Enabled", "", |ui| {
            if kit::toggle_named(ui, &mut on, "", "Start at login").changed() {
                cx.submit("autostart", SetAutostart(on));
            }
        });
    });
    Ok(())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn desktop_entry(exe: &str) -> io::Result<String> {
    if exe.contains(['\n', '\r']) {
        return Err(io::Error::other("Invalid executable path"));
    }
    let mut quoted = String::new();
    for c in exe.chars() {
        match c {
            '%' => quoted.push_str("%%"),
            '"' | '\\' | '$' | '`' => {
                quoted.push('\\');
                quoted.push(c);
            }
            _ => quoted.push(c),
        }
    }
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=homeport\nExec=\"{}\" background\nTerminal=false\n",
        quoted.replace('\\', "\\\\")
    ))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launch_agent(exe: &str) -> String {
    let xml = exe
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>io.github.homeport</string>\n\t<key>ProgramArguments</key>\n\t<array>\n\
\t\t<string>{xml}</string>\n\t\t<string>background</string>\n\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n</dict>\n</plist>\n"
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod entry {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
    };
    fn home(var: &str, fallback: &str) -> io::Result<PathBuf> {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|s| PathBuf::from(s).join(fallback)))
            .ok_or_else(|| io::Error::other("Missing home directory"))
    }
    #[cfg(target_os = "linux")]
    pub fn path() -> io::Result<PathBuf> {
        Ok(home("XDG_CONFIG_HOME", ".config")?.join("autostart/homeport.desktop"))
    }
    #[cfg(target_os = "macos")]
    pub fn path() -> io::Result<PathBuf> {
        Ok(home("HOME", "")?.join("Library/LaunchAgents/io.github.homeport.plist"))
    }
    fn utf8(exe: &Path) -> io::Result<&str> {
        exe.to_str()
            .ok_or_else(|| io::Error::other("Executable path is not UTF-8"))
    }
    #[cfg(target_os = "linux")]
    pub fn content(exe: &Path) -> io::Result<String> {
        desktop_entry(utf8(exe)?)
    }
    /// The executable inside `homeport.app` runs as the app. An app still on its
    /// disk image or quarantined (App Translocation) has a path that vanishes.
    #[cfg(target_os = "macos")]
    pub fn content(exe: &Path) -> io::Result<String> {
        let exe = utf8(exe)?;
        if exe.starts_with("/Volumes/") || exe.contains("/AppTranslocation/") {
            return Err(io::Error::other(
                "Move Homeport to the Applications folder and open it from there first.",
            ));
        }
        Ok(launch_agent(exe))
    }
    pub fn read() -> io::Result<Option<String>> {
        match fs::read_to_string(path()?) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            r => r.map(Some),
        }
    }
    pub fn write(content: Option<&str>) -> io::Result<()> {
        let path = path()?;
        let Some(content) = content else {
            return match fs::remove_file(path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                r => r,
            };
        };
        let dir = path.parent().expect("entry folder");
        fs::create_dir_all(dir)?;
        let tmp = dir.join(".homeport-login.tmp");
        fs::write(&tmp, content)?;
        fs::rename(tmp, path)
    }
}

#[cfg(windows)]
mod entry {
    use super::*;
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    pub fn content(exe: &std::path::Path) -> io::Result<String> {
        Ok(format!("\"{}\" background", exe.display()))
    }
    pub fn read() -> io::Result<Option<String>> {
        let run = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(RUN) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            r => r?,
        };
        match run.get_value::<String, _>("homeport") {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            r => r.map(Some),
        }
    }
    pub fn write(content: Option<&str>) -> io::Result<()> {
        let (run, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN)?;
        match content {
            Some(v) => run.set_value("homeport", &v),
            None => match run.delete_value("homeport") {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                r => r,
            },
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod entry {
    use super::*;
    pub fn content(_: &std::path::Path) -> io::Result<String> {
        Err(io::Error::other("Not supported on this system"))
    }
    pub fn read() -> io::Result<Option<String>> {
        Ok(None)
    }
    pub fn write(_: Option<&str>) -> io::Result<()> {
        Err(io::Error::other("Not supported on this system"))
    }
}

/// Flatpak: `org.freedesktop.portal.Background.RequestBackground`. The portal
/// writes the host autostart entry itself (`flatpak run --command=homeport … background`).
mod portal {
    #[cfg(target_os = "linux")]
    pub async fn request(enable: bool) -> Result<bool, String> {
        use std::collections::HashMap;
        use zbus::{
            export::futures_core::Stream,
            zvariant::{OwnedObjectPath, OwnedValue, Value},
        };
        let err = |e: zbus::Error| format!("The Background portal failed: {e}");
        let bus = zbus::Connection::session().await.map_err(err)?;
        // Subscribe to the reply before asking, as the portal documentation requires.
        let token = format!(
            "homeport{}",
            homeport_kernel::hex(&homeport_kernel::random_bytes::<4>())
        );
        let sender = bus
            .unique_name()
            .ok_or("No D-Bus connection name.")?
            .trim_start_matches(':')
            .replace('.', "_");
        let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
        let request = zbus::Proxy::new(
            &bus,
            "org.freedesktop.portal.Desktop",
            path.as_str(),
            "org.freedesktop.portal.Request",
        )
        .await
        .map_err(err)?;
        let mut responses = request.receive_signal("Response").await.map_err(err)?;
        let options: HashMap<&str, Value> = HashMap::from([
            ("handle_token", Value::from(token.as_str())),
            (
                "reason",
                Value::from("Start Homeport in the background when you log in."),
            ),
            ("autostart", Value::from(enable)),
            ("commandline", Value::from(vec!["homeport", "background"])),
        ]);
        let background = zbus::Proxy::new(
            &bus,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Background",
        )
        .await
        .map_err(err)?;
        let _: OwnedObjectPath = background
            .call("RequestBackground", &("", options))
            .await
            .map_err(err)?;
        let reply = std::future::poll_fn(|cx| std::pin::Pin::new(&mut responses).poll_next(cx))
            .await
            .ok_or("The Background portal did not answer.")?;
        let (code, results): (u32, HashMap<String, OwnedValue>) =
            reply.body().deserialize().map_err(err)?;
        if code != 0 {
            return Err("Start at login was not allowed.".into());
        }
        Ok(results
            .get("autostart")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(enable))
    }
    #[cfg(not(target_os = "linux"))]
    pub async fn request(_: bool) -> Result<bool, String> {
        Err("The Background portal exists only on Linux.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_entries_quote_the_executable() {
        let entry = desktop_entry("/home/a b/100%/homeport\"$").unwrap();
        assert!(
            entry.contains("Exec=\"/home/a b/100%%/homeport\\\\\"\\\\$\" background\n"),
            "{entry}"
        );
        assert!(desktop_entry("/bad\npath").is_err());
    }
    #[test]
    fn launch_agents_run_the_app_binary_at_load() {
        let plist = launch_agent("/Applications/R&D.app/Contents/MacOS/homeport");
        assert!(plist.contains("<string>/Applications/R&amp;D.app/Contents/MacOS/homeport</string>\n\t\t<string>background</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(plist.contains("<string>io.github.homeport</string>"));
    }
}
