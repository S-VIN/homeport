//! # chain-exit
//! A second hop after the selected server: this device → selected server (entry)
//! → exit server → internet. The entry is whatever the user or Auto select
//! connects to; the exit is one stable server added on this plugin's page.
//!
//! Mechanics: the exit becomes the mihomo proxy `exit`, dialed through the
//! `main` group (`dialer-proxy`), and answers `servers::ROUTE`, so every rule
//! that sends traffic "via server" goes through both hops. It fails closed:
//! with no entry, or an exit that cannot be used, traffic is blocked rather
//! than sent past the exit.
//!
//! Cloudflare WARP can be the exit, or the backup exit when the exit server
//! does not answer (order: exit → WARP → blocked). The entry can also be "this
//! machine": on a node that serves clients, the exit is then dialed directly,
//! so client traffic never leaves from the node's own address.
use homeport_kernel::{mihomo, prelude::*};
use plugin_servers as servers;
use plugin_subscriptions as subscriptions;
use std::time::Duration;

pub static PLUGIN: Plugin = Plugin {
    id: "chain-exit",
    name: "Chain exit",
    description: "Send proxied traffic through the current server, then one stable exit server.",
    requires: &[&plugin_servers::PLUGIN, &plugin_subscriptions::PLUGIN],
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

mod ui;
pub mod warp;

/// mihomo names of the exit proxy, the WARP proxy and the exit-then-WARP group.
pub const EXIT: &str = "exit";
pub const WARP: &str = "warp";
pub const EXIT_ROUTE: &str = "exit-route";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "chain_exit_servers")]
pub struct Exit {
    pub id: Id,
    pub name: String,
    /// What was pasted: a share link or a config any format of subscriptions
    /// reads ([`subscriptions::LINK`]).
    pub link: String,
    /// The server read from it when added.
    pub protocol: servers::Protocol,
    pub server: String,
    pub port: u16,
    pub options: servers::Options,
}
impl Exit {
    /// An exit to add: adding reads the server from `link`.
    pub fn new(link: impl Into<String>) -> Self {
        Self {
            id: Id::random(),
            name: String::new(),
            link: link.into(),
            protocol: servers::Protocol::Vless,
            server: String::new(),
            port: 0,
            options: servers::Options::default(),
        }
    }
    pub fn address(&self) -> String {
        format!("{}:{}", self.server, self.port)
    }
}

pub static EXITS: State<Vec<Exit>> = State::new("chain-exit.list");
/// The exit in use; `None` = off (one hop, as without this plugin).
pub static ACTIVE: State<Option<Id>> = State::new("chain-exit.active");
/// Last end-to-end probe through both hops: `(result, unix time)`.
pub static LAST_CHECK: State<Option<(std::result::Result<u32, String>, u64)>> =
    State::new("chain-exit.last-check");

choices!(pub enum Entry {
    Selected => "Current server",
    Direct => "Directly",
});
choices!(pub enum WarpUse {
    Off => "Off",
    Backup => "If exit fails",
    Only => "Always",
});
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    /// What the exit is dialed through.
    pub entry: Entry,
    pub warp: WarpUse,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            entry: Entry::Selected,
            warp: WarpUse::Off,
        }
    }
}
pub static SETTINGS: State<Settings> = State::new("chain-exit.settings");
/// The registered WARP device, if set up.
pub static WARP_ACCOUNT: State<Option<warp::Account>> = State::new("chain-exit.warp");

/// Register a WARP device (job "Set up WARP").
pub struct SetUpWarp;
impl Op for SetUpWarp {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Set up WARP").page(&PLUGIN))
    }
}
/// Forget the WARP device.
pub struct RemoveWarp;
impl Op for RemoveWarp {
    type Reply = ();
}

/// What carries traffic after the entry.
#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    Exit(Exit),
    Warp,
    /// The exit server; WARP when it does not answer.
    ExitThenWarp(Exit),
}
impl Plan {
    /// The mihomo name traffic is sent to.
    pub fn target(&self) -> &'static str {
        match self {
            Self::Exit(_) => EXIT,
            Self::Warp => WARP,
            Self::ExitThenWarp(_) => EXIT_ROUTE,
        }
    }
    pub fn name(&self) -> String {
        match self {
            Self::Exit(e) => e.name.clone(),
            Self::Warp => "Cloudflare WARP".into(),
            Self::ExitThenWarp(e) => format!("{} (WARP if it fails)", e.name),
        }
    }
}
pub fn plan(snap: &Snapshot) -> Option<Plan> {
    let warp = snap.get(&WARP_ACCOUNT).is_some();
    let use_warp = snap.get(&SETTINGS).warp;
    match (active(snap), warp, use_warp) {
        (_, true, WarpUse::Only) => Some(Plan::Warp),
        (Some(e), true, WarpUse::Backup) => Some(Plan::ExitThenWarp(e)),
        (Some(e), _, _) => Some(Plan::Exit(e)),
        (None, true, WarpUse::Backup) => Some(Plan::Warp),
        _ => None,
    }
}
fn through_entry(snap: &Snapshot) -> bool {
    snap.get(&SETTINGS).entry == Entry::Selected
}
/// While traffic goes server → exit, servers are tested the same way: each
/// through the exit (`via-<server>`), so a rating says how the chain works.
fn tests_through_exit(snap: &Snapshot) -> bool {
    through_entry(snap) && plan(snap).is_some()
}
/// The mihomo name of the exit dialed through server `id`.
fn via(id: Id) -> String {
    format!("via-{id}")
}

