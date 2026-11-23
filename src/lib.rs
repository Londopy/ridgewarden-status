//! ridgewarden-status: "is the mesh up?" for served agencies, in plain words.
//!
//! Green / yellow / red per site and per key service, a headline anyone can read, an
//! incident banner signed by the ham who set it, incidents and maintenance notices, a map,
//! and a printable one-page layout. It is a read-only view built from the same data and the
//! same code as ridgewarden-health and ridgewarden-linkdoctor, so the three never disagree.

pub mod aredn_feed;
pub mod config;
pub mod embed;
pub mod incidents;
pub mod maintenance;
pub mod map;
pub mod mirror;
pub mod pages;
pub mod power;
pub mod status;
pub mod subscribe;
#[cfg(test)]
mod testing;
pub mod usage;
pub mod web;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Result;
use ridgewarden_client::alerts;
use ridgewarden_client::core::Projection;
use ridgewarden_client::mesh::Mesh;
use ridgewarden_client::util::now_ms;
use ridgewarden_client::{CoreClient, open_db};
use ridgewarden_health::probes::Probes;
use ridgewarden_linkdoctor::history::Notes;
use rusqlite::Connection;

use crate::config::Config;
use crate::incidents::{Incident, Incidents};
use crate::maintenance::Maintenance;
use crate::power::Monitor;
use crate::status::{Banners, Report};

pub const PROJECTIONS: &[&dyn Projection] =
    &[&Mesh, &Probes, &Notes, &Banners, &Incidents, &Maintenance];

/// The tables the app keeps outside its projections.
pub const SCHEMAS: [&str; 4] = [
    alerts::SCHEMA,
    status::SCHEMA,
    subscribe::SCHEMA,
    usage::SCHEMA,
];

/// When something sent off this box last worked, and when it last failed.
#[derive(Clone, Copy, Default)]
pub struct Sent {
    pub ok: Option<i64>,
    pub failed: Option<i64>,
}

/// What the pages show, built in the background and shared, so a page costs nothing and
/// never waits for the analysis.
pub struct View {
    pub report: Report,
    /// this site's node: the pages show the network from its side
    pub anchor: String,
    /// incidents for the history page
    pub history: Vec<Incident>,
    /// built after the feed was read to the end at least once; notices wait for that, so a
    /// half-read history sends nothing
    pub settled: bool,
}

pub struct App {
    pub cfg: Config,
    pub core: CoreClient,
    /// for the feed and everything the app records
    pub db: Mutex<Connection>,
    /// a second connection for building the view, so the analysis never holds the first
    reader: Mutex<Connection>,
    /// for alerts, subscriptions, the mirror and the site monitor, which may be on the
    /// internet over TLS
    pub alert_http: reqwest::Client,
    pub pushed: Mutex<Sent>,
    /// what the site monitor's hub last said
    power: Mutex<Monitor>,
    view: Mutex<Option<Arc<View>>>,
    caught_up: AtomicBool,
}

impl App {
    pub fn new(cfg: Config, with_token: bool) -> Result<Self> {
        let token = with_token.then_some(cfg.core.token_file.as_path());
        let core = CoreClient::new(&cfg.core.url, token)?;
        let db = open_db(&cfg.db_path(), PROJECTIONS)?;
        for s in SCHEMAS {
            db.execute_batch(s)?;
        }
        let reader = open_db(&cfg.db_path(), PROJECTIONS)?;
        let alert_http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("ridgewarden-status/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            cfg,
            core,
            db: Mutex::new(db),
            reader: Mutex::new(reader),
            alert_http,
            pushed: Mutex::new(Sent::default()),
            power: Mutex::new(Monitor::default()),
            view: Mutex::new(None),
            caught_up: AtomicBool::new(false),
        })
    }

    pub fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Build the view as seen from this site's node, whose core has `key`.
    pub fn build_view(&self, anchor: &str, key: &str) -> Result<View> {
        let settled = self.caught_up.load(Ordering::Relaxed);
        let c = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        status::remember_sites(&c)?;
        let now = now_ms();
        let snap = status::snapshot(&c, &self.cfg, now, anchor)?;
        let power = self.monitor().readings;
        let report = status::build(&c, &self.cfg, &snap, anchor, key, &power)?;
        let history = incidents::history(&c, &self.cfg, now)?;
        Ok(View {
            report,
            anchor: anchor.to_string(),
            history,
            settled,
        })
    }

    /// Ask the core who this site is, rebuild the view off the async threads, and keep it.
    pub async fn refresh(self: &Arc<Self>) -> Result<Arc<View>> {
        let who = self.core.whoami().await.ok();
        let (node, key) = who.map(|w| (w.node, w.key)).unwrap_or_default();
        let app = self.clone();
        let view = tokio::task::spawn_blocking(move || app.build_view(&node, &key)).await??;
        let view = Arc::new(view);
        *self.view.lock().unwrap_or_else(|e| e.into_inner()) = Some(view.clone());
        Ok(view)
    }

    /// The latest view, building the first one if there is none yet.
    pub async fn view(self: &Arc<Self>) -> Result<Arc<View>> {
        let kept = self.view.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match kept {
            Some(v) => Ok(v),
            None => self.refresh().await,
        }
    }

    pub fn monitor(&self) -> Monitor {
        self.power.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Read every watched site's power from the site monitor's hub, and keep it. Mapped sites
    /// the hub gives no reading for are logged when that list changes.
    pub async fn read_power(&self, hub: &str) -> Result<()> {
        let now = now_ms();
        let prev = self.monitor().readings;
        let got = power::fetch(&self.alert_http, hub, &self.cfg.power, now, &prev).await;
        let mut m = self.power.lock().unwrap_or_else(|e| e.into_inner());
        let (readings, gaps) = match got {
            Ok(r) => r,
            Err(e) => {
                m.read.failed = Some(now);
                return Err(e);
            }
        };
        m.readings = readings;
        m.read.ok = Some(now);
        if let Some(gaps) = gaps
            && gaps != m.gaps
        {
            if gaps.is_empty() {
                tracing::info!("site monitor: every mapped site has a reading again");
            } else {
                tracing::warn!(
                    "site monitor: {} No new readings come for them.",
                    gaps.words(&self.cfg.power.metric)
                );
            }
            m.gaps = gaps;
        }
        Ok(())
    }

    /// Read the core's feed up to the end. Notices wait until this has worked once.
    pub async fn follow(&self) -> Result<usize> {
        let n = ridgewarden_client::follow(&self.core, &self.db, PROJECTIONS).await?;
        self.caught_up.store(true, Ordering::Relaxed);
        Ok(n)
    }
}

