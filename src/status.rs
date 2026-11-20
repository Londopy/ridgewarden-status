//! Regional status page model (spec 08): green / yellow / red per site and per key
//! service, in plain words. When the data is old it says "unknown" rather than guess:
//! a wrong green costs more trust than an honest "we don't know".

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use ridgewarden_client::Event;
use ridgewarden_client::core::Projection;
use ridgewarden_client::ui::Color;
use ridgewarden_client::util::{human_duration, norm_node, short_time};
use ridgewarden_health::health::{self, Class, Rules, ServiceView};
use ridgewarden_linkdoctor::links::{self, AlertKind, LinkState, PairView, Topology};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use crate::config::Config;
use crate::incidents::{self, Incident, TEXT_MAX, clip};
use crate::maintenance::{self, Notice};

#[derive(Clone, Debug)]
pub struct SiteTile {
    pub node: String,
    pub label: String,
    pub color: Color,
    pub last_seen: Option<i64>,
    pub reason: String,
    /// (lat, lon) from the node's latest poll, if it reports a location
    pub pos: Option<(f64, f64)>,
}

#[derive(Clone, Debug)]
pub struct ServiceLine {
    pub name: String,
    pub site: String,
    pub color: Color,
    pub words: &'static str,
    pub reason: String,
    /// a push heartbeat a site receives, rather than an advertised service
    pub heartbeat: bool,
}

#[derive(Clone, Debug)]
pub struct LinkLine {
    pub a: String,
    pub b: String,
    pub state: LinkState,
}

#[derive(Clone, Debug)]
pub struct Banner {
    pub text: String,
    pub by: String,
    pub at: i64,
}

#[derive(Clone)]
pub struct Report {
    pub generated: i64,
    pub fresh: bool,
    pub headline: String,
    pub headline_color: Color,
    pub sites: Vec<SiteTile>,
    pub services: Vec<ServiceLine>,
    pub banner: Option<Banner>,
    /// when the host list the sites were judged by was polled
    pub viewpoint: Option<i64>,
    /// every link seen in the last week
    pub links: Vec<LinkLine>,
    /// open incidents, newest first
    pub incidents: Vec<Incident>,
    /// maintenance under way or starting in the next 14 days, soonest first
    pub maintenance: Vec<Notice>,
}