/// Use an exit server, or turn the second hop off with `None`.
pub struct Use(pub Option<Id>);
impl Op for Use {
    type Reply = ();
    const SAVES: bool = true;
}
/// Measure latency through the entry and the exit (job "Check exit").
pub struct CheckExit;
impl Op for CheckExit {
    type Reply = u32;
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Test proxy chain").quiet().page(&PLUGIN))
    }
}

/// Read the server from what was pasted, with the formats of subscriptions.
async fn read_exit(cx: Cx, mut op: Add<Exit>, next: Next<Add<Exit>>) -> Result<Id> {
    const ONE: &str = "Paste the link of one server.";
    let e = &mut op.item;
    e.link = e.link.trim().to_owned();
    match cx.snapshot().first(&subscriptions::LINK, &e.link) {
        Some(Ok(())) => {}
        Some(Err(err)) => return Err(err),
        None => bail!("{ONE}"),
    }
    let parsed = cx
        .call(subscriptions::Parse {
            source: e.id,
            body: e.link.clone(),
        })
        .await?;
    let [px] = <[servers::Proxy; 1]>::try_from(parsed.proxies).map_err(|_| Error::msg(ONE))?;
    if e.name.trim().is_empty() {
        e.name = px.name;
    }
    (e.protocol, e.server, e.port) = (px.protocol, px.server, px.port);
    e.options = servers::Options(std::sync::Arc::new(px.options));
    next.run(&cx, op).await
}

pub fn active(snap: &Snapshot) -> Option<Exit> {
    let id = (*snap.get(&ACTIVE))?;
    snap.get(&EXITS).iter().find(|e| e.id == id).cloned()
}

fn setup(p: &mut Setup) -> Result<()> {
    p.list(&EXITS)
        .validate(|_, e, _, others| validate(e, others));
    p.wrap(read_exit);
    p.setting(&ACTIVE);
    p.editable_setting(&SETTINGS);
    p.setting(&WARP_ACCOUNT);
    p.live(&LAST_CHECK, || None);
    p.handle(set_up_warp);
    p.handle(remove_warp);
    p.on(first_in_use);
    p.on(removed_in_use);
    p.handle(use_exit);
    p.handle(check_exit);
    p.every(
        "check",
        // Only while there is something to check: no entry is shown on the page, not as an error.
        |s| {
            plan(s)
                .filter(|_| !through_entry(s) || s.get(&servers::SELECTED).is_some())
                .map(|_| Duration::from_secs(120))
        },
        || CheckExit,
    );
    p.on(redial_on_new_entry).latest();
    p.core(fragment);
    p.hook(&servers::ROUTE, |snap, _| route(snap));
    p.hook(&servers::TEST_PATH, |snap, s| {
        tests_through_exit(snap).then(|| via(s.id))
    });
    ui::register(p);
    Ok(())
}

/// Named after its server unless named; no duplicates.
fn validate(e: &mut Exit, others: &[Exit]) -> Result<()> {
    e.name = e.name.trim().to_owned();
    if e.name.is_empty() {
        e.name = e.server.clone();
    }
    if others.iter().any(|o| o.link == e.link) {
        bail!("This exit server is already added.");
    }
    Ok(())
}

/// The first exit added is the one in use.
async fn first_in_use(cx: Cx, e: Added<Exit>) -> Result<()> {
    if cx.get(&ACTIVE).is_none() {
        cx.call(Use(Some(e.item.id))).await?;
    }
    Ok(())
}
async fn removed_in_use(cx: Cx, e: Removed<Exit>) -> Result<()> {
    if *cx.get(&ACTIVE) == Some(e.item.id) {
        cx.call(Use(None)).await?;
    }
    Ok(())
}

async fn set_up_warp(cx: Cx, _: SetUpWarp) -> Result<()> {
    let account = warp::register(&cx).await?;
    cx.write(move |w| {
        w.set(&WARP_ACCOUNT, Some(account));
        if w.get(&SETTINGS).warp == WarpUse::Off {
            let warp = if active(w.base()).is_some() {
                WarpUse::Backup
            } else {
                WarpUse::Only
            };
            w.update(&SETTINGS, |s| s.warp = warp);
        }
        Ok(())
    })
    .await?;
    cx.set_live(&LAST_CHECK, None);
    info!("WARP account registered");
    cx.summary("WARP is ready");
    cx.spawn(CheckExit)?;
    Ok(())
}

async fn remove_warp(cx: Cx, _: RemoveWarp) -> Result<()> {
    cx.write(|w| {
        w.set(&WARP_ACCOUNT, None);
        w.update(&SETTINGS, |s| s.warp = WarpUse::Off);
        Ok(())
    })
    .await?;
    info!("WARP removed");
    cx.set_live(&LAST_CHECK, None);
    Ok(())
}

