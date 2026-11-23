//! Incidents (spec 08): a title, the sites it affects, and a timeline of updates through
//! the stages investigating, identified, monitoring and resolved. Stations that may post
//! the banner may open and update them. Updates are ordered by lamport clock, so every
//! site shows the same timeline whatever order the events arrived in.

use std::collections::HashMap;

use anyhow::Result;
use ridgewarden_client::Event;
use ridgewarden_client::alerts::{Alert, Severity};
use ridgewarden_client::core::Projection;
use ridgewarden_client::mesh::s;
use ridgewarden_client::ui::Color;
use ridgewarden_client::util::{norm_node, valid_node};
use rusqlite::{Connection, Transaction, params};
use serde_json::Value;

use crate::config::Config;
use crate::status::Report;

pub const STAGES: [&str; 4] = ["investigating", "identified", "monitoring", "resolved"];
pub const TITLE_MAX: usize = 200;
pub const TEXT_MAX: usize = 2000;
pub const SITES_MAX: usize = 100;

#[derive(Clone, Debug)]
pub struct Update {
    /// the id of the event that posted it
    pub id: String,
    pub stage: String,
    pub text: String,
    pub by: String,
    pub at: i64,
}

#[derive(Clone, Debug)]
pub struct Incident {
    /// the id of the event that opened it
    pub id: String,
    pub title: String,
    pub sites: Vec<String>,
    /// oldest first; the first is the opening post
    pub updates: Vec<Update>,
}

impl Incident {
    pub fn latest(&self) -> &Update {
        self.updates
            .last()
            .expect("an incident always has its opening post")
    }

    pub fn is_open(&self) -> bool {
        self.latest().stage != "resolved"
    }
}

/// "Identified" for "identified".
pub fn stage_word(stage: &str) -> String {
    let mut c = stage.chars();
    c.next()
        .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
        .unwrap_or_default()
}

pub fn stage_color(stage: &str) -> Color {
    if stage == "resolved" {
        Color::Green
    } else {
        Color::Yellow
    }
}

/// Trimmed and cut to `max` characters, so one huge post can't swamp the page.
pub fn clip(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

/// Node names from a post's "sites" list; anything that isn't a node name is dropped.
pub fn sites(p: &Value) -> Vec<String> {
    let mut v: Vec<String> = p
        .get("sites")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(norm_node)
        .filter(|n| valid_node(n))
        .take(SITES_MAX)
        .collect();
    v.sort();
    v.dedup();
    v
}

/// `status.incident` events. One that names no `incident` opens one, and its id becomes
/// the incident's id; one that does is an update to it. Posts that don't make sense are
/// skipped, and who may post is checked when reading, so a config change needs no rebuild.
pub struct Incidents;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS incidents (
    id      TEXT PRIMARY KEY,
    lamport INTEGER NOT NULL,
    key     TEXT NOT NULL,
    title   TEXT NOT NULL,
    sites   TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS incident_updates (
    event_id TEXT PRIMARY KEY,
    incident TEXT NOT NULL,
    lamport  INTEGER NOT NULL,
    ts       INTEGER NOT NULL,
    call     TEXT NOT NULL,
    key      TEXT NOT NULL,
    stage    TEXT NOT NULL,
    text     TEXT NOT NULL
);";

impl Projection for Incidents {
    fn schema(&self) -> &'static str {
        SCHEMA
    }

    fn reset(&self, tx: &Transaction) -> Result<()> {
        tx.execute_batch("DELETE FROM incidents; DELETE FROM incident_updates;")?;
        Ok(())
    }

    fn apply(&self, tx: &Transaction, e: &Event, ts: i64) -> Result<()> {
        if e.kind != "status.incident" {
            return Ok(());
        }
        let p = &e.payload;
        let Some(stage) = s(p, "stage").filter(|st| STAGES.contains(st)) else {
            return Ok(());
        };
        let lamport = e.lamport.min(i64::MAX as u64) as i64;
        let incident = match s(p, "incident") {
            Some(id) => id.to_string(),
            None => {
                let title = clip(s(p, "title").unwrap_or(""), TITLE_MAX);
                if title.is_empty() {
                    return Ok(());
                }
                tx.execute(
                    "INSERT OR IGNORE INTO incidents (id, lamport, key, title, sites) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![e.id, lamport, e.author.key, title, serde_json::to_string(&sites(p))?],
                )?;
                e.id.clone()
            }
        };
        tx.execute(
            "INSERT OR IGNORE INTO incident_updates (event_id, incident, lamport, ts, call, key, stage, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.id,
                incident,
                lamport,
                ts,
                e.author.call,
                e.author.key,
                stage,
                clip(s(p, "text").unwrap_or(""), TEXT_MAX)
            ],
        )?;
        Ok(())
    }
}

