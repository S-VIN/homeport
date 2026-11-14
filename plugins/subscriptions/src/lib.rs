//! # subscriptions
//!
//! Sources of servers: subscription URLs and direct links. Loading a source is
//! a chain of overridable ops:
//!
//! ```text
//! RefreshAll ─► RefreshSource{id} ─► Fetch{url} → body ─► Parse{body} → proxies ─► servers::ReplaceFromSource
//! ```
//! * format plugins answer [`Parse`] for the documents they recognize (claim by content);
//! * a plugin with its own download algorithm wraps [`Fetch`] or claims [`RefreshSource`]
//!   for the sources it owns;
//! * [`LINK`] lets format plugins accept direct links (`vless://…`) when a source is added.
use plugin_servers::{self as servers, Proxy, ReplaceFromSource};
use rproxy_kernel::{
    kit::{self, Tone},
    prelude::*,
    ui::{Column, ListEditor},
};
use std::io::Read;

pub static PLUGIN: Plugin = Plugin {
    id: "subscriptions",
    name: "Subscriptions",
    description: "Load servers from subscription URLs and links, refreshed on a schedule.",
    category: categories::SERVERS,
    requires: &["servers"],
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
    /// What the provider calls the subscription (`profile-title`).
    pub title: Option<String>,
    /// Traffic used and allowed, in bytes, and the end of the plan (unix
    /// seconds), from the provider's `subscription-userinfo`.
    pub used: Option<u64>,
    pub total: Option<u64>,
    pub expires: Option<u64>,
}
impl Source {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            id: Id::random(),
            url: url.into(),
            updated: None,
            error: None,
            title: None,
            used: None,
            total: None,
            expires: None,
        }
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
            Ok(u) => {
                let name = u.fragment().map(percent).filter(|f| !f.is_empty());
                name.unwrap_or_else(|| format!("{} link", u.scheme()))
            }
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
        Some(JobSpec::new("Refresh subscriptions").page("subscriptions"))
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
            JobSpec::keyed(self.id.to_string(), "Load subscription")
                .page("subscriptions")
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
    type Reply = Vec<Proxy>;
}

/// Accept a direct link as a source: `Some(Ok)` = valid, `Some(Err)` = known but invalid.
pub static LINK: Hook<str, Option<std::result::Result<(), String>>> =
    Hook::new("subscriptions.link");
/// Blocks below the subscription list.
pub static SECTIONS: Slot<()> = Slot::new(
    "subscriptions.sections",
    "Subscriptions page: sections under the list",
);

pub const MAX_BYTES: usize = 10 * 1024 * 1024;

/// Sources being downloaded right now (rows show a spinner).
pub static LOADING: State<Vec<Id>> = State::new("subscriptions.loading");

fn validate(snap: &Snapshot, s: &mut Source, others: &[Source]) -> Result<()> {
    let raw = s.url.trim();
    let url = url::Url::parse(raw)
        .map_err(|_| Error::msg("Enter an HTTP(S) subscription URL or a supported link."))?;
    if matches!(url.scheme(), "http" | "https") {
        if url.host().is_none() {
            bail!("Enter an HTTP(S) subscription URL or a supported link.");
        }
        s.url = url.to_string();
    } else {
        match snap.hooks(&LINK, raw).into_iter().flatten().next() {
            Some(Ok(())) => s.url = raw.to_owned(),
            Some(Err(e)) => bail!("{e}"),
            None => bail!(
                "No enabled format plugin understands {}:// links.",
                url.scheme()
            ),
        }
    }
    if others.iter().any(|o| o.url == s.url) {
        bail!("Subscription already exists.");
    }
    Ok(())
}