impl Report {
    /// The same data as the page, for /api/status and the mirror's status.json.
    pub fn to_json(&self) -> Value {
        json!({
            "generated": self.generated,
            "fresh": self.fresh,
            "headline": self.headline,
            "headline_color": self.headline_color.word(),
            "banner": self.banner.as_ref().map(|b| json!({"text": b.text, "by": b.by, "at": b.at})),
            "incidents": self.incidents.iter().map(|i| json!({
                "id": i.id, "title": i.title, "sites": i.sites, "stage": i.latest().stage,
                "updates": i.updates.iter().map(|u| json!({"stage": u.stage, "text": u.text, "by": u.by, "at": u.at})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "maintenance": self.maintenance.iter().map(|n| json!({
                "id": n.id, "text": n.text, "from": n.from, "to": n.to, "sites": n.sites,
                "by": n.by, "active": n.active(self.generated),
            })).collect::<Vec<_>>(),
            "sites": self.sites.iter().map(|s| json!({
                "node": s.node, "label": s.label, "state": s.color.word(), "reason": s.reason,
                "last_seen": s.last_seen, "lat": s.pos.map(|p| p.0), "lon": s.pos.map(|p| p.1),
            })).collect::<Vec<_>>(),
            "links": self.links.iter().map(|l| json!({"a": l.a, "b": l.b, "state": link_word(l.state)})).collect::<Vec<_>>(),
            "services": self.services.iter().map(|s| json!({
                "name": s.name, "site": s.site, "state": s.color.word(), "words": s.words,
                "detail": s.reason, "heartbeat": s.heartbeat,
            })).collect::<Vec<_>>(),
        })
    }
}

pub fn link_word(s: LinkState) -> &'static str {
    match s {
        LinkState::Up => "up",
        LinkState::Down => "down",
        _ => "unknown",
    }
}

/// Everything the dashboard pages need, computed once per request.
pub struct Snapshot {
    pub now: i64,
    pub fresh_ms: i64,
    pub pairs: Vec<PairView>,
    pub sites: BTreeSet<String>,
    pub topo: Topology,
    pub services: Vec<ServiceView>,
    /// sites whose own links we have fresh data for (polled on the LAN or crawled)
    pub observed: BTreeSet<String>,
}

pub fn snapshot(conn: &Connection, cfg: &Config, now: i64, anchor: &str) -> Result<Snapshot> {
    let fresh_ms = fresh_ms(cfg);
    let pairs = links::analyze(conn, now, fresh_ms, &cfg.links)?;
    let sites = links::known_sites(conn, now)?;
    let topo = links::topology(&pairs, &sites, Some(anchor));
    let rules = Rules {
        fresh_ms,
        fail_after: cfg.fail_after,
        ok_after: cfg.ok_after,
    };
    let services = health::classify(conn, now, &rules)?;
    let observed = conn
        .prepare("SELECT DISTINCT node FROM polls WHERE ok = 1 AND ts >= ?1")?
        .query_map([now - fresh_ms], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    Ok(Snapshot {
        now,
        fresh_ms,
        pairs,
        sites,
        topo,
        services,
        observed,
    })
}

pub fn fresh_ms(cfg: &Config) -> i64 {
    cfg.fresh_s as i64 * 1000
}

pub fn label(cfg: &Config, node: &str) -> String {
    cfg.site_names
        .iter()
        .find(|(k, _)| norm_node(k) == node)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| node.to_string())
}

/// "Hilltop relay, North site"
pub fn labels(cfg: &Config, nodes: &[String]) -> String {
    nodes
        .iter()
        .map(|n| label(cfg, n))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Each node's latest reported location, from its own polls.
fn positions(conn: &Connection) -> Result<BTreeMap<String, (f64, f64)>> {
    let mut st = conn.prepare(
        "SELECT node, lat, lon, max(ts) FROM polls
         WHERE ok = 1 AND lat IS NOT NULL AND lon IS NOT NULL GROUP BY node",
    )?;
    let mut out = BTreeMap::new();
    for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)))? {
        let (node, lat, lon): (String, f64, f64) = r?;
        // 0, 0 is what a node reports when nobody set its location
        if lat.abs() <= 90.0 && lon.abs() <= 180.0 && (lat, lon) != (0.0, 0.0) {
            out.insert(node, (lat, lon));
        }
    }
    Ok(out)
}

fn last_heard(conn: &Connection) -> Result<BTreeMap<String, i64>> {
    let mut out: BTreeMap<String, i64> = BTreeMap::new();
    let mut st = conn.prepare(
        "SELECT node, max(ts) FROM polls WHERE ok = 1 GROUP BY node
         UNION ALL SELECT neighbor, max(ts) FROM link_samples GROUP BY neighbor",
    )?;
    for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (n, ts) = r?;
        let e = out.entry(n).or_insert(ts);
        *e = (*e).max(ts);
    }
    Ok(out)
}

/// The incident banner: the newest `status.banner` event by lamport clock, which every
/// site agrees on. Only stations listed in `banner_authors` (if any are) can set it.
pub struct Banners;

impl Projection for Banners {
    fn schema(&self) -> &'static str {
        "CREATE TABLE IF NOT EXISTS banners (
            event_id TEXT PRIMARY KEY,
            lamport  INTEGER NOT NULL,
            ts       INTEGER NOT NULL,
            call     TEXT NOT NULL,
            key      TEXT NOT NULL,
            text     TEXT NOT NULL
        );"
    }

    fn reset(&self, tx: &Transaction) -> Result<()> {
        tx.execute("DELETE FROM banners", [])?;
        Ok(())
    }

    fn apply(&self, tx: &Transaction, e: &Event, ts: i64) -> Result<()> {
        if e.kind != "status.banner" {
            return Ok(());
        }
        let text = clip(
            e.payload.get("text").and_then(|v| v.as_str()).unwrap_or(""),
            TEXT_MAX,
        );
        tx.execute(
            "INSERT OR IGNORE INTO banners (event_id, lamport, ts, call, key, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![e.id, e.lamport.min(i64::MAX as u64) as i64, ts, e.author.call, e.author.key, text],
        )?;
        Ok(())
    }
}

