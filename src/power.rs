//! Power at each site, from a sitemon hub (github.com/Londopy/sitemon). sitemon's agent at a
//! site measures the supply and reports to its hub, whose rules judge it; this reads the
//! hub's JSON (`/api/groups`, then `/api/group/<slug>`) for the sites mapped to nodes. The
//! hub's answers aren't signed, so its text is cleaned and cut short before a tile shows it.
//! A reading counts as current only while it is fresh.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Url;
use ridgewarden_client::util::{human_duration, norm_node};
use serde_json::Value;

use crate::Sent;
use crate::config::PowerCfg;

/// The hub's latest word on each mapped site, by node name.
pub type Readings = BTreeMap<String, Reading>;

#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// the value with its unit, e.g. "12.6 V"
    pub text: String,
    /// when the site took it
    pub at: i64,
    /// the hub's open power alerts for the site, in its own words, cleaned and cut short
    pub alarms: Vec<String>,
}

/// What a site's tile says about its power.
#[derive(Clone, Debug, PartialEq)]
pub enum Power {
    Reading(Reading),
    /// the last reading is older than `fresh_s`
    Stale(Reading),
    /// read from the hub, but it has given no reading for the site
    Unknown,
}

impl Power {
    /// Whether the hub's last word on the site, fresh or not, was a power alarm.
    pub fn alarmed(&self) -> bool {
        matches!(self, Power::Reading(r) | Power::Stale(r) if !r.alarms.is_empty())
    }

    /// The line under the site's tile at `now`, e.g. "Power 12.1 V: Battery low: 12.06 V...".
    pub fn line(&self, now: i64) -> String {
        let (r, stale) = match self {
            Power::Reading(r) => (r, false),
            Power::Stale(r) => (r, true),
            Power::Unknown => return "Power unknown: no recent reading.".into(),
        };
        let ago = human_duration(now - r.at);
        if stale && r.alarms.is_empty() {
            return format!("Power unknown: last reading {ago} ago.");
        }
        let mut s = format!("Power {}", r.text);
        if !r.alarms.is_empty() {
            s = format!("{s}: {}", r.alarms.join("; "));
        }
        if stale {
            s = format!("{s} (last reading {ago} ago)");
        }
        if !s.ends_with('.') {
            s.push('.');
        }
        s
    }
}

/// What `node`'s tile says about its power, or None when it isn't read.
pub fn of(
    cfg: &PowerCfg,
    readings: &Readings,
    node: &str,
    now: i64,
    fresh_ms: i64,
) -> Option<Power> {
    if !cfg.watches(node) {
        return None;
    }
    Some(match readings.get(node) {
        Some(r) if r.at >= now - fresh_ms => Power::Reading(r.clone()),
        Some(r) => Power::Stale(r.clone()),
        None => Power::Unknown,
    })
}

/// What the app knows from the hub, for the tiles and the about page.
#[derive(Clone, Default)]
pub struct Monitor {
    pub readings: Readings,
    /// when a read of the hub last worked, and when one last failed
    pub read: Sent,
    pub gaps: Gaps,
}

/// Mapped sites the hub gives no reading for, by sitemon id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gaps {
    pub unlisted: BTreeSet<String>,
    /// listed, but without a number for the metric
    pub no_metric: BTreeSet<String>,
}

impl Gaps {
    pub fn is_empty(&self) -> bool {
        self.unlisted.is_empty() && self.no_metric.is_empty()
    }

    /// e.g. "Not listed by the hub: hilltop. Listed without batt.v: ridge."
    pub fn words(&self, metric: &str) -> String {
        let list = |s: &BTreeSet<String>| s.iter().cloned().collect::<Vec<_>>().join(", ");
        let mut out = vec![];
        if !self.unlisted.is_empty() {
            out.push(format!("Not listed by the hub: {}.", list(&self.unlisted)));
        }
        if !self.no_metric.is_empty() {
            out.push(format!(
                "Listed without {metric}: {}.",
                list(&self.no_metric)
            ));
        }
        out.join(" ")
    }
}