/// Download a subscription document (size limit, gzip sniffing, UTF-8). Plugins that
/// wrap [`Fetch`] reuse it with their own client.
pub async fn http_get(client: &reqwest::Client, url: &str, device_id: &str) -> Result<Document> {
    let mut res = client
        .get(url)
        .header("x-hwid", device_id)
        .header("x-device-os", std::env::consts::OS)
        .header("x-device-model", "rproxy")
        // Providers pick the document format by client; this one gets plain share links.
        .header(
            reqwest::header::USER_AGENT,
            concat!("rproxy/", env!("CARGO_PKG_VERSION"), " (v2rayN compatible)"),
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
            "The provider did not accept this installation's device ID. Contact your subscription provider to allow rproxy.",
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
        if cx.call(RefreshSource { id }).await.is_err() {
            cx.checkpoint()?;
            failed.push(id);
        }
        cx.progress(i as u64 + 1, total);
    }
    if failed.is_empty() {
        return Ok(());
    }
    // Name them, and promise kept servers only where some were loaded before.
    let sources = cx.get(&SOURCES);
    let names: Vec<String> = failed
        .iter()
        .filter_map(|id| label(&sources, *id))
        .collect();
    let kept = sources
        .iter()
        .any(|s| failed.contains(&s.id) && s.updated.is_some());
    let what = if names.len() == 1 {
        names[0].clone()
    } else {
        kit::plural(names.len(), "subscription", "subscriptions")
    };
    bail!(
        "{what} could not refresh.{}",
        if kept {
            " Servers loaded before are kept."
        } else {
            ""
        }
    )
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
        let proxies = if body.trim().trim_start_matches('\u{feff}').trim().is_empty() {
            Vec::new()
        } else {
            cx.call(Parse {
                source: op.id,
                body,
            })
            .await?
        };
        cx.checkpoint()?;
        let still = input.clone();
        let replace = ReplaceFromSource {
            source: op.id,
            proxies,
            still_current: Some(Box::new(move |w| still.current(w))),
        };
        cx.call(replace).await.map(|_| info)
    }
    .await;
    if result.as_ref().is_err_and(|e| e.cancelled()) {
        return result.map(drop);
    }
    let outcome = result.as_ref().cloned().map_err(|e| e.message.clone());
    cx.write(move |w| {
        input.check(w)?;
        w.update(&SOURCES, |list| {
            if let Some(s) = list.iter_mut().find(|s| s.id == op.id) {
                match outcome {
                    Ok(info) => {
                        s.updated = Some(now());
                        s.error = None;
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
    result.map(drop)
}

fn setup(p: &mut Setup) -> Result<()> {
    let dir = p.data_dir();
    // Earlier versions kept the ID in the app folder; move it so providers
    // that limit devices keep recognising this installation.
    if let Some(app) = dir
        .ancestors()
        .nth(2)
        .filter(|_| !dir.join("device-id").exists())
    {
        let _ = std::fs::rename(app.join("device-id"), dir.join("device-id"));
    }
    let identity = device_id(&dir)?;
    p.live(&DEVICE_ID, move || identity.clone());
    p.live(&LOADING, Vec::new);
    p.list(&SOURCES)
        .validate(|snap, s, _, others| validate(snap, s, others));
    p.editable_setting(&SETTINGS);

    p.handle(refresh_all);
    p.handle(refresh_source);
    p.handle(|cx: Cx, f: Fetch| async move {
        if f.url.starts_with("http://") || f.url.starts_with("https://") {
            fetch_http(&cx, cx.http(), &f.url).await
        } else {
            // A direct link is its own document.
            Ok(Document {
                body: f.url,
                info: Info::default(),
            })
        }
    })
    .rank(Rank::Fallback);
    p.handle(|_: Cx, _: Parse| async move {
        Err::<Vec<Proxy>, _>(Error::msg(
            "Unrecognized subscription format. Is a format plugin enabled?",
        ))
    })
    .rank(Rank::Fallback);

    p.on(|cx: Cx, e: Added<Source>| async move { cx.spawn(RefreshSource { id: e.item.id }).map(drop) });
    p.on(|cx: Cx, e: Saved<Source>| async move {
        if e.before.url != e.item.url {
            cx.spawn(RefreshSource { id: e.item.id })?;
        }
        Ok(())
    });
    p.on(|cx: Cx, e: Removed<Source>| async move {
        if let Some(run) = cx.running(&RefreshSource { id: e.item.id }) {
            let _ = cx.cancel(run.id);
        }
        cx.call(servers::RemoveSource(e.item.id)).await
    });
    p.on(|cx: Cx, _: Started| async move {
        if !cx.get(&SOURCES).is_empty() {
            cx.spawn(RefreshAll)?;
        }
        Ok(())
    });
    p.every(
        "refresh",
        |s| {
            Some(std::time::Duration::from_secs(
                s.get(&SETTINGS).refresh_secs.into(),
            ))
        },
        || RefreshAll,
    );
    p.hook(&servers::SOURCE_LABEL, |snap, id| {
        label(&snap.get(&SOURCES), *id)
    });
    p.hook(&servers::SOURCE_HUE, |snap, id| {
        hue(&snap.get(&SOURCES), *id)
    });

    p.page_fn(
        PageSpec {
            id: "subscriptions",
            title: "Subscriptions",
            icon: Icon::Subscriptions,
            order: 20,
        },
        page,
    );
    p.fill(&SETTINGS_SECTIONS, "refresh", 0, |ui, cx, _| {
        cx.setting(
            ui,
            "Refresh subscriptions",
            &SETTINGS,
            |s| &mut s.refresh_secs,
            |ui, v| kit::interval(ui, "refresh", v),
        );
    });
    Ok(())
}

/// Tag colors: each source keeps its preferred color unless an earlier one has
/// it, so up to eight subscriptions never share a color.
fn hue(sources: &[Source], id: Id) -> Option<usize> {
    let mut taken = [false; kit::palette::TAG_HUES];
    for s in sources {
        let preferred = servers::hue_of(s.id);
        let hue = (0..taken.len())
            .map(|i| (preferred + i) % taken.len())
            .find(|h| !taken[*h])
            .unwrap_or(preferred);
        taken[hue] = true;
        if s.id == id {
            return Some(hue);
        }
    }
    None
}

/// Traffic and term of the plan, when the provider reports them.
fn plan(s: &Source) -> Option<(String, bool)> {
    let traffic = match (s.used, s.total) {
        (Some(used), Some(total)) => Some(format!("{} of {}", kit::bytes(used), kit::bytes(total))),
        // Without a limit only real use says something.
        (Some(used), None) if used > 0 => Some(format!("{} used", kit::bytes(used))),
        _ => None,
    };
    // Providers put a far-away date for "no end"; past ten years it is that.
    let left = s
        .expires
        .map(|end| end as i64 - now() as i64)
        .filter(|left| *left < 10 * 365 * 86_400);
    let term = left.map(|left| match (left + 43_200) / 86_400 {
        _ if left <= 0 => "expired".to_owned(),
        0 => "expires today".to_owned(),
        days => format!("expires in {}", kit::plural(days as usize, "day", "days")),
    });
    let alarming =
        left.is_some_and(|l| l <= 3 * 86_400) || s.used.zip(s.total).is_some_and(|(u, t)| u >= t);
    let text: Vec<String> = traffic.into_iter().chain(term).collect();
    (!text.is_empty()).then(|| (text.join(" · "), alarming))
}

fn status(s: &Source, loading: bool) -> (String, Tone) {
    let (text, tone) = load_status(s, loading);
    match plan(s).filter(|_| !loading) {
        Some((plan, alarming)) => {
            let tone = if alarming { Tone::Warning } else { tone };
            (format!("{text} · {plan}"), tone)
        }
        None => (text, tone),
    }
}

fn load_status(s: &Source, loading: bool) -> (String, Tone) {
    let age = s.updated.filter(|&t| t > 0).map(kit::age);
    match (&s.error, loading) {
        (_, true) => ("Loading…".into(), Tone::Neutral),
        (Some(e), _) => (
            match age {
                Some(a) => format!(
                    "Refresh failed · {} · last loaded {a}",
                    e.trim_end_matches('.')
                ),
                None => format!("Refresh failed · {}", e.trim_end_matches('.')),
            },
            Tone::Warning,
        ),
        (None, _) => (
            age.map_or_else(|| "Not loaded yet".into(), |a| format!("Updated {a}")),
            Tone::Neutral,
        ),
    }
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let snap = cx.snapshot().clone();
    let sources = snap.get(&SOURCES);
    let servers = snap.get(&servers::SERVERS);
    let counter = if sources.is_empty() {
        String::new()
    } else {
        format!(
            "{} · {}",
            kit::plural(sources.len(), "subscription", "subscriptions"),
            kit::plural(servers.len(), "server", "servers")
        )
    };
    kit::page_header(ui, "Subscriptions", &counter, |ui| {
        cx.job_button(
            ui,
            Icon::Refresh,
            "Refresh all",
            !sources.is_empty(),
            || RefreshAll,
        );
    });
    kit::page_note(
        ui,
        "Auto select prefers servers from subscriptions higher in the list.",
    );

    let loading = cx.live(&LOADING);
    ListEditor::new(&SOURCES)
        .add_form(
            "Add subscription",
            || Source::new(""),
            |ui, s, width| {
                let r = kit::input(
                    ui,
                    "url",
                    &mut s.url,
                    "Subscription URL or server link",
                    width,
                    false,
                );
                kit::submitted(ui, &r) && !s.url.trim().is_empty()
            },
        )
        .leading(8., |ui, cx, s: &Source| {
            kit::tag_dot(ui, servers::source_hue(cx.snapshot(), s.id));
        })
        .column(
            Column::edit("Subscription", "Subscription URL", |s: &mut Source| {
                &mut s.url
            })
            .display(move |s: &Source| label(&sources, s.id).unwrap_or_else(|| s.label())),
        )
        .column(
            Column::custom("Servers", 56., |ui, cx, s: &Source| {
                if loading.contains(&s.id) {
                    kit::spinner(ui);
                } else {
                    let n = cx
                        .get(&servers::SERVERS)
                        .iter()
                        .filter(|x| x.source == s.id)
                        .count();
                    ui.label(n.to_string());
                }
            })
            .right(),
        )
        .column(
            Column::custom("", 30., move |ui, cx, s: &Source| {
                let idle = !cx.live(&LOADING).contains(&s.id);
                if kit::icon_button(ui, Icon::Refresh, "Refresh", idle).clicked() {
                    cx.run(RefreshSource { id: s.id });
                }
            })
            .right(),
        )
        .subtitle(|rows, i| Some(status(&rows[i], loading.contains(&rows[i].id))))
        .empty(
            Icon::Subscriptions,
            "No subscriptions",
            "Paste a subscription link or a vless:// link above.",
        )
        .show(ui, cx);
    cx.slot(ui, &SECTIONS, &());
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
        let (text, alarming) = plan(&s).unwrap();
        assert!(text.starts_with("2.9 KB of 10.0 GB · expires in"), "{text}");
        assert!(!alarming);
    }
}