pub fn banner(conn: &Connection, cfg: &Config) -> Result<Option<Banner>> {
    let mut st = conn
        .prepare("SELECT text, call, ts, key FROM banners ORDER BY lamport DESC, event_id DESC")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for r in rows {
        let (text, by, at, key) = r?;
        if !cfg.may_post(&key) {
            continue;
        }
        return Ok((!text.is_empty()).then_some(Banner { text, by, at }));
    }
    Ok(None)
}

fn service_color(c: Class) -> Color {
    match c {
        Class::Up | Class::UnpublishedAlive => Color::Green,
        Class::Partial => Color::Yellow,
        Class::Down | Class::Stale => Color::Red,
        Class::Unknown | Class::Unpublished => Color::Unknown,
    }
}

/// The node whose host list was used, when it was polled, and the hosts it could reach.
struct Reach {
    node: String,
    ts: i64,
    hosts: BTreeSet<String>,
}

/// Which nodes the mesh itself says are reachable, from the freshest viewpoint we have:
/// our own node's `hosts` list if it is fresh, otherwise any agent's. This is the routing
/// table's answer, so it is right even for sites whose links no agent can see. Polls from
/// this site's own core (`key`) come first, since any trusted station can publish a poll
/// that names our node but only our core really polls it.
fn reachability(
    conn: &Connection,
    now: i64,
    fresh_ms: i64,
    anchor: &str,
    key: &str,
) -> Result<Option<Reach>> {
    let row: Option<(String, i64, String)> = conn
        .query_row(
            "SELECT p.node, p.ts, l.json FROM polls p JOIN lists l ON l.hash = p.hosts_hash
             WHERE p.ok = 1 AND p.ts >= ?1
             ORDER BY (p.author_key = ?3) DESC, (p.node = ?2) DESC, p.ts DESC LIMIT 1",
            rusqlite::params![now - fresh_ms, anchor, key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row.map(|(node, ts, json)| {
        let mut hosts: BTreeSet<String> = serde_json::from_str::<Vec<String>>(&json)
            .unwrap_or_default()
            .iter()
            .map(|h| norm_node(h))
            .collect();
        hosts.insert(node.clone());
        Reach { node, ts, hosts }
    }))
}

/// Sites kept after they fall silent, so a dead site stays on the page as cut off instead
/// of dropping off the list after a week. Not a projection: a core reset must not lose it.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS site_list (
    node       TEXT PRIMARY KEY,
    last_heard INTEGER NOT NULL
);";

/// Note every site heard from so far, and when it was last heard.
pub fn remember_sites(conn: &Connection) -> Result<()> {
    for (node, ts) in last_heard(conn)? {
        conn.execute(
            "INSERT INTO site_list (node, last_heard) VALUES (?1, ?2)
             ON CONFLICT(node) DO UPDATE SET last_heard = max(last_heard, excluded.last_heard)",
            params![node, ts],
        )?;
    }
    Ok(())
}

/// Take a site off the page until it is heard from again. Returns whether it was listed.
pub fn forget(conn: &Connection, node: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM site_list WHERE node = ?1", [node])? > 0)
}

