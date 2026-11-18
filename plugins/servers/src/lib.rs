//! # servers (required)
//!
//! The outbound catalog every other plugin builds on: servers grouped by
//! source, the manual selection and the main route of mihomo, server tests
//! with their rating ([`checks`]) and Auto select on them ([`auto`]).
//!
//! Extension points for other plugins:
//! * ops [`ReplaceFromSource`], [`RemoveSource`], [`Select`], [`CheckAll`];
//! * tests: checks that count in the rating ([`CHECKS`], [`add_check`]) and
//!   the way tests reach a server ([`TEST_PATH`]);
//! * hooks [`EXCLUDE`] (why a server must not be used), [`SOURCE`] (label,
//!   color and priority of a source), [`ROUTE`], [`FAILOVER`], [`IDLE`],
//!   [`FREEZE`]; columns ([`column`]) may sort the list;
//! * UI zones on the Servers page: [`TOOLBAR`], [`COLUMNS`],
//!   [`DETAILS`] and in the connection card: [`CONNECTION`].
pub mod auto;
mod checks;
mod import;
mod link;
mod ui;

pub use checks::*;
use homeport_kernel::{prelude::*, shell::TrayLink};
pub use import::{Options, Proxy};
pub use link::is_link;
use std::collections::HashSet;

pub static PLUGIN: Plugin = Plugin {
    id: "servers",
    name: "Servers",
    description: "Server catalog, manual connection and the main route.",
    category: categories::SERVERS,
    required: true,
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

choices!(pub enum Protocol {
    Vless => "vless",
    Ss => "ss",
    Hysteria2 => "hysteria2",
    Http => "http",
    Socks5 => "socks5",
    Trojan => "trojan",
    Vmess => "vmess",
    Hysteria => "hysteria",
    Tuic => "tuic",
    AnyTls => "anytls",
    Ssr => "ssr",
    Snell => "snell",
    WireGuard => "wireguard",
});
impl Protocol {
    /// TLS is implied by the protocol.
    pub(crate) fn implies_tls(self) -> bool {
        matches!(
            self,
            Self::Hysteria2 | Self::Trojan | Self::AnyTls | Self::Tuic
        )
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "servers")]
pub struct Server {
    pub id: Id,
    pub source: Id,
    pub name: String,
    pub protocol: Protocol,
    pub address: String,
    pub port: u16,
    /// Protocol options for mihomo (everything except name, type, server, port).
    pub options: Options,
}

pub static SERVERS: State<Vec<Server>> = State::new("servers.list");
pub static SELECTED: State<Option<Id>> = State::new("servers.selected");

/// Name of the mihomo group that carries user traffic (`select`, or `fallback` with [`FAILOVER`]).
pub const MAIN: &str = "main";
/// Where traffic sent "via server" goes. A plugin may answer with another
/// target, e.g. an exit server dialed through `main` (selected → exit → internet).
pub static ROUTE: Hook<(), Option<Target>> = Hook::exclusive("servers.route");
/// The target for traffic sent via the server (see [`ROUTE`]).
pub fn route(snap: &Snapshot) -> Target {
    snap.first(&ROUTE, &())
        .unwrap_or_else(|| Target::Group(MAIN.into()))
}
/// How proxied traffic leaves, for a status line: tone, icon and text
/// ("Via the selected server", "Blocked: no server is selected").
pub fn route_text(snap: &Snapshot) -> (kit::Tone, Icon, &'static str) {
    match route(snap) {
        Target::Reject => (kit::Tone::Danger, Icon::Error, "Blocked"),
        Target::Direct => (kit::Tone::Success, Icon::Exit, "Leaves from this device"),
        Target::Group(g) if g == MAIN && selected(snap).is_none() => (
            kit::Tone::Danger,
            Icon::Error,
            "Blocked: no server is selected",
        ),
        Target::Group(g) if g == MAIN => {
            (kit::Tone::Neutral, Icon::Routing, "Via the selected server")
        }
        _ => (kit::Tone::Neutral, Icon::Routing, "Via the proxy chain"),
    }
}

// ---- ops ------------------------------------------------------------------

/// Replace all servers of one source. Ids are stable across refreshes.
pub struct ReplaceFromSource {
    pub source: Id,
    pub proxies: Vec<Proxy>,
    /// Checked in the same write as the replacement: drops a stale response
    /// (say, the subscription link changed while it was loading).
    pub guard: Option<homeport_kernel::Guard>,
}
#[derive(Clone, Debug, Default)]
pub struct Replaced {
    pub added: Vec<Id>,
    pub removed: Vec<Id>,
    pub total: usize,
}
impl Op for ReplaceFromSource {
    type Reply = Replaced;
}
/// Drop all servers of a source.
pub struct RemoveSource(pub Id);
impl Op for RemoveSource {
    type Reply = ();
}
/// Connect to a server (`None` disconnects).
pub struct Select {
    pub id: Option<Id>,
    /// An automatic decision, made while `from` was selected. If the
    /// selection changed meanwhile (the user picked a server), it is cancelled.
    pub auto: Option<Option<Id>>,
}
impl Select {
    /// The user's choice.
    pub fn manual(id: Option<Id>) -> Self {
        Self { id, auto: None }
    }
    /// An automatic switch away from `from`.
    pub fn auto(id: Option<Id>, from: Option<Id>) -> Self {
        Self {
            id,
            auto: Some(from),
        }
    }
}
impl Op for Select {
    type Reply = ();
}

async fn replace_source(cx: Cx, op: ReplaceFromSource) -> Result<Replaced> {
    let mut fresh = Vec::new();
    let mut seen = HashSet::new();
    for proxy in op.proxies {
        let server = proxy.normalize(op.source)?;
        if seen.insert(server.id) {
            fresh.push(server);
        }
    }
    cx.write(move |w| {
        if let Some(guard) = &op.guard {
            guard.check(w)?;
        }
        let before: Vec<Id> = w
            .get(&SERVERS)
            .iter()
            .filter(|s| s.source == op.source)
            .map(|s| s.id)
            .collect();
        let ids: HashSet<Id> = fresh.iter().map(|s| s.id).collect();
        let added: Vec<Id> = fresh
            .iter()
            .map(|s| s.id)
            .filter(|id| !before.contains(id))
            .collect();
        let removed: Vec<Id> = before
            .iter()
            .copied()
            .filter(|id| !ids.contains(id))
            .collect();
        let total = fresh.len();
        // A refresh that changes the connected server's settings changes its id:
        // keep the connection on its successor instead of disconnecting.
        let current = *w.get(&SELECTED);
        let successor = current
            .filter(|id| !ids.contains(id))
            .and_then(|id| {
                w.get(&SERVERS)
                    .iter()
                    .find(|s| s.id == id && s.source == op.source)
                    .cloned()
            })
            .and_then(|old| successor(&old, &fresh));
        w.update(&SERVERS, |list| {
            list.retain(|s| s.source != op.source);
            list.extend(fresh);
        });
        if let Some(next) = successor {
            set_selected(w, Some(next), true);
        } else if current.is_some_and(|id| !w.get(&SERVERS).iter().any(|s| s.id == id)) {
            set_selected(w, None, true);
        }
        if !added.is_empty() || !removed.is_empty() {
            w.emit(ServersChanged {
                source: op.source,
                added: added.clone(),
                removed: removed.clone(),
            });
        }
        Ok(Replaced {
            added,
            removed,
            total,
        })
    })
    .await
}

// ---- events ---------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ServersChanged {
    pub source: Id,
    pub added: Vec<Id>,
    pub removed: Vec<Id>,
}
/// The connection changed: by the user, a policy (`auto`) or because the
/// connected server was refreshed or removed (`auto` too).
#[derive(Clone, Debug)]
pub struct SelectionChanged {
    pub from: Option<Id>,
    pub to: Option<Id>,
    pub auto: bool,
}

