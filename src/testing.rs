//! Test helpers: a fake history of polls and posts, and the report built from it.

use ridgewarden_client::event::list_hash;
use ridgewarden_client::mesh::Mesh;
use ridgewarden_client::mesh::testing::Fake;
use ridgewarden_client::util::ms_to_rfc3339;
use ridgewarden_client::{Author, Event};
use ridgewarden_health::probes::Probes;
use ridgewarden_linkdoctor::history::Notes;
use serde_json::{Value, json};

use crate::config::{Config, EXAMPLE};
use crate::incidents::{self, Incidents};
use crate::maintenance::Maintenance;
use crate::power::Readings;
use crate::status::{Banners, Report, build, snapshot};
use crate::{PROJECTIONS, SCHEMAS, View};

pub const MIN: i64 = 60_000;
pub const NOW: i64 = 1_800_000_000_000;

/// The app's projections, plus the tables it keeps outside them.
pub fn fake() -> Fake {
    let f = Fake::new(vec![
        Box::new(Mesh),
        Box::new(Probes),
        Box::new(Notes),
        Box::new(Banners),
        Box::new(Incidents),
        Box::new(Maintenance),
    ]);
    for schema in SCHEMAS {
        f.conn.execute_batch(schema).unwrap();
    }
    f
}

/// The example config, with the fake station allowed to post.
pub fn cfg() -> Config {
    let mut c: Config = toml::from_str(EXAMPLE).unwrap();
    c.banner_authors = vec![fake_key()];
    c
}

/// The key `Fake` signs with.
pub fn fake_key() -> String {
    Fake::new(vec![]).add("x", json!({}), 0).author.key
}

/// A poll of `node` at `ts`: its links and the hosts its node can reach.
pub fn poll(f: &mut Fake, ts: i64, node: &str, links: &[&str], hosts: &[&str]) {
    poll_at(f, ts, node, links, hosts, None);
}

/// A poll from a node that may report its location.
pub fn poll_at(
    f: &mut Fake,
    ts: i64,
    node: &str,
    links: &[&str],
    hosts: &[&str],
    pos: Option<(f64, f64)>,
) {
    let links: Vec<_> = links
        .iter()
        .map(|n| json!({"neighbor": n, "type": "RF", "snr": 25.0, "quality": 99.0}))
        .collect();
    let hosts = json!(hosts);
    let h = list_hash(&hosts);
    f.add("mesh.list", json!({"hash": h, "list": hosts}), ts);
    f.add(
        "node.poll",
        json!({"node": node, "ok": true, "via": "lan", "links": links, "hosts_hash": h,
               "lat": pos.map(|p| p.0), "lon": pos.map(|p| p.1)}),
        ts,
    );
}

/// Earlier everyone was reachable; now a's node no longer lists c, and b lost its link.
pub fn cut_off_c(f: &mut Fake) {
    poll(f, NOW - 30 * MIN, "b", &["a", "c"], &["a", "b", "c"]);
    poll(f, NOW - 5 * MIN, "a", &["b"], &["a", "b"]);
    // b stops hearing c; two polls in a row, so it isn't a one-poll hiccup
    poll(f, NOW - 10 * MIN, "b", &["a"], &["a", "b"]);
    poll(f, NOW - 5 * MIN, "b", &["a"], &["a", "b"]);
}

pub fn report(f: &Fake, c: &Config, anchor: &str) -> Report {
    let snap = snapshot(&f.conn, c, NOW, anchor).unwrap();
    build(&f.conn, c, &snap, anchor, "", &Readings::new()).unwrap()
}

/// What the pages are built from, as the app's background task makes it.
pub fn view(f: &Fake, c: &Config, anchor: &str) -> View {
    View {
        report: report(f, c, anchor),
        anchor: anchor.into(),
        history: incidents::history(&f.conn, c, NOW).unwrap(),
        settled: true,
    }
}

/// An event from another station, which `Fake` can't sign for. Projections don't check
/// signatures (the core does), so an unsigned one is enough here.
pub fn add_as(f: &mut Fake, key: &str, lamport: u64, kind: &str, payload: Value, ts: i64) -> Event {
    let e = Event {
        id: format!("{lamport:064x}"),
        author: Author {
            call: "N0OTHER".into(),
            key: key.into(),
        },
        seq: lamport,
        prev: None,
        lamport,
        wall_time: ms_to_rfc3339(ts),
        mono_time: 0,
        kind: kind.into(),
        payload,
        sig: String::new(),
        log: None,
    };
    let tx = f.conn.transaction().unwrap();
    for p in PROJECTIONS {
        p.apply(&tx, &e, ts).unwrap();
    }
    tx.commit().unwrap();
    e
}

pub fn maintenance(f: &mut Fake, text: &str, from: i64, to: i64, sites: &[&str]) -> Event {
    f.add(
        "status.maintenance",
        json!({"text": text, "from": ms_to_rfc3339(from), "to": ms_to_rfc3339(to), "sites": sites}),
        NOW - 10 * MIN,
    )
}
