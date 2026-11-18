//! # health-checks
//! Test URLs (the Tests page): every server test opens each through the
//! server, with the expected status, by mihomo's delay test (in parallel,
//! for every protocol). The share that passed counts in the rating as the
//! check "Test URLs" ([`servers::CHECKS`]); a server's details list each result.
use homeport_kernel::mihomo::Delay;
use homeport_kernel::{
    kit,
    prelude::*,
    ui::{Column, ListEditor},
};
use plugin_servers::{self as servers, Probe, Reading, Server};
use tokio::task::JoinSet;

pub static PLUGIN: Plugin = Plugin {
    id: "health-checks",
    name: "Tests",
    description: "Test servers with your own sites; the share that passes counts in the rating.",
    category: categories::SERVERS,
    requires: &[&plugin_servers::PLUGIN],
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

choices!(pub enum Expected {
    NoContent => "Status 204",
    Success => "Status 2xx",
    Below400 => "Status < 400",
    Below500 => "Status < 500",
    Below503 => "Status < 503",
    Any => "Any status",
});
impl Expected {
    pub fn accepts(self, code: u16) -> bool {
        match self {
            Self::NoContent => code == 204,
            Self::Success => (200..300).contains(&code),
            Self::Below400 => code < 400,
            Self::Below500 => code < 500,
            Self::Below503 => code < 503,
            Self::Any => true,
        }
    }
    /// The same range in mihomo's syntax (`None` accepts every status).
    pub fn mihomo(self) -> Option<&'static str> {
        match self {
            Self::NoContent => Some("204"),
            Self::Success => Some("200-299"),
            Self::Below400 => Some("100-399"),
            Self::Below500 => Some("100-499"),
            Self::Below503 => Some("100-502"),
            Self::Any => None,
        }
    }
}

choices!(pub enum Failure {
    Timeout => "Timeout",
    WrongStatus => "Wrong status",
    Failed => "Failed",
});

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "http_tests")]
pub struct Test {
    pub id: Id,
    pub url: String,
    pub expected: Expected,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "test_results")]
pub struct Measurement {
    pub id: Id,
    pub server: Id,
    pub test: Id,
    /// HTTP status of older checks; the delay test does not report it.
    pub status: Option<u16>,
    pub passed: bool,
    /// Response time through the server when passed.
    #[serde(default)]
    pub delay_ms: Option<u32>,
    /// Why the test failed, when known.
    #[serde(default)]
    pub failure: Option<Failure>,
}
pub static TESTS: State<Vec<Test>> = State::new("health-checks.tests");
pub static RESULTS: State<Vec<Measurement>> = State::new("health-checks.results");

/// The check of a server with every test URL.
pub struct UrlTests(Probe);
impl Op for UrlTests {
    type Reply = Option<Reading>;
}
impl servers::CheckOp for UrlTests {
    fn new(probe: Probe) -> Self {
        Self(probe)
    }
}

/// Open every test URL through the server at once; a full test stores the results.
async fn url_tests(cx: Cx, UrlTests(probe): UrlTests) -> Result<Option<Reading>> {
    // An edit of the URLs meanwhile makes these results stale.
    let input = cx.input(|snap| Some(snap.get(&TESTS).to_vec()));
    let tests = input.value().cloned().unwrap_or_default();
    if tests.is_empty() {
        return Ok(None);
    }
    let mut set = JoinSet::new();
    for (i, t) in tests.iter().cloned().enumerate() {
        let (cx, probe) = (cx.clone(), probe.clone());
        set.spawn(async move { (i, probe.delay(&cx, &t.url, t.expected.mihomo(), 2).await) });
    }
    let mut outcomes = vec![Delay::Failed; tests.len()];
    while let Some(done) = cx.or_cancel(set.join_next()).await? {
        if let Ok((i, d)) = done {
            outcomes[i] = d;
        }
    }
    let server = probe.server;
    let rows: Vec<Measurement> = (tests.iter().zip(outcomes))
        .map(|(t, d)| Measurement {
            id: server.child(t.id.0.as_bytes()),
            server,
            test: t.id,
            status: None,
            passed: d.ms().is_some(),
            delay_ms: d.ms(),
            failure: match d {
                Delay::Passed(_) => None,
                Delay::Timeout => Some(Failure::Timeout),
                Delay::WrongStatus => Some(Failure::WrongStatus),
                Delay::Failed => Some(Failure::Failed),
            },
        })
        .collect();
    let (passed, n) = (rows.iter().filter(|r| r.passed).count(), rows.len());
    let best = rows.iter().filter_map(|r| r.delay_ms).min();
    if !probe.quick {
        cx.write(move |w| {
            if !input.current(w) || !w.get(&servers::SERVERS).iter().any(|s| s.id == server) {
                return Ok(());
            }
            w.update(&RESULTS, |l| {
                l.retain(|m| m.server != server);
                l.extend(rows);
            });
            Ok(())
        })
        .await?;
    }
    Ok(Some(Reading {
        value: passed as f64 / n as f64,
        text: format!("{passed} of {n} passed"),
        delay_ms: best,
    }))
}