/// The connected server was dropped because a plugin excluded it.
#[derive(Clone, Debug)]
pub struct Excluded {
    pub server: Server,
    pub reason: String,
}

// ---- hooks ----------------------------------------------------------------

/// Why a server must not be used. Excluded servers close the list in groups
/// under the caption `group`.
#[derive(Clone, Debug)]
pub struct Exclusion {
    pub reason: String,
    pub group: &'static str,
    /// The user's choice (e.g. a name filter): the server is never tested and
    /// is disconnected when selected. Otherwise (e.g. it failed a test)
    /// automatic choices avoid it and tests retry it; the user may still connect.
    pub permanent: bool,
}
pub static EXCLUDE: Hook<Server, Option<Exclusion>> = Hook::new("servers.exclude");
/// What the owner of a source (e.g. a subscription) says about it.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceInfo {
    /// What the source is, e.g. "Subscription" (names its field and filter).
    pub kind: &'static str,
    /// Human label, e.g. the subscription host.
    pub label: String,
    /// Tag color (index into the kit's tag colors): every screen shows the
    /// source in the same color.
    pub hue: usize,
    /// Priority for automatic choices, lower first (e.g. the position in the list).
    pub rank: usize,
}
/// About a source id; see [`source`] for the answer without an owner.
pub static SOURCE: Hook<Id, Option<SourceInfo>> = Hook::new("servers.source");
/// Keep row order stable (e.g. while a check updates ratings).
pub static FREEZE: Hook<(), bool> = Hook::new("servers.freeze");
/// Backup servers for the connected one (the hook input). With an answer and
/// at least one backup, [`MAIN`] becomes a mihomo `fallback` group
/// `[selected, backups…]`: traffic moves to the first live backup within
/// seconds, without waiting for a decision. Answers change the member order, which
/// is a full core reload, so they should change only when a policy decides.
#[derive(Clone, Debug, PartialEq)]
pub struct Failover {
    pub backups: Vec<Id>,
    /// URL mihomo tests the members with.
    pub url: String,
    pub interval_secs: u32,
}
pub static FAILOVER: Hook<Id, Option<Failover>> = Hook::exclusive("servers.failover");
/// Why nothing is connected (e.g. "Auto select · No server answers…"), shown in
/// the connection card instead of the generic hint.
pub static IDLE: Hook<(), Option<String>> = Hook::exclusive("servers.idle");

