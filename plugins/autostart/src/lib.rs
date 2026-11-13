//! # autostart
//! Start rproxy in the background when the user logs in (XDG autostart entry).
use rproxy_kernel::{kit, prelude::*};
use std::{fs, io, path::PathBuf};

pub static PLUGIN: Plugin = Plugin {
    id: "autostart",
    name: "Start at login",
    description: "Start rproxy in the background when you log in (XDG autostart).",
    category: categories::NETWORK,
    requires: &[],
    required: false,
    default_enabled: true,
    setup,
};

/// Whether the autostart entry exists (re-read after every change).
pub static ENABLED: State<bool> = State::new("autostart.enabled");

pub struct SetAutostart(pub bool);
impl Op for SetAutostart {
    type Reply = ();
}

fn xdg(var: &str, fallback: &str) -> io::Result<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|s| PathBuf::from(s).join(fallback)))
        .ok_or_else(|| io::Error::other("Missing home directory"))
}
pub fn path() -> io::Result<PathBuf> {
    Ok(xdg("XDG_CONFIG_HOME", ".config")?.join("autostart/rproxy.desktop"))
}
pub fn set(enable: bool) -> io::Result<()> {
    let path = path()?;
    if !enable {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_absolute())
        .map(Ok).unwrap_or_else(std::env::current_exe)?;
    let raw = exe.to_str().ok_or_else(|| io::Error::other("Executable path is not UTF-8"))?;
    if raw.contains(['\n', '\r']) {
        return Err(io::Error::other("Invalid executable path"));
    }
    let mut quoted = String::new();
    for c in raw.chars() {
        match c {
            '%' => quoted.push_str("%%"),
            '"' | '\\' | '$' | '`' => {
                quoted.push('\\');
                quoted.push(c);
            }
            _ => quoted.push(c),
        }
    }
    let line = format!("[Desktop Entry]\nType=Application\nName=rproxy\nExec=\"{}\" --background\nTerminal=false\n", quoted.replace('\\', "\\\\"));
    let dir = path.parent().unwrap();
    fs::create_dir_all(dir)?;
    let tmp = dir.join(".rproxy.desktop.tmp");
    fs::write(&tmp, line)?;
    fs::rename(tmp, path)
}

fn setup(p: &mut Setup) -> Result<()> {
    p.memory(&ENABLED, || path().is_ok_and(|p| p.is_file()));
    p.handle(|cx: Cx, SetAutostart(on): SetAutostart| async move {
        set(on).map_err(|e| Error::msg(e.to_string()))?;
        cx.write(|w| {
            w.set(&ENABLED, path().is_ok_and(|p| p.is_file()));
            Ok(())
        })
        .await
    });
    p.fill(&SETTINGS_SECTIONS, "toggle", 0, |ui, cx, _| {
        let mut on = *cx.get(&ENABLED);
        kit::settings_row(ui, "Start at login", "Start rproxy in the background when you log in", |ui| {
            if kit::toggle(ui, &mut on, "").changed() {
                cx.submit("autostart", SetAutostart(on));
            }
        });
    });
    Ok(())
}
