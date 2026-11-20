//! Planned maintenance notices: what, when, and which sites, posted as
//! `status.maintenance` events by the same stations that may post the banner. The page
//! shows them while they're under way and for 14 days before.

use std::collections::HashMap;

use anyhow::Result;
use ridgewarden_client::Event;
use ridgewarden_client::core::Projection;
use ridgewarden_client::mesh::s;
use ridgewarden_client::util::{rfc3339_to_ms, short_time};
use rusqlite::{Connection, Transaction, params};

use crate::config::Config;
use crate::incidents::{TEXT_MAX, clip, sites};

#[derive(Clone, Debug)]
pub struct Notice {
    /// the id of the event that posted it
    pub id: String,
    pub text: String,
    pub from: i64,
    pub to: i64,
    pub sites: Vec<String>,
    pub by: String,
    pub posted: i64,
    /// when a station allowed to post called it off
    pub cancelled: Option<i64>,
}

impl Notice {
    pub fn active(&self, now: i64) -> bool {
        self.from <= now && now < self.to
    }

    /// "until 2026-11-22 19:00 UTC" while under way, otherwise both ends.
    pub fn when(&self, now: i64) -> String {
        if self.active(now) {
            format!("until {}", short_time(self.to))
        } else {
            format!("{} to {}", short_time(self.from), short_time(self.to))
        }
    }
}

/// `status.maintenance` events: a notice with text, from, to and sites, or
/// `{"cancel": <notice id>}`.
pub struct Maintenance;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS maintenance (
    id      TEXT PRIMARY KEY,
    ts      INTEGER NOT NULL,
    call    TEXT NOT NULL,
    key     TEXT NOT NULL,
    text    TEXT NOT NULL,
    from_ms INTEGER NOT NULL,
    to_ms   INTEGER NOT NULL,
    sites   TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS maintenance_cancels (
    event_id TEXT PRIMARY KEY,
    ts       INTEGER NOT NULL,
    notice   TEXT NOT NULL,
    key      TEXT NOT NULL
);";

impl Projection for Maintenance {
    fn schema(&self) -> &'static str {
        SCHEMA
    }

    fn reset(&self, tx: &Transaction) -> Result<()> {
        tx.execute_batch("DELETE FROM maintenance; DELETE FROM maintenance_cancels;")?;
        Ok(())
    }

    fn apply(&self, tx: &Transaction, e: &Event, ts: i64) -> Result<()> {
        if e.kind != "status.maintenance" {
            return Ok(());
        }
        let p = &e.payload;
        if let Some(notice) = s(p, "cancel") {
            tx.execute(
                "INSERT OR IGNORE INTO maintenance_cancels (event_id, ts, notice, key) VALUES (?1, ?2, ?3, ?4)",
                params![e.id, ts, notice, e.author.key],
            )?;
            return Ok(());
        }
        let text = clip(s(p, "text").unwrap_or(""), TEXT_MAX);
        let from = s(p, "from").and_then(rfc3339_to_ms);
        let to = s(p, "to").and_then(rfc3339_to_ms);
        let (Some(from), Some(to)) = (from, to) else {
            return Ok(());
        };
        if text.is_empty() || to <= from {
            return Ok(());
        }
        tx.execute(
            "INSERT OR IGNORE INTO maintenance (id, ts, call, key, text, from_ms, to_ms, sites)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.id,
                ts,
                e.author.call,
                e.author.key,
                text,
                from,
                to,
                serde_json::to_string(&sites(p))?
            ],
        )?;
        Ok(())
    }
}

/// Notices under way or starting in the next 14 days, soonest first, leaving out
/// cancelled ones.
pub fn current(conn: &Connection, cfg: &Config, now: i64) -> Result<Vec<Notice>> {
    Ok(ending_after(conn, cfg, now)?
        .into_iter()
        .filter(|n| n.cancelled.is_none() && n.from <= now + 14 * 86_400_000)
        .collect())
}

/// Every notice from a station allowed to post that ends after `t`, cancelled or not,
/// soonest first.
pub fn ending_after(conn: &Connection, cfg: &Config, t: i64) -> Result<Vec<Notice>> {
    let mut cancelled: HashMap<String, i64> = HashMap::new();
    let mut st =
        conn.prepare("SELECT notice, key, ts FROM maintenance_cancels ORDER BY ts DESC")?;
    for r in st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get(2)?))
    })? {
        let (notice, key, ts) = r?;
        if cfg.may_post(&key) {
            cancelled.insert(notice, ts);
        }
    }
    let mut st = conn.prepare(
        "SELECT id, key, text, from_ms, to_ms, sites, call, ts FROM maintenance
         WHERE to_ms > ?1 ORDER BY from_ms, id",
    )?;
    let rows = st.query_map(params![t], |r| {
        Ok((
            r.get::<_, String>(1)?,
            r.get::<_, String>(5)?,
            Notice {
                id: r.get(0)?,
                text: r.get(2)?,
                from: r.get(3)?,
                to: r.get(4)?,
                sites: vec![],
                by: r.get(6)?,
                posted: r.get(7)?,
                cancelled: None,
            },
        ))
    })?;
    let mut out = vec![];
    for r in rows {
        let (key, sites, mut n) = r?;
        if cfg.may_post(&key) {
            n.sites = serde_json::from_str(&sites)?;
            n.cancelled = cancelled.get(&n.id).copied();
            out.push(n);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MIN, NOW, cfg, fake, maintenance};
    use serde_json::json;

    const H: i64 = 60 * MIN;
    const DAY: i64 = 24 * H;

    #[test]
    fn notices_show_while_under_way_or_coming_up() {
        let mut f = fake();
        maintenance(&mut f, "last week", NOW - 7 * DAY, NOW - 7 * DAY + H, &[]);
        maintenance(&mut f, "tower work", NOW - H, NOW + H, &["a"]);
        maintenance(&mut f, "generator test", NOW + DAY, NOW + DAY + H, &[]);
        maintenance(
            &mut f,
            "next month",
            NOW + 30 * DAY,
            NOW + 30 * DAY + H,
            &[],
        );
        let called_off = maintenance(&mut f, "called off", NOW + 2 * H, NOW + 3 * H, &[]);
        f.add(
            "status.maintenance",
            json!({"cancel": called_off.id}),
            NOW - MIN,
        );
        let mut c = cfg();
        let texts = |c: &Config, now: i64| -> Vec<String> {
            current(&f.conn, c, now)
                .unwrap()
                .into_iter()
                .map(|n| n.text)
                .collect()
        };
        assert_eq!(texts(&c, NOW), ["tower work", "generator test"]);
        assert!(texts(&c, NOW + 2 * DAY).is_empty());
        assert_eq!(texts(&c, NOW + 20 * DAY), ["next month"]);
        let now = current(&f.conn, &c, NOW).unwrap();
        assert!(now[0].active(NOW) && !now[1].active(NOW));
        c.banner_authors = vec!["0".repeat(64)];
        assert!(texts(&c, NOW).is_empty(), "poster not on the list");
    }
}