fn setup(p: &mut Setup) -> Result<()> {
    p.list(&TESTS).validate(|_, t, _, others| {
        let url = url::Url::parse(t.url.trim())
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
            .ok_or_else(|| Error::msg("Enter an HTTP(S) test URL."))?;
        t.url = url.to_string();
        if others.iter().any(|o| o.url == t.url) {
            bail!("Test URL already exists.");
        }
        Ok(())
    });
    p.collection(&RESULTS);
    p.cascade(&servers::SERVERS, &RESULTS, |m| m.server);
    p.cascade(&TESTS, &RESULTS, |m| m.test);
    // Editing a test invalidates its old results.
    p.on(|cx: Cx, e: Saved<Test>| async move {
        cx.write(|w| {
            w.update(&RESULTS, |r| r.retain(|m| m.test != e.item.id));
            Ok(())
        })
        .await
    });
    p.handle(url_tests);
    servers::add_check::<UrlTests>(
        p,
        servers::CheckSpec {
            id: "urls",
            title: "Test URLs",
            weight: 40.,
            quick: true,
        },
        |snap| !snap.get(&TESTS).is_empty(),
    );
    // The user's first site is what "the server works" means to them.
    p.hook(&servers::PROBE, |snap, _| {
        snap.get(&TESTS).first().map(|t| t.url.clone())
    });
    p.fill(&servers::DETAILS, "tests", 20, details);
    p.page_fn(
        PageSpec {
            title: "Tests",
            icon: Icon::Checks,
            order: 40,
        },
        page,
    );
    Ok(())
}

/// "Passed · 120 ms", "Failed · Timeout", "Failed · HTTP 404" (older checks) …
/// "Passed · 120 ms", "Failed · Timeout", "Failed · status not 2xx".
fn result_text(r: &Measurement, expected: Expected) -> String {
    let detail = match (r.delay_ms, r.failure, r.status) {
        (Some(ms), ..) => format!("{ms} ms"),
        (None, Some(Failure::WrongStatus), _) => format!("status not {expected}"),
        (None, Some(Failure::Failed), _) | (None, None, None) => {
            return if r.passed { "Passed" } else { "Failed" }.into();
        }
        (None, Some(f), _) => f.to_string(),
        (None, None, Some(code)) => format!("HTTP {code}"),
    };
    format!("{} · {detail}", if r.passed { "Passed" } else { "Failed" })
}

fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}

/// Each test URL's result on one server, in its expanded row.
fn details(ui: &mut egui::Ui, cx: &mut UiCx, s: &Server) {
    let snap = cx.snapshot().clone();
    let pal = kit::colors(ui);
    // One "Tests" property, a line per test: the result first, then the
    // site, which a narrow row cuts (the full URL is on hover).
    for (i, t) in snap.get(&TESTS).iter().enumerate() {
        let r = snap
            .get(&RESULTS)
            .iter()
            .find(|r| r.server == s.id && r.test == t.id)
            .cloned();
        kit::property_with(ui, if i == 0 { "Tests" } else { "" }, |ui| {
            ui.spacing_mut().item_spacing.x = 4.;
            let (icon, text, color) = match r {
                None => (Icon::Info, "Not tested".to_owned(), pal.muted),
                Some(r) => (
                    if r.passed {
                        Icon::Connected
                    } else {
                        Icon::Error
                    },
                    result_text(&r, t.expected),
                    if r.passed { pal.success } else { pal.danger },
                ),
            };
            kit::icon(ui, icon, 14., color);
            ui.colored_label(color, text).on_hover_text(&t.url);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("· {}", host(&t.url))).color(pal.muted),
                )
                .truncate(),
            )
            .on_hover_text(&t.url);
        });
    }
}

/// How a test did on the last check: a test that fails on every server is
/// more likely wrong itself (URL or expected status).
fn test_note(test: &Test, results: &[Measurement]) -> Option<(String, kit::Tone)> {
    let mine: Vec<&Measurement> = results.iter().filter(|r| r.test == test.id).collect();
    let passed = mine.iter().filter(|r| r.passed).count();
    match (mine.len(), passed) {
        (0, _) => None,
        (n, 0) if n > 1 => Some((
            format!("Failed on all {n} servers: check the URL and the expected status"),
            kit::Tone::Warning,
        )),
        (n, p) => Some((format!("Passed on {p} of {n} servers"), kit::Tone::Neutral)),
    }
}

/// The test URLs.
fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let n = cx.get(&TESTS).len();
    let counter = kit::count(n, "test URL", "test URLs");
    cx.page_header(ui, &counter, |ui, cx| {
        let ok = cx.core_status().running() && !cx.get(&servers::SERVERS).is_empty();
        cx.job_button(ui, Icon::Checks, "Test all", ok, || servers::CheckAll);
    });
    let results = cx.get(&RESULTS);

    ListEditor::new(&TESTS)
        .unordered()
        .subtitle(move |tests, i| test_note(&tests[i], &results))
        .add_form(
            "Add test URL",
            || Test {
                id: Id::random(),
                url: String::new(),
                expected: Expected::NoContent,
            },
            |ui, t, width| {
                kit::Fields::new(width)
                    .input("url", &mut t.url, "https://example.org/")
                    .help(
                        "HEAD requests; redirects are not followed. Only a matching status passes.",
                    )
                    .choice("expected", &mut t.expected, Expected::ALL)
                    .show(ui)
                    && !t.url.trim().is_empty()
            },
        )
        .column(Column::edit(
            "URL",
            "https://example.org/",
            |t: &mut Test| &mut t.url,
        ))
        .column(Column::choice(
            "Expected status",
            120.,
            |t: &mut Test| &mut t.expected,
            Expected::ALL,
        ))
        .empty(
            Icon::Checks,
            "No test URLs",
            "Add a site above to test servers with it.",
        )
        .show(ui, cx);
}