/// Every incident opened by a station allowed to post, newest first, with the updates
/// from allowed stations. Updates to an incident nobody may see are dropped with it.
pub fn list(conn: &Connection, cfg: &Config) -> Result<Vec<Incident>> {
    let mut out = vec![];
    let mut index = HashMap::new();
    let mut st =
        conn.prepare("SELECT id, key, title, sites FROM incidents ORDER BY lamport DESC, id DESC")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for r in rows {
        let (id, key, title, sites) = r?;
        if !cfg.may_post(&key) {
            continue;
        }
        index.insert(id.clone(), out.len());
        out.push(Incident {
            id,
            title,
            sites: serde_json::from_str(&sites)?,
            updates: vec![],
        });
    }
    let mut st = conn.prepare(
        "SELECT incident, key, stage, text, call, ts, event_id FROM incident_updates
         ORDER BY lamport, event_id",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            Update {
                id: r.get(6)?,
                stage: r.get(2)?,
                text: r.get(3)?,
                by: r.get(4)?,
                at: r.get(5)?,
            },
        ))
    })?;
    for r in rows {
        let (incident, key, u) = r?;
        if let Some(&i) = index.get(&incident)
            && cfg.may_post(&key)
        {
            out[i].updates.push(u);
        }
    }
    Ok(out)
}

pub fn open(conn: &Connection, cfg: &Config) -> Result<Vec<Incident>> {
    Ok(list(conn, cfg)?
        .into_iter()
        .filter(Incident::is_open)
        .collect())
}

/// Incidents still open or updated in the last 90 days, newest first.
pub fn history(conn: &Connection, cfg: &Config, now: i64) -> Result<Vec<Incident>> {
    Ok(list(conn, cfg)?
        .into_iter()
        .filter(|i| i.is_open() || i.latest().at >= now - 90 * 86_400_000)
        .collect())
}

