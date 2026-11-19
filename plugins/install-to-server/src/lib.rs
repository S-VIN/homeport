//! Sets up homeport on the user's own Linux server over SSH: the app with its
//! web panel (the Compose stack from `docker/`), and keeps what is needed to
//! open it. Inbounds are set up in the panel. Uninstalling takes away
//! everything the installation added to the server.
use homeport_kernel::prelude::*;
use serde::{Deserialize, Serialize};
pub mod deploy;
mod page;

pub static PLUGIN: Plugin = Plugin {
    id: "install-to-server",
    name: "Install to server",
    description: "Install Homeport with its web panel on your Linux server over SSH.",
    platforms: Platform::NATIVE,
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

/// A server installed from here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Table)]
#[table(name = "install_to_server_deployments")]
pub struct Deployment {
    pub id: Id,
    pub host: String,
    pub ssh_port: u16,
    /// The SSH login it was installed with: uninstalling offers it.
    pub ssh_login: String,
    /// The installation folder on the server.
    pub directory: String,
    /// Address of the web panel.
    pub panel: String,
    pub login: String,
    pub password: String,
    /// False while it is being installed, and when that was interrupted.
    pub ready: bool,
}
pub static DEPLOYMENTS: State<Vec<Deployment>> = State::new("install-to-server.deployments");

/// What the installation does right now, or why the last one failed.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Idle,
    Working(String),
    Failed(String),
}
pub static PROGRESS: State<Status> = State::new("install-to-server.progress");
/// The same for uninstalling a server.
pub static REMOVAL: State<Status> = State::new("install-to-server.removal");
/// What the last installation or uninstallation did and what the server answered, line by
/// line, without secrets.
pub static LOG: State<Vec<String>> = State::new("install-to-server.log");

pub struct Install(pub deploy::Options);
impl Op for Install {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Install server").page(&PLUGIN))
    }
}

/// Remove homeport and everything its installer added from a server, then
/// every record of that server. The SSH password comes from the user again.
pub struct Uninstall {
    pub id: Id,
    pub login: String,
    pub password: String,
}
impl Op for Uninstall {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Uninstall server").page(&PLUGIN))
    }
}

fn setup(p: &mut Setup) -> Result<()> {
    p.list(&DEPLOYMENTS);
    p.live(&PROGRESS, Status::default);
    p.live(&REMOVAL, Status::default);
    p.live(&LOG, Vec::new);
    p.handle(install);
    p.handle(uninstall);
    p.page::<page::DeployPage>(PageSpec {
        title: "Install to server",
        icon: Icon::Terminal,
        order: 85,
    });
    Ok(())
}

async fn install(cx: Cx, Install(options): Install) -> Result<()> {
    cx.failure_title(format!("Couldn't install {}", options.host.trim()));
    let result = deploy::install(&cx, &options).await;
    finish(&cx, &options, &PROGRESS, result)
}

async fn uninstall(cx: Cx, op: Uninstall) -> Result<()> {
    let record = (cx.get(&DEPLOYMENTS).iter())
        .find(|d| d.id == op.id)
        .cloned()
        .ok_or_else(|| Error::msg("This server is no longer in the list."))?;
    cx.failure_title(format!("Couldn't uninstall from {}", record.host));
    let options = deploy::Options {
        host: record.host.clone(),
        ssh_port: record.ssh_port,
        login: op.login,
        password: op.password,
        ..deploy::Options::default()
    };
    let result = deploy::uninstall(&cx, &options, &record).await;
    if result.is_ok() {
        cx.summary(format!("Uninstalled from {}", record.host));
    }
    finish(&cx, &options, &REMOVAL, result)
}

/// The outcome in the log and in the page's status.
fn finish(
    cx: &Cx,
    options: &deploy::Options,
    status: &State<Status>,
    result: Result<()>,
) -> Result<()> {
    if let Err(e) = result.as_ref()
        && !e.cancelled()
    {
        deploy::log(cx, options, &format!("✖ {}", e.message));
    }
    cx.set_live(
        status,
        match &result {
            Ok(()) => Status::Idle,
            Err(e) if e.cancelled() => Status::Idle,
            Err(e) => Status::Failed(e.message.clone()),
        },
    );
    result
}
