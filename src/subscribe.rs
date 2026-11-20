//! Subscriptions for agency contacts: a message when an incident is opened, updated or
//! resolved, when maintenance is posted or called off, and when a site is cut off and when
//! it is back. Mail goes out through the box's own sendmail, like other alerts; an
//! email-to-SMS gateway is an address that gets one short line; a webhook gets the same
//! JSON as alert webhooks.
//!
//! Each channel gets at most one message a round, with everything new in it, and only so
//! many an hour. News is judged by when this box first saw it, not by the poster's clock,
//! so a station whose clock is behind isn't left out. A channel added later starts with
//! what happens next, not with old news, and one that fails waits longer each time
//! without holding up the others.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Result, bail};
use ridgewarden_client::alerts::{self, AlertCfg, EmailCfg, Severity};
use ridgewarden_client::ui::Color;
use ridgewarden_client::util::{norm_node, short_time};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Sent;
use crate::config::{Config, Contact, SubscriptionsCfg};
use crate::incidents::{self, Incident, stage_word};
use crate::maintenance::{self, Notice};
use crate::status::{Report, label, labels};

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sub_channels (
    target    TEXT PRIMARY KEY,
    since     INTEGER NOT NULL,
    last_ok   INTEGER,
    last_fail INTEGER,
    fails     INTEGER NOT NULL DEFAULT 0,
    next_try  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS sub_sent (
    target TEXT NOT NULL,
    item   TEXT NOT NULL,
    at     INTEGER NOT NULL,
    PRIMARY KEY (target, item)
);
CREATE TABLE IF NOT EXISTS sub_messages (
    target TEXT NOT NULL,
    at     INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS news_seen (
    item  TEXT PRIMARY KEY,
    first INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS site_watch (
    node       TEXT PRIMARY KEY,
    first_red  INTEGER NOT NULL,
    first_poll INTEGER,
    since      INTEGER,
    back_since INTEGER,
    up_at      INTEGER
);";

/// One way of reaching one contact.
pub struct Target<'a> {
    /// "County EOC (email)"; also what its records are kept under
    pub id: String,
    pub contact: &'a Contact,
    /// an SMS gateway: one short line
    pub short: bool,
    pub send: AlertCfg,
}

pub fn targets(s: &SubscriptionsCfg) -> Vec<Target<'_>> {
    let mail = |to: &[String]| AlertCfg {
        email: Some(EmailCfg {
            to: to.to_vec(),
            from: s.from.clone(),
            sendmail: s.sendmail.clone(),
        }),
        ..Default::default()
    };
    let mut out = vec![];
    for c in &s.contacts {
        let mut add = |kind: &str, short, send| {
            out.push(Target {
                id: format!("{} ({kind})", c.name),
                contact: c,
                short,
                send,
            })
        };
        if !c.email.is_empty() {
            add("email", false, mail(&c.email));
        }
        if !c.sms.is_empty() {
            add("sms", true, mail(&c.sms));
        }
        if let Some(w) = &c.webhook {
            let send = AlertCfg {
                webhooks: vec![w.clone()],
                ..Default::default()
            };
            add("webhook", false, send);
        }
    }
    out
}

/// Something worth telling a contact about.
#[derive(Clone, Debug, PartialEq)]
pub struct News {
    pub id: String,
    /// when it happened, by the clock of whoever posted it
    pub at: i64,
    /// when this box first saw it, which decides whether it is new
    pub seen: i64,
    pub topic: &'static str,
    /// the sites it is about; empty for the whole mesh
    pub sites: Vec<String>,
    pub title: String,
    pub text: String,
    /// the whole thing in one line, for SMS
    pub short: String,
    /// only for channels that were sent this first: an all-clear needs its warning
    pub after: Option<String>,
    /// for a site cut off (false) or back (true): its name, so several go in one line
    pub site: Option<(String, bool)>,
}

fn news_item(id: String, at: i64, topic: &'static str, sites: Vec<String>) -> News {
    News {
        id,
        at,
        seen: at,
        topic,
        sites,
        title: String::new(),
        text: String::new(),
        short: String::new(),
        after: None,
        site: None,
    }
}

fn incident_news(cfg: &Config, i: &Incident) -> Vec<News> {
    i.updates
        .iter()
        .enumerate()
        .map(|(k, u)| {
            let stage = stage_word(&u.stage);
            let title = match (k, u.stage.as_str()) {
                (0, _) => format!("Incident: {}", i.title),
                (_, "resolved") => format!("Resolved: {}", i.title),
                _ => format!("{stage}: {}", i.title),
            };
            let what = if u.text.is_empty() {
                format!("{stage}.")
            } else {
                format!("{stage}: {}", u.text)
            };
            let mut text = what.clone();
            if !i.sites.is_empty() {
                text += &format!("\nSites: {}", labels(cfg, &i.sites));
            }
            text += &format!("\nPosted by {} at {}.", u.by, short_time(u.at));
            News {
                short: format!("{title}. {what}"),
                title,
                text,
                ..news_item(
                    format!("incident {}", u.id),
                    u.at,
                    "incidents",
                    i.sites.clone(),
                )
            }
        })
        .collect()
}

fn maintenance_news(cfg: &Config, n: &Notice) -> News {
    let when = format!("{} to {}", short_time(n.from), short_time(n.to));
    let what = cut(&n.text, 100);
    // the title says what the work is, so the body repeats it only when the title is cut
    let mut text = if what == n.text {
        String::new()
    } else {
        format!("{}\n", n.text)
    };
    text += &format!("When: {when}");
    if !n.sites.is_empty() {
        text += &format!("\nSites: {}", labels(cfg, &n.sites));
    }
    text += &format!("\nPosted by {}.", n.by);
    let news = News {
        title: format!("Planned maintenance: {what}"),
        short: format!("Planned maintenance {when}: {}", n.text),
        text,
        ..news_item(
            format!("maintenance {}", n.id),
            n.posted,
            "maintenance",
            n.sites.clone(),
        )
    };
    match n.cancelled {
        None => news,
        Some(at) => News {
            id: format!("maintenance {} cancelled", n.id),
            at,
            seen: at,
            title: format!("Called off: {}", news.title),
            short: format!("Called off: {}", news.short),
            text: format!("This planned maintenance is called off.\n{}", news.text),
            after: Some(news.id),
            ..news
        },
    }
}

/// A site watched because it was seen cut off.
struct Watch {
    node: String,
    since: i64,
    back_since: Option<i64>,
    up_at: Option<i64>,
}

fn site_news(cfg: &Config, r: &Report, w: &Watch) -> News {
    let name = label(cfg, &w.node);
    let id = format!("site {} {}", w.node, w.since);
    let mut news = News {
        title: format!("{name} is cut off from the mesh"),
        text: format!("Cut off since {}.", short_time(w.since)),
        site: Some((name.clone(), false)),
        ..news_item(id.clone(), w.since, "sites", vec![w.node.clone()])
    };
    if let Some(t) = r
        .sites
        .iter()
        .find(|s| s.node == w.node && s.color == Color::Red)
    {
        news.text += &format!(" {}", t.reason);
    }
    if let (Some(up), Some(back)) = (w.up_at, w.back_since) {
        news.id = format!("{id} back");
        news.at = up;
        news.title = format!("{name} is back on the mesh");
        news.text = format!(
            "Reachable again since {}, after being cut off from {}.",
            short_time(back),
            short_time(w.since)
        );
        news.after = Some(id);
        news.site = Some((name, true));
    }
    news.short = format!("{}. {}", news.title, news.text);
    news
}

/// Keep track of which sites are cut off. A site counts as cut off once it is red on a
/// second host list or for 10 minutes, so one bad poll warns nobody; it counts as back once
/// it has been seen reachable for 15 minutes, so a flapping link sends one warning and one
/// all-clear. Unknown says nothing either way, and time spent unknown isn't time seen up.
pub fn track(conn: &Connection, r: &Report, now: i64) -> Result<()> {
    type Row = (i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>);
    for s in &r.sites {
        let row: Option<Row> = conn
            .query_row(
                "SELECT first_red, first_poll, since, back_since, up_at FROM site_watch WHERE node = ?1",
                [&s.node],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let set = |sql: &str| conn.execute(sql, params![s.node, now]);
        match (s.color, row) {
            (Color::Red, None | Some((.., Some(_)))) => conn.execute(
                "INSERT OR REPLACE INTO site_watch (node, first_red, first_poll) VALUES (?1, ?2, ?3)",
                params![s.node, now, r.viewpoint],
            )?,
            (Color::Red, Some((first, poll, None, _, None)))
                if poll != r.viewpoint || now - first >= 10 * MIN =>
            {
                conn.execute(
                    "UPDATE site_watch SET since = first_red WHERE node = ?1",
                    [&s.node],
                )?
            }
            (Color::Red, Some((_, _, Some(_), Some(_), None)))
            | (Color::Unknown, Some((_, _, Some(_), Some(_), None))) => conn.execute(
                "UPDATE site_watch SET back_since = NULL WHERE node = ?1",
                [&s.node],
            )?,
            (Color::Red | Color::Unknown, _) => 0,
            // reachable again before anyone was warned: it was a blip
            (_, Some((_, _, None, _, None))) => {
                conn.execute("DELETE FROM site_watch WHERE node = ?1", [&s.node])?
            }
            (_, Some((_, _, Some(_), None, None))) => {
                set("UPDATE site_watch SET back_since = ?2 WHERE node = ?1")?
            }
            (_, Some((_, _, Some(_), Some(b), None))) if now - b >= 15 * MIN => {
                set("UPDATE site_watch SET up_at = ?2 WHERE node = ?1")?
            }
            _ => 0,
        };
    }
    conn.execute(
        "DELETE FROM site_watch WHERE up_at < ?1",
        params![now - 2 * DAY],
    )?;
    Ok(())
}

/// Everything worth telling someone about, each with when this box first saw it.
pub fn news(conn: &Connection, cfg: &Config, r: &Report, now: i64) -> Result<Vec<News>> {
    let mut out: Vec<News> = vec![];
    for i in incidents::list(conn, cfg)? {
        out.extend(incident_news(cfg, &i));
    }
    for n in maintenance::ending_after(conn, cfg, now - DAY)? {
        out.push(maintenance_news(cfg, &n));
    }
    let mut st = conn
        .prepare("SELECT node, since, back_since, up_at FROM site_watch WHERE since IS NOT NULL")?;
    let watched = st.query_map([], |r| {
        Ok(Watch {
            node: r.get(0)?,
            since: r.get(1)?,
            back_since: r.get(2)?,
            up_at: r.get(3)?,
        })
    })?;
    for w in watched {
        out.push(site_news(cfg, r, &w?));
    }
    for n in &mut out {
        conn.execute(
            "INSERT OR IGNORE INTO news_seen (item, first) VALUES (?1, ?2)",
            params![n.id, now],
        )?;
        n.seen = conn.query_row(
            "SELECT first FROM news_seen WHERE item = ?1",
            [&n.id],
            |r| r.get(0),
        )?;
    }
    out.sort_by_key(|n| n.seen);
    Ok(out)
}

/// What `ridgewarden-status subscribers test` sends.
pub fn test_news(cfg: &Config, now: i64) -> News {
    News {
        title: "Test message".into(),
        text: format!(
            "A test from {}. News about the mesh you signed up for will arrive like this.",
            cfg.title
        ),
        short: format!("Test message from {}.", cfg.title),
        ..news_item("test".into(), now, "test", vec![])
    }
}

fn wanted(c: &Contact, n: &News) -> bool {
    c.topics.iter().any(|t| t == n.topic)
        && (c.sites.is_empty()
            || n.sites.is_empty()
            || c.sites.iter().any(|s| n.sites.contains(&norm_node(s))))
}

fn sent(conn: &Connection, target: &str, item: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sub_sent WHERE target = ?1 AND item = ?2)",
        params![target, item],
        |r| r.get(0),
    )?)
}

