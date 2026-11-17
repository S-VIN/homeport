//! Lists of share links ([`Proxy::from_link`]) and Base64-wrapped documents.
use crate::{LINK, Parse};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE},
};
use plugin_servers::{Proxy, is_link};
use rproxy_kernel::prelude::*;

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

/// Parse supported share links without dropping any entries from mixed lists.
pub fn parse_links(body: &str) -> Result<Vec<Proxy>> {
    plain(body)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Proxy::from_link)
        .collect()
}

pub(crate) fn register(p: &mut Setup) {
    p.handle(|cx: Cx, op: Parse| async move {
        let decoded = decode_base64(&op.body)?;
        if plain(&decoded).is_empty() {
            return Ok(Vec::new());
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
        is_link(raw).then(|| Proxy::from_link(raw).map(drop).map_err(|e| e.message))
    });
}
