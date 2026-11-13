use tokio::sync::watch;

// Share one revision signal across job and document changes.
#[derive(Clone)]
pub struct Changes(watch::Sender<()>);
impl Default for Changes {
    fn default() -> Self {
        Self(watch::channel(()).0)
    }
}
impl Changes {
    pub fn notify(&self) {
        self.0.send_replace(());
    }
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.0.subscribe()
    }
}