/// One alert per open incident, resolved when the incident is. Critical when a site it
/// lists is down on the report, so it gets through quiet hours.
pub fn alerts(r: &Report) -> Vec<Alert> {
    r.incidents
        .iter()
        .map(|i| {
            let down = r
                .sites
                .iter()
                .any(|s| s.color == Color::Red && i.sites.contains(&s.node));
            let u = i.latest();
            let stage = stage_word(&u.stage);
            Alert {
                key: format!("incident {}", i.id),
                severity: if down {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                title: i.title.clone(),
                text: if u.text.is_empty() {
                    stage
                } else {
                    format!("{stage}: {}", u.text)
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MIN, NOW, add_as, cfg, cut_off_c, fake, poll, report};
    use ridgewarden_client::alerts::{open_alerts, update};
    use serde_json::json;

    #[test]
    fn an_incident_moves_through_its_stages_and_its_alert_resolves() {
        let mut f = fake();
        poll(&mut f, NOW - 5 * MIN, "a", &["b"], &["a", "b"]);
        let c = cfg();
        let opened = f.add(
            "status.incident",
            json!({"title": "North site unreachable", "stage": "investigating",
                   "sites": ["B.local.mesh"], "text": "Looking into it"}),
            NOW - 20 * MIN,
        );
        f.add(
            "status.incident",
            json!({"incident": opened.id, "stage": "identified", "text": "Feed line at the hilltop"}),
            NOW - 15 * MIN,
        );

        let r = report(&f, &c, "a");
        assert_eq!(r.incidents.len(), 1);
        assert_eq!(r.incidents[0].sites, ["b"]);
        assert_eq!(r.incidents[0].latest().stage, "identified");
        let active = alerts(&r);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].key, format!("incident {}", opened.id));
        assert_eq!(active[0].title, "North site unreachable");
        assert_eq!(active[0].text, "Identified: Feed line at the hilltop");
        assert_eq!(active[0].severity, Severity::Warning);
        update(&f.conn, &active, &|_| false, NOW).unwrap();
        assert_eq!(open_alerts(&f.conn).unwrap().len(), 1);

        f.add(
            "status.incident",
            json!({"incident": opened.id, "stage": "resolved", "text": "Replaced"}),
            NOW - 5 * MIN,
        );
        let r = report(&f, &c, "a");
        assert!(r.incidents.is_empty());
        update(&f.conn, &alerts(&r), &|_| false, NOW + MIN).unwrap();
        assert!(open_alerts(&f.conn).unwrap().is_empty(), "resolved with it");

        let h = history(&f.conn, &c, NOW).unwrap();
        let timeline: Vec<(&str, &str)> = h[0]
            .updates
            .iter()
            .map(|u| (u.stage.as_str(), u.text.as_str()))
            .collect();
        assert_eq!(
            timeline,
            [
                ("investigating", "Looking into it"),
                ("identified", "Feed line at the hilltop"),
                ("resolved", "Replaced")
            ]
        );
        assert!(
            history(&f.conn, &c, NOW + 91 * 24 * 60 * MIN)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn an_incident_on_a_cut_off_site_is_critical() {
        let mut f = fake();
        cut_off_c(&mut f);
        let post = |title: &str, site: &str| json!({"title": title, "stage": "investigating", "sites": [site]});
        f.add("status.incident", post("c is off the air", "c"), NOW - MIN);
        f.add("status.incident", post("Chat is slow", "a"), NOW - MIN);
        let got: Vec<(String, Severity)> = alerts(&report(&f, &cfg(), "a"))
            .into_iter()
            .map(|a| (a.title, a.severity))
            .collect();
        assert_eq!(
            got,
            [
                ("Chat is slow".to_string(), Severity::Warning),
                ("c is off the air".to_string(), Severity::Critical)
            ]
        );
    }

    #[test]
    fn only_listed_stations_can_open_or_update_incidents() {
        let mut f = fake();
        let opened = f.add(
            "status.incident",
            json!({"title": "Hilltop down", "stage": "investigating"}),
            NOW - 3 * MIN,
        );
        let other = "ab".repeat(32);
        add_as(
            &mut f,
            &other,
            1000,
            "status.incident",
            json!({"incident": opened.id, "stage": "resolved"}),
            NOW - 2 * MIN,
        );
        let mut c = cfg();
        c.banner_authors = vec![];
        assert!(
            list(&f.conn, &c).unwrap().is_empty(),
            "nobody may post until someone is listed"
        );
        c.banner_authors = vec![opened.author.key.clone()];
        let still = open(&f.conn, &c).unwrap();
        assert_eq!(still.len(), 1, "the other station's update is ignored");
        assert_eq!(still[0].updates.len(), 1);
        c.banner_authors.push(other.clone());
        assert!(
            open(&f.conn, &c).unwrap().is_empty(),
            "once listed, its update resolves it"
        );
        c.banner_authors = vec![other];
        assert!(
            list(&f.conn, &c).unwrap().is_empty(),
            "nor is an incident opened by a station not on the list"
        );
    }

    #[test]
    fn updates_are_ordered_by_lamport_clock_not_arrival() {
        let mut f = fake();
        let opened = f.add(
            "status.incident",
            json!({"title": "Hilltop down", "stage": "investigating"}),
            NOW - 10 * MIN,
        );
        let key = "ab".repeat(32);
        // the later update arrives first, as it can when sites sync in a different order
        for (lamport, stage) in [(20, "resolved"), (10, "monitoring")] {
            add_as(
                &mut f,
                &key,
                lamport,
                "status.incident",
                json!({"incident": opened.id, "stage": stage}),
                NOW - MIN,
            );
        }
        let mut c = cfg();
        c.banner_authors.push(key);
        let i = &list(&f.conn, &c).unwrap()[0];
        let stages: Vec<&str> = i.updates.iter().map(|u| u.stage.as_str()).collect();
        assert_eq!(stages, ["investigating", "monitoring", "resolved"]);
        assert!(!i.is_open());
    }

    #[test]
    fn malformed_posts_are_skipped() {
        let mut f = fake();
        for p in [
            json!({"title": "No stage"}),
            json!({"title": "Odd stage", "stage": "panicking"}),
            json!({"stage": "investigating"}),
            json!("not an object"),
            json!({"incident": "0".repeat(64), "stage": "resolved"}),
        ] {
            f.add("status.incident", p, NOW - MIN);
        }
        let long = f.add(
            "status.incident",
            json!({"title": "x".repeat(500), "stage": "investigating", "sites": ["ok-node", "<b>"]}),
            NOW - MIN,
        );
        let all = list(&f.conn, &cfg()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, long.id);
        assert_eq!(all[0].title.len(), TITLE_MAX);
        assert_eq!(all[0].sites, ["ok-node"]);
        assert_eq!(all[0].updates.len(), 1);
    }
}
