//! # subscriptions
//!
//! Sources of servers: subscription URLs and direct links. Loading a source is
//! a chain of overridable ops:
//!
//! ```text
//! RefreshAll ─► RefreshSource{id} ─► Fetch{url} → body ─► Parse{body} → proxies ─► servers::ReplaceFromSource
//! ```
//! * [`formats`] answer [`Parse`] for the documents they recognize (claim by content);
//!   another plugin adds a format the same way;
//! * a plugin with its own download algorithm wraps [`Fetch`] or claims [`RefreshSource`]
//!   for the sources it owns;
//! * [`LINK`] accepts direct links (`vless://…`) when a source is added.
use homeport_kernel::{
    kit::{self, Tone, theme},
    prelude::*,
    ui::{Field, ListEditor},
};
use plugin_servers::{self as servers, Proxy, ReplaceFromSource};
use std::io::Read;

pub mod formats;
mod ui;

pub static PLUGIN: Plugin = Plugin {
    id: "subscriptions",
    name: "Subscriptions",
    description: "Load servers from subscription URLs and links, refreshed on a schedule.",
    category: plugin_servers::CATEGORY,
    requires: &[&plugin_servers::PLUGIN],
    required: true,
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "subscriptions")]
pub struct Source {
    pub id: Id,
    pub url: String,
    /// Unix seconds of the last successful load.
    pub updated: Option<u64>,
    pub error: Option<String>,
    /// Servers of the last load that could not be read, and why most of them.
    pub skipped: u64,
    pub skipped_why: Option<String>,
    /// What the provider calls the subscription (`profile-title`).
    pub title: Option<String>,
    /// Traffic used and allowed, in bytes, and the end of the plan (unix
    /// seconds), from the provider's `subscription-userinfo`.
    pub used: Option<u64>,
    pub total: Option<u64>,
    pub expires: Option<u64>,
    /// Tag color, chosen when the source is added and kept after others go.
    pub hue: Option<u8>,
}
impl Source {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            id: Id::random(),
            url: url.into(),
            updated: None,
            error: None,
            skipped: 0,
            skipped_why: None,
            title: None,
            used: None,
            total: None,
            expires: None,
            hue: None,
        }
    }
    /// Loaded from a provider (and refreshed), not a link or config kept as is.
    pub fn remote(&self) -> bool {
        url::Url::parse(&self.url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
    }
    /// The provider's name for it, else its host, or the link scheme for direct links.
    pub fn label(&self) -> String {
        if let Some(title) = self.title.as_ref().filter(|t| !t.trim().is_empty()) {
            return title.trim().to_owned();
        }
        match url::Url::parse(&self.url) {
            Ok(u) if matches!(u.scheme(), "http" | "https") => {
                u.host_str().unwrap_or("Subscription").to_owned()
            }
            Ok(u) if u.scheme() == "vpn" => "Amnezia key".into(),
            Ok(u) => {
                let name = u.fragment().map(percent).filter(|f| !f.is_empty());
                name.unwrap_or_else(|| format!("{} link", u.scheme()))
            }
            Err(_) if formats::wireguard::is_config(&self.url) => "WireGuard config".into(),
            Err(_) => "Subscription".into(),
        }
    }
}
/// The label of a source as shown everywhere: equal labels get `#2`, `#3`…
/// in list order, so two subscriptions from one provider stay apart.
pub fn label(sources: &[Source], id: Id) -> Option<String> {
    let at = sources.iter().position(|s| s.id == id)?;
    let base = sources[at].label();
    let same = sources[..at].iter().filter(|s| s.label() == base).count();
    Some(if same == 0 {
        base
    } else {
        format!("{base} #{}", same + 1)
    })
}

/// Percent-decode a link label. A `+` is literal here, not a space.
fn percent(s: &str) -> String {
    let query = format!("x={}", s.replace('+', "%2B"));
    url::form_urlencoded::parse(query.as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub refresh_secs: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            refresh_secs: 86_400,
        }
    }
}

pub static DEVICE_ID: State<String> = State::new("subscriptions.device-id");

pub static SOURCES: State<Vec<Source>> = State::new("subscriptions.sources");
pub static SETTINGS: State<Settings> = State::new("subscriptions.settings");

