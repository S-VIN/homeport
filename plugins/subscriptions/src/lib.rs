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
    required: false,
    default_enabled: true,
    setup,
};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "subscriptions")]
pub struct Source {
    pub id: Id,
    pub url: String,
    /// Unix seconds of the last successful load.
    pub updated: Option<u64>,
    pub error: Option<String>,
}
impl Source {
    pub fn new(url: impl Into<String>) -> Self {
        Self { id: Id::random(), url: url.into(), updated: None, error: None }
    }
    /// Provider host, or the link scheme for direct links.
    pub fn label(&self) -> String {
        match url::Url::parse(&self.url) {
            Ok(u) if matches!(u.scheme(), "http" | "https") => u.host_str().unwrap_or("Source").to_owned(),
            Ok(u) => {
                let name = u.fragment().map(percent).filter(|f| !f.is_empty());
                name.unwrap_or_else(|| format!("{} link", u.scheme()))
            }
            Err(_) => "Source".into(),
        }
    }
}
fn percent(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={s}").as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub refresh_secs: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self { refresh_secs: 86_400 }
    }
}

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
        Some(JobSpec::keyed(format!("subscriptions/{}", self.id), "Load subscription").page("subscriptions"))
    }
}
/// Download the document of a source.
#[derive(Clone)]
pub struct Fetch {
    pub source: Id,
    pub url: String,
}
impl Op for Fetch {
    type Reply = String;
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
pub static LINK: Hook<str, Option<std::result::Result<(), String>>> = Hook::new("subscriptions.link");
/// Blocks below the subscription list.
pub static SECTIONS: Slot<()> = Slot::new("subscriptions.sections", "Subscriptions page: sections under the list");

pub const MAX_BYTES: usize = 10 * 1024 * 1024;

/// Sources being downloaded right now (rows show a spinner).
static LOADING: std::sync::Mutex<Vec<Id>> = std::sync::Mutex::new(Vec::new());
pub fn loading(source: Id) -> bool {
    LOADING.lock().unwrap().contains(&source)
}
struct Loading(Id);
impl Drop for Loading {
    fn drop(&mut self) {
        LOADING.lock().unwrap().retain(|x| *x != self.0);
    }
}

fn validate(snap: &Snapshot, s: &mut Source, others: &[Source]) -> Result<()> {
    let raw = s.url.trim();
    let url = url::Url::parse(raw).map_err(|_| Error::msg("Enter an HTTP(S) subscription URL or a supported link."))?;
    if matches!(url.scheme(), "http" | "https") {
        if url.host().is_none() {
            bail!("Enter an HTTP(S) subscription URL or a supported link.");
        }
        s.url = url.to_string();
    } else {
        match snap.hooks(&LINK, raw).into_iter().flatten().next() {
            Some(Ok(())) => s.url = raw.to_owned(),
            Some(Err(e)) => bail!("{e}"),
            None => bail!("No enabled format plugin understands {}:// links.", url.scheme()),
        }
    }
    if others.iter().any(|o| o.url == s.url) {
        bail!("Subscription already exists.");
    }
    Ok(())
}

/// Download a subscription document (size limit, gzip sniffing, UTF-8). Plugins that
/// wrap [`Fetch`] reuse it with their own client.
pub async fn http_get(client: &reqwest::Client, url: &str) -> Result<String> {
    let mut res = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::msg(match e.status() {
            Some(s) => format!("Source returned HTTP {}.", s.as_u16()),
            None => "Cannot reach the subscription provider.".into(),
        }))?;
    let mut bytes = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|_| Error::msg("Incomplete subscription response."))? {
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
    String::from_utf8(bytes).map_err(|_| Error::msg("Subscription is not UTF-8."))
}