pub fn exclusion(snap: &Snapshot, s: &Server) -> Option<Exclusion> {
    snap.first(&EXCLUDE, s)
}
/// [`SOURCE`], or for a source nobody describes: "Source", a color its id
/// picks and the last rank.
pub fn source(snap: &Snapshot, id: Id) -> SourceInfo {
    snap.first(&SOURCE, &id).unwrap_or_else(|| SourceInfo {
        kind: "Source",
        label: "Source".into(),
        hue: hue_of(id),
        rank: usize::MAX,
    })
}
/// The preferred tag color of an id (stable across runs).
pub fn hue_of(id: Id) -> usize {
    id.0.as_bytes()
        .iter()
        .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(*b as usize))
        % kit::palette::TAG_HUES
}
/// The colored tag of a source, truncated to `max_width`.
pub(crate) fn source_tag(
    ui: &mut egui::Ui,
    snap: &Snapshot,
    id: Id,
    max_width: f32,
) -> egui::Response {
    let SourceInfo { label, hue, .. } = source(snap, id);
    kit::tag(ui, &label, hue, max_width)
}
/// Sort key that orders "Auto 2" before "Auto 10" (case-insensitive,
/// ignoring leading emoji).
pub(crate) fn natural_key(s: &str) -> Vec<(String, u64)> {
    // Leading flags and emoji ("🇫🇮 Helsinki") do not decide the order.
    let s = s.trim_start_matches(|c: char| !c.is_alphanumeric());
    let mut out = Vec::new();
    let mut text = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            let mut n: u64 = c.to_digit(10).unwrap().into();
            while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
                n = n.saturating_mul(10).saturating_add(d.into());
                chars.next();
            }
            out.push((std::mem::take(&mut text), n));
        } else {
            text.extend(c.to_lowercase());
        }
    }
    if !text.is_empty() {
        out.push((text, 0));
    }
    out
}
/// The server with this id.
pub fn server(snap: &Snapshot, id: Id) -> Option<Server> {
    snap.get(&SERVERS).iter().find(|s| s.id == id).cloned()
}
pub fn selected(snap: &Snapshot) -> Option<Server> {
    server(snap, (*snap.get(&SELECTED))?)
}