/// Refresh every source (job "Refresh subscriptions").
pub struct RefreshAll;
impl Op for RefreshAll {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Refresh subscriptions").page(&PLUGIN))
    }
}
/// Load one source and replace its servers.
pub struct RefreshSource {
    pub id: Id,
}
impl Op for RefreshSource {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(
            JobSpec::keyed(self.id.to_string(), "Refresh subscription")
                .page(&PLUGIN)
                .latest(),
        )
    }
}
/// Download the document of a source.
#[derive(Clone)]
pub struct Fetch {
    pub source: Id,
    pub url: String,
}
impl Op for Fetch {
    type Reply = Document;
}
/// A downloaded subscription and what its provider says about it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub body: String,
    pub info: Info,
}
/// Provider headers that other proxy apps show too.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub title: Option<String>,
    pub used: Option<u64>,
    pub total: Option<u64>,
    pub expires: Option<u64>,
}
impl Info {
    fn read(headers: &reqwest::header::HeaderMap) -> Info {
        let text = |key: &str| headers.get(key).and_then(|v| v.to_str().ok());
        // `upload=1; download=2; total=3; expire=4`
        let field = |name: &str| {
            text("subscription-userinfo")?
                .split(';')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
                .and_then(|(_, v)| v.trim().parse::<f64>().ok())
                .map(|v| v as u64)
        };
        let used = match (field("upload"), field("download")) {
            (None, None) => None,
            (up, down) => Some(up.unwrap_or(0).saturating_add(down.unwrap_or(0))),
        };
        let title = text("profile-title").map(|t| match t.strip_prefix("base64:") {
            Some(b64) => {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD
                    .decode(b64.trim())
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                    .unwrap_or_default()
            }
            None => t.to_owned(),
        });
        Info {
            title: title.filter(|t| !t.trim().is_empty()),
            used,
            total: field("total").filter(|t| *t > 0),
            expires: field("expire").filter(|e| *e > 0),
        }
    }
}
/// Turn a document into proxies. Format plugins claim the documents they know.
#[derive(Clone)]
pub struct Parse {
    pub source: Id,
    pub body: String,
}
impl Op for Parse {
    type Reply = Parsed;
}
/// What a document gave: its servers, and how many could not be read (the
/// subscription row says so; the log says why).
#[derive(Clone, Debug, Default)]
pub struct Parsed {
    pub proxies: Vec<Proxy>,
    /// Why each skipped server could not be read.
    pub skipped: Vec<String>,
}
impl From<Vec<Proxy>> for Parsed {
    fn from(proxies: Vec<Proxy>) -> Self {
        Self {
            proxies,
            skipped: Vec::new(),
        }
    }
}

/// Accept a direct link as a source: `Some(Ok)` = valid, `Some(Err)` = known but invalid.
pub static LINK: Hook<str, Option<Result<()>>> = Hook::new("subscriptions.link");
/// Blocks below the subscription list.
pub static SECTIONS: Slot<()> = Slot::new("subscriptions.sections");

pub const MAX_BYTES: usize = 10 * 1024 * 1024;

/// Sources being downloaded right now (rows show a spinner).
pub static LOADING: State<Vec<Id>> = State::new("subscriptions.loading");

fn validate(snap: &Snapshot, s: &mut Source, others: &[Source]) -> Result<()> {
    let raw = s.url.trim();
    const HINT: &str = "Enter an HTTP(S) subscription URL or a supported link.";
    match snap.first(&LINK, raw) {
        Some(Ok(())) => s.url = raw.to_owned(),
        Some(Err(e)) => return Err(e),
        // A pasted config is not a URL: formats answer for it above.
        None => {
            // Pasted text with a colon (`proxies: …`) is not a URL either.
            if !raw.contains("://") {
                bail!("{HINT}");
            }
            let url = url::Url::parse(raw).map_err(|_| Error::msg(HINT))?;
            if !matches!(url.scheme(), "http" | "https") {
                bail!("{}:// links are not supported.", url.scheme());
            }
            if url.host().is_none() {
                bail!("{HINT}");
            }
            s.url = url.to_string();
        }
    }
    if others.iter().any(|o| o.url == s.url) {
        bail!("Subscription already exists.");
    }
    if s.hue.is_none() {
        s.hue = Some(free_hue(others, s.id));
    }
    Ok(())
}

/// The id's preferred tag color, or the next one no other source has.
fn free_hue(others: &[Source], id: Id) -> u8 {
    let n = kit::palette::TAG_HUES;
    let preferred = servers::hue_of(id);
    let taken = |h: usize| others.iter().any(|o| o.hue == Some(h as u8));
    let hue = (0..n)
        .map(|i| (preferred + i) % n)
        .find(|h| !taken(*h))
        .unwrap_or(preferred);
    hue as u8
}

