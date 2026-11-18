//! Cloudflare WARP as an exit: a free WireGuard account registered by the app.
//! Traffic leaves from a Cloudflare address, like the 1.1.1.1 app's users.
use base64::{Engine, engine::general_purpose::STANDARD};
use homeport_kernel::{Cx, Error, Result, mihomo, random_bytes};
use serde_json::{Value, json};
use std::time::Duration;

const REGISTER: &str = "https://api.cloudflareclient.com/v0a2158/reg";
/// WARP's WireGuard port; the API answers endpoints with port 0.
const PORT: u16 = 2408;

/// A registered WARP device.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub private_key: String,
    pub peer_public_key: String,
    /// IPv4 address of the WireGuard endpoint.
    pub endpoint: String,
    pub address_v4: String,
    pub address_v6: String,
    /// Client id bytes WARP expects in every packet.
    pub reserved: [u8; 3],
}

/// Register a new device: direct first, then through the ordinary route.
pub async fn register(cx: &Cx) -> Result<Account> {
    let secret = x25519_dalek::StaticSecret::from(random_bytes::<32>());
    let public = x25519_dalek::PublicKey::from(&secret);
    let body = json!({
        "key": STANDARD.encode(public.as_bytes()), "install_id": "", "fcm_token": "",
        "tos": "2026-01-01T00:00:00.000Z", "model": "PC", "type": "Android", "locale": "en_US",
    });
    let post = |client: reqwest::Client| {
        client
            .post(REGISTER)
            .header("User-Agent", "okhttp/3.12.1")
            .header("CF-Client-Version", "a-6.10-2158")
            .timeout(Duration::from_secs(20))
            .json(&body)
            .send()
    };
    let response = cx.core().direct_or_routed("Cloudflare", post).await?;
    let v: Value = response
        .json()
        .await
        .map_err(|e| Error::msg(format!("Unexpected answer from Cloudflare: {e}")))?;
    let c = &v["config"];
    let text = |v: &Value| {
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::msg("Unexpected answer from Cloudflare."))
    };
    let endpoint = text(&c["peers"][0]["endpoint"]["v4"])?;
    let client_id = STANDARD
        .decode(text(&c["client_id"])?)
        .map_err(|_| Error::msg("Unexpected answer from Cloudflare."))?;
    Ok(Account {
        private_key: STANDARD.encode(secret.to_bytes()),
        peer_public_key: text(&c["peers"][0]["public_key"])?,
        endpoint: endpoint
            .rsplit_once(':')
            .map_or(endpoint.clone(), |(ip, _)| ip.to_owned()),
        address_v4: text(&c["interface"]["addresses"]["v4"])?,
        address_v6: text(&c["interface"]["addresses"]["v6"])?,
        reserved: client_id
            .get(..3)
            .and_then(|b| b.try_into().ok())
            .unwrap_or_default(),
    })
}

/// The WireGuard outbound. Junk packets before the handshake (AmneziaWG
/// options WARP tolerates) keep DPI from recognizing plain WireGuard.
pub fn proxy(name: &str, a: &Account, dial: Option<mihomo::Target>) -> mihomo::Proxy {
    let mut options = serde_json::Map::new();
    options.insert("ip".into(), json!(a.address_v4));
    options.insert("ipv6".into(), json!(a.address_v6));
    options.insert("private-key".into(), json!(a.private_key));
    options.insert("public-key".into(), json!(a.peer_public_key));
    options.insert("reserved".into(), json!(a.reserved));
    options.insert("udp".into(), json!(true));
    options.insert("mtu".into(), json!(1280));
    options.insert(
        "amnezia-wg-option".into(),
        json!({"jc": 4, "jmin": 40, "jmax": 70}),
    );
    mihomo::Proxy {
        name: name.into(),
        protocol: "wireguard".into(),
        server: a.endpoint.clone(),
        port: PORT,
        options,
        dialer: dial,
    }
}
