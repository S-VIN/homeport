//! Lists of share links ([`Proxy::from_link`]) and Base64-wrapped documents.
use crate::{LINK, Parse, Parsed};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE},
};
use homeport_kernel::prelude::*;
use plugin_servers::{Proxy, is_link};

fn invalid(m: impl Into<String>) -> Error {
    Error::msg(m)
}

fn plain(body: &str) -> &str {
    body.trim().trim_start_matches('\u{feff}').trim()
}
fn looks_base64(body: &str) -> bool {
    let b = plain(body);
    !b.is_empty() && !b.starts_with("vless://") && !b.contains([':', '{', '['])
}

/// Decode a Base64 (standard or URL-safe, padding optional) document.
pub fn decode_base64(body: &str) -> Result<String> {
    let mut compact: String = plain(body).split_whitespace().collect();
    while !compact.len().is_multiple_of(4) {
        compact.push('=');
    }
    let bytes = STANDARD
        .decode(&compact)
        .or_else(|_| URL_SAFE.decode(&compact))
        .map_err(|_| invalid("Unrecognized subscription format."))?;
    String::from_utf8(bytes).map_err(|_| invalid("Subscription is not UTF-8."))
}

/// Parse share links, one per line; a link that cannot be read is skipped
/// (see [`super::usable`]).
pub fn parse_links(body: &str) -> Result<Parsed> {
    let links = plain(body).lines().map(str::trim).filter(|s| !s.is_empty());
    super::usable(
        links
            .enumerate()
            .map(|(i, link)| (link_name(link, i), Proxy::from_link(link).map(|p| vec![p]))),
    )
}
/// The name in a link's `#fragment`, else its line number.
fn link_name(link: &str, i: usize) -> String {
    let name = link.rsplit_once('#').map(|(_, name)| {
        let (mut out, bytes) = (Vec::new(), name.as_bytes());
        let mut at = 0;
        while at < bytes.len() {
            let hex = name.get(at + 1..at + 3).filter(|_| bytes[at] == b'%');
            match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(b) => (out.push(b), at += 3),
                None => (out.push(bytes[at]), at += 1),
            };
        }
        String::from_utf8_lossy(&out).into_owned()
    });
    name.filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("Line {}", i + 1))
}

pub(crate) fn register(p: &mut Setup) {
    p.handle(|cx: Cx, op: Parse| async move {
        let decoded = decode_base64(&op.body)?;
        if plain(&decoded).is_empty() {
            return Ok(Parsed::default());
        }
        // Re-dispatch: the decoded text is claimed by whichever format knows it.
        cx.call(Parse {
            source: op.source,
            body: decoded,
        })
        .await
    })
    .claim(|_, op| looks_base64(&op.body).then_some(Claim::content(90)));
    p.handle(|_: Cx, op: Parse| async move { parse_links(&op.body) })
        .claim(|_, op| is_link(plain(&op.body)).then_some(Claim::content(80)));
    p.hook(&LINK, |_, raw| {
        is_link(raw).then(|| Proxy::from_link(raw).map(drop))
    });
}
