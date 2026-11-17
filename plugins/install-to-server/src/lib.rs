//! Sets up rproxy on the user's own Linux server over SSH: the app with its
//! web panel (the Compose stack from `docker/`), and keeps what is needed to
//! open it. Inbounds are set up in the panel.
use rproxy_kernel::prelude::*;
use serde::{Deserialize, Serialize};
pub mod deploy;
mod page;

pub static PLUGIN: Plugin = Plugin {
    id: "install-to-server",
    name: "Install to server",
    description: "Install rproxy with its web panel on your Linux server over SSH.",
    platforms: Platform::DESKTOP,
    setup,
    ..Plugin::DEFAULT
};

/// A server installed from here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Table)]
#[table(name = "deployments")]
pub struct Deployment {
    pub id: Id,
    pub host: String,
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
pub static STATUS: State<Status> = State::new("install-to-server.status");

pub struct Install(pub deploy::Options);
impl Op for Install {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Install server").page("install-to-server"))
    }
}

fn setup(p: &mut Setup) -> Result<()> {
    p.list(&DEPLOYMENTS);
    p.live(&STATUS, Status::default);
    p.handle(install);
    p.page::<page::DeployPage>(PageSpec {
        id: "install-to-server",
        title: "Install to server",
        icon: Icon::Globe,
        order: 85,
    });
    Ok(())
}

async fn install(cx: Cx, Install(options): Install) -> Result<()> {
    cx.failure_title(format!("Couldn't install {}", options.host.trim()));
    let result = deploy::install(&cx, &options).await;
    cx.set_live(
        &STATUS,
        match &result {
            Ok(()) => Status::Idle,
            Err(e) if e.cancelled() => Status::Idle,
            Err(e) => Status::Failed(e.message.clone()),
        },
    );
    result
}
