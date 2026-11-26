//! Manages homeport on the user's own Linux servers over SSH. Connecting adds
//! a server: homeport signs in with the password once, then with its own SSH
//! key. Installing sets up the app with its web panel (the Compose stack from
//! `docker/`); the panel's address and sign-in are kept on the server in
//! `~/homeport-panel.txt`, so any homeport that connects later finds them.
//! Cleaning up takes away everything the installation added to the server.
use homeport_kernel::prelude::*;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};
pub mod deploy;
mod page;

pub static PLUGIN: Plugin = Plugin {
    id: "install-to-server",
    name: "Install to server",
    description: "Connect your Linux servers over SSH, install Homeport with its web panel on them and manage it.",
    platforms: Platform::NATIVE,
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

/// A server connected from here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Table)]
#[table(name = "install_to_server_servers")]
pub struct Server {
    pub id: Id,
    pub host: String,
    pub ssh_port: u16,
    pub ssh_login: String,
    /// "Ubuntu 24.04 LTS on x86_64".
    pub system: String,
    /// The login is not root and sudo asks for its password: actions that
    /// change the server ask for it too.
    pub sudo_password: bool,
    /// Address of the web panel; empty while homeport is not installed.
    pub panel: String,
    pub login: String,
    pub password: String,
    /// The installation folder on the server.
    pub directory: String,
    /// False while it is being installed, and when that was interrupted.
    pub ready: bool,
}
impl Server {
    pub fn installed(&self) -> bool {
        !self.panel.is_empty()
    }
}
pub static SERVERS: State<Vec<Server>> = State::new("install-to-server.servers");

/// What the current or last action does, or why it failed.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Idle,
    Working(String),
    Failed(String),
}
/// The action on a server (`None`: connecting a new one).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Activity {
    pub server: Option<Id>,
    pub status: Status,
}
pub static ACTIVITY: State<Activity> = State::new("install-to-server.activity");
/// What the last action did and what the server answered, line by line,
/// without secrets.
pub static LOG: State<Vec<String>> = State::new("install-to-server.log");

/// Whether the web panel of an installed server answers.
#[derive(Clone, Debug, PartialEq)]
pub enum Health {
    Checking,
    Works,
    Fails(String),
}
pub static HEALTH: State<HashMap<Id, Health>> = State::new("install-to-server.health");

/// The homeport image an installed server runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// "0.2.0 · built 2026-11-20".
    pub version: String,
    /// Its registry has a newer image under the same tag: Update installs it.
    pub update: bool,
}
pub static RELEASES: State<HashMap<Id, Release>> = State::new("install-to-server.releases");

/// Homeport's own SSH key, for signing in to a server by hand.
pub static KEY_FILE: State<PathBuf> = State::new("install-to-server.key-file");

/// Sign in with the SSH password, let homeport's key in and add the server
/// (with its panel, when homeport is installed there already).
pub struct Connect(pub deploy::Access);
impl Op for Connect {
    type Reply = Id;
    const SAVES: bool = true;
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::keyed("connect", "Connect server").page(&PLUGIN))
    }
}

/// Install homeport on a connected server, or update its installation.
pub struct Install {
    pub id: Id,
    pub settings: deploy::Settings,
    /// For logins whose sudo asks for a password.
    pub sudo: String,
}
impl Op for Install {
    type Reply = ();
    const SAVES: bool = true;
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::keyed(format!("install-{}", self.id), "Install server").page(&PLUGIN))
    }
}

/// Restart homeport's services on the server.
pub struct Restart {
    pub id: Id,
    pub sudo: String,
}
impl Op for Restart {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::keyed(format!("restart-{}", self.id), "Restart server").page(&PLUGIN))
    }
}

/// Show the last lines of homeport's services in the output.
pub struct Logs {
    pub id: Id,
    pub sudo: String,
}
impl Op for Logs {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::keyed(format!("logs-{}", self.id), "Read server logs").page(&PLUGIN))
    }
}

/// Remove homeport, everything its installer added and homeport's SSH key
/// from the server; then the server leaves the list.
pub struct Clean {
    pub id: Id,
    pub sudo: String,
}
impl Op for Clean {
    type Reply = ();
    const SAVES: bool = true;
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::keyed(format!("clean-{}", self.id), "Clean up server").page(&PLUGIN))
    }
}

/// Read the panel file again and check that the panel answers.
pub struct Check(pub Id);
impl Op for Check {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::keyed(format!("check-{}", self.0), "Check server").quiet())
    }
}

