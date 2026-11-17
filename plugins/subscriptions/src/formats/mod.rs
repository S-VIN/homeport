//! Built-in document formats: links and Base64, Xray JSON, Mihomo / Clash YAML,
//! WireGuard / AmneziaWG configs and Amnezia VPN keys (`vpn://`).
//! Each answers [`Parse`](crate::Parse) for what it recognizes (claim by
//! content); another plugin adds a format the same way, with its own
//! `p.handle(…Parse…).claim(…)` and, for direct links, the [`LINK`](crate::LINK) hook.
pub mod amnezia;
pub mod clash;
pub mod uri;
pub mod wireguard;
pub mod xray;

pub(crate) fn register(p: &mut rproxy_kernel::prelude::Setup) {
    uri::register(p);
    xray::register(p);
    clash::register(p);
    wireguard::register(p);
    amnezia::register(p);
}
