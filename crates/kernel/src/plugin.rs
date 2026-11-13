use crate::{Result, Setup};

/// How plugins of one category coexist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// At most one enabled plugin (e.g. a server-selection strategy).
    Exclusive,
    /// Any number of enabled plugins.
    Multiple,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Category {
    pub id: &'static str,
    pub name: &'static str,
    pub policy: Policy,
}
impl Category {
    pub const fn new(id: &'static str, name: &'static str, policy: Policy) -> Self {
        Self { id, name, policy }
    }
}

/// Well-known categories: the groups of Settings → Plugins. A plugin may declare
/// its own with [`Category::new`]; all plugins naming the same id must agree on
/// the policy. Keep the list short: a group should hold several plugins.
pub mod categories {
    use super::{Category, Policy::*};
    /// The server catalog and everything that fills, filters or measures it.
    pub const SERVERS: Category = Category::new("servers", "Servers", Multiple);
    /// Parsers of subscription documents.
    pub const FORMATS: Category = Category::new("subscription-format", "Subscription formats", Multiple);
    /// Strategies that pick the server automatically; one at a time.
    pub const SERVER_SELECTION: Category = Category::new("server-selection", "Server selection", Exclusive);
    /// Local traffic (routing, listeners) and desktop integration.
    pub const NETWORK: Category = Category::new("network", "Network & system", Multiple);
}

/// An operating system a plugin can run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    Linux,
    Windows,
    Macos,
}
impl Platform {
    /// For plugins without OS-specific code.
    pub const ALL: &'static [Platform] = &[Platform::Linux, Platform::Windows, Platform::Macos];
    /// The system this build runs on (`None` on others, e.g. FreeBSD).
    pub const fn current() -> Option<Platform> {
        if cfg!(target_os = "linux") {
            Some(Platform::Linux)
        } else if cfg!(windows) {
            Some(Platform::Windows)
        } else if cfg!(target_os = "macos") {
            Some(Platform::Macos)
        } else {
            None
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Platform::Linux => "Linux",
            Platform::Windows => "Windows",
            Platform::Macos => "macOS",
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
    /// Plugins that must be enabled for this one to work.
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
    /// Whether the plugin runs on this system.
    pub fn supported(&self) -> bool {
        Platform::current().is_some_and(|p| self.platforms.contains(&p))
    }
    /// "Linux only", "Linux and macOS only"; `None` when every system is supported.
    pub fn platforms_note(&self) -> Option<String> {
        if Platform::ALL.iter().all(|p| self.platforms.contains(p)) {
            return None;
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
        Plugin { id: "p", name: "P", description: "", category: categories::NETWORK, requires: &[], required: false, default_enabled: true, platforms, setup: |_| Ok(()) }
    }
    #[test]
    fn platforms_decide_support_and_the_note() {
        assert!(plugin(Platform::ALL).supported());
        assert_eq!(plugin(Platform::ALL).platforms_note(), None);
        assert_eq!(plugin(&[Platform::Linux]).platforms_note().as_deref(), Some("Linux only"));
        assert_eq!(plugin(&[Platform::Linux, Platform::Windows]).platforms_note().as_deref(), Some("Linux and Windows only"));
        assert!(!plugin(&[]).supported());
        let elsewhere: &'static [Platform] = if cfg!(windows) { &[Platform::Linux] } else { &[Platform::Windows] };
        assert!(!plugin(elsewhere).supported());
    }
}
