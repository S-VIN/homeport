//! The rproxy application: the list of compiled-in plugins. Dependencies are
//! resolved by the kernel; the order here only breaks ties (e.g. which handler
//! runs first among equals).
//! To add a plugin: add its crate to Cargo.toml and one line here.
use rproxy_kernel::Plugin;

pub fn catalog() -> Vec<&'static Plugin> {
    vec![
        &plugin_servers::PLUGIN,
        &plugin_subscriptions::PLUGIN,
        &plugin_name_filters::PLUGIN,
        &plugin_routing::PLUGIN,
        &plugin_local_proxy::PLUGIN,
        &plugin_health_checks::PLUGIN,
        &plugin_chain_exit::PLUGIN,
        &plugin_vless_inbound::PLUGIN,
        &plugin_connections::PLUGIN,
        &plugin_logs::PLUGIN,
        #[cfg(feature = "desktop")]
        &plugin_install_to_server::PLUGIN,
        // Desktop integration (Flatpak uses the Background portal); the plugin
        // itself declares the systems it supports.
        #[cfg(feature = "desktop")]
        &plugin_autostart::PLUGIN,
    ]
}
