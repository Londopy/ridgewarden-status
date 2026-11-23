use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use ridgewarden_client::util::{
    ms_to_rfc3339, norm_node, now_ms, rfc3339_to_ms, short_time, valid_node,
};
use serde_json::json;

use ridgewarden_status::config::{Config, EXAMPLE};
use ridgewarden_status::incidents::{self, SITES_MAX, STAGES, TEXT_MAX, TITLE_MAX};
use ridgewarden_status::{App, maintenance, mirror, spawn_loops, status, subscribe, web};

#[derive(Parser)]
#[command(
    version,
    about = "Regional status page for AREDN meshes (a ridgewarden app)"
)]
struct Cli {
    #[arg(
        long,
        short,
        global = true,
        default_value = "/etc/ridgewarden/status.toml"
    )]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write a starter config
    Init,
    /// Follow the core and serve the status page
    Run,
    /// Set or clear the incident banner (signed by this site's core, under its callsign)
    Banner {
        /// banner text; omit with --clear
        text: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Open, update or list incidents (signed by this site's core, under its callsign)
    Incident {
        #[command(subcommand)]
        cmd: IncidentCmd,
    },
    /// Post a planned maintenance notice, or list or cancel them
    Maintenance(MaintenanceArgs),
    /// Write the internet mirror's files into a folder once, e.g. to copy them by hand
    Export {
        /// the folder to write them to
        dir: PathBuf,
    },
    /// Check that messages reach subscribed contacts
    Subscribers {
        #[command(subcommand)]
        cmd: SubscribersCmd,
    },
    /// Take a site that has gone for good off the page (it comes back if heard from again)
    Forget {
        /// its node name
        node: String,
    },
}

#[derive(Subcommand)]
enum SubscribersCmd {
    /// Send a test message to every channel of one contact, or of every contact
    Test {
        /// the contact's name as in the config; every contact when left out
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum IncidentCmd {
    /// Open an incident
    Open {
        /// what is wrong, in one line
        title: String,
        #[arg(long, default_value = "investigating", value_parser = STAGES)]
        stage: String,
        /// node names it affects, separated by commas
        #[arg(long, value_delimiter = ',')]
        sites: Vec<String>,
        /// more detail
        #[arg(long, default_value = "")]
        text: String,
    },
    /// Post an update; --stage resolved closes the incident
    Update {
        /// the incident's id, or at least its first 8 characters
        id: String,
        #[arg(long, value_parser = STAGES)]
        stage: String,
        #[arg(long, default_value = "")]
        text: String,
    },
    /// List open incidents and their ids
    List,
}

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
struct MaintenanceArgs {
    #[command(subcommand)]
    cmd: Option<MaintenanceCmd>,
    /// what will happen, e.g. "Tower work at the hilltop; North site may drop"
    text: Option<String>,
    /// start, e.g. 2026-11-22T15:00:00Z
    #[arg(long)]
    from: Option<String>,
    /// end, e.g. 2026-11-22T19:00:00Z
    #[arg(long)]
    to: Option<String>,
    /// node names it affects, separated by commas
    #[arg(long, value_delimiter = ',')]
    sites: Vec<String>,
}

#[derive(Subcommand)]
enum MaintenanceCmd {
    /// Cancel a notice
    Cancel {
        /// the notice's id, or at least its first 8 characters
        id: String,
    },
    /// List notices under way or in the next 14 days, and their ids
    List,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ridgewarden_status=info,ridgewarden_client=info".into()),
        )
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init => {
            if cli.config.exists() {
                bail!(
                    "{} already exists; not overwriting it",
                    cli.config.display()
                );
            }
            if let Some(d) = cli.config.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&cli.config, EXAMPLE)?;
            println!("Wrote {}", cli.config.display());
            Ok(())
        }
        Cmd::Run => {
            let cfg = Config::load(&cli.config)?;
            tokio::runtime::Runtime::new()?.block_on(run(cfg))
        }
        Cmd::Banner { text, clear } => {
            let text = match (text, clear) {
                (_, true) => String::new(),
                (Some(t), false) if !t.trim().is_empty() => fits(&t, TEXT_MAX, "the banner")?,
                _ => bail!("give the banner text, or --clear"),
            };
            let cfg = Config::load(&cli.config)?;
            tokio::runtime::Runtime::new()?.block_on(async {
                let app = poster(cfg).await?;
                app.core
                    .append("status.banner", json!({"text": text}))
                    .await
            })?;
            println!(
                "{}",
                if text.is_empty() {
                    "Banner cleared."
                } else {
                    "Banner set."
                }
            );
            Ok(())
        }
        Cmd::Forget { node } => forget(Config::load(&cli.config)?, &norm_node(&node)),
        Cmd::Incident { cmd } => {
            let cfg = Config::load(&cli.config)?;
            tokio::runtime::Runtime::new()?.block_on(incident_cmd(cfg, cmd))
        }
        Cmd::Maintenance(args) => {
            let cfg = Config::load(&cli.config)?;
            tokio::runtime::Runtime::new()?.block_on(maintenance_cmd(cfg, args))
        }
        Cmd::Export { dir } => {
            let cfg = Config::load(&cli.config)?;
            tokio::runtime::Runtime::new()?.block_on(export(cfg, &dir))
        }
        Cmd::Subscribers {
            cmd: SubscribersCmd::Test { name },
        } => {
            let cfg = Config::load(&cli.config)?;
            tokio::runtime::Runtime::new()?.block_on(test_subscribers(cfg, name.as_deref()))
        }
    }
}