async fn use_exit(cx: Cx, Use(id): Use) -> Result<()> {
    cx.write(move |w| {
        if id.is_some_and(|id| !w.get(&EXITS).iter().any(|e| e.id == id)) {
            bail!("This exit server no longer exists.");
        }
        w.set(&ACTIVE, id);
        Ok(())
    })
    .await?;
    let exit = id.and_then(|id| cx.get(&EXITS).iter().find(|e| e.id == id).cloned());
    match exit {
        Some(e) => info!("Exit server {} in use", e.name),
        None => info!("Exit server off"),
    }
    cx.set_live(&LAST_CHECK, None);
    if id.is_some() {
        cx.spawn(CheckExit)?;
    }
    Ok(())
}

async fn check_exit(cx: Cx, _: CheckExit) -> Result<u32> {
    let snap = cx.snapshot();
    let Some(plan) = plan(&snap) else {
        bail!("No exit server is in use.");
    };
    if through_entry(&snap) && snap.get(&servers::SELECTED).is_none() {
        bail!("Connect to a server first: it is the entry to the exit server.");
    }
    let result = cx
        .core()
        .delay(plan.target(), &servers::probe_url(&snap), 6000)
        .await
        .map_err(|e| e.message);
    // When it starts or stops answering, not on every check.
    let before = (*cx.live(&LAST_CHECK)).as_ref().map(|(r, _)| r.is_ok());
    match &result {
        Err(e) if before != Some(false) => warn!("The exit server does not answer: {e}"),
        Ok(ms) if before == Some(false) => info!("The exit server answers again ({ms} ms)"),
        _ => {}
    }
    cx.set_live(&LAST_CHECK, Some((result.clone(), now())));
    result.map_err(|_| Error::msg("The exit did not answer."))
}

/// The exit keeps its transport connections (XHTTP reuse) open for a long
/// time. A new entry must take effect now: rebuild the exit so new
/// connections are dialed through the current entry ([`route`] blocks
/// without one).
async fn redial_on_new_entry(cx: Cx, _: servers::SelectionChanged) -> Result<()> {
    let snap = cx.snapshot();
    if plan(&snap).is_some() && through_entry(&snap) {
        cx.core().reapply().await?;
    }
    Ok(())
}

fn fragment(snap: &Snapshot) -> Fragment {
    let Some(plan) = plan(snap) else {
        return Fragment::default();
    };
    let dial = through_entry(snap).then(|| Target::Group(servers::MAIN.into()));
    let mut f = Fragment {
        labels: vec![
            (EXIT.into(), "Exit server".into()),
            (WARP.into(), "WARP".into()),
        ],
        ..Fragment::default()
    };
    let exit = match &plan {
        Plan::Exit(e) | Plan::ExitThenWarp(e) => Some(e),
        Plan::Warp => None,
    };
    let account = snap.get(&WARP_ACCOUNT);
    // The exit as a mihomo proxy dialed through `dial`.
    let hop = |name: String, dial: Option<Target>| match &exit {
        Some(e) => Some(mihomo::Proxy {
            name,
            protocol: e.protocol.to_string(),
            server: e.server.clone(),
            port: e.port,
            options: (*e.options.0).clone(),
            dialer: dial,
        }),
        None => (*account).as_ref().map(|a| warp::proxy(&name, a, dial)),
    };
    f.proxies
        .extend(hop(EXIT.into(), dial.clone()).filter(|_| exit.is_some()));
    if let (Plan::Warp | Plan::ExitThenWarp(_), Some(account)) = (&plan, &*account) {
        f.proxies.push(warp::proxy(WARP, account, dial));
    }
    // Every server as the entry, for its tests (see [`tests_through_exit`]).
    if tests_through_exit(snap) {
        for s in snap.get(&servers::SERVERS).iter() {
            f.proxies
                .extend(hop(via(s.id), Some(servers::proxy_of(s.id))));
            f.labels.push((via(s.id), format!("{} → exit", s.name)));
        }
    }
    if matches!(plan, Plan::ExitThenWarp(_)) {
        f.groups.push(mihomo::Group {
            name: EXIT_ROUTE.into(),
            kind: mihomo::GroupKind::Fallback {
                url: servers::probe_url(snap),
                interval_secs: 60,
            },
            members: vec![Target::Proxy(EXIT.into()), Target::Proxy(WARP.into())],
            selected: None,
        });
    }
    f
}

/// Fail closed: an exit in use that dials through an entry while none is
/// connected blocks instead of skipping it. The block lands in the same
/// commit as the disconnect, not after a redial.
fn route(snap: &Snapshot) -> Option<Target> {
    let plan = plan(snap)?;
    let no_entry = through_entry(snap) && snap.get(&servers::SELECTED).is_none();
    Some(if no_entry {
        Target::Reject
    } else if matches!(plan, Plan::ExitThenWarp(_)) {
        Target::Group(EXIT_ROUTE.into())
    } else {
        Target::Proxy(plan.target().into())
    })
}
