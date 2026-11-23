//! The map: every site whose node reports a location, in its current color, with the links
//! between them. Drawn on the server as SVG with no tiles and nothing from the internet, so
//! it works on a mesh with no way out.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ridgewarden_client::util::{esc, short_time};
use ridgewarden_linkdoctor::links::LinkState;

use crate::status::{Report, SiteTile};

const W: f64 = 640.0;
const PAD: f64 = 40.0;
const KM_PER_DEGREE: f64 = 111.32;

/// The map as inline SVG, or None when no site reports a location.
pub fn svg(r: &Report) -> Option<String> {
    let placed: Vec<(&SiteTile, (f64, f64))> = r
        .sites
        .iter()
        .filter_map(|s| s.pos.map(|p| (s, p)))
        .collect();
    if placed.is_empty() {
        return None;
    }
    // x east and y south, both in degrees of latitude, so distances keep their shape
    let lat0 = placed.iter().map(|(_, p)| p.0).sum::<f64>() / placed.len() as f64;
    let k = lat0.to_radians().cos();
    let deg: Vec<(f64, f64)> = placed.iter().map(|(_, p)| (p.1 * k, -p.0)).collect();
    let (min_x, max_x) = span(deg.iter().map(|d| d.0));
    let (min_y, max_y) = span(deg.iter().map(|d| d.1));
    // one site, or sites in a line, still get a sensible frame
    let (dx, dy) = ((max_x - min_x).max(0.02), (max_y - min_y).max(0.02));
    let h = (W * dy / dx).clamp(W * 0.45, W * 1.1);
    let scale = ((W - 2.0 * PAD) / dx).min((h - 2.0 * PAD) / dy);
    let ox = (W - scale * (max_x - min_x)) / 2.0;
    let oy = (h - scale * (max_y - min_y)) / 2.0;
    let at: BTreeMap<&str, (f64, f64)> = placed
        .iter()
        .zip(&deg)
        .map(|((s, _), d)| {
            let p = (ox + (d.0 - min_x) * scale, oy + (d.1 - min_y) * scale);
            (s.node.as_str(), p)
        })
        .collect();

    let mut o = format!(
        "<svg viewBox=\"0 0 {W} {h:.0}\" role=img aria-label=\"Map of {} sites\" \
         style=\"width:100%;height:auto;display:block;border:1px solid var(--line);border-radius:8px\">",
        placed.len()
    );
    for l in &r.links {
        if let (Some(a), Some(b)) = (at.get(l.a.as_str()), at.get(l.b.as_str())) {
            let style = match l.state {
                LinkState::Up => "stroke:var(--mute)",
                LinkState::Down => "stroke:var(--red);stroke-dasharray:6 4",
                _ => "stroke:var(--line);stroke-dasharray:2 4",
            };
            let _ = write!(
                o,
                "<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" style=\"{style};stroke-width:2\"/>",
                a.0, a.1, b.0, b.1
            );
        }
    }
    for (s, _) in &placed {
        let (x, y) = at[s.node.as_str()];
        // labels go left of sites near the right edge so they stay on the map
        let (lx, anchor) = if x > W * 0.7 {
            (x - 11.0, "end")
        } else {
            (x + 11.0, "start")
        };
        let why = match s.last_seen {
            Some(t) => format!("{} Last heard {}.", s.reason, short_time(t)),
            None => s.reason.clone(),
        };
        let _ = write!(
            o,
            "<g><circle cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"8\" style=\"fill:var(--{});stroke:var(--bg);stroke-width:2\">\
             <title>{}: {}. {}</title></circle><text x=\"{lx:.1}\" y=\"{:.1}\" text-anchor=\"{anchor}\" font-size=\"13\" \
             style=\"fill:var(--fg);paint-order:stroke;stroke:var(--bg);stroke-width:3px\">{}</text></g>",
            s.color.css(),
            esc(&s.label),
            s.color.word(),
            esc(&why),
            y + 4.0,
            esc(&s.label)
        );
    }
    scale_bar(&mut o, KM_PER_DEGREE / scale, h);
    o.push_str("</svg>");
    Some(o)
}