/// Download a subscription document (size limit, gzip sniffing, UTF-8). Plugins that
/// wrap [`Fetch`] reuse it with their own client.
pub async fn http_get(client: &reqwest::Client, url: &str, device_id: &str) -> Result<Document> {
    let mut res = client
        .get(url)
        .header("x-hwid", device_id)
        .header("x-device-os", std::env::consts::OS)
        .header("x-device-model", "homeport")
        // Providers pick the document format by client; this one gets plain share links.
        .header(
            reqwest::header::USER_AGENT,
            concat!(
                "homeport/",
                env!("CARGO_PKG_VERSION"),
                " (v2rayN compatible)"
            ),
        )
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| {
            Error::msg(match e.status() {
                Some(s) => format!("The subscription server returned HTTP {}.", s.as_u16()),
                None => "Cannot reach the subscription provider.".into(),
            })
        })?;
    if ["x-hwid-max-devices-reached", "x-hwid-limit"]
        .iter()
        .any(|key| res.headers().get(*key).is_some_and(|v| v == "true"))
    {
        return Err(Error::new(
            "device-limit",
            "This subscription's device limit is reached. Remove an old device in your provider account, then refresh.",
        ));
    }
    if res
        .headers()
        .get("x-hwid-not-supported")
        .is_some_and(|v| v == "true")
    {
        return Err(Error::new(
            "device-id",
            "The provider did not accept this installation's device ID. Contact your subscription provider to allow Homeport.",
        ));
    }
    if !res.status().is_success() {
        bail!(
            "The subscription server returned HTTP {}.",
            res.status().as_u16()
        );
    }
    let info = Info::read(res.headers());
    let mut bytes = Vec::new();
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|_| Error::msg("Incomplete subscription response."))?
    {
        if bytes.len() + chunk.len() > MAX_BYTES {
            bail!("Subscription exceeds 10 MiB.");
        }
        bytes.extend_from_slice(&chunk);
    }
    // Some providers send gzip without a Content-Encoding header.
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut plain = Vec::new();
        flate2::read::MultiGzDecoder::new(&bytes[..])
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut plain)
            .map_err(|_| Error::msg("Invalid compressed subscription."))?;
        if plain.len() > MAX_BYTES {
            bail!("Subscription exceeds 10 MiB.");
        }
        bytes = plain;
    }
    let body = String::from_utf8(bytes).map_err(|_| Error::msg("Subscription is not UTF-8."))?;
    Ok(Document { body, info })
}

/// A direct link is its own document. A provider unreachable directly is
/// retried the way user traffic goes: through the selected server.
async fn fetch(cx: Cx, f: Fetch) -> Result<Document> {
    if !f.url.starts_with("http://") && !f.url.starts_with("https://") {
        return Ok(Document {
            body: f.url,
            info: Info::default(),
        });
    }
    let direct = fetch_http(&cx, cx.http(), &f.url).await;
    let Err(e) = &direct else { return direct };
    if e.cancelled()
        || matches!(e.code.as_str(), "device-limit" | "device-id")
        || cx.get(&servers::SELECTED).is_none()
    {
        return direct;
    }
    let Ok(routed) = cx.core().routed_http().await else {
        return direct;
    };
    fetch_http(&cx, &routed, &f.url).await
}

/// Use the same identity for direct requests and retries through the selected proxy.
pub async fn fetch_http(cx: &Cx, client: &reqwest::Client, url: &str) -> Result<Document> {
    let id = cx.live(&DEVICE_ID);
    http_get(client, url, id.as_str()).await
}

/// An installation ID, deliberately outside exported configuration so a second
/// device never impersonates the first. Restarts and upgrades keep the same ID.
pub fn device_id(data: &std::path::Path) -> Result<String> {
    use std::io::Write;
    let path = data.join("device-id");
    let valid = |s: &str| {
        (10..=64).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'=')
    };
    if let Ok(id) = std::fs::read_to_string(&path) {
        if valid(id.trim()) {
            return Ok(id.trim().to_owned());
        }
        bail!("The saved device ID is invalid.");
    }
    let id = Id::random().to_string();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => file
            .write_all(id.as_bytes())
            .map_err(|e| Error::msg(format!("Cannot save the device ID: {e}")))?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let saved = std::fs::read_to_string(path).map_err(|e| Error::msg(e.to_string()))?;
            if valid(saved.trim()) {
                return Ok(saved.trim().to_owned());
            }
            bail!("The saved device ID is invalid.");
        }
        Err(e) => bail!("Cannot save the device ID: {e}"),
    }
    Ok(id)
}