async fn export(cfg: Config, dir: &Path) -> Result<()> {
    let app = App::new(cfg, false)?;
    catch_up(&app).await?;
    if let Some(hub) = &app.cfg.power.hub
        && let Err(e) = app.read_power(hub).await
    {
        eprintln!("Power shows as unknown in the copy: {e:#}");
    }
    let me = app.core.whoami().await?;
    let view = app.build_view(&me.node, &me.key)?;
    let files = mirror::files(&app.cfg, &view, now_ms());
    mirror::write(dir, &files)?;
    println!("Wrote {} files to {}.", files.len(), dir.display());
    Ok(())
}

fn forget(cfg: Config, node: &str) -> Result<()> {
    let app = App::new(cfg, false)?;
    if status::forget(&app.db(), node)? {
        println!("Forgot {node}. It shows again if it is heard from.");
    } else {
        println!("{node} isn't on the list of sites.");
    }
    if app.cfg.expected_sites.iter().any(|s| norm_node(s) == node) {
        println!(
            "It is in expected_sites, so it stays on the page until you take it out of the config."
        );
    }
    Ok(())
}

/// The app with the core's token, once it's clear this site may post: a post from a key
/// that `banner_authors` leaves out would be ignored by this page and any with the same
/// list, so it is refused here instead.
async fn poster(cfg: Config) -> Result<App> {
    let app = App::new(cfg, true)?;
    let me = app
        .core
        .whoami()
        .await
        .with_context(|| format!("asking the core at {} who this site is", app.core.base()))?;
    if app.cfg.banner_authors.is_empty() {
        bail!(
            "banner_authors is empty, so no page would show the post; add this site's key {} to \
             banner_authors here and at every site whose page should show it",
            me.key
        );
    }
    if !app.cfg.may_post(&me.key) {
        bail!(
            "this site's key {} isn't in banner_authors, so the post would be ignored; add it there, \
             or post from a site that is listed",
            me.key
        );
    }
    Ok(app)
}