fn setup(p: &mut Setup) -> Result<()> {
    p.list(&SERVERS);
    p.live(&ACTIVITY, Activity::default);
    p.live(&LOG, Vec::new);
    p.live(&HEALTH, HashMap::new);
    p.live(&RELEASES, HashMap::new);
    let key = deploy::key_file(&p.data_dir());
    p.live(&KEY_FILE, move || key.clone());
    p.handle(connect);
    p.handle(install);
    p.handle(restart);
    p.handle(logs);
    p.handle(clean);
    p.handle(check);
    p.page::<page::ServersPage>(PageSpec {
        title: "Install to server",
        icon: Icon::Terminal,
        order: 85,
    });
    Ok(())
}

fn record(cx: &Cx, id: Id) -> Result<Server> {
    (cx.get(&SERVERS).iter())
        .find(|s| s.id == id)
        .cloned()
        .ok_or_else(|| Error::msg("This server is no longer in the list."))
}

async fn connect(cx: Cx, Connect(access): Connect) -> Result<Id> {
    cx.failure_title(format!("Couldn't connect {}", access.host.trim()));
    start(&cx, None);
    let result = deploy::connect(&cx, &access).await;
    let id = finish(&cx, &access.secrets(), result)?;
    if record(&cx, id)?.installed() {
        cx.spawn(Check(id))?;
    }
    Ok(id)
}

async fn install(cx: Cx, op: Install) -> Result<()> {
    let server = record(&cx, op.id)?;
    cx.failure_title(format!("Couldn't install on {}", server.host));
    start(&cx, Some(op.id));
    let secrets = [op.sudo.clone(), op.settings.panel_password.clone()];
    let result = deploy::install(&cx, &server, &op.settings, &op.sudo).await;
    let result = finish(&cx, &secrets, result);
    if result.is_ok() {
        cx.spawn(Check(op.id))?;
    }
    result
}

async fn restart(cx: Cx, op: Restart) -> Result<()> {
    let server = record(&cx, op.id)?;
    cx.failure_title(format!("Couldn't restart {}", server.host));
    start(&cx, Some(op.id));
    let result = deploy::restart(&cx, &server, &op.sudo).await;
    let result = finish(&cx, &[op.sudo], result);
    if result.is_ok() {
        cx.summary(format!("Restarted Homeport on {}", server.host));
        cx.spawn(Check(op.id))?;
    }
    result
}

async fn logs(cx: Cx, op: Logs) -> Result<()> {
    let server = record(&cx, op.id)?;
    cx.failure_title(format!("Couldn't read the logs of {}", server.host));
    start(&cx, Some(op.id));
    let result = deploy::logs(&cx, &server, &op.sudo).await;
    finish(&cx, &[op.sudo, server.password.clone()], result)
}

async fn clean(cx: Cx, op: Clean) -> Result<()> {
    let server = record(&cx, op.id)?;
    cx.failure_title(format!("Couldn't clean up {}", server.host));
    start(&cx, Some(op.id));
    let result = deploy::clean(&cx, &server, &op.sudo).await;
    if result.is_ok() {
        cx.summary(format!("Cleaned up {}", server.host));
    }
    finish(&cx, &[op.sudo], result)
}

async fn check(cx: Cx, Check(id): Check) -> Result<()> {
    let server = record(&cx, id)?;
    cx.update_live(&HEALTH, |h| drop(h.insert(id, Health::Checking)));
    let (health, release) = match deploy::check(&cx, &server).await {
        Ok(None) => (None, None),
        Ok(Some((health, release))) => (Some(health), release),
        Err(e) => (Some(Health::Fails(e.message)), None),
    };
    // An unreadable image keeps what was read before.
    let release_known = health.is_none() || release.is_some();
    put(&cx, &HEALTH, id, health);
    if release_known {
        put(&cx, &RELEASES, id, release);
    }
    Ok(())
}

/// A server's entry in a live map; `None` takes it out.
fn put<T: Clone + PartialEq + Send + Sync + 'static>(
    cx: &Cx,
    state: &'static State<HashMap<Id, T>>,
    id: Id,
    value: Option<T>,
) {
    cx.update_live(state, |all| match value {
        Some(value) => drop(all.insert(id, value)),
        None => drop(all.remove(&id)),
    });
}

fn start(cx: &Cx, server: Option<Id>) {
    cx.set_live(&LOG, Vec::new());
    cx.set_live(
        &ACTIVITY,
        Activity {
            server,
            status: Status::Working("Starting…".into()),
        },
    );
}

/// The outcome in the log and in the page's status.
fn finish<T>(cx: &Cx, secrets: &[String], result: Result<T>) -> Result<T> {
    if let Err(e) = result.as_ref()
        && !e.cancelled()
    {
        deploy::log(cx, secrets, &format!("✖ {}", e.message));
    }
    let status = match &result {
        Ok(_) => Status::Idle,
        Err(e) if e.cancelled() => Status::Idle,
        Err(e) => Status::Failed(e.message.clone()),
    };
    cx.update_live(&ACTIVITY, |a| a.status = status);
    result
}