/// What each channel should be sent this round: (target index, news indexes). A channel
/// that is backing off after a failure, or has had its messages for the hour, gets nothing
/// now; its news waits and goes out together later. Nothing first seen over a day ago goes.
pub fn due(
    conn: &Connection,
    targets: &[Target],
    news: &[News],
    per_hour: u32,
    now: i64,
) -> Result<Vec<(usize, Vec<usize>)>> {
    let mut out = vec![];
    for (ti, t) in targets.iter().enumerate() {
        conn.execute(
            "INSERT OR IGNORE INTO sub_channels (target, since) VALUES (?1, ?2)",
            params![t.id, now],
        )?;
        let (since, next_try): (i64, i64) = conn.query_row(
            "SELECT since, next_try FROM sub_channels WHERE target = ?1",
            [&t.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let this_hour: u32 = conn.query_row(
            "SELECT count(*) FROM sub_messages WHERE target = ?1 AND at > ?2",
            params![t.id, now - HOUR],
            |r| r.get(0),
        )?;
        if next_try > now || this_hour >= per_hour {
            continue;
        }
        let mut items = vec![];
        for (ni, n) in news.iter().enumerate() {
            if n.seen > since
                && n.seen >= now - DAY
                && wanted(t.contact, n)
                && !sent(conn, &t.id, &n.id)?
                && match &n.after {
                    Some(a) => sent(conn, &t.id, a)?,
                    None => true,
                }
            {
                items.push(ni);
            }
        }
        if !items.is_empty() {
            out.push((ti, items));
        }
    }
    Ok(out)
}

/// Several pieces of news as one message: (subject, body, one line). Sites cut off and
/// sites back are each named together, "3 sites cut off: A, B, C".
pub fn digest(items: &[&News]) -> (String, String, String) {
    if let [one] = items {
        return (one.title.clone(), one.text.clone(), one.short.clone());
    }
    let mut parts = vec![];
    for (back, what) in [(false, "cut off"), (true, "back on the mesh")] {
        let names: Vec<&str> = items
            .iter()
            .filter_map(|n| n.site.as_ref().filter(|s| s.1 == back))
            .map(|s| s.0.as_str())
            .collect();
        match names.len() {
            0 => {}
            1 => parts.push(format!("{} is {what}", names[0])),
            k => parts.push(format!("{k} sites {what}: {}", names.join(", "))),
        }
    }
    parts.extend(
        items
            .iter()
            .filter(|n| n.site.is_none())
            .map(|n| n.title.clone()),
    );
    let title = parts.join("; ");
    let text = items
        .iter()
        .map(|n| format!("{}\n{}", n.title, n.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    (title.clone(), text, title)
}

/// `s` cut to `max` characters at a space, marked with "..." when anything was cut.
fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(3)).collect();
    let head = match head.rfind(' ') {
        Some(i) if i > max / 2 => &head[..i],
        _ => &head,
    };
    format!("{}...", head.trim_end())
}

/// A message ready to go: through the alert channels, or as mail to an SMS gateway.
pub enum Message {
    Alert(Box<AlertCfg>, alerts::Notice),
    Mail(EmailCfg, String),
}

/// The message one channel gets for these items.
pub fn message(cfg: &Config, t: &Target, items: &[&News]) -> Message {
    let (title, text, short) = digest(items);
    if let (true, Some(e)) = (t.short, &t.send.email) {
        // some gateways show only the body, others the subject too, so the line goes in both
        let body = cut(&short, 140);
        let mail = alerts::email_message(e, &cfg.title, &cut(&short, 60), &body, None);
        return Message::Mail(e.clone(), mail);
    }
    let text = match &cfg.subscriptions.link {
        Some(l) => format!("{text}\n\nThe status page: {l}"),
        None => text,
    };
    let notice = alerts::Notice {
        key: items
            .iter()
            .map(|n| n.id.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        state: "open",
        severity: Severity::Warning,
        title,
        text,
        opened_ms: items.iter().map(|n| n.at).min().unwrap_or_default(),
    };
    Message::Alert(Box::new(t.send.clone()), notice)
}

/// Send one message, giving up after 30 seconds. `app` names the page in subject lines.
pub async fn send(http: &reqwest::Client, app: &str, m: Message) -> Result<()> {
    let go = async {
        match &m {
            Message::Alert(cfg, n) => alerts::send(http, cfg, app, n, "").await,
            Message::Mail(e, mail) => alerts::send_email(e, mail).await,
        }
    };
    match tokio::time::timeout(Duration::from_secs(30), go).await {
        Ok(r) => r,
        Err(_) => bail!("timed out"),
    }
}

fn record_ok(conn: &Connection, target: &str, items: &[String], now: i64) -> Result<()> {
    for i in items {
        conn.execute(
            "INSERT OR IGNORE INTO sub_sent (target, item, at) VALUES (?1, ?2, ?3)",
            params![target, i, now],
        )?;
    }
    conn.execute(
        "INSERT INTO sub_messages (target, at) VALUES (?1, ?2)",
        params![target, now],
    )?;
    conn.execute(
        "UPDATE sub_channels SET last_ok = ?2, fails = 0, next_try = 0 WHERE target = ?1",
        params![target, now],
    )?;
    Ok(())
}

/// A failed channel waits a minute before the next try, then twice as long each time it
/// fails again, up to an hour.
fn record_fail(conn: &Connection, target: &str, now: i64) -> Result<()> {
    let fails: u32 = conn.query_row(
        "SELECT fails FROM sub_channels WHERE target = ?1",
        [target],
        |r| r.get(0),
    )?;
    let wait = (MIN << fails.min(6)).min(HOUR);
    conn.execute(
        "UPDATE sub_channels SET last_fail = ?2, fails = fails + 1, next_try = ?3 WHERE target = ?1",
        params![target, now, now + wait],
    )?;
    Ok(())
}

/// One round: note which sites are cut off, then send every channel what it hasn't had,
/// all at once, so one dead gateway holds up nobody else. Returns how many messages went.
pub async fn round(
    http: &reqwest::Client,
    db: &std::sync::Mutex<Connection>,
    cfg: &Config,
    r: &Report,
    now: i64,
) -> Result<usize> {
    let targets = targets(&cfg.subscriptions);
    let lock = || db.lock().unwrap_or_else(|e| e.into_inner());
    let (news, plan) = {
        let c = lock();
        track(&c, r, now)?;
        let news = news(&c, cfg, r, now)?;
        let plan = due(&c, &targets, &news, cfg.subscriptions.max_per_hour, now)?;
        c.execute(
            "DELETE FROM sub_messages WHERE at < ?1",
            params![now - 2 * HOUR],
        )?;
        c.execute(
            "DELETE FROM sub_sent WHERE at < ?1",
            params![now - 90 * DAY],
        )?;
        c.execute(
            "DELETE FROM news_seen WHERE first < ?1",
            params![now - 90 * DAY],
        )?;
        (news, plan)
    };
    let mut jobs = tokio::task::JoinSet::new();
    for (ti, items) in plan {
        let picked: Vec<&News> = items.iter().map(|&i| &news[i]).collect();
        let m = message(cfg, &targets[ti], &picked);
        let ids: Vec<String> = picked.iter().map(|n| n.id.clone()).collect();
        let (http, app, target) = (http.clone(), cfg.title.clone(), targets[ti].id.clone());
        jobs.spawn(async move { (target, ids, send(&http, &app, m).await) });
    }
    let mut n = 0;
    while let Some(done) = jobs.join_next().await {
        let (target, ids, r) = done?;
        match r {
            Ok(()) => {
                record_ok(&lock(), &target, &ids, now)?;
                n += 1;
            }
            Err(e) => {
                tracing::warn!("message to {target} not sent: {e:#}");
                record_fail(&lock(), &target, now)?;
            }
        }
    }
    Ok(n)
}

/// When each channel last delivered and last failed, for the about page.
pub fn health(conn: &Connection) -> Result<HashMap<String, Sent>> {
    let mut st = conn.prepare("SELECT target, last_ok, last_fail FROM sub_channels")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get(0)?,
            Sent {
                ok: r.get(1)?,
                failed: r.get(2)?,
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{NOW, cfg, cut_off_c, fake, maintenance, poll, report};
    use ridgewarden_client::mesh::testing::Fake;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn contact(name: &str, topics: &[&str], sites: &[&str]) -> Contact {
        Contact {
            name: name.into(),
            email: vec![],
            sms: vec![],
            webhook: Some("http://127.0.0.1:9/hook".into()),
            topics: topics.iter().map(|t| t.to_string()).collect(),
            sites: sites.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The channels exist from `at` on.
    fn start(f: &Fake, c: &Config, at: i64) {
        due(&f.conn, &targets(&c.subscriptions), &[], 99, at).unwrap();
    }

    /// One round with every message delivered: who got which message.
    fn send(f: &Fake, c: &Config, r: &Report, now: i64) -> Vec<(String, String)> {
        track(&f.conn, r, now).unwrap();
        let news = news(&f.conn, c, r, now).unwrap();
        let t = targets(&c.subscriptions);
        let mut got = vec![];
        for (ti, items) in due(&f.conn, &t, &news, c.subscriptions.max_per_hour, now).unwrap() {
            let picked: Vec<&News> = items.iter().map(|&i| &news[i]).collect();
            let ids: Vec<String> = picked.iter().map(|n| n.id.clone()).collect();
            record_ok(&f.conn, &t[ti].id, &ids, now).unwrap();
            got.push((t[ti].contact.name.clone(), digest(&picked).0));
        }
        got.sort();
        got
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    fn set(r: &mut Report, node: &str, color: Color) {
        r.sites.iter_mut().find(|s| s.node == node).unwrap().color = color;
    }

    #[test]
    fn each_contact_hears_what_they_signed_up_for_once() {
        let mut f = fake();
        poll(&mut f, NOW - 5 * MIN, "a", &["b"], &["a", "b"]);
        poll(&mut f, NOW - 5 * MIN, "b", &["a"], &["a", "b"]);
        let mut c = cfg();
        c.subscriptions.contacts = vec![
            contact("All news", &["incidents", "maintenance", "sites"], &[]),
            contact("Maintenance only", &["maintenance"], &[]),
            contact("Another site", &["incidents"], &["tst-other"]),
        ];
        start(&f, &c, NOW - 60 * MIN);
        let opened = f.add(
            "status.incident",
            json!({"title": "B is down", "stage": "investigating", "sites": ["b"]}),
            NOW - 3 * MIN,
        );
        let work = maintenance(&mut f, "Tower work", NOW + 60 * MIN, NOW + 120 * MIN, &[]);
        let r = report(&f, &c, "a");
        assert_eq!(
            send(&f, &c, &r, NOW),
            pairs(&[
                (
                    "All news",
                    "Incident: B is down; Planned maintenance: Tower work"
                ),
                ("Maintenance only", "Planned maintenance: Tower work"),
            ]),
            "one message each, with everything new in it"
        );
        assert!(send(&f, &c, &r, NOW + MIN).is_empty(), "nothing twice");

        // someone added now hears what happens next, not what already went out
        c.subscriptions
            .contacts
            .push(contact("Late", &["incidents", "maintenance"], &[]));
        start(&f, &c, NOW + 2 * MIN);
        f.add(
            "status.incident",
            json!({"incident": opened.id, "stage": "resolved", "text": "Fixed"}),
            NOW + 3 * MIN,
        );
        f.add(
            "status.maintenance",
            json!({"cancel": work.id}),
            NOW + 3 * MIN,
        );
        assert_eq!(
            send(&f, &c, &r, NOW + 4 * MIN),
            pairs(&[
                (
                    "All news",
                    "Resolved: B is down; Called off: Planned maintenance: Tower work"
                ),
                ("Late", "Resolved: B is down"),
                (
                    "Maintenance only",
                    "Called off: Planned maintenance: Tower work"
                ),
            ])
        );
    }

    #[test]
    fn news_from_a_station_whose_clock_is_behind_still_goes_out() {
        let mut f = fake();
        let mut c = cfg();
        c.subscriptions.contacts = vec![contact("EOC", &["incidents"], &[])];
        start(&f, &c, NOW - MIN);
        // its own clock says two days ago, but this box only now saw it
        f.add(
            "status.incident",
            json!({"title": "Relay down", "stage": "investigating"}),
            NOW - 2 * DAY,
        );
        let r = report(&f, &c, "a");
        assert_eq!(
            send(&f, &c, &r, NOW),
            pairs(&[("EOC", "Incident: Relay down")])
        );
    }

    #[test]
    fn a_site_warning_waits_for_a_second_host_list_or_ten_minutes() {
        let mut f = fake();
        cut_off_c(&mut f);
        let mut c = cfg();
        c.subscriptions.contacts = vec![contact("Sites", &["sites"], &["C"])];
        start(&f, &c, NOW - 60 * MIN);
        let mut r = report(&f, &c, "a");
        r.viewpoint = Some(1);
        assert!(
            send(&f, &c, &r, NOW).is_empty(),
            "one poll could be a glitch"
        );
        set(&mut r, "c", Color::Green);
        assert!(send(&f, &c, &r, NOW + MIN).is_empty(), "and it was");
        set(&mut r, "c", Color::Red);
        assert!(send(&f, &c, &r, NOW + 2 * MIN).is_empty());
        assert!(
            send(&f, &c, &r, NOW + 5 * MIN).is_empty(),
            "the same poll again"
        );
        r.viewpoint = Some(2);
        assert_eq!(
            send(&f, &c, &r, NOW + 6 * MIN),
            pairs(&[("Sites", "c is cut off from the mesh")]),
            "a second host list agrees"
        );

        // without a host list to compare, ten minutes of red will do
        let mut f = fake();
        cut_off_c(&mut f);
        start(&f, &c, NOW - 60 * MIN);
        let r = report(&f, &c, "a");
        assert!(send(&f, &c, &r, NOW).is_empty());
        assert!(send(&f, &c, &r, NOW + 9 * MIN).is_empty());
        assert_eq!(send(&f, &c, &r, NOW + 10 * MIN).len(), 1);
    }

    #[test]
    fn an_all_clear_waits_until_the_site_has_been_seen_up_for_15_minutes() {
        let mut f = fake();
        cut_off_c(&mut f);
        let mut c = cfg();
        c.subscriptions.contacts = vec![contact("Sites", &["sites"], &[])];
        start(&f, &c, NOW - 60 * MIN);
        let mut r = report(&f, &c, "a");
        send(&f, &c, &r, NOW);
        assert_eq!(send(&f, &c, &r, NOW + 10 * MIN).len(), 1, "warned");

        set(&mut r, "c", Color::Green);
        assert!(send(&f, &c, &r, NOW + 11 * MIN).is_empty());
        set(&mut r, "c", Color::Red);
        assert!(send(&f, &c, &r, NOW + 12 * MIN).is_empty(), "flapped");
        set(&mut r, "c", Color::Green);
        assert!(send(&f, &c, &r, NOW + 13 * MIN).is_empty());
        set(&mut r, "c", Color::Unknown);
        assert!(send(&f, &c, &r, NOW + 20 * MIN).is_empty());
        set(&mut r, "c", Color::Green);
        assert!(
            send(&f, &c, &r, NOW + 29 * MIN).is_empty(),
            "time it wasn't seen doesn't count"
        );
        c.subscriptions
            .contacts
            .push(contact("Late", &["sites"], &[]));
        start(&f, &c, NOW + 30 * MIN);
        assert!(send(&f, &c, &r, NOW + 40 * MIN).is_empty());
        assert_eq!(
            send(&f, &c, &r, NOW + 44 * MIN),
            pairs(&[("Sites", "c is back on the mesh")]),
            "only to whoever was told it was cut off"
        );
        assert!(send(&f, &c, &r, NOW + 45 * MIN).is_empty());
    }

    #[test]
    fn several_sites_cut_off_in_one_round_go_in_one_message() {
        let mut f = fake();
        cut_off_c(&mut f);
        let mut c = cfg();
        c.subscriptions.contacts = vec![contact("Sites", &["sites"], &[])];
        start(&f, &c, NOW - 60 * MIN);
        let mut r = report(&f, &c, "a");
        let mut more = r.sites[0].clone();
        for node in ["d", "e"] {
            more.node = node.into();
            more.label = node.into();
            more.color = Color::Red;
            r.sites.push(more.clone());
        }
        send(&f, &c, &r, NOW);
        assert_eq!(
            send(&f, &c, &r, NOW + 10 * MIN),
            pairs(&[("Sites", "3 sites cut off: c, d, e")])
        );
    }

    #[test]
    fn a_message_with_several_things_in_it_says_each_once() {
        let mut f = fake();
        let c = cfg();
        f.add(
            "status.incident",
            json!({"title": "B is down", "stage": "investigating", "sites": ["b"]}),
            NOW - 3 * MIN,
        );
        let long = "Tower work at the hilltop. The crew swaps the feed line on the north \
                    sector, so the North site drops for most of the morning.";
        maintenance(&mut f, "Antenna swap", NOW + 60 * MIN, NOW + 90 * MIN, &[]);
        maintenance(&mut f, long, NOW + 120 * MIN, NOW + 180 * MIN, &[]);
        let r = report(&f, &c, "a");
        let news = news(&f.conn, &c, &r, NOW).unwrap();
        let (title, text, _) = digest(&news.iter().collect::<Vec<_>>());
        assert_eq!(text.matches("B is down").count(), 1, "{text}");
        assert_eq!(text.matches("Antenna swap").count(), 1, "{text}");
        assert!(!title.contains(long), "a long notice is cut in the title");
        assert!(text.contains(long), "and given whole in the body: {text}");
    }

    #[test]
    fn a_channel_gets_so_many_messages_an_hour_and_the_rest_together() {
        let mut f = fake();
        let mut c = cfg();
        c.subscriptions.max_per_hour = 2;
        c.subscriptions.contacts = vec![contact("EOC", &["incidents"], &[])];
        start(&f, &c, NOW - 60 * MIN);
        let r = report(&f, &c, "a");
        let mut got = 0;
        for (k, title) in ["One", "Two", "Three", "Four"].into_iter().enumerate() {
            let t = NOW + k as i64 * MIN;
            f.add(
                "status.incident",
                json!({"title": title, "stage": "investigating"}),
                t,
            );
            got += send(&f, &c, &r, t).len();
        }
        assert_eq!(got, 2, "the hour's limit");
        assert_eq!(
            send(&f, &c, &r, NOW + 61 * MIN),
            pairs(&[("EOC", "Incident: Three; Incident: Four")]),
            "what waited goes out in one message"
        );
    }

    #[test]
    fn an_sms_gets_one_line_in_the_body_cut_at_a_word() {
        let mut c = cfg();
        c.subscriptions.from = "status@example.org".into();
        let mut eoc = contact("EOC", &["sites"], &[]);
        eoc.webhook = None;
        eoc.sms = vec!["5550100@sms.example.net".into()];
        c.subscriptions.contacts = vec![eoc];
        let t = targets(&c.subscriptions);
        let mut n = test_news(&c, NOW);
        n.short = "word ".repeat(60);
        let Message::Mail(_, mail) = message(&c, &t[0], &[&n]) else {
            panic!("an sms goes as mail");
        };
        let body = mail.split("\r\n\r\n").nth(1).unwrap().trim_end();
        assert!(body.ends_with("word..."), "{body}");
        assert!(body.chars().count() <= 140);
    }

    #[tokio::test]
    async fn a_dead_gateway_backs_off_without_holding_up_the_others() {
        let got: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
        let hook = axum::routing::post({
            let got = got.clone();
            move |axum::Json(v): axum::Json<serde_json::Value>| {
                let got = got.clone();
                async move { got.lock().unwrap().push(v) }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, axum::Router::new().route("/hook", hook))
                .await
                .unwrap()
        });

        let mut f = fake();
        let mut c = cfg();
        c.subscriptions.link = Some("https://status.example.org/".into());
        let mut live = contact("EOC", &["incidents"], &[]);
        live.webhook = Some(format!("http://{addr}/hook"));
        // nothing listens on the discard port
        let dead = contact("Gone", &["incidents"], &[]);
        c.subscriptions.contacts = vec![dead, live];
        start(&f, &c, NOW - 60 * MIN);
        f.add(
            "status.incident",
            json!({"title": "B is down", "stage": "identified", "text": "Feed line", "sites": ["b"]}),
            NOW - 3 * MIN,
        );
        let r = report(&f, &c, "a");
        let db = std::sync::Mutex::new(f.conn);
        let http = reqwest::Client::new();
        assert_eq!(round(&http, &db, &c, &r, NOW).await.unwrap(), 1);
        {
            let got = got.lock().unwrap();
            assert_eq!(got.len(), 1);
            assert_eq!(got[0]["title"], "Incident: B is down");
            let text = got[0]["text"].as_str().unwrap();
            assert!(
                text.starts_with("Identified: Feed line\nSites: b"),
                "{text}"
            );
            assert!(text.ends_with("https://status.example.org/"), "{text}");
        }
        let gone = |db: &std::sync::Mutex<Connection>| -> (i64, i64) {
            db.lock()
                .unwrap()
                .query_row(
                    "SELECT fails, next_try FROM sub_channels WHERE target = ?1",
                    ["Gone (webhook)"],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap()
        };
        assert_eq!(gone(&db), (1, NOW + MIN), "waits a minute");
        assert_eq!(round(&http, &db, &c, &r, NOW + 30_000).await.unwrap(), 0);
        assert_eq!(gone(&db).0, 1, "not tried again yet");
        assert_eq!(round(&http, &db, &c, &r, NOW + MIN).await.unwrap(), 0);
        assert_eq!(gone(&db), (2, NOW + 3 * MIN), "then twice as long");
        assert_eq!(
            got.lock().unwrap().len(),
            1,
            "the live one isn't sent it twice"
        );
    }
}