fn span(v: impl Iterator<Item = f64>) -> (f64, f64) {
    v.fold((f64::MAX, f64::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)))
}

/// A round distance about a quarter of the map wide, in the bottom left corner.
fn scale_bar(o: &mut String, km_per_px: f64, h: f64) {
    let want = W / 4.0 * km_per_px;
    let km = [0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0]
        .into_iter()
        .rfind(|k| *k <= want)
        .unwrap_or(0.5);
    let px = km / km_per_px;
    let y = h - 14.0;
    let _ = write!(
        o,
        "<path d=\"M12 {:.1}v6h{px:.1}v-6\" style=\"fill:none;stroke:var(--fg);stroke-width:1.5\"/>\
         <text x=\"12\" y=\"{:.1}\" font-size=\"12\" style=\"fill:var(--fg)\">{km} km</text>",
        y - 6.0,
        y - 10.0
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MIN, NOW, cfg, fake, poll_at, report};

    /// The part of the map drawn for one site.
    fn site<'a>(svg: &'a str, label: &str) -> &'a str {
        svg.split("<g>")
            .find(|g| g.contains(&format!("<title>{label}:")))
            .unwrap_or_else(|| panic!("{label} not on the map: {svg}"))
    }

    #[test]
    fn sites_with_a_location_are_drawn_in_their_state() {
        let mut f = fake();
        let at = |lat, lon| Some((lat, lon));
        // b lost its link to c, and a's node no longer reaches c
        poll_at(
            &mut f,
            NOW - 30 * MIN,
            "b",
            &["a", "c"],
            &["a", "b", "c"],
            at(35.5, -120.7),
        );
        poll_at(
            &mut f,
            NOW - 30 * MIN,
            "c",
            &["b"],
            &["a", "b", "c"],
            at(35.6, -120.7),
        );
        poll_at(
            &mut f,
            NOW - 10 * MIN,
            "b",
            &["a"],
            &["a", "b"],
            at(35.5, -120.7),
        );
        poll_at(
            &mut f,
            NOW - 5 * MIN,
            "b",
            &["a"],
            &["a", "b"],
            at(35.5, -120.7),
        );
        poll_at(
            &mut f,
            NOW - 5 * MIN,
            "a",
            &["b", "d"],
            &["a", "b", "d"],
            at(35.3, -120.6),
        );
        // a node with no location set reports 0, 0
        poll_at(
            &mut f,
            NOW - 5 * MIN,
            "d",
            &["a"],
            &["a", "b", "d"],
            at(0.0, 0.0),
        );
        let mut c = cfg();
        c.site_names.insert("a".into(), "<A&B>".into());
        let r = report(&f, &c, "a");
        let svg = svg(&r).unwrap();

        assert!(site(&svg, "c").contains("var(--red)"), "c is cut off");
        assert!(
            site(&svg, "c").contains("Not reachable. Last heard 2027-01-15 07:30 UTC.</title>"),
            "{}",
            site(&svg, "c")
        );
        assert!(site(&svg, "b").contains("var(--green)"));
        assert!(site(&svg, "&lt;A&amp;B&gt;").contains("var(--green)"));
        assert!(!svg.contains("<title>d:"), "0, 0 is no location");
        assert!(
            svg.contains("stroke:var(--red);stroke-dasharray"),
            "the lost link is drawn as down: {svg}"
        );
        assert!(svg.contains(" km</text>"), "has a scale");
    }

    #[test]
    fn no_locations_means_no_map() {
        let mut f = fake();
        poll_at(&mut f, NOW - 5 * MIN, "a", &[], &["a"], None);
        assert!(svg(&report(&f, &cfg(), "a")).is_none());
    }
}
