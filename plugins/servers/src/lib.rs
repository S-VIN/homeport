//! # servers (required)
//!
//! The outbound catalog every other plugin builds on: servers grouped by
//! source, the manual selection and the main route of mihomo.
//!
//! Extension points for other plugins:
//! * ops [`ReplaceFromSource`], [`RemoveSource`], [`Select`];
//! * hooks [`EXCLUDE`] (why a server must not be used), [`SOURCE_LABEL`],
//!   [`SORT_KEYS`], [`FREEZE`];
//! * UI zones on the Servers page: [`TOOLBAR`], [`TOOLS`], [`COLUMNS`],
//!   [`DETAILS`] and in the connection card: [`CONNECTION`].
mod import;
mod outbound;
mod ui;

pub use import::Proxy;
pub use outbound::{Definition, Definitions};
use rproxy_kernel::{prelude::*, shell::TrayLink};
use std::{collections::HashSet, sync::Arc};

pub static PLUGIN: Plugin = Plugin {
    id: "servers",
    name: "Servers",
    description: "Server catalog, manual connection and the main route.",
    category: categories::SERVERS,
    required: true,
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
    /// TCP-based protocols can be pinged with a TCP connect.
    pub fn tcp(self) -> bool {
        !matches!(
            self,
            Self::Hysteria | Self::Hysteria2 | Self::Tuic | Self::WireGuard
        )
    }
    /// TLS is implied by the protocol.
    pub fn implies_tls(self) -> bool {
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
}

pub static SERVERS: State<Vec<Server>> = State::new("servers.list");
pub static DEFINITIONS: State<Definitions> = State::new("servers.definitions");
pub static SELECTED: State<Option<Id>> = State::new("servers.selected");

/// Name of the mihomo group that carries user traffic (`select`, or `fallback` with [`FAILOVER`]).
pub const MAIN: &str = "main";
pub fn main_target() -> Target {
    Target::Group(MAIN.into())
}
/// How a core rule reads to the user: the core reports e.g.
/// `DomainRegex ^.*\.youtube\.com$`, the plugin that made it answers
/// `*.youtube.com`.
pub static RULE_LABEL: Hook<str, Option<String>> = Hook::new("servers.rule-label");
/// The user's words for a core rule (see [`RULE_LABEL`]), else the rule itself.
pub fn rule_label(snap: &Snapshot, rule: &str) -> String {
    snap.hooks(&RULE_LABEL, rule)
        .into_iter()
        .flatten()
        .next()
        .unwrap_or_else(|| match rule {
            // The core's final rule: whatever no other rule caught.
            "Match" => "Everything else".to_owned(),
            rule => rule.to_owned(),
        })
}
/// Where traffic sent "via server" goes. A plugin may answer with another
/// target, e.g. an exit server dialed through `main` (selected → exit → internet).
pub static ROUTE: Hook<(), Option<Target>> = Hook::new("servers.route");
/// The target for traffic sent via the server (see [`ROUTE`]).
pub fn route(snap: &Snapshot) -> Target {
    snap.hooks(&ROUTE, &())
        .into_iter()
        .flatten()
        .next()
        .unwrap_or_else(main_target)
}

// ---- ops ------------------------------------------------------------------

/// Replace all servers of one source. Ids are stable across refreshes.
pub struct ReplaceFromSource {
    pub source: Id,
    pub proxies: Vec<Proxy>,
    /// Checked in the same write as the replacement: `false` drops a stale
    /// response (say, the subscription link changed while it was loading).
    pub still_current: Option<SourceCheck>,
}
pub type SourceCheck = Box<dyn FnOnce(&rproxy_kernel::Write<'_>) -> bool + Send>;
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
    let mut defs = Vec::new();
    let mut seen = HashSet::new();
    for proxy in op.proxies {
        let (server, def) = proxy.normalize(op.source)?;
        if seen.insert(server.id) {
            fresh.push(server);
            defs.push(def);
        }
    }
    cx.write(move |w| {
        if !op.still_current.is_none_or(|check| check(w)) {
            return Err(Error::new(
                "cancelled",
                "Subscription changed while loading",
            ));
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
            w.set(&SELECTED, Some(next));
            w.emit(SelectionChanged {
                from: current,
                to: Some(next),
                auto: true,
            });
        }
        let alive: HashSet<Id> = w.get(&SERVERS).iter().map(|s| s.id).collect();
        w.update(&DEFINITIONS, |d| {
            let map = Arc::make_mut(d);
            for def in defs {
                map.entry(def.fields.id).or_insert_with(|| Arc::new(def));
            }
            map.retain(|id, _| alive.contains(id));
        });
        if w.get(&SELECTED).is_some_and(|id| !alive.contains(&id)) {
            w.set(&SELECTED, None);
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

/// Why a server must not be used. `disconnect` also drops it when selected.
/// Excluded servers close the list in groups under the caption `group`.
#[derive(Clone, Debug)]
pub struct Exclusion {
    pub reason: String,
    pub group: &'static str,
    pub disconnect: bool,
}
pub static EXCLUDE: Hook<Server, Option<Exclusion>> = Hook::new("servers.exclude");
/// Human label of a source id (e.g. the subscription host).
pub static SOURCE_LABEL: Hook<Id, Option<String>> = Hook::new("servers.source-label");
/// Tag color of a source (index into the kit's tag colors), so every screen
/// shows a source in the same color. Without an answer the id picks one.
pub static SOURCE_HUE: Hook<Id, Option<usize>> = Hook::new("servers.source-hue");
/// Extra sort orders for the list: `(label, key)`; lower keys first.
pub type SortKey = (&'static str, fn(&Snapshot, &Server) -> i64);
pub static SORT_KEYS: Hook<(), Vec<SortKey>> = Hook::new("servers.sort-keys");
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
pub static FAILOVER: Hook<Id, Option<Failover>> = Hook::new("servers.failover");
/// Why nothing is connected (e.g. "Auto select · No server answers…"), shown in
/// the connection card instead of the generic hint.
pub static IDLE: Hook<(), Option<String>> = Hook::new("servers.idle");

pub fn exclusion(snap: &Snapshot, s: &Server) -> Option<Exclusion> {
    snap.hooks(&EXCLUDE, s).into_iter().flatten().next()
}
pub fn source_label(snap: &Snapshot, id: Id) -> String {
    snap.hooks(&SOURCE_LABEL, &id)
        .into_iter()
        .flatten()
        .next()
        .unwrap_or_else(|| "Subscription".into())
}
pub fn source_hue(snap: &Snapshot, id: Id) -> usize {
    snap.hooks(&SOURCE_HUE, &id)
        .into_iter()
        .flatten()
        .next()
        .unwrap_or_else(|| hue_of(id))
}
/// The preferred tag color of an id (stable across runs).
pub fn hue_of(id: Id) -> usize {
    id.0.as_bytes()
        .iter()
        .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(*b as usize))
        % kit::palette::TAG_HUES
}
/// The colored tag of a source, truncated to `max_width`.
pub fn source_tag(ui: &mut egui::Ui, snap: &Snapshot, id: Id, max_width: f32) -> egui::Response {
    let label = source_label(snap, id);
    kit::tag(ui, &label, source_hue(snap, id), max_width)
}
/// Sort key that orders "Auto 2" before "Auto 10" (case-insensitive,
/// ignoring leading emoji).
pub fn natural_key(s: &str) -> Vec<(String, u64)> {
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
pub fn selected(snap: &Snapshot) -> Option<Server> {
    let id = (*snap.get(&SELECTED))?;
    snap.get(&SERVERS).iter().find(|s| s.id == id).cloned()
}

// ---- UI zones ---------------------------------------------------------------

/// Inline controls in the Servers toolbar (toggles, task buttons).
pub static TOOLBAR: Slot<()> =
    Slot::new("servers.toolbar", "Servers page toolbar: compact controls");
/// Full-width blocks under the toolbar (editors opened from the toolbar).
pub static TOOLS: Slot<()> = Slot::new("servers.tools", "Servers page: blocks under the toolbar");
/// Fixed-width cells on the right of every row. Draw a caption when `header`.
/// Fill it through [`column`].
pub static COLUMNS: Slot<Cell> =
    Slot::new("servers.columns", "Servers rows: extra fixed-width columns");
/// Sections in an expanded server row.
pub static DETAILS: Slot<Server> =
    Slot::new("servers.details", "Expanded server: extra properties");
/// Lines in the connection card for the selected server.
pub static CONNECTION: Slot<Server> = Slot::new(
    "servers.connection",
    "Connection card: lines about the selected server",
);

/// What a column contribution draws: a caption (`server == None`) or a cell.
pub struct Cell {
    /// `None` in the header row.
    pub(crate) server: Option<Server>,
    pub(crate) wide: bool,
    /// The server can't be used: its cells are muted.
    pub(crate) excluded: bool,
}

/// An extra column of the server list (see [`column`]).
pub struct Column {
    /// Unique within the plugin.
    pub id: &'static str,
    /// Lower is further left.
    pub order: i32,
    pub title: &'static str,
    /// Tooltip of the title.
    pub help: &'static str,
    /// Hidden when the list is narrow (a phone): only the most useful columns stay.
    pub secondary: bool,
}
/// Add a column to the server list: `cell` draws one server's value. The list
/// draws the title, keeps the columns aligned and drops secondary ones on
/// narrow screens.
pub fn column(
    p: &mut Setup,
    c: Column,
    cell: impl Fn(&mut egui::Ui, &mut UiCx, &Server) + Send + Sync + 'static,
) {
    p.fill(&COLUMNS, c.id, c.order, move |ui, cx, props: &Cell| {
        if c.secondary && !props.wide {
            return;
        }
        kit::cell_right(ui, 60., |ui| match &props.server {
            None => drop(kit::caption(ui, c.title).on_hover_text(c.help)),
            Some(s) => {
                if props.excluded {
                    ui.multiply_opacity(0.45);
                }
                cell(ui, cx, s)
            }
        });
    });
}

fn setup(p: &mut Setup) -> Result<()> {
    p.collection(&SERVERS);
    p.persist(
        &DEFINITIONS,
        outbound::register,
        outbound::read,
        outbound::sync,
    );
    p.setting(&SELECTED);

    p.handle(replace_source);
    p.handle(remove_source);
    p.handle(select);
    // A filter that excludes the connected server disconnects it.
    p.on(|cx: Cx, _: Changed| async move {
        let snap = cx.snapshot();
        if let Some(s) = selected(&snap)
            && let Some(e) = exclusion(&snap, &s).filter(|e| e.disconnect)
        {
            cx.call(Select::auto(None, Some(s.id))).await?;
            cx.notify(&format!("Disconnected {}: {}", s.name, e.reason));
            cx.emit(Excluded {
                server: s,
                reason: e.reason,
            });
        }
        Ok(())
    });
    p.core(fragment);
    p.hook(&rproxy_kernel::shell::TRAY, |snap, _| {
        snap.get(&SELECTED).map(|_| TrayLink::Connected)
    });
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
        w.update(&DEFINITIONS, |d| {
            Arc::make_mut(d).retain(|id, _| !removed.contains(id))
        });
        if w.get(&SELECTED).is_some_and(|id| removed.contains(&id)) {
            w.set(&SELECTED, None);
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
        if let Some(e) = exclusion(&snap, &server) {
            bail!("{}", e.reason);
        }
        if !cx.core().status().running() {
            bail!("Mihomo is offline. Restart the core first.");
        }
    }
    cx.write(move |w| {
        let from = *w.get(&SELECTED);
        if op.auto.is_some_and(|expected| expected != from) {
            return Err(Error::new("cancelled", "The selection changed meanwhile."));
        }
        if from != op.id {
            w.set(&SELECTED, op.id);
            w.emit(SelectionChanged {
                from,
                to: op.id,
                auto: op.auto.is_some(),
            });
        }
        Ok(())
    })
    .await
}

/// Every server as a mihomo proxy, and the main group that routes through the
/// selected one (a fallback group when a plugin offers backups).
fn fragment(snap: &Snapshot) -> Fragment {
    let servers = snap.get(&SERVERS);
    let defs = snap.get(&DEFINITIONS);
    let proxies: Vec<mihomo::Proxy> = servers
        .iter()
        .filter_map(|s| {
            let def = defs.get(&s.id)?;
            Some(mihomo::Proxy {
                name: s.id.to_string(),
                protocol: s.protocol.to_string(),
                server: s.address.clone(),
                port: s.port,
                options: def.options(),
            })
        })
        .collect();
    let known: HashSet<Id> = servers
        .iter()
        .filter(|s| defs.contains_key(&s.id))
        .map(|s| s.id)
        .collect();
    let selected = (*snap.get(&SELECTED)).filter(|id| known.contains(id));
    let failover = selected
        .and_then(|id| {
            snap.hooks(&FAILOVER, &id)
                .into_iter()
                .flatten()
                .next()
                .map(|f| (id, f))
        })
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
    Fragment {
        groups: vec![main],
        proxies,
        rules: vec![(Stage::Final, CoreRule::Match(route(snap)))],
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
pub type Properties = Vec<(String, String)>;
/// Safe, displayable properties of a server's outbound settings:
/// `(common, rare)`; rare ones belong under "More details".
pub fn details(snap: &Snapshot, s: &Server) -> (Properties, Properties) {
    let defs = snap.get(&DEFINITIONS);
    let Some(def) = defs.get(&s.id) else {
        return (vec![], vec![]);
    };
    let f = &def.fields;
    let mut common = vec![(
        "TLS".to_string(),
        if f.tls == Some(true) || s.protocol.implies_tls() {
            "On"
        } else {
            "Off"
        }
        .to_string(),
    )];
    let mut rare = Vec::new();
    for (label, value, is_rare) in [
        ("Transport", &f.network, false),
        (
            "SNI",
            if f.sni.is_some() {
                &f.sni
            } else {
                &f.server_name
            },
            false,
        ),
        ("Fingerprint", &f.fingerprint, true),
        ("Cipher", &f.cipher, true),
        ("Flow", &f.flow, true),
    ] {
        if let Some(v) = value.as_ref().filter(|v| !v.is_empty()) {
            if is_rare { &mut rare } else { &mut common }.push((label.into(), v.clone()));
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
        if let Some(v) = def
            .extra
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