async fn refresh_all(cx: Cx, _: RefreshAll) -> Result<()> {
    let ids: Vec<Id> = cx.get(&SOURCES).iter().map(|s| s.id).collect();
    let total = Some(ids.len() as u64);
    cx.progress(0, total);
    let mut failed = Vec::new();
    for (i, id) in ids.into_iter().enumerate() {
        cx.checkpoint()?;
        if let Err(e) = cx.call(RefreshSource { id }).await {
            cx.checkpoint()?;
            failed.push((id, e.message));
        }
        cx.progress(i as u64 + 1, total);
    }
    if failed.is_empty() {
        return Ok(());
    }
    // One says why ("Name: reason"); promise kept servers only where some
    // were loaded before.
    let sources = cx.get(&SOURCES);
    let kept =
        (sources.iter()).any(|s| s.updated.is_some() && failed.iter().any(|(id, _)| *id == s.id));
    let what = match &failed[..] {
        [(_, why)] => why.trim_end_matches('.').to_owned(),
        _ => format!(
            "{} could not refresh",
            kit::plural(failed.len(), "subscription", "subscriptions")
        ),
    };
    bail!(
        "{what}.{}",
        if kept {
            " Servers loaded before are kept."
        } else {
            ""
        }
    )
}

/// One log line when a refresh changed the servers (an error is the job's).
fn report(cx: &Cx, id: Id, result: &Result<((Info, Vec<String>), servers::Replaced)>) {
    let sources = cx.get(&SOURCES);
    let (Some(name), Some(source)) = (label(&sources, id), sources.iter().find(|s| s.id == id))
    else {
        return;
    };
    match result {
        Ok((_, r)) if !r.added.is_empty() || !r.removed.is_empty() || source.error.is_some() => {
            let change = match (r.added.len(), r.removed.len()) {
                (0, 0) => String::new(),
                (added, 0) => format!(" ({added} new)"),
                (0, removed) => format!(" ({removed} removed)"),
                (added, removed) => format!(" ({added} new, {removed} removed)"),
            };
            info!(
                "{name}: {}{change}",
                kit::plural(r.total, "server", "servers")
            );
        }
        _ => {}
    }
}

async fn refresh_source(cx: Cx, op: RefreshSource) -> Result<()> {
    // What this load depends on: the source's link. An edit or a removal
    // meanwhile makes the result stale, and it is dropped instead of saved.
    let input = cx.input(move |snap| {
        snap.get(&SOURCES)
            .iter()
            .find(|s| s.id == op.id)
            .map(|s| s.url.clone())
    });
    let Some(url) = input.value().cloned() else {
        return Ok(());
    };
    let _loading = cx.mark_live(&LOADING, op.id);
    let result = async {
        let Document { body, info } = cx
            .or_cancel(cx.call(Fetch { source: op.id, url }))
            .await??;
        let Parsed { proxies, skipped } =
            if body.trim().trim_start_matches('\u{feff}').trim().is_empty() {
                Parsed::default()
            } else {
                cx.call(Parse {
                    source: op.id,
                    body,
                })
                .await?
            };
        cx.checkpoint()?;
        let replace = ReplaceFromSource {
            source: op.id,
            proxies,
            guard: Some(input.guard()),
        };
        cx.call(replace)
            .await
            .map(|replaced| ((info, skipped), replaced))
    }
    .await;
    if result.as_ref().is_err_and(|e| e.cancelled()) {
        return result.map(drop);
    }
    report(&cx, op.id, &result);
    let result = result.map(|(info, _)| info);
    let outcome = result.as_ref().cloned().map_err(|e| e.message.clone());
    cx.write(move |w| {
        input.check(w)?;
        w.update(&SOURCES, |list| {
            if let Some(s) = list.iter_mut().find(|s| s.id == op.id) {
                match outcome {
                    Ok((info, skipped)) => {
                        s.updated = Some(now());
                        s.error = None;
                        s.skipped = skipped.len() as u64;
                        s.skipped_why = most_common(skipped);
                        (s.title, s.used, s.total, s.expires) =
                            (info.title, info.used, info.total, info.expires);
                    }
                    Err(e) => s.error = Some(e),
                }
            }
        });
        Ok(())
    })
    .await?;
    // The kernel logs a job's error once: it names the subscription.
    result
        .map(drop)
        .map_err(|e| match label(&cx.get(&SOURCES), op.id) {
            Some(name) => Error::msg(format!("{name}: {}", e.message)),
            None => e,
        })
}