// ---- UI zones ---------------------------------------------------------------

/// Inline controls in the Servers toolbar (toggles, task buttons).
pub static TOOLBAR: Slot<()> =
    Slot::new("servers.toolbar", "Servers page toolbar: compact controls");
/// Under "No servers yet": ways to get servers.
pub static EMPTY: Slot<()> = Slot::new(
    "servers.empty",
    "Servers page without servers: ways to add some",
);
/// Columns on the right of every row (rating, latency…): `p.column(&COLUMNS, …)`.
pub static COLUMNS: Columns<Server> =
    Columns::new("servers.columns", "Servers rows: extra columns");
/// Sections in an expanded server row.
pub static DETAILS: Slot<Server> =
    Slot::new("servers.details", "Expanded server: extra properties");
/// Lines in the connection card for the selected server.
pub static CONNECTION: Slot<Server> = Slot::new(
    "servers.connection",
    "Connection card: lines about the selected server",
);

fn setup(p: &mut Setup) -> Result<()> {
    p.collection(&SERVERS);
    p.setting(&SELECTED);

    p.handle(replace_source);
    p.handle(remove_source);
    p.handle(select);
    // A filter that excludes the connected server disconnects it, also one
    // turned on while the app was closed.
    p.watch(excluded_selection, |cx: Cx, _| disconnect_excluded(cx));
    p.on(|cx: Cx, _: Started| disconnect_excluded(cx));
    p.core(fragment);
    // Red when the connected server failed its last test.
    p.hook(&homeport_kernel::shell::TRAY, |snap, _| {
        let id = (*snap.get(&SELECTED))?;
        Some(
            match health(snap, id).is_some_and(|h| h.rating == Some(0)) {
                true => TrayLink::Failed,
                false => TrayLink::Connected,
            },
        )
    });
    checks::register(p);
    auto::setup(p);
    ui::register(p);
    Ok(())
}

async fn remove_source(cx: Cx, RemoveSource(source): RemoveSource) -> Result<()> {
    cx.write(move |w| {
        let removed: Vec<Id> = w
            .get(&SERVERS)
            .iter()
            .filter(|s| s.source == source)
            .map(|s| s.id)
            .collect();
        if removed.is_empty() {
            return Ok(());
        }
        w.update(&SERVERS, |l| l.retain(|s| s.source != source));
        if w.get(&SELECTED).is_some_and(|id| removed.contains(&id)) {
            set_selected(w, None, true);
        }
        w.emit(ServersChanged {
            source,
            added: vec![],
            removed,
        });
        Ok(())
    })
    .await
}

async fn select(cx: Cx, op: Select) -> Result<()> {
    let snap = cx.snapshot();
    if let Some(id) = op.id {
        let server = snap
            .get(&SERVERS)
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| Error::msg("Server no longer exists."))?;
        // A manual choice may override a failed test, never a filter.
        if let Some(e) = exclusion(&snap, &server).filter(|e| e.permanent || op.auto.is_some()) {
            bail!("{}", e.reason);
        }
        if !cx.core().status().running() {
            bail!("Mihomo is offline. Restart the core first.");
        }
    }
    cx.write(move |w| {
        let from = *w.get(&SELECTED);
        if op.auto.is_some_and(|expected| expected != from) {
            return Err(Error::cancel("The selection changed meanwhile."));
        }
        if op
            .id
            .is_some_and(|id| !w.get(&SERVERS).iter().any(|s| s.id == id))
        {
            bail!("Server no longer exists.");
        }
        set_selected(w, op.id, op.auto.is_some());
        Ok(())
    })
    .await?;
    // Policies say why they switch themselves.
    if op.auto.is_none() {
        match op.id.and_then(|id| server(&cx.snapshot(), id)) {
            Some(s) => info!("Connected to {}", s.name),
            None => info!("Disconnected"),
        }
    }
    Ok(())
}

