//! AREDN's local alert messages. A node with `aredn.@alerts[0].localpath` set fetches
//! `<path>/<node>.txt`, then `<group>.txt` for each of its message groups, then `all.txt`,
//! about once an hour, and pastes each file into its own pages as HTML. An empty file
//! means no message, so the feed is empty when nothing is going on, and always escaped.

use ridgewarden_client::util::{esc, norm_node};

use crate::config::Config;
use crate::incidents::Incident;
use crate::maintenance::Notice;
use crate::status::Report;

/// The file name without ".txt", if a node could ask for it: lowercase letters, digits
/// and hyphens, as long as a node name can be.
pub fn base(name: &str) -> Option<&str> {
    let b = name.strip_suffix(".txt")?;
    let ok = !b.is_empty()
        && b.len() <= 63
        && b.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
    ok.then_some(b)
}

/// Whether a file name means anything here: `all`, a configured group, or a site on the
/// page. Anything else gets an empty file.
pub fn known(r: &Report, cfg: &Config, base: &str) -> bool {
    base == "all" || group(cfg, base).is_some() || r.sites.iter().any(|s| s.node == base)
}

/// What one file says. `all` has the banner, every open incident and the maintenance
/// under way; a node or group name has the incidents and maintenance (under way or
/// coming up) that list its sites. A few short lines joined by <br>, or nothing.
pub fn text(r: &Report, cfg: &Config, base: &str) -> String {
    let now = r.generated;
    let mut lines = vec![];
    if base == "all" {
        if let Some(b) = &r.banner {
            lines.push(format!("Notice: {} ({})", b.text, b.by));
        }
        lines.extend(r.incidents.iter().map(incident_line));
        lines.extend(
            r.maintenance
                .iter()
                .filter(|n| n.active(now))
                .map(|n| maintenance_line(n, now)),
        );
    } else {
        let sites = group(cfg, base).unwrap_or_else(|| vec![base.to_string()]);
        let ours = |list: &[String]| list.iter().any(|s| sites.contains(s));
        lines.extend(
            r.incidents
                .iter()
                .filter(|i| ours(&i.sites))
                .map(incident_line),
        );
        lines.extend(
            r.maintenance
                .iter()
                .filter(|n| ours(&n.sites))
                .map(|n| maintenance_line(n, now)),
        );
    }
    // node pages have little room for this
    let more = lines.len().saturating_sub(5);
    lines.truncate(5);
    if more > 0 {
        lines.push(format!("and {more} more on the status page"));
    }
    lines
        .iter()
        .map(|l| esc(&short(l)))
        .collect::<Vec<_>>()
        .join("<br>")
}

fn group(cfg: &Config, name: &str) -> Option<Vec<String>> {
    cfg.aredn_groups
        .iter()
        .find(|(g, _)| g.eq_ignore_ascii_case(name))
        .map(|(_, nodes)| nodes.iter().map(|n| norm_node(n)).collect())
}

fn incident_line(i: &Incident) -> String {
    format!("Incident: {} ({})", i.title, i.latest().stage)
}

fn maintenance_line(n: &Notice, now: i64) -> String {
    format!("Maintenance {}: {}", n.when(now), n.text)
}

fn short(line: &str) -> String {
    if line.chars().count() <= 160 {
        line.to_string()
    } else {
        line.chars().take(157).collect::<String>() + "..."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MIN, NOW, cfg, fake, maintenance, poll, report};
    use serde_json::json;

    #[test]
    fn only_names_a_node_can_ask_for() {
        assert_eq!(base("all.txt"), Some("all"));
        assert_eq!(base("tst-hilltop.txt"), Some("tst-hilltop"));
        let long = format!("{}.txt", "a".repeat(64));
        for bad in [
            "TST.txt",
            "../all.txt",
            "all.html",
            ".txt",
            "a b.txt",
            &long,
        ] {
            assert_eq!(base(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_sites_and_groups_mean_anything() {
        let mut f = fake();
        poll(&mut f, NOW - 5 * MIN, "a", &[], &["a"]);
        let mut c = cfg();
        c.aredn_groups.insert("Coast".into(), vec!["a".into()]);
        let r = report(&f, &c, "a");
        for name in ["all", "a", "coast"] {
            assert!(known(&r, &c, name), "{name}");
        }
        assert!(
            !known(&r, &c, "made-up-name"),
            "nobody's file, nothing kept"
        );
    }

    #[test]
    fn feed_is_escaped_and_picks_items_per_node_and_group() {
        let mut f = fake();
        poll(&mut f, NOW - 5 * MIN, "a", &["b"], &["a", "b", "c"]);
        let mut c = cfg();
        c.aredn_groups.insert("Coast".into(), vec!["C".into()]);
        let quiet = report(&f, &c, "a");
        for name in ["all", "b", "coast"] {
            assert_eq!(text(&quiet, &c, name), "", "{name}: nothing going on");
        }

        f.add(
            "status.banner",
            json!({"text": "<script>alert(1)</script>"}),
            NOW - 4 * MIN,
        );
        f.add(
            "status.incident",
            json!({"title": "B & C down", "stage": "identified", "sites": ["b"]}),
            NOW - 3 * MIN,
        );
        maintenance(&mut f, "Tower work", NOW - MIN, NOW + 60 * MIN, &["c"]);
        maintenance(
            &mut f,
            "Generator test",
            NOW + 24 * 60 * MIN,
            NOW + 25 * 60 * MIN,
            &["c"],
        );
        let r = report(&f, &c, "a");

        let all = text(&r, &c, "all");
        assert_eq!(all.split("<br>").count(), 3, "{all}");
        assert!(
            all.contains("&lt;script&gt;") && !all.contains("<script"),
            "{all}"
        );
        assert!(
            all.contains("Incident: B &amp; C down (identified)"),
            "{all}"
        );
        assert!(
            all.contains("Tower work") && !all.contains("Generator"),
            "{all}"
        );

        let b = text(&r, &c, "b");
        assert_eq!(b, "Incident: B &amp; C down (identified)");
        let coast = text(&r, &c, "coast");
        assert!(coast.starts_with("Maintenance until "), "{coast}");
        assert!(
            coast.contains("Generator test") && !coast.contains("Incident"),
            "{coast}"
        );
        assert_eq!(text(&r, &c, "d"), "");
    }
}
