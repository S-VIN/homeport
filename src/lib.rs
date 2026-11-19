//! The homeport application: the list of compiled-in plugins. Dependencies are
//! resolved by the kernel; the order here only breaks ties (e.g. which handler
//! runs first among equals).
//! To add a plugin: add its crate to Cargo.toml and one line here.
use homeport_kernel::Plugin;

pub fn catalog() -> Vec<&'static Plugin> {
    vec![
        // In page order: Settings → Plugins lists them like the navigation.
        &plugin_servers::PLUGIN,
        &plugin_subscriptions::PLUGIN,
        &plugin_url_tests::PLUGIN,
        &plugin_name_filters::PLUGIN,
        &plugin_routing::PLUGIN,
        &plugin_local_proxy::PLUGIN,
        &plugin_vless_inbound::PLUGIN,
        &plugin_dns::PLUGIN,
        &plugin_statistics::PLUGIN,
        &plugin_logs::PLUGIN,
        &plugin_chain_exit::PLUGIN,
        #[cfg(feature = "desktop")]
        &plugin_install_to_server::PLUGIN,
        // Desktop integration (Flatpak uses the Background portal); the plugin
        // itself declares the systems it supports.
        #[cfg(feature = "desktop")]
        &plugin_autostart::PLUGIN,
    ]
}

#[cfg(test)]
mod tests {
    /// `requires` lists every plugin crate a plugin depends on (the compiler
    /// checks the other way), so the kernel turns a plugin off when a plugin
    /// it uses is missing.
    #[test]
    fn requires_matches_the_plugin_crates_used() {
        for p in super::catalog() {
            let manifest = format!("{}/plugins/{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"), p.id);
            let manifest = std::fs::read_to_string(&manifest).unwrap();
            let mut used: Vec<_> = manifest
                .lines()
                .filter_map(|l| l.strip_prefix("plugin-")?.split([' ', '=']).next())
                .collect();
            let mut requires: Vec<_> = p.requires.iter().map(|r| r.id).collect();
            used.sort();
            requires.sort();
            assert_eq!(used, requires, "{}", p.id);
        }
    }

    /// A plugin builds only on required ones: optional plugins reach each
    /// other through extension points, never through each other's crates.
    #[test]
    fn plugins_require_only_required_plugins() {
        for p in super::catalog() {
            for r in p.requires {
                assert!(r.required, "{} requires optional {}", p.id, r.id);
            }
        }
    }
}