/// The connected server, when the user's choice (a filter) excludes it.
fn excluded_selection(snap: &Snapshot) -> Option<(Server, String)> {
    let s = selected(snap)?;
    let e = exclusion(snap, &s).filter(|e| e.permanent)?;
    Some((s, e.reason))
}
async fn disconnect_excluded(cx: Cx) -> Result<()> {
    let Some((s, reason)) = excluded_selection(&cx.snapshot()) else {
        return Ok(());
    };
    cx.call(Select::auto(None, Some(s.id))).await?;
    cx.notify(&format!("Disconnected {}: {reason}", s.name));
    cx.emit(Excluded { server: s, reason });
    Ok(())
}

/// The only writer of [`SELECTED`]: every change emits [`SelectionChanged`].
fn set_selected(w: &mut homeport_kernel::Write<'_>, to: Option<Id>, auto: bool) {
    let from = *w.get(&SELECTED);
    if from != to {
        w.set(&SELECTED, to);
        w.emit(SelectionChanged { from, to, auto });
    }
}

/// Every server as a mihomo proxy, and the main group that routes through the
/// selected one (a fallback group when a plugin offers backups).
fn fragment(snap: &Snapshot) -> Fragment {
    let servers = snap.get(&SERVERS);
    let proxies: Vec<mihomo::Proxy> = servers
        .iter()
        .map(|s| mihomo::Proxy {
            name: s.id.to_string(),
            protocol: s.protocol.to_string(),
            server: s.address.clone(),
            port: s.port,
            options: s.options.0.as_ref().clone(),
            dialer: None,
        })
        .collect();
    let known: HashSet<Id> = servers.iter().map(|s| s.id).collect();
    let selected = (*snap.get(&SELECTED)).filter(|id| known.contains(id));
    let failover = selected
        .and_then(|id| snap.first(&FAILOVER, &id).map(|f| (id, f)))
        .map(|(id, mut f)| {
            let mut seen = HashSet::from([id]);
            f.backups.retain(|b| known.contains(b) && seen.insert(*b));
            (id, f)
        })
        .filter(|(_, f)| !f.backups.is_empty());
    let main = match failover {
        Some((id, f)) => mihomo::Group {
            name: MAIN.into(),
            kind: mihomo::GroupKind::Fallback {
                url: f.url,
                interval_secs: f.interval_secs,
            },
            members: std::iter::once(id)
                .chain(f.backups)
                .map(|id| Target::Proxy(id.to_string()))
                .collect(),
            selected: Some(Target::Proxy(id.to_string())),
        },
        None => mihomo::Group {
            name: MAIN.into(),
            kind: mihomo::GroupKind::Select,
            members: proxies
                .iter()
                .map(|p| Target::Proxy(p.name.clone()))
                .collect(),
            selected: selected.map(|id| Target::Proxy(id.to_string())),
        },
    };
    let mut labels: Vec<(String, String)> = servers
        .iter()
        .map(|s| (s.id.to_string(), s.name.clone()))
        .collect();
    labels.push((MAIN.into(), "proxy".into()));
    // The core's final rule: whatever no other rule caught.
    labels.push(("Match".into(), "Everything else".into()));
    Fragment {
        groups: vec![main],
        proxies,
        rules: vec![(Stage::Final, CoreRule::Match(route(snap)))],
        labels,
        ..Default::default()
    }
}