async fn test_subscribers(cfg: Config, name: Option<&str>) -> Result<()> {
    let app = App::new(cfg, false)?;
    let targets: Vec<_> = subscribe::targets(&app.cfg.subscriptions)
        .into_iter()
        .filter(|t| name.is_none_or(|n| t.contact.name.eq_ignore_ascii_case(n)))
        .collect();
    if targets.is_empty() {
        match name {
            Some(n) => bail!("no contact called {n:?} under [subscriptions]"),
            None => bail!("no contacts under [subscriptions]"),
        }
    }
    let news = subscribe::test_news(&app.cfg, now_ms());
    let mut failed = 0;
    for t in &targets {
        let m = subscribe::message(&app.cfg, t, &[&news]);
        match subscribe::send(&app.alert_http, &app.cfg.title, m).await {
            Ok(()) => println!("{}: sent", t.id),
            Err(e) => {
                failed += 1;
                println!("{}: failed: {e:#}", t.id);
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} channels failed", targets.len());
    }
    Ok(())
}

async fn incident_cmd(cfg: Config, cmd: IncidentCmd) -> Result<()> {
    match cmd {
        IncidentCmd::Open {
            title,
            stage,
            sites,
            text,
        } => {
            let title = fits(&title, TITLE_MAX, "the title")?;
            if title.is_empty() {
                bail!("give the incident a title");
            }
            let text = fits(&text, TEXT_MAX, "--text")?;
            let sites = nodes(&sites)?;
            let app = poster(cfg).await?;
            let e = app
                .core
                .append(
                    "status.incident",
                    json!({"title": title, "stage": stage, "sites": sites, "text": text}),
                )
                .await?;
            println!("Incident {} opened.", &e.id[..12]);
        }
        IncidentCmd::Update { id, stage, text } => {
            let text = fits(&text, TEXT_MAX, "--text")?;
            let app = poster(cfg).await?;
            catch_up(&app).await?;
            let ids = incidents::list(&app.db(), &app.cfg)?
                .into_iter()
                .map(|i| i.id);
            let id = pick(&id, ids, "incident list")?;
            app.core
                .append(
                    "status.incident",
                    json!({"incident": id, "stage": stage, "text": text}),
                )
                .await?;
            println!("Incident {} is now {stage}.", &id[..12]);
        }
        IncidentCmd::List => {
            let app = App::new(cfg, false)?;
            catch_up(&app).await?;
            let open = incidents::open(&app.db(), &app.cfg)?;
            if open.is_empty() {
                println!("No open incidents.");
            }
            for i in &open {
                let u = i.latest();
                println!(
                    "{}  {:<13}  {}  {}",
                    &i.id[..12],
                    u.stage,
                    short_time(u.at),
                    i.title
                );
            }
        }
    }
    Ok(())
}

async fn maintenance_cmd(cfg: Config, a: MaintenanceArgs) -> Result<()> {
    match a.cmd {
        Some(MaintenanceCmd::Cancel { id }) => {
            let app = poster(cfg).await?;
            catch_up(&app).await?;
            let ids = maintenance::current(&app.db(), &app.cfg, now_ms())?
                .into_iter()
                .map(|n| n.id);
            let id = pick(&id, ids, "maintenance list")?;
            app.core
                .append("status.maintenance", json!({"cancel": id}))
                .await?;
            println!("Maintenance notice {} cancelled.", &id[..12]);
        }
        Some(MaintenanceCmd::List) => {
            let app = App::new(cfg, false)?;
            catch_up(&app).await?;
            let now = now_ms();
            let list = maintenance::current(&app.db(), &app.cfg, now)?;
            if list.is_empty() {
                println!("No maintenance under way or in the next 14 days.");
            }
            for n in &list {
                println!("{}  {}  {}", &n.id[..12], n.when(now), n.text);
            }
        }
        None => {
            let (Some(text), Some(from), Some(to)) = (a.text, a.from, a.to) else {
                bail!(
                    "give the notice text, --from and --to, e.g. maintenance \"Tower work at the hilltop\" \
                     --from 2026-11-22T15:00:00Z --to 2026-11-22T19:00:00Z"
                );
            };
            let text = fits(&text, TEXT_MAX, "the notice")?;
            if text.is_empty() {
                bail!("the notice text is empty");
            }
            let (from, to) = (time_arg(&from, "--from")?, time_arg(&to, "--to")?);
            if to <= from {
                bail!("--to must be after --from");
            }
            if to <= now_ms() {
                bail!("that window has already ended");
            }
            let sites = nodes(&a.sites)?;
            let app = poster(cfg).await?;
            let e = app
                .core
                .append(
                    "status.maintenance",
                    json!({"text": text, "from": ms_to_rfc3339(from), "to": ms_to_rfc3339(to), "sites": sites}),
                )
                .await?;
            println!("Maintenance notice {} posted.", &e.id[..12]);
        }
    }
    Ok(())
}

/// `text` trimmed; refused rather than cut short when it's over `max` characters.
fn fits(text: &str, max: usize, what: &str) -> Result<String> {
    let t = text.trim();
    let n = t.chars().count();
    if n > max {
        bail!("{what} is {n} characters; keep it to {max}");
    }
    Ok(t.to_string())
}

/// Node names from --sites, refusing anything that can't be one.
fn nodes(sites: &[String]) -> Result<Vec<String>> {
    let v: Vec<String> = sites
        .iter()
        .map(|s| norm_node(s))
        .filter(|s| !s.is_empty())
        .collect();
    if let Some(bad) = v.iter().find(|s| !valid_node(s)) {
        bail!("{bad:?} is not a node name; they look like tst-hilltop");
    }
    if v.len() > SITES_MAX {
        bail!("list at most {SITES_MAX} sites");
    }
    Ok(v)
}

fn time_arg(s: &str, flag: &str) -> Result<i64> {
    rfc3339_to_ms(s)
        .with_context(|| format!("{flag} {s:?} is not a time like 2026-11-22T15:00:00Z"))
}

/// Read what's new from the core first, so something posted a moment ago is found.
async fn catch_up(app: &App) -> Result<()> {
    app.follow()
        .await
        .with_context(|| format!("reading the core's feed at {}", app.core.base()))?;
    Ok(())
}

/// The one id that starts with `prefix`, which must be at least 8 hex characters.
fn pick(prefix: &str, ids: impl Iterator<Item = String>, list: &str) -> Result<String> {
    let p = prefix.trim().to_ascii_lowercase();
    if p.len() < 8 || !p.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!(
            "give at least the first 8 characters of the id, as `ridgewarden-status {list}` shows it"
        );
    }
    let found: Vec<String> = ids.filter(|id| id.starts_with(&p)).collect();
    match found.as_slice() {
        [id] => Ok(id.clone()),
        [] => bail!("no id starts with {p}; `ridgewarden-status {list}` shows them"),
        _ => bail!("more than one id starts with {p}; give more of it"),
    }
}

