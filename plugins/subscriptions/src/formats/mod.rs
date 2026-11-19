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

pub(crate) fn register(p: &mut homeport_kernel::prelude::Setup) {
    uri::register(p);
    xray::register(p);
    clash::register(p);
    wireguard::register(p);
    amnezia::register(p);
}

/// The servers of a document that could be read; the others are counted and
/// skipped with one warning in the log. Fails only when there were servers and
/// none could be read (the last working list is kept then). `name` labels each server.
pub fn usable(
    items: impl IntoIterator<Item = (String, homeport_kernel::Result<Vec<plugin_servers::Proxy>>)>,
) -> homeport_kernel::Result<crate::Parsed> {
    const NAMED: usize = 3;
    let (mut good, mut bad, mut why, mut read) = (Vec::new(), Vec::new(), Vec::new(), 0);
    for (name, item) in items {
        read += 1;
        match item {
            Ok(p) => good.extend(p),
            Err(e) => {
                bad.push(format!("{name}: {e}"));
                why.push(e.message);
            }
        }
    }
    if good.is_empty() && !bad.is_empty() {
        return Err(homeport_kernel::Error::msg(bad.swap_remove(0)));
    }
    if !bad.is_empty() {
        let more = match bad.len().saturating_sub(NAMED) {
            0 => String::new(),
            n => format!(" and {n} more"),
        };
        homeport_kernel::log::warn!(
            "Skipped {} of {} servers: {}{more}",
            bad.len(),
            read,
            bad[..bad.len().min(NAMED)].join("; ")
        );
    }
    Ok(crate::Parsed {
        proxies: good,
        skipped: why,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn unreadable_servers_are_counted() {
        let parsed = super::uri::parse_links(
            "vless://00000000-0000-0000-0000-000000000001@example.org:443?security=tls#A\nnot-a-link\nss://broken",
        )
        .unwrap();
        assert_eq!((parsed.proxies.len(), parsed.skipped.len()), (1, 2));
    }
}
