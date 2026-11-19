//! Toasts: notices for every window, logged when they are errors.
use super::*;

/// A message for the user (toast). `error` ones are shown in red.
#[derive(Clone, Debug)]
pub struct Notice {
    pub id: u64,
    pub title: String,
    pub text: String,
    pub error: bool,
    /// Page with the details; the toast offers to open it.
    pub page: Option<&'static str>,
    /// A button on the toast, see [`Cx::notify_with`].
    pub action: Option<NoticeAction>,
}
/// A toast button that runs an op of the plugin that showed the toast.
#[derive(Clone)]
pub struct NoticeAction {
    pub label: &'static str,
    pub(crate) plugin: Option<usize>,
    pub(crate) run: Arc<dyn Fn(&mut crate::UiCx) + Send + Sync>,
}
impl std::fmt::Debug for NoticeAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label)
    }
}

impl Kernel {
    pub fn notify(&self, title: &str, text: &str) {
        self.push_notice(None, title, text, false, None, None);
    }
    /// An error toast, also written to the log.
    pub fn notify_error(&self, title: &str, text: &str) {
        self.push_notice(None, title, text, true, None, None);
    }
    /// An error whose details live on `page`.
    pub fn notify_error_at(&self, title: &str, text: &str, page: Option<&'static str>) {
        self.push_notice(None, title, text, true, page, None);
    }
    /// An error toast of `plugin`'s work.
    pub(crate) fn notice(&self, plugin: Option<usize>, title: &str, text: &str) {
        self.push_notice(plugin, title, text, true, None, None);
    }
    /// Write `text` to the log as `plugin`'s line (the app's when `None`).
    pub(crate) fn log(&self, plugin: Option<usize>, level: log::Level, text: &str) {
        match plugin {
            Some(p) => {
                let target = crate::logging::target(self.reg.plugins[p].id);
                log::log!(target: &target, level, "{text}");
            }
            None => log::log!(level, "{text}"),
        }
    }
    pub(super) fn push_notice(
        &self,
        plugin: Option<usize>,
        title: &str,
        text: &str,
        error: bool,
        page: Option<&'static str>,
        action: Option<NoticeAction>,
    ) {
        if error {
            let name = plugin.map(|p| self.reg.plugins[p].name);
            let line = match name {
                Some(name) if name == title => text.to_owned(),
                _ => format!("{title}: {text}"),
            };
            self.log(plugin, log::Level::Error, &line);
        }
        let id = self.next_notice.fetch_add(1, Ordering::Relaxed);
        let mut n = self.notices.lock().unwrap();
        n.push_back(Notice {
            id,
            title: title.into(),
            text: text.into(),
            error,
            page,
            action,
        });
        while n.len() > 16 {
            n.pop_front();
        }
        drop(n);
        self.changes.notify();
    }
    /// Notices newer than `id` (0: all kept), e.g. to check in a test that
    /// an error was shown.
    pub fn notices_after(&self, id: u64) -> Vec<Notice> {
        self.notices
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.id > id)
            .cloned()
            .collect()
    }
    /// Notices whose action nobody has run yet.
    pub(crate) fn notice_actions(&self) -> Vec<u64> {
        let notices = self.notices.lock().unwrap();
        notices
            .iter()
            .filter(|n| n.action.is_some())
            .map(|n| n.id)
            .collect()
    }
    /// The action of a notice, for the first window that presses it.
    pub(crate) fn take_notice_action(&self, id: u64) -> Option<NoticeAction> {
        let mut notices = self.notices.lock().unwrap();
        notices.iter_mut().find(|n| n.id == id)?.action.take()
    }
    pub(crate) fn last_notice(&self) -> u64 {
        self.next_notice.load(Ordering::Relaxed) - 1
    }
}