/// A view that notices may be sent from: one built after the feed was read to the end.
async fn settled(app: &Arc<App>) -> Result<Option<Arc<View>>> {
    let v = app.view().await?;
    Ok(v.settled.then_some(v))
}

async fn alert_round(app: &Arc<App>) -> Result<usize> {
    let Some(v) = settled(app).await? else {
        return Ok(0);
    };
    alerts::run(
        &app.alert_http,
        &app.db,
        &app.cfg.alerts,
        "ridgewarden-status",
        incidents::alerts(&v.report),
        // an incident's alert ends only when the incident is resolved
        &|_| false,
    )
    .await
}

async fn message_round(app: &Arc<App>) -> Result<usize> {
    let Some(v) = settled(app).await? else {
        return Ok(0);
    };
    subscribe::round(&app.alert_http, &app.db, &app.cfg, &v.report, now_ms()).await
}

async fn mirror_round(app: &Arc<App>) -> Result<()> {
    let v = app.view().await?;
    let files = mirror::files(&app.cfg, &v, now_ms());
    let (dir, copy) = (app.cfg.mirror_dir(), files.clone());
    tokio::task::spawn_blocking(move || mirror::write(&dir, &copy)).await??;
    mirror::push(&app.alert_http, &app.cfg, &files).await
}

fn note_push(app: &App, r: Result<()>) {
    let now = now_ms();
    let mut p = app.pushed.lock().unwrap_or_else(|e| e.into_inner());
    match r {
        Ok(()) => p.ok = Some(now),
        Err(e) => {
            tracing::warn!("mirror: {e:#}");
            p.failed = Some(now);
        }
    }
}

/// Run `round` every `every`, after `first`, logging what it did.
fn every<F, Fut>(app: &Arc<App>, first: u64, every: u64, what: &'static str, round: F)
where
    F: Fn(Arc<App>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<usize>> + Send,
{
    let app = app.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(first)).await;
        loop {
            match round(app.clone()).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("{n} {what} sent"),
                Err(e) => tracing::warn!("{what}: {e:#}"),
            }
            tokio::time::sleep(Duration::from_secs(every)).await;
        }
    });
}

/// Follow the core's feed, keep the view current, keep history pruned, send alerts and
/// messages, keep the mirror current and read the site monitor, forever. Alerts and
/// messages run apart, so a slow mail server can't hold up an alert.
pub fn spawn_loops(app: Arc<App>) {
    let a = app.clone();
    tokio::spawn(async move {
        loop {
            if let Err(e) = a.follow().await {
                tracing::warn!("following the core: {e:#}");
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
    let a = app.clone();
    tokio::spawn(async move {
        loop {
            if let Err(e) = a.refresh().await {
                tracing::warn!("building the page: {e:#}");
            }
            tokio::time::sleep(Duration::from_secs(20)).await;
        }
    });
    let a = app.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(120)).await;
        loop {
            let now = now_ms();
            let r = ridgewarden_health::probes::rollup(&a.db(), now);
            if let Err(e) = r {
                tracing::warn!("rolling up probe history: {e:#}");
            }
            let r = ridgewarden_linkdoctor::history::rollup(&a.db(), now);
            if let Err(e) = r {
                tracing::warn!("rolling up link history: {e:#}");
            }
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
    if app.cfg.mirror.enabled() {
        let a = app.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            loop {
                let r = mirror_round(&a).await;
                note_push(&a, r);
                tokio::time::sleep(Duration::from_secs(a.cfg.mirror.every_s)).await;
            }
        });
    }
    if let Some(hub) = app.cfg.power.hub.clone() {
        let a = app.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = a.read_power(&hub).await {
                    tracing::warn!("reading power from the site monitor: {e:#}");
                }
                // sitemon samples every 30 s; every two minutes is plenty over a radio link
                tokio::time::sleep(Duration::from_secs(120)).await;
            }
        });
    }
    every(&app, 30, 60, "alert notices", |a| async move {
        alert_round(&a).await
    });
    if !app.cfg.subscriptions.contacts.is_empty() {
        every(&app, 30, 60, "messages to contacts", |a| async move {
            message_round(&a).await
        });
    }
}