// the hub's answers are a few kB a site; anything far bigger isn't the hub
const BODY_MAX: usize = 4 << 20;
// a hub listing more groups than this is broken, or wants to keep us busy
const GROUPS_MAX: usize = 50;
// what a tile has room for
const TEXT_MAX: usize = 24;

/// One read of the hub, merged into `prev`. A group that fails is logged and keeps its
/// sites' old readings; only a hub that can't list its groups fails the read. The gaps are
/// None when a group failed, since its sites can't be told apart from unlisted ones.
pub async fn fetch(
    http: &reqwest::Client,
    hub: &str,
    cfg: &PowerCfg,
    now: i64,
    prev: &Readings,
) -> Result<(Readings, Option<Gaps>)> {
    let groups = get(http, endpoint(hub, &["api", "groups"])?).await?;
    let mut readings = prev.clone();
    let (mut listed, mut measured) = (BTreeSet::new(), BTreeSet::new());
    let mut complete = true;
    let slugs = groups
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| g["slug"].as_str())
        .take(GROUPS_MAX);
    for slug in slugs {
        let group = match get(http, endpoint(hub, &["api", "group", slug])?).await {
            Ok(g) => g,
            Err(e) => {
                let slug: String = slug.chars().take(64).collect();
                tracing::warn!("site monitor: group {slug:?} kept its old readings: {e:#}");
                complete = false;
                continue;
            }
        };
        for (id, node, reading) in read_group(cfg, &group, now) {
            if let Some(r) = reading {
                readings.insert(node, r);
                measured.insert(id.clone());
            }
            listed.insert(id);
        }
    }
    let gaps = complete.then(|| Gaps {
        unlisted: cfg
            .sites
            .keys()
            .filter(|id| !listed.contains(*id))
            .cloned()
            .collect(),
        no_metric: listed.difference(&measured).cloned().collect(),
    });
    Ok((readings, gaps))
}

fn endpoint(hub: &str, path: &[&str]) -> Result<Url> {
    let mut url = Url::parse(hub).with_context(|| format!("power.hub {hub:?}"))?;
    url.path_segments_mut()
        .map_err(|()| anyhow!("power.hub {hub:?} can't have a path"))?
        .pop_if_empty()
        .extend(path);
    Ok(url)
}

async fn get(http: &reqwest::Client, url: Url) -> Result<Value> {
    let mut res = http
        .get(url.clone())
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("fetching {url}"))?;
    let mut body = vec![];
    while let Some(chunk) = res
        .chunk()
        .await
        .with_context(|| format!("reading {url}"))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > BODY_MAX {
            bail!("{url} sent over {} MB", BODY_MAX >> 20);
        }
    }
    serde_json::from_slice(&body).with_context(|| format!("{url} didn't send JSON"))
}

/// The mapped sites in one `/api/group/<slug>` answer, as (sitemon id, node, reading). A
/// site without a number for the metric has no reading.
fn read_group(cfg: &PowerCfg, group: &Value, now: i64) -> Vec<(String, String, Option<Reading>)> {
    let mut out = vec![];
    for site in group["sites"].as_array().into_iter().flatten() {
        let Some((id, node)) = site["id"]
            .as_str()
            .and_then(|id| cfg.sites.get_key_value(id))
        else {
            continue;
        };
        let m = &site["latest"][cfg.metric.as_str()];
        let reading = match (m["value"].as_f64(), m["age_s"].as_f64()) {
            (Some(value), Some(age)) => Some(Reading {
                text: clean(&format!("{value:.1}{}", unit(&cfg.metric)), TEXT_MAX),
                at: now - (age.max(0.0) * 1000.0) as i64,
                alarms: alarms(cfg, site),
            }),
            _ => None,
        };
        out.push((id.clone(), norm_node(node), reading));
    }
    out
}

