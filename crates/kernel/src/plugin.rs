use crate::{Result, Setup};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Category {
    pub id: &'static str,
    pub name: &'static str,
}
impl Category {
    pub const fn new(id: &'static str, name: &'static str) -> Self {
        Self { id, name }
    }
}

/// Well-known categories: the groups of Settings → Plugins. A plugin may declare
/// its own with [`Category::new`]. Keep the list short: a group should hold
/// several plugins.
pub mod categories {
    use super::Category;
    /// The server catalog and everything that fills, filters or measures it.
    pub const SERVERS: Category = Category::new("servers", "Servers");
    /// Local traffic (routing, listeners) and desktop integration.
    pub const NETWORK: Category = Category::new("network", "Network & system");
}

/// A system a plugin can run on: a desktop operating system, or the Docker
/// image (Linux without a desktop session, system proxy or TUN).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    Linux,
    Windows,
    Macos,
    Docker,
}
impl Platform {
    /// For plugins without OS-specific code.
    pub const ALL: &'static [Platform] = &[
        Platform::Linux,
        Platform::Windows,
        Platform::Macos,
        Platform::Docker,
    ];
    /// For plugins that integrate with a desktop session.
    pub const DESKTOP: &'static [Platform] = &[Platform::Linux, Platform::Windows, Platform::Macos];
    /// The system this process runs on. Plugins may branch on it at runtime,
    /// e.g. `if Platform::current() == Platform::Docker { … }`. Docker is
    /// detected by `RPROXY_DOCKER=1` (set by the image) or the files container
    /// runtimes create; other systems report their OS (`None` on e.g. FreeBSD).
    pub fn current() -> Option<Platform> {
        static CURRENT: std::sync::OnceLock<Option<Platform>> = std::sync::OnceLock::new();
        *CURRENT.get_or_init(|| {
            let container = cfg!(target_os = "linux")
                && (std::env::var_os("RPROXY_DOCKER").is_some_and(|v| v == "1")
                    || ["/.dockerenv", "/run/.containerenv"]
                        .iter()
                        .any(|f| std::path::Path::new(f).exists()));
            if container {
                Some(Platform::Docker)
            } else if cfg!(target_os = "linux") {
                Some(Platform::Linux)
            } else if cfg!(windows) {
                Some(Platform::Windows)
            } else if cfg!(target_os = "macos") {
                Some(Platform::Macos)
            } else {
                None
            }
        })
    }
    /// Shorthand for `Platform::current() == Some(Platform::Docker)`.
    pub fn is_docker() -> bool {
        Platform::current() == Some(Platform::Docker)
    }
    /// Flatpak's sandbox cannot configure the host system proxy or TUN through
    /// the native command integrations.
    pub fn is_flatpak() -> bool {
        cfg!(target_os = "linux")
            && (std::env::var_os("FLATPAK_ID").is_some()
                || std::path::Path::new("/.flatpak-info").is_file())
    }
    /// The AppImage file this process runs from. `APPIMAGE` alone is not
    /// enough: it leaks into programs the AppImage starts.
    pub fn appimage() -> Option<std::path::PathBuf> {
        let image = std::path::PathBuf::from(std::env::var_os("APPIMAGE")?);
        let mount = std::path::PathBuf::from(std::env::var_os("APPDIR")?);
        let exe = std::env::current_exe().ok()?;
        (image.is_absolute() && mount.is_absolute() && exe.starts_with(&mount)).then_some(image)
    }
    pub const fn name(self) -> &'static str {
        match self {
            Platform::Linux => "Linux",
            Platform::Windows => "Windows",
            Platform::Macos => "macOS",
            Platform::Docker => "Docker",
        }
    }
}

/// A plugin is a compile-time crate exporting one `pub static PLUGIN: Plugin`.
/// Enabling or disabling it takes effect after the kernel restarts.
pub struct Plugin {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub category: Category,
    /// Plugin ids that must be enabled for this one to work.
    pub requires: &'static [&'static str],
    /// Required plugins cannot be disabled.
    pub required: bool,
    pub default_enabled: bool,
    /// Systems the plugin supports. The plugin writes its own code for each of
    /// them (`#[cfg(...)]` inside the crate); the crate must still compile on
    /// every system. On others the plugin is listed in Settings as unavailable
    /// and never set up.
    pub platforms: &'static [Platform],
    pub setup: fn(&mut Setup) -> Result<()>,
}
impl Plugin {
    /// The usual values; a plugin states only what differs:
    /// `Plugin { id: "routing", name: "Routing", setup, ..Plugin::DEFAULT }`.
    /// Off by default only with `default_enabled: false`; every system unless
    /// `platforms` says otherwise.
    pub const DEFAULT: Plugin = Plugin {
        id: "",
        name: "",
        description: "",
        category: categories::NETWORK,
        requires: &[],
        required: false,
        default_enabled: true,
        platforms: Platform::ALL,
        setup: |_| Ok(()),
    };
    /// Whether the plugin runs on this system.
    pub fn supported(&self) -> bool {
        Platform::current().is_some_and(|p| self.platforms.contains(&p))
    }
    /// "Desktop only", "Linux only", "Linux and macOS only"; `None` when every system is supported.
    pub fn platforms_note(&self) -> Option<String> {
        let has = |list: &[Platform]| list.iter().all(|p| self.platforms.contains(p));
        if has(Platform::ALL) {
            return None;
        }
        if has(Platform::DESKTOP) {
            return Some("Desktop only".into());
        }
        let names: Vec<&str> = self.platforms.iter().map(|p| p.name()).collect();
        Some(match names.as_slice() {
            [] => "Not available on any system".into(),
            [one] => format!("{one} only"),
            [rest @ .., last] => format!("{} and {last} only", rest.join(", ")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plugin(platforms: &'static [Platform]) -> Plugin {
        Plugin {
            id: "p",
            name: "P",
            description: "",
            category: categories::NETWORK,
            requires: &[],
            required: false,
            default_enabled: true,
            platforms,
            setup: |_| Ok(()),
        }
    }
    #[test]
    fn platforms_decide_support_and_the_note() {
        assert!(plugin(Platform::ALL).supported());
        assert_eq!(plugin(Platform::ALL).platforms_note(), None);
        assert_eq!(
            plugin(Platform::DESKTOP).platforms_note().as_deref(),
            Some("Desktop only")
        );
        assert_eq!(
            plugin(&[Platform::Linux]).platforms_note().as_deref(),
            Some("Linux only")
        );
        assert_eq!(
            plugin(&[Platform::Linux, Platform::Windows])
                .platforms_note()
                .as_deref(),
            Some("Linux and Windows only")
        );
        assert_eq!(
            plugin(&[Platform::Linux, Platform::Docker])
                .platforms_note()
                .as_deref(),
            Some("Linux and Docker only")
        );
        assert!(!plugin(&[]).supported());
        let elsewhere: &'static [Platform] = if cfg!(windows) {
            &[Platform::Linux]
        } else {
            &[Platform::Windows]
        };
        assert!(!plugin(elsewhere).supported());
    }
    #[test]
    fn the_current_system_is_detected_once() {
        let current = Platform::current();
        assert_eq!(current, Platform::current());
        assert!(current.is_some());
        // Docker supports exactly the plugins that declare it.
        assert_eq!(
            plugin(Platform::DESKTOP).supported(),
            current != Some(Platform::Docker)
        );
        assert!(plugin(Platform::ALL).supported());
        assert!(Platform::appimage().is_none() || std::env::var_os("APPDIR").is_some());
    }
}