fn setup(p: &mut Setup) -> Result<()> {
    formats::register(p);
    let identity = device_id(&p.data_dir())?;
    p.live(&DEVICE_ID, move || identity.clone());
    p.live(&LOADING, Vec::new);
    p.list(&SOURCES)
        .validate(|snap, s, _, others| validate(snap, s, others));
    p.editable_setting(&SETTINGS);

    p.handle(refresh_all);
    p.handle(refresh_source);
    p.handle(fetch).rank(Rank::Fallback);
    p.handle(|_: Cx, _: Parse| async move {
        Err::<Parsed, _>(Error::msg("Unrecognized subscription format."))
    })
    .rank(Rank::Fallback);
    p.wrap(remove_with_servers);
    p.on(|cx: Cx, e: Added<Source>| async move { cx.spawn(RefreshSource { id: e.item.id }).map(drop) });
    p.on(link_changed);
    p.on(|cx: Cx, _: Started| async move {
        if !cx.get(&SOURCES).is_empty() {
            cx.spawn(RefreshAll)?;
        }
        Ok(())
    });
    p.every(
        "refresh",
        // Nothing to refresh without subscriptions.
        |s| {
            (!s.get(&SOURCES).is_empty())
                .then(|| std::time::Duration::from_secs(s.get(&SETTINGS).refresh_secs.into()))
        },
        || RefreshAll,
    );
    p.hook(&servers::SOURCE, source_info);
    ui::register(p);
    Ok(())
}

/// A new link loads the servers again.
async fn link_changed(cx: Cx, e: Saved<Source>) -> Result<()> {
    if e.before.url != e.item.url {
        cx.spawn(RefreshSource { id: e.item.id })?;
    }
    Ok(())
}

/// Its servers go first: a crash between the two steps leaves a subscription
/// without servers, which the next refresh fills, never servers of a gone one.
async fn remove_with_servers(cx: Cx, op: Remove<Source>, next: Next<Remove<Source>>) -> Result<()> {
    if let Some(run) = cx.running(&RefreshSource { id: op.id }) {
        let _ = cx.cancel(run.id);
    }
    cx.call(servers::RemoveSource(op.id)).await?;
    next.run(&cx, op).await
}

/// How the servers list labels and orders the servers of a subscription.
fn source_info(snap: &Snapshot, id: &Id) -> Option<servers::SourceInfo> {
    let sources = snap.get(&SOURCES);
    let rank = sources.iter().position(|s| s.id == *id)?;
    let s = &sources[rank];
    Some(servers::SourceInfo {
        kind: "Subscription",
        label: label(&sources, s.id)?,
        hue: s.hue.map_or_else(|| servers::hue_of(s.id), usize::from),
        rank,
    })
}

/// The reason most skipped servers share.
fn most_common(reasons: Vec<String>) -> Option<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for r in reasons {
        match counts.iter_mut().find(|(x, _)| *x == r) {
            Some((_, n)) => *n += 1,
            None => counts.push((r, 1)),
        }
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(r, _)| r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_headers_give_name_traffic_and_term() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            "subscription-userinfo",
            "upload=1000; download=2000; total=10737418240; expire=1900000000"
                .parse()
                .unwrap(),
        );
        // "My VPN" in base64.
        h.insert("profile-title", "base64:TXkgVlBO".parse().unwrap());
        let info = Info::read(&h);
        assert_eq!(info.title.as_deref(), Some("My VPN"));
        assert_eq!(info.used, Some(3000));
        assert_eq!(info.total, Some(10737418240));
        assert_eq!(info.expires, Some(1900000000));
        assert_eq!(
            Info::read(&reqwest::header::HeaderMap::new()),
            Info::default()
        );

        let mut s = Source::new("https://example.org/sub");
        (s.title, s.used, s.total, s.expires) = (info.title, info.used, info.total, info.expires);
        assert_eq!(s.label(), "My VPN");
        let (text, alarming) = ui::plan(&s).unwrap();
        assert!(text.starts_with("2.9 KB of 10.0 GB · expires in"), "{text}");
        assert!(!alarming);
    }
}