/// The refreshed server that replaces `old`: same name (preferring the same
/// endpoint), else the same protocol, address and port.
fn successor(old: &Server, fresh: &[Server]) -> Option<Id> {
    let endpoint = |s: &Server| {
        s.protocol == old.protocol
            && s.address.eq_ignore_ascii_case(&old.address)
            && s.port == old.port
    };
    let named: Vec<&Server> = fresh.iter().filter(|s| s.name == old.name).collect();
    named
        .iter()
        .find(|s| endpoint(s))
        .or_else(|| named.first())
        .map(|s| s.id)
        .or_else(|| fresh.iter().find(|s| endpoint(s)).map(|s| s.id))
}

/// `(label, value)` lines of a property list.
pub(crate) type Properties = Vec<(String, String)>;
/// Safe, displayable properties of a server's outbound settings:
/// `(common, rare)`; rare ones belong under "More details".
pub(crate) fn details(s: &Server) -> (Properties, Properties) {
    let text = |key: &str| {
        s.options
            .0
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
    };
    let tls = s.options.0.get("tls").and_then(|v| v.as_bool()) == Some(true);
    let reality = s.options.0.contains_key("reality-opts");
    let mut common = vec![(
        "TLS".to_string(),
        match (reality, tls || s.protocol.implies_tls()) {
            (true, _) => "REALITY",
            (false, true) => "On",
            (false, false) => "Off",
        }
        .to_string(),
    )];
    let mut rare = Vec::new();
    for (label, value, is_rare) in [
        ("Transport", text("network"), false),
        ("SNI", text("sni").or_else(|| text("servername")), false),
        ("Fingerprint", text("client-fingerprint"), true),
        ("Cipher", text("cipher"), true),
        ("Flow", text("flow"), true),
    ] {
        if let Some(v) = value {
            if is_rare { &mut rare } else { &mut common }.push((label.into(), v.to_owned()));
        }
    }
    for (label, outer, key) in [
        ("WebSocket path", "ws-opts", "path"),
        ("WebSocket host", "ws-opts", "host"),
        ("gRPC service", "grpc-opts", "grpc-service-name"),
        ("XHTTP path", "xhttp-opts", "path"),
        ("XHTTP mode", "xhttp-opts", "mode"),
        ("Reality key", "reality-opts", "public-key"),
        ("Reality short ID", "reality-opts", "short-id"),
    ] {
        if let Some(v) = s
            .options
            .0
            .get(outer)
            .and_then(|o| o.get(key))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
        {
            rare.push((label.into(), v.to_owned()));
        }
    }
    (common, rare)
}

#[cfg(test)]
mod tests {
    use super::natural_key;
    #[test]
    fn a_refreshed_server_keeps_its_connection() {
        use super::{Id, Protocol, Server, successor};
        let source = Id::random();
        let server = |name: &str, address: &str, port: u16| Server {
            id: Id::random(),
            source,
            name: name.into(),
            protocol: Protocol::Vless,
            address: address.into(),
            port,
            options: Default::default(),
        };
        let old = server("Berlin", "a.example", 443);
        let renamed = server("Berlin 2", "A.example", 443);
        let moved = server("Berlin", "b.example", 8443);
        let twin = server("Berlin", "a.example", 443);
        assert_eq!(
            successor(&old, &[renamed.clone(), moved.clone()]),
            Some(moved.id),
            "same name first"
        );
        assert_eq!(
            successor(&old, &[moved.clone(), twin.clone()]),
            Some(twin.id),
            "same name and endpoint best"
        );
        assert_eq!(
            successor(&old, std::slice::from_ref(&renamed)),
            Some(renamed.id),
            "else the same endpoint"
        );
        assert_eq!(successor(&old, &[server("Paris", "c.example", 443)]), None);
    }
    #[test]
    fn natural_order_puts_2_before_10() {
        let mut names = vec!["Auto 10", "auto 2", "Auto 1", "Beta", "Auto"];
        names.sort_by_key(|n| natural_key(n));
        assert_eq!(names, ["Auto", "Auto 1", "auto 2", "Auto 10", "Beta"]);
    }
}
