//! Operations: typed actions with an ordered chain of handlers.
//!
//! The owner registers the default handler. Any plugin can put its own handler
//! in front of it — to *replace* it (don't call `next`), *wrap* it (call `next`
//! and adjust the result) or *claim* only some inputs (`claim` returns `None`
//! for inputs it does not handle).
use crate::{Cx, Error, Result, Snapshot};
use std::{any::Any, future::Future, pin::Pin, sync::Arc};

/// Run as a visible background job when spawned.
#[derive(Clone, Debug)]
pub struct JobSpec {
    /// Equal keys join a running job instead of starting another (empty: the op type).
    pub key: String,
    /// Shown in Activity.
    pub label: String,
    /// Frequent background work: only failures appear in Activity.
    pub quiet: bool,
    /// When a run with the same key is active: `false` joins it (the new op is
    /// dropped), `true` runs the new op right after it (at most one waits per key).
    pub queue: bool,
    /// When a run with the same key is active: cancel it and run the new op
    /// right after (the newest request wins, e.g. a subscription link edited
    /// while it loads).
    pub latest: bool,
    /// Page that shows the details when the job fails (a toast offers to open it).
    pub page: Option<&'static str>,
}
impl JobSpec {
    /// One job per op type: a second request joins the running one.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            key: String::new(),
            label: label.into(),
            quiet: false,
            queue: false,
            latest: false,
            page: None,
        }
    }
    /// One job per `key` (e.g. per subscription). Keys are local to the plugin
    /// that handles the op: the kernel prefixes them with its id.
    pub fn keyed(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            quiet: false,
            queue: false,
            latest: false,
            page: None,
        }
    }
    pub fn queued(mut self) -> Self {
        self.queue = true;
        self
    }
    pub fn latest(mut self) -> Self {
        self.latest = true;
        self
    }
    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }
    pub fn page(mut self, page: &'static str) -> Self {
        self.page = Some(page);
        self
    }
}

/// An action. Any `Send` type can be one.
pub trait Op: Send + 'static {
    type Reply: Send + 'static;
    /// `Some` makes [`Cx::spawn`] and UI buttons run this op as a background job
    /// with progress and cancellation.
    fn job(&self) -> Option<JobSpec> {
        None
    }
}

/// Handler priority; earlier ranks run first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    Specific,
    Default,
    Fallback,
}

/// Weight returned by a claim: higher wins among handlers of the same rank.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Claim(pub u32);
impl Claim {
    /// The input belongs to this plugin (e.g. a subscription it created).
    pub const OWNED: Claim = Claim(1000);
    /// Recognized by content.
    pub const fn content(confidence: u32) -> Claim {
        Claim(confidence)
    }
}

pub(crate) type BoxFut<T> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;
pub(crate) type HandlerFn<O> =
    Arc<dyn Fn(Cx, O, Next<O>) -> BoxFut<<O as Op>::Reply> + Send + Sync>;
pub(crate) type ClaimFn<O> = Arc<dyn Fn(&Snapshot, &O) -> Option<Claim> + Send + Sync>;

pub(crate) struct Handler<O: Op> {
    pub f: HandlerFn<O>,
    pub claim: Option<ClaimFn<O>>,
    pub rank: Rank,
    pub plugin: usize,
    pub seq: usize,
}
impl<O: Op> Clone for Handler<O> {
    fn clone(&self) -> Self {
        Self {
            f: self.f.clone(),
            claim: self.claim.clone(),
            rank: self.rank,
            plugin: self.plugin,
            seq: self.seq,
        }
    }
}

pub(crate) struct Handlers<O: Op>(pub Vec<Handler<O>>);

/// The rest of the chain.
pub struct Next<O: Op> {
    chain: Arc<Vec<Handler<O>>>,
    at: usize,
}
impl<O: Op> Next<O> {
    /// Hand the (possibly modified) op to the next handler.
    pub async fn run(self, cx: &Cx, op: O) -> Result<O::Reply> {
        let Some(h) = self.chain.get(self.at).cloned() else {
            return Err(Error::new(
                "unhandled",
                format!("No plugin handles {}", short_name::<O>()),
            ));
        };
        let next = Next {
            chain: self.chain,
            at: self.at + 1,
        };
        (h.f)(cx.as_plugin(Some(h.plugin)), op, next).await
    }
    /// True when another handler follows.
    pub fn exists(&self) -> bool {
        self.at < self.chain.len()
    }
}

/// A type's name without module paths: `Add<Source>`.
pub(crate) fn short_name<T>() -> String {
    let mut out = String::new();
    for part in std::any::type_name::<T>().split_inclusive(['<', '>', ',', ' ']) {
        let (path, end) = part.split_at(part.trim_end_matches(['<', '>', ',', ' ']).len());
        out.push_str(path.rsplit("::").next().unwrap_or(path));
        out.push_str(end);
    }
    out
}

/// Order the handlers that accept `op`: rank, claim weight, dependents before
/// their dependencies (higher plugin index first), then registration order.
pub(crate) fn chain<O: Op>(all: &[Handler<O>], snap: &Snapshot, op: &O) -> Vec<Handler<O>> {
    let mut out: Vec<(Handler<O>, Claim)> = all
        .iter()
        .filter_map(|h| match &h.claim {
            None => Some((h.clone(), Claim(0))),
            Some(c) => c(snap, op).map(|w| (h.clone(), w)),
        })
        .collect();
    out.sort_by(|(a, wa), (b, wb)| {
        a.rank
            .cmp(&b.rank)
            .then(wb.cmp(wa))
            .then(b.plugin.cmp(&a.plugin))
            .then(a.seq.cmp(&b.seq))
    });
    out.into_iter().map(|(h, _)| h).collect()
}

pub(crate) fn run<O: Op>(
    cx: &Cx,
    handlers: Option<&(dyn Any + Send + Sync)>,
    op: O,
) -> BoxFut<O::Reply> {
    let snap = cx.snapshot();
    let list = handlers
        .and_then(|h| h.downcast_ref::<Handlers<O>>())
        .map(|h| chain(&h.0, &snap, &op))
        .unwrap_or_default();
    let cx = cx.clone();
    Box::pin(async move {
        Next {
            chain: Arc::new(list),
            at: 0,
        }
        .run(&cx, op)
        .await
    })
}