/// The site's open alerts from the power rules, in the hub's words.
fn alarms(cfg: &PowerCfg, site: &Value) -> Vec<String> {
    site["alerts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| {
            a["rule"]
                .as_str()
                .and_then(|r| r.strip_prefix("rule:"))
                .is_some_and(|r| cfg.rules.iter().any(|p| p == r))
        })
        .filter_map(|a| a["message"].as_str())
        .map(|m| clean(m, TEXT_MAX))
        .filter(|m| !m.is_empty())
        .take(5)
        .collect()
}

/// Text from the hub fit for a tile: without control or bidi characters, with single
/// spaces, and cut at a word to at most `max` characters.
fn clean(text: &str, max: usize) -> String {
    let kept: String = text
        .chars()
        .filter(|&c| c.is_whitespace() || !(c.is_control() || bidi(c)))
        .collect();
    let text = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    let cut: String = text.chars().take(max - 3).collect();
    let cut = cut.rfind(' ').map_or(cut.as_str(), |i| &cut[..i]);
    format!("{}...", cut.trim_end_matches([':', ';', ',', '(']))
}

/// Characters that reorder the text around them, so a tile could seem to say something else.
fn bidi(c: char) -> bool {
    matches!(
        c,
        '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// sitemon's power sensors report volts, amps and watts as `<name>.v`, `.a` and `.w`.
fn unit(metric: &str) -> &'static str {
    match metric.rsplit('.').next() {
        Some("v") => " V",
        Some("a") => " A",
        Some("w") => " W",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000_000;
    const MIN: i64 = 60_000;

    fn cfg() -> PowerCfg {
        let mut c = PowerCfg {
            hub: Some("http://127.0.0.1:9/".into()),
            ..Default::default()
        };
        c.sites
            .insert("hilltop".into(), "TST-Hilltop.local.mesh".into());
        c.sites.insert("ridge".into(), "tst-north".into());
        c
    }

    /// The shape of sitemon 0.4's /api/group/<slug>, cut down from its demo hub's answer.
    fn group() -> Value {
        json!({"slug": "demo", "name": "Demo sites", "version": "0.1.0", "sites": [
            {"id": "hilltop", "name": "Hilltop", "group": "demo", "status": "warn", "last_contact_s": 1,
             "latest": {"batt.v": {"value": 12.599, "age_s": 1}, "batt.a": {"value": 2.045, "age_s": 1},
                        "door.open": {"value": true, "age_s": 21}},
             "alerts": [{"rule": "rule:Door open", "severity": "warn", "message": "Door open", "since_s": 16}],
             "spark": {}},
            {"id": "ridge", "name": "North Ridge", "group": "demo", "status": "warn", "last_contact_s": 1,
             "latest": {"batt.v": {"value": 12.058, "age_s": 30}},
             "alerts": [{"rule": "rule:Battery low", "severity": "warn",
                         "message": "Battery low: 12.06 V (below 12.2 V)", "since_s": 16},
                        {"rule": "silence", "severity": "warn", "message": "No word for 6 min", "since_s": 5}],
             "spark": {}},
            {"id": "shack", "name": "Shack", "group": "demo", "status": "ok", "last_contact_s": 1,
             "latest": {"batt.v": {"value": 13.4, "age_s": 1}}, "alerts": [], "spark": {}},
            {"id": "broken", "latest": {"batt.v": {"value": "high", "age_s": 1}}},
            "not a site"
        ]})
    }

    fn readings(c: &PowerCfg, g: &Value) -> Readings {
        read_group(c, g, NOW)
            .into_iter()
            .filter_map(|(_, node, r)| Some((node, r?)))
            .collect()
    }

    #[test]
    fn reads_the_mapped_sites_and_only_their_power_alarms() {
        let mut c = cfg();
        c.sites.insert("broken".into(), "tst-broken".into());
        let got = read_group(&c, &group(), NOW);
        let ids: Vec<(&str, bool)> = got
            .iter()
            .map(|(id, _, r)| (id.as_str(), r.is_some()))
            .collect();
        assert_eq!(
            ids,
            [("hilltop", true), ("ridge", true), ("broken", false)],
            "shack isn't mapped, and broken has no number"
        );
        let r = readings(&c, &group());
        let hill = &r["tst-hilltop"];
        assert_eq!(hill.text, "12.6 V");
        assert_eq!(hill.at, NOW - 1000);
        assert!(hill.alarms.is_empty(), "an open door isn't a power problem");
        let north = &r["tst-north"];
        assert_eq!(north.alarms, ["Battery low: 12.06 V..."]);
        assert_eq!(north.at, NOW - 30_000);
    }

    #[test]
    fn hub_text_loses_control_and_bidi_characters_and_is_cut_at_a_word() {
        assert_eq!(
            clean("Battery\u{202e} low:\n12.06 V (below 12.2 V)", 24),
            "Battery low: 12.06 V..."
        );
        assert_eq!(clean("Door\u{0} \u{2066}open\u{2069}", 24), "Door open");
        assert_eq!(clean(&"x".repeat(40), 24), format!("{}...", "x".repeat(21)));
        assert_eq!(clean("12.1 V", 24), "12.1 V");
    }

    #[test]
    fn a_reading_is_fresh_up_to_fresh_s_and_then_stale() {
        let c = cfg();
        let fresh = 16 * MIN;
        let at = |t: i64| {
            Readings::from([(
                "tst-north".to_string(),
                Reading {
                    text: "12.1 V".into(),
                    at: t,
                    alarms: vec![],
                },
            )])
        };
        assert!(matches!(
            of(&c, &at(NOW - fresh), "tst-north", NOW, fresh),
            Some(Power::Reading(_))
        ));
        assert!(matches!(
            of(&c, &at(NOW - fresh - 1), "tst-north", NOW, fresh),
            Some(Power::Stale(_))
        ));
        assert_eq!(
            of(&c, &Readings::new(), "tst-north", NOW, fresh),
            Some(Power::Unknown)
        );
        assert_eq!(
            of(&c, &at(NOW), "tst-shack", NOW, fresh),
            None,
            "not mapped"
        );
    }

    #[test]
    fn the_line_says_what_is_known_and_how_old() {
        let r = |alarms: &[&str]| Reading {
            text: "12.1 V".into(),
            at: NOW - 30 * MIN,
            alarms: alarms.iter().map(|a| a.to_string()).collect(),
        };
        let low = ["Battery low: 12.06 V..."];
        assert_eq!(Power::Reading(r(&[])).line(NOW), "Power 12.1 V.");
        assert_eq!(
            Power::Reading(r(&low)).line(NOW),
            "Power 12.1 V: Battery low: 12.06 V..."
        );
        assert_eq!(
            Power::Stale(r(&low)).line(NOW),
            "Power 12.1 V: Battery low: 12.06 V... (last reading 30 min ago)."
        );
        assert_eq!(
            Power::Stale(r(&[])).line(NOW),
            "Power unknown: last reading 30 min ago."
        );
        assert!(Power::Stale(r(&low)).alarmed() && !Power::Stale(r(&[])).alarmed());
        assert!(!Power::Unknown.alarmed());
    }

    #[test]
    fn the_unit_comes_from_the_metric_name() {
        let mut c = cfg();
        c.metric = "batt.w".into();
        let g = json!({"sites": [{"id": "hilltop", "latest": {"batt.w": {"value": 25.13, "age_s": 0}}}]});
        assert_eq!(readings(&c, &g)["tst-hilltop"].text, "25.1 W");
        c.metric = "psu.volts".into();
        let g = json!({"sites": [{"id": "hilltop", "latest": {"psu.volts": {"value": 13.8, "age_s": 0}}}]});
        assert_eq!(readings(&c, &g)["tst-hilltop"].text, "13.8");
    }

    /// A hub on a local port: `groups` from /hub/api/groups, `group()` as "demo", and every
    /// other group failing. Counts the group requests.
    async fn hub(groups: Value) -> (String, Arc<AtomicUsize>) {
        use axum::extract::Path;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::get as route;

        let asked = Arc::new(AtomicUsize::new(0));
        let n = asked.clone();
        let north = json!({"slug": "north county", "sites": [
            {"id": "paso", "latest": {"batt.v": {"value": 11.7, "age_s": 2}},
             "alerts": [{"rule": "rule:Battery low", "message": "Battery low: 11.70 V (below 11.8 V)"}]}]});
        let app = axum::Router::new()
            .route(
                "/hub/api/groups",
                route(move || async move { axum::Json(groups) }),
            )
            .route(
                "/hub/api/group/{slug}",
                route(move |Path(slug): Path<String>| async move {
                    n.fetch_add(1, Ordering::SeqCst);
                    match slug.as_str() {
                        "demo" => axum::Json(group()).into_response(),
                        "north county" => axum::Json(north).into_response(),
                        "empty" => axum::Json(json!({"sites": []})).into_response(),
                        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/hub/"), asked)
    }

    #[tokio::test]
    async fn fetches_every_group_from_the_hub() {
        let (url, _) = hub(json!([{"slug": "demo"}, {"slug": "north county"}])).await;
        let mut c = cfg();
        c.sites.insert("paso".into(), "tst-paso".into());
        c.sites.insert("gone".into(), "tst-gone".into());
        let http = reqwest::Client::new();
        let (r, gaps) = fetch(&http, &url, &c, NOW, &Readings::new()).await.unwrap();
        assert_eq!(r["tst-hilltop"].text, "12.6 V");
        assert_eq!(r["tst-north"].text, "12.1 V");
        assert_eq!(r["tst-paso"].text, "11.7 V", "a group name with a space");
        assert_eq!(r["tst-paso"].alarms.len(), 1);
        let gaps = gaps.unwrap();
        assert_eq!(gaps.unlisted, BTreeSet::from(["gone".to_string()]));
        assert_eq!(gaps.words("batt.v"), "Not listed by the hub: gone.");

        let gone = format!("{url}nothing-here/");
        assert!(
            fetch(&http, &gone, &c, NOW, &r).await.is_err(),
            "no group list, no read"
        );
    }

    #[tokio::test]
    async fn a_failed_group_keeps_its_old_readings() {
        let (url, _) = hub(json!([{"slug": "demo"}, {"slug": "broken"}])).await;
        let mut c = cfg();
        c.sites.insert("paso".into(), "tst-paso".into());
        let old = Reading {
            text: "11.9 V".into(),
            at: NOW - 5 * MIN,
            alarms: vec![],
        };
        let prev = Readings::from([("tst-paso".to_string(), old.clone())]);
        let http = reqwest::Client::new();
        let (r, gaps) = fetch(&http, &url, &c, NOW, &prev).await.unwrap();
        assert_eq!(
            r["tst-paso"], old,
            "its group failed, so the old reading stays"
        );
        assert_eq!(
            r["tst-hilltop"].text, "12.6 V",
            "the other group still counts"
        );
        assert!(gaps.is_none(), "paso may be in the failed group");
    }

    #[tokio::test]
    async fn reads_at_most_fifty_groups() {
        let many: Vec<Value> = (0..60).map(|_| json!({"slug": "empty"})).collect();
        let (url, asked) = hub(Value::Array(many)).await;
        let http = reqwest::Client::new();
        fetch(&http, &url, &cfg(), NOW, &Readings::new())
            .await
            .unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), GROUPS_MAX);
    }
}