fn remembered(conn: &Connection) -> Result<BTreeMap<String, i64>> {
    let mut st = conn.prepare("SELECT node, last_heard FROM site_list")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// `anchor` is this site's node and `key` its core's key: the page shows the network from
/// their side.
pub fn build(
    conn: &Connection,
    cfg: &Config,
    snap: &Snapshot,
    anchor: &str,
    key: &str,
) -> Result<Report> {
    let now = snap.now;
    let mut heard = last_heard(conn)?;
    let newest = heard.values().copied().max();
    for (node, t) in remembered(conn)? {
        let e = heard.entry(node).or_insert(t);
        *e = (*e).max(t);
    }
    let pos = positions(conn)?;
    let fresh = newest.is_some_and(|t| t >= now - snap.fresh_ms);
    let heard_fresh = |n: &str| heard.get(n).is_some_and(|t| *t >= now - snap.fresh_ms);
    let reach = reachability(conn, now, snap.fresh_ms, anchor, key)?;
    // without a hosts list (very old firmware), fall back to "heard from recently"
    let reachable = |n: &str| match &reach {
        Some(r) => r.hosts.contains(n),
        None => heard_fresh(n),
    };

    let mut by_site: BTreeMap<&str, Vec<&PairView>> = BTreeMap::new();
    for p in &snap.pairs {
        by_site.entry(p.a.as_str()).or_default().push(p);
        by_site.entry(p.b.as_str()).or_default().push(p);
    }

    // a site that has gone quiet stays listed, as cut off, until someone forgets it
    let mut all = snap.sites.clone();
    all.extend(heard.keys().cloned());
    all.extend(cfg.expected_sites.iter().map(|s| norm_node(s)));
    let mut sites = vec![];
    for node in &all {
        let last = heard.get(node).copied();
        let lbl = label(cfg, node);
        let (color, reason) = if !fresh {
            (Color::Unknown, "No fresh data.".to_string())
        } else if !reachable(node) && heard_fresh(node) {
            let from = reach
                .as_ref()
                .map(|r| label(cfg, &r.node))
                .unwrap_or_default();
            (
                Color::Red,
                format!(
                    "Running, but not reachable from {from}: cut off from this side of the network."
                ),
            )
        } else if !reachable(node) {
            (
                Color::Red,
                match last {
                    Some(t) => format!(
                        "Not reachable; last heard from {} ({} ago).",
                        short_time(t),
                        human_duration(now - t)
                    ),
                    None => "Not reachable, and not heard from yet.".into(),
                },
            )
        } else {
            let mut issues = vec![];
            for p in by_site.get(node.as_str()).into_iter().flatten() {
                let other = if &p.a == node { &p.b } else { &p.a };
                for a in &p.alerts {
                    // a lost link is reported against the cut-off side instead, and one
                    // measuring below its plan is work for the next site visit, not a fault
                    if (a.kind == AlertKind::Loss && !reachable(other))
                        || a.kind == AlertKind::BelowPlan
                    {
                        continue;
                    }
                    issues.push(format!("Link to {}: {}", label(cfg, other), a.text));
                }
            }
            for s in snap.services.iter().filter(|s| &s.svc.host == node) {
                match s.class {
                    Class::Down if s.is_heartbeat() => {
                        issues.push(format!("{} stopped checking in (heartbeat).", s.svc.name))
                    }
                    Class::Down | Class::Partial => issues.push(format!(
                        "{}: {}.",
                        s.svc.name,
                        s.class.plain().to_lowercase()
                    )),
                    _ => {}
                }
            }
            if !issues.is_empty() {
                (Color::Yellow, issues.join(" "))
            } else {
                let up = by_site.get(node.as_str()).map_or(0, |ps| {
                    ps.iter().filter(|p| p.state == LinkState::Up).count()
                });
                if up > 0 {
                    (
                        Color::Green,
                        format!("Up, {up} link{} seen.", if up == 1 { "" } else { "s" }),
                    )
                } else {
                    (Color::Green, "Reachable on the mesh.".into())
                }
            }
        };
        sites.push(SiteTile {
            node: node.clone(),
            label: lbl,
            color,
            last_seen: last,
            reason,
            pos: pos.get(node).copied(),
        });
    }
    sites.sort_by(|a, b| a.color.cmp(&b.color).then_with(|| a.label.cmp(&b.label)));

    let (headline, headline_color) = if !fresh {
        (
            match newest {
                Some(t) => format!(
                    "No fresh data. This page last heard from the mesh at {} ({} ago); everything below is unknown.",
                    short_time(t),
                    human_duration(now - t)
                ),
                None => "No data yet.".into(),
            },
            Color::Unknown,
        )
    } else {
        let red: Vec<&SiteTile> = sites.iter().filter(|s| s.color == Color::Red).collect();
        let yellow = sites.iter().filter(|s| s.color == Color::Yellow).count();
        if !red.is_empty() {
            let red_set: BTreeSet<&str> = red.iter().map(|s| s.node.as_str()).collect();
            // the lost links that separate the cut-off sites from everyone else
            let cuts: Vec<&PairView> = snap
                .pairs
                .iter()
                .filter(|p| p.state == LinkState::Down)
                .filter(|p| red_set.contains(p.a.as_str()) != red_set.contains(p.b.as_str()))
                .collect();
            // cut off since the *last* separating link went down; before that another
            // path still reached them
            let since = cuts
                .iter()
                .filter_map(|p| p.sides.iter().filter_map(|s| s.down_since).min())
                .max();
            let names = red
                .iter()
                .map(|s| s.label.clone())
                .collect::<Vec<_>>()
                .join(", ");
            let mut h = format!(
                "{names} {} cut off",
                if red.len() == 1 { "is" } else { "are" }
            );
            if let Some(t) = since {
                h += &format!(" since {}", short_time(t));
            }
            if !cuts.is_empty() {
                let paths: Vec<String> = cuts
                    .iter()
                    .map(|p| format!("{} to {}", label(cfg, &p.a), label(cfg, &p.b)))
                    .collect();
                h += &format!("; link down: {}", paths.join(", "));
            }
            (h + ".", Color::Red)
        } else if yellow > 0 {
            (
                format!(
                    "All {} sites reachable; {yellow} need attention.",
                    sites.len()
                ),
                Color::Yellow,
            )
        } else if sites.is_empty() {
            ("No sites seen yet.".into(), Color::Unknown)
        } else {
            (format!("All {} sites up.", sites.len()), Color::Green)
        }
    };

    let keys: Vec<String> = cfg
        .key_services
        .iter()
        .map(|k| k.to_ascii_lowercase())
        .collect();
    let services = snap
        .services
        .iter()
        .filter(|s| s.class != Class::Unpublished)
        .filter(|s| {
            keys.is_empty()
                || keys
                    .iter()
                    .any(|k| s.svc.name.to_ascii_lowercase().contains(k))
        })
        .map(|s| {
            let color = if fresh {
                service_color(s.class)
            } else {
                Color::Unknown
            };
            ServiceLine {
                name: s.svc.name.clone(),
                site: label(cfg, &s.svc.host),
                color,
                words: match (fresh, s.is_heartbeat(), s.class) {
                    (false, _, _) => "Unknown",
                    (true, true, Class::Down) => "Stopped checking in",
                    (true, true, Class::Up) => "Checking in",
                    (true, _, c) => c.plain(),
                },
                reason: if fresh {
                    s.since_text(now)
                } else {
                    String::new()
                },
                heartbeat: s.is_heartbeat(),
            }
        })
        .collect();
    let links = snap
        .pairs
        .iter()
        .map(|p| LinkLine {
            a: p.a.clone(),
            b: p.b.clone(),
            state: p.state,
        })
        .collect();

    Ok(Report {
        generated: now,
        fresh,
        headline,
        headline_color,
        sites,
        services,
        banner: banner(conn, cfg)?,
        viewpoint: reach.map(|r| r.ts),
        links,
        incidents: incidents::open(conn, cfg)?,
        maintenance: maintenance::current(conn, cfg, now)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MIN, NOW, add_as, cfg, cut_off_c, fake, poll, report};
    const DAY: i64 = 24 * 60 * MIN;
    use ridgewarden_client::mesh::testing::Fake;

    fn color(r: &Report, node: &str) -> Color {
        r.sites.iter().find(|s| s.node == node).unwrap().color
    }

    #[test]
    fn all_up() {
        let mut f = fake();
        poll(&mut f, NOW - 5 * MIN, "a", &["b"], &["a", "b", "c"]);
        poll(&mut f, NOW - 5 * MIN, "b", &["a", "c"], &["a", "b", "c"]);
        let r = report(&f, &cfg(), "a");
        assert_eq!(r.headline, "All 3 sites up.");
        assert_eq!(r.headline_color, Color::Green);
    }

    #[test]
    fn a_site_the_mesh_cannot_reach_is_cut_off() {
        let mut f = fake();
        cut_off_c(&mut f);
        let r = report(&f, &cfg(), "a");
        assert_eq!(color(&r, "c"), Color::Red);
        assert_eq!(r.headline_color, Color::Red);
        assert!(
            r.headline.starts_with("c is cut off since"),
            "{}",
            r.headline
        );
        assert!(r.headline.contains("link down: b to c"), "{}", r.headline);
        assert_eq!(color(&r, "a"), Color::Green);
    }

    #[test]
    fn old_data_is_unknown_never_green() {
        let mut f = fake();
        poll(&mut f, NOW - 3 * 60 * MIN, "a", &["b"], &["a", "b"]);
        let r = report(&f, &cfg(), "a");
        assert_eq!(r.headline_color, Color::Unknown);
        assert!(r.sites.iter().all(|s| s.color == Color::Unknown));
    }

    #[test]
    fn banner_is_the_newest_and_respects_allowed_authors() {
        let mut f = fake();
        poll(&mut f, NOW - 5 * MIN, "a", &[], &["a"]);
        let e = f.add(
            "status.banner",
            json!({"text": "Hilltop on generator"}),
            NOW - 2 * MIN,
        );
        let mut c = cfg();
        assert_eq!(
            banner(&f.conn, &c).unwrap().unwrap().text,
            "Hilltop on generator"
        );
        c.banner_authors = vec!["0".repeat(64)];
        assert!(
            banner(&f.conn, &c).unwrap().is_none(),
            "author not on the list"
        );
        c.banner_authors = vec![e.author.key.clone()];
        assert!(banner(&f.conn, &c).unwrap().is_some());
        f.add("status.banner", json!({"text": ""}), NOW - MIN);
        assert!(banner(&f.conn, &c).unwrap().is_none(), "cleared");
    }

    #[test]
    fn a_heartbeat_that_stopped_counts_against_its_site() {
        let mut f = fake();
        poll(&mut f, NOW - 5 * MIN, "a", &["b"], &["a", "b"]);
        poll(&mut f, NOW - 5 * MIN, "b", &["a"], &["a", "b"]);
        let beat = |f: &mut Fake, ok: bool, ts: i64| {
            f.add(
                "svc.beat",
                json!({"vantage": "a", "results": [{"name": "Generator", "link": "push://a/generator",
                       "host": "a", "ok": ok, "check": "push"}]}),
                ts,
            );
        };
        beat(&mut f, true, NOW - 3 * MIN);
        let r = report(&f, &cfg(), "a");
        assert_eq!(color(&r, "a"), Color::Green);
        let line = r.services.iter().find(|s| s.heartbeat).unwrap();
        assert_eq!(line.words, "Checking in");

        beat(&mut f, false, NOW - 2 * MIN);
        let r = report(&f, &cfg(), "a");
        let a = r.sites.iter().find(|s| s.node == "a").unwrap();
        assert_eq!(a.color, Color::Yellow);
        assert!(
            a.reason
                .contains("Generator stopped checking in (heartbeat)"),
            "{}",
            a.reason
        );
        assert_eq!(color(&r, "b"), Color::Green, "only the receiving site");
    }

    #[test]
    fn a_link_below_its_plan_does_not_need_attention() {
        let mut f = fake();
        poll(&mut f, NOW - 5 * MIN, "a", &["b"], &["a", "b"]);
        poll(&mut f, NOW - 5 * MIN, "b", &["a"], &["a", "b"]);
        let c = cfg();
        let mut snap = snapshot(&f.conn, &c, NOW, "a").unwrap();
        let alert = |kind| links::Alert {
            kind,
            severity: 1.0,
            text: "Should be 30 dB, measures 18.".into(),
            since: None,
        };
        snap.pairs[0].alerts.push(alert(AlertKind::BelowPlan));
        let r = build(&f.conn, &c, &snap, "a", "").unwrap();
        assert!(r.sites.iter().all(|s| s.color == Color::Green));
        snap.pairs[0].alerts.push(alert(AlertKind::Aim));
        let r = build(&f.conn, &c, &snap, "a", "").unwrap();
        assert!(
            r.sites.iter().all(|s| s.color == Color::Yellow),
            "an aim problem does"
        );
    }

    #[test]
    fn a_site_gone_quiet_stays_cut_off_until_it_is_forgotten() {
        let mut f = fake();
        poll(&mut f, NOW - 10 * DAY, "c", &["a"], &["a", "c"]);
        remember_sites(&f.conn).unwrap();
        // the raw history is pruned after a month; the list of sites is not
        f.conn
            .execute("DELETE FROM polls WHERE node = 'c'", [])
            .unwrap();
        f.conn.execute("DELETE FROM link_samples", []).unwrap();
        poll(&mut f, NOW - 5 * MIN, "a", &[], &["a"]);
        let r = report(&f, &cfg(), "a");
        let c = r.sites.iter().find(|s| s.node == "c").unwrap();
        assert_eq!(c.color, Color::Red);
        assert!(c.reason.contains("(10 d ago)"), "{}", c.reason);
        assert_eq!(r.headline_color, Color::Red, "not a quiet green");

        assert!(forget(&f.conn, "c").unwrap());
        let r = report(&f, &cfg(), "a");
        assert!(r.sites.iter().all(|s| s.node != "c"));
        assert_eq!(r.headline_color, Color::Green);
    }

    #[test]
    fn an_expected_site_never_heard_from_is_cut_off() {
        let mut f = fake();
        poll(&mut f, NOW - 5 * MIN, "a", &[], &["a"]);
        let mut c = cfg();
        c.expected_sites = vec!["TST-New".into()];
        let r = report(&f, &c, "a");
        let new = r.sites.iter().find(|s| s.node == "tst-new").unwrap();
        assert_eq!(new.color, Color::Red);
        assert!(new.reason.contains("not heard from yet"), "{}", new.reason);
    }

    #[test]
    fn a_poll_naming_our_node_from_another_station_does_not_decide_who_is_reachable() {
        let mut f = fake();
        let mine = f.add("x", json!({}), NOW - 6 * MIN).author.key;
        poll(&mut f, NOW - 5 * MIN, "a", &["b"], &["a", "b"]);
        poll(&mut f, NOW - 5 * MIN, "b", &["a"], &["a", "b"]);
        // a newer poll that claims to be from our node, by a key that isn't our core's
        let hosts = json!(["a"]);
        let h = ridgewarden_client::event::list_hash(&hosts);
        let other = "ab".repeat(32);
        add_as(
            &mut f,
            &other,
            900,
            "mesh.list",
            json!({"hash": h, "list": hosts}),
            NOW - MIN,
        );
        add_as(
            &mut f,
            &other,
            901,
            "node.poll",
            json!({"node": "a", "ok": true, "via": "lan", "hosts_hash": h}),
            NOW - MIN,
        );
        let c = cfg();
        let snap = snapshot(&f.conn, &c, NOW, "a").unwrap();
        let r = build(&f.conn, &c, &snap, "a", &mine).unwrap();
        assert_eq!(color(&r, "b"), Color::Green, "our own core's poll decides");
    }
}
