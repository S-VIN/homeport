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
    pub setup: fn(&mut Setup) -> Result<()>,
}
