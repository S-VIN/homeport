//! Mihomo / Clash documents: YAML or JSON with a top-level `proxies:` list.
//! Only outbound nodes are imported; listeners, rules and the controller stay homeport's.
use crate::Parse;
use homeport_kernel::prelude::*;
use plugin_servers::Proxy;
use serde_json::Value;

/// Parse the `proxies` list of a Mihomo/Clash document.
pub fn parse(body: &str) -> Result<Vec<Proxy>> {
    let doc: Value = serde_saphyr::from_str(body.trim_start_matches('\u{feff}'))
        .map_err(|_| Error::msg("Invalid YAML or JSON subscription."))?;
    let nodes = doc
        .get("proxies")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::msg("Expected a proxies list."))?;
    super::usable(nodes.iter().enumerate().map(|(i, v)| {
        let name = (v["name"].as_str()).map_or_else(|| format!("Server {}", i + 1), str::to_owned);
        (name, Proxy::from_value(v.clone(), i + 1).map(|p| vec![p]))
    }))
}

pub(crate) fn register(p: &mut Setup) {
    p.handle(|_: Cx, op: Parse| async move { parse(&op.body) })
        .claim(|_, op| {
            (op.body.contains("proxies:") || op.body.contains("\"proxies\""))
                .then_some(Claim::content(75))
        });
}

#[cfg(test)]
mod tests {
    #[test]
    fn proxies_list_and_rejects_bad_nodes() {
        let ok =
            super::parse("proxies:\n  - { name: a, type: http, server: localhost, port: 8080 }")
                .unwrap();
        assert_eq!(ok.len(), 1);
        assert!(
            super::parse("proxies:\n  - { name: bad, type: http, server: localhost, port: 0 }")
                .is_err()
        );
        assert!(
            super::parse(
                "proxies:\n  - { name: x, type: http, server: h, port: 1, dialer-proxy: y }"
            )
            .is_err()
        );
    }
}
