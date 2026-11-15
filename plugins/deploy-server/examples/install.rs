//! Opt-in end-to-end harness. Credentials come only from the environment.
use plugin_deploy_server::deploy::{Options, install};
#[tokio::main]
async fn main() {
    let options = Options {
        host: std::env::var("DEPLOY_TEST_HOST").expect("DEPLOY_TEST_HOST"),
        password: std::env::var("DEPLOY_TEST_PASSWORD").expect("DEPLOY_TEST_PASSWORD"),
        directory: "/srv/rproxy-plugin-test".into(),
        ..Options::default()
    };
    let result = tokio::task::spawn_blocking(move || {
        let data = std::path::Path::new("/tmp/rproxy-deploy-harness");
        install(
            options,
            data,
            |stage| println!("{stage}"),
            |record| {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(data.join("result.json"))
                    .unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                }
                file.write_all(&serde_json::to_vec(&record).unwrap())
                    .unwrap();
                Ok(())
            },
        )
    })
    .await
    .unwrap();
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