fn setup(p: &mut Setup) -> Result<()> {
    p.list(&SOURCES).validate(|snap, s, _, others| validate(snap, s, others));
    p.editable_setting(&SETTINGS);

    p.handle(|cx: Cx, _: RefreshAll| async move {
        let ids: Vec<Id> = cx.get(&SOURCES).iter().map(|s| s.id).collect();
        let total = Some(ids.len() as u64);
        cx.progress(0, total);
        let mut failed = 0;
        for (i, id) in ids.into_iter().enumerate() {
            cx.checkpoint()?;
            if cx.call(RefreshSource { id }).await.is_err() {
                cx.checkpoint()?;
                failed += 1;
            }
            cx.progress(i as u64 + 1, total);
        }
        if failed > 0 {
            bail!("{} could not refresh. Cached servers are kept.", kit::plural(failed, "subscription", "subscriptions"));
        }
        Ok(())
    });
    p.handle(|cx: Cx, op: RefreshSource| async move {
        let Some(source) = cx.get(&SOURCES).iter().find(|s| s.id == op.id).cloned() else { return Ok(()) };
        LOADING.lock().unwrap().push(source.id);
        let _loading = Loading(source.id);
        cx.kernel().repaint();
        let result: Result<usize> = async {
            let body = cx.or_cancel(cx.call(Fetch { source: source.id, url: source.url.clone() })).await??;
            let proxies = if body.trim().trim_start_matches('\u{feff}').trim().is_empty() {
                Vec::new()
            } else {
                cx.call(Parse { source: source.id, body }).await?
            };
            cx.checkpoint()?;
            let total = cx.call(ReplaceFromSource { source: source.id, proxies }).await?.total;
            // Removed while loading: drop what this load just added.
            if !cx.get(&SOURCES).iter().any(|s| s.id == source.id) {
                cx.call(servers::RemoveSource(source.id)).await?;
                return Err(Error::new("cancelled", "Subscription was removed"));
            }
            Ok(total)
        }
        .await;
        if result.as_ref().is_err_and(|e| e.cancelled()) {
            return result.map(drop);
        }
        let outcome = result.as_ref().map(drop).map_err(|e| e.message.clone());
        cx.write(move |w| {
            w.update(&SOURCES, |list| {
                if let Some(s) = list.iter_mut().find(|s| s.id == op.id) {
                    match outcome {
                        Ok(()) => {
                            s.updated = Some(now());
                            s.error = None;
                        }
                        Err(e) => s.error = Some(e),
                    }
                }
            });
            Ok(())
        })
        .await?;
        result.map(drop)
    });
    p.handle(|cx: Cx, f: Fetch| async move {
        if f.url.starts_with("http://") || f.url.starts_with("https://") {
            http_get(cx.http(), &f.url).await
        } else {
            // A direct link is its own document.
            Ok(f.url)
        }
    })
    .rank(Rank::Fallback);
    p.handle(|_: Cx, _: Parse| async move { Err::<Vec<Proxy>, _>(Error::msg("Unrecognized subscription format. Is a format plugin enabled?")) })
        .rank(Rank::Fallback);

    p.on(|cx: Cx, e: Added<Source>| async move { cx.spawn(RefreshSource { id: e.item.id }).map(drop) });
    p.on(|cx: Cx, e: Saved<Source>| async move {
        if e.before.url != e.item.url {
            cx.spawn(RefreshSource { id: e.item.id })?;
        }
        Ok(())
    });
    p.on(|cx: Cx, e: Removed<Source>| async move {
        if let Some(run) = (RefreshSource { id: e.item.id }).job().and_then(|j| cx.running(&j.key)) {
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
    p.every("refresh", |s| Some(std::time::Duration::from_secs(s.get(&SETTINGS).refresh_secs.into())), || RefreshAll);
    p.hook(&servers::SOURCE_LABEL, |snap, id| snap.get(&SOURCES).iter().find(|s| s.id == *id).map(Source::label));
    p.hook(&servers::SOURCE_HUE, |snap, id| hue(&snap.get(&SOURCES), *id));

    p.page_fn(PageSpec { id: "subscriptions", title: "Subscriptions", icon: Icon::Subscriptions, order: 20 }, page);
    p.fill(&SETTINGS_SECTIONS, "refresh", 0, |ui, cx, _| {
        let mut secs = cx.get(&SETTINGS).refresh_secs;
        kit::settings_row(ui, "Refresh subscriptions", "Reload every subscription on this schedule", |ui| {
            if kit::interval(ui, "refresh", &mut secs) {
                cx.update("refresh-interval", &SETTINGS, move |s| s.refresh_secs = secs);
            }
        });
    });
    Ok(())
}

/// Tag colors: each source keeps its preferred color unless an earlier one has
/// it, so up to eight subscriptions never share a color.
fn hue(sources: &[Source], id: Id) -> Option<usize> {
    let mut taken = [false; kit::palette::TAG_HUES];
    for s in sources {
        let preferred = servers::hue_of(s.id);
        let hue = (0..taken.len()).map(|i| (preferred + i) % taken.len()).find(|h| !taken[*h]).unwrap_or(preferred);
        taken[hue] = true;
        if s.id == id {
            return Some(hue);
        }
    }
    None
}

fn status(s: &Source, loading: bool) -> (String, Tone) {
    let age = s.updated.map(kit::age);
    match (&s.error, loading) {
        (_, true) => ("Loading…".into(), Tone::Neutral),
        (Some(e), _) => (
            match age {
                Some(a) => format!("Refresh failed · {e} · last loaded {a}"),
                None => format!("Refresh failed · {e}"),
            },
            Tone::Warning,
        ),
        (None, _) => (age.map_or_else(|| "Not loaded yet".into(), |a| format!("Updated {a}")), Tone::Neutral),
    }
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let snap = cx.snapshot().clone();
    let sources = snap.get(&SOURCES);
    let servers = snap.get(&servers::SERVERS);
    let counter = if sources.is_empty() {
        String::new()
    } else {
        format!("{} · {}", kit::plural(sources.len(), "subscription", "subscriptions"), kit::plural(servers.len(), "server", "servers"))
    };
    kit::page_header(ui, "Subscriptions", &counter, |ui| {
        cx.job_button(ui, Icon::Refresh, "Refresh all", 104., !sources.is_empty(), || RefreshAll);
    });
    kit::helper(ui, "Higher subscriptions win ties when Auto select picks a server. Each color tags that subscription's servers.");
    ListEditor::new(&SOURCES)
        .add_form("Add subscription", || Source::new(""), |ui, s, width| {
            let r = kit::input(ui, "url", &mut s.url, "Subscription URL or link (vless://…)", width, false);
            kit::submitted(ui, &r) && !s.url.trim().is_empty()
        })
        .leading(8., |ui, cx, s: &Source| {
            kit::tag_dot(ui, servers::source_hue(cx.snapshot(), s.id));
        })
        .column(Column::edit("Subscription", "Subscription URL", |s: &mut Source| &mut s.url).display(Source::label))
        .column(
            Column::custom("Servers", 56., |ui, cx, s: &Source| {
                if loading(s.id) {
                    kit::spinner(ui);
                } else {
                    let n = cx.get(&servers::SERVERS).iter().filter(|x| x.source == s.id).count();
                    ui.label(n.to_string());
                }
            })
            .right(),
        )
        .subtitle(|rows, i| Some(status(&rows[i], loading(rows[i].id))))
        .empty(Icon::Subscriptions, "No subscriptions", "Paste a subscription URL or a vless:// link above to load servers.")
        .show(ui, cx);
    cx.slot(ui, &SECTIONS, &());
}