async fn run(cfg: Config) -> Result<()> {
    let listen = cfg.listen;
    let app = Arc::new(App::new(cfg, false)?);
    tracing::info!(
        "ridgewarden-status {} following {}, page on http://{listen}",
        env!("CARGO_PKG_VERSION"),
        app.core.base()
    );
    if app.cfg.banner_authors.is_empty() {
        tracing::info!(
            "banner_authors is empty, so the page shows no banner, incidents or maintenance; \
             list the stations that may post"
        );
    }
    spawn_loops(app.clone());
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    axum::serve(listener, web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A core on a local port whose station has `key`.
    async fn core(key: &str) -> String {
        let me = json!({"call": "N0CALL", "key": key, "node": "tst-hilltop"});
        let app = axum::Router::new().route(
            "/v1/whoami",
            axum::routing::get(move || async move { axum::Json(me) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn only_a_listed_site_may_post() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("local.token");
        std::fs::write(&token, "test-token").unwrap();
        let mine = "ab".repeat(32);
        let mut cfg: Config = toml::from_str(EXAMPLE).unwrap();
        cfg.core.url = core(&mine).await;
        cfg.core.token_file = token;
        cfg.data_dir = dir.path().to_path_buf();
        let refusal = |authors: Vec<String>| {
            let mut c = cfg.clone();
            c.banner_authors = authors;
            async move { poster(c).await.err().map(|e| e.to_string()) }
        };

        let empty = refusal(vec![]).await.unwrap();
        assert!(
            empty.contains("banner_authors is empty") && empty.contains(&mine),
            "{empty}"
        );
        let other = refusal(vec!["cd".repeat(32)]).await.unwrap();
        assert!(
            other.contains("isn't in banner_authors") && other.contains(&mine),
            "{other}"
        );
        assert!(
            refusal(vec![mine.to_uppercase()]).await.is_none(),
            "a listed key may post, in either case"
        );
    }
}
