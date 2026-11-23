//! What other sites show: a status badge, and a small HTML fragment for an iframe on
//! another website. Both carry their own colors, the light palette of the shared CSS, since
//! the page around them has none of ours.

use std::fmt::Write as _;

use ridgewarden_client::ui::Color;
use ridgewarden_client::util::{esc, norm_node, short_time, valid_node};

use crate::config::Config;
use crate::incidents::stage_word;
use crate::pages::{age_note, stale_after};
use crate::status::{Report, label};

/// (text, background) for a state.
fn palette(c: Color) -> (&'static str, &'static str) {
    match c {
        Color::Green => ("#1e7b34", "#e3f4e6"),
        Color::Yellow => ("#8a5a00", "#fff2cc"),
        Color::Red => ("#b3261e", "#fde4e1"),
        Color::Unknown => ("#555", "#ececec"),
    }
}

/// A two-part badge like "mesh | OK", the right part in the state's color.
pub fn badge(label: &str, color: Color) -> String {
    let value = color.word();
    // about 7 px a character in 11 px Verdana, plus padding
    let width = |s: &str| 12 + 7 * s.chars().count();
    let (lw, vw) = (width(label), width(value));
    let label = esc(label);
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"20\" role=\"img\" \
         aria-label=\"{label}: {value}\"><title>{label}: {value}</title>\
         <clipPath id=\"r\"><rect width=\"{w}\" height=\"20\" rx=\"3\"/></clipPath>\
         <g clip-path=\"url(#r)\"><rect width=\"{lw}\" height=\"20\" fill=\"#333\"/>\
         <rect x=\"{lw}\" width=\"{vw}\" height=\"20\" fill=\"{fill}\"/></g>\
         <g fill=\"#fff\" font-family=\"Verdana,DejaVu Sans,sans-serif\" font-size=\"11\" \
         text-anchor=\"middle\"><text x=\"{lx}\" y=\"14\">{label}</text>\
         <text x=\"{vx}\" y=\"14\">{value}</text></g></svg>",
        w = lw + vw,
        fill = palette(color).0,
        lx = lw / 2,
        vx = lw + vw / 2,
    )
}

/// The badge for the whole mesh, or for one site. None when `site` can't be a node name;
/// a site the report doesn't know is UNKNOWN.
pub fn badge_for(r: &Report, cfg: &Config, site: Option<&str>) -> Option<String> {
    let Some(site) = site else {
        return Some(badge("mesh", r.headline_color));
    };
    let node = norm_node(site);
    if !valid_node(&node) {
        return None;
    }
    Some(match r.sites.iter().find(|s| s.node == node) {
        Some(s) => badge(&s.label, s.color),
        None => badge(&label(cfg, &node), Color::Unknown),
    })
}

/// The headline, open incidents and every site, styled inline so it can go anywhere. Its
/// "updated" line turns into a warning, and its colors grey, once it is older than the
/// mirror's stale time: an embed left open, or a mirror the mesh stopped pushing, must not
/// go on showing green.
pub fn fragment(r: &Report, cfg: &Config) -> String {
    let mut b = String::new();
    let _ = write!(
        b,
        "<div data-copy style=\"font:14px/1.4 system-ui,-apple-system,'Segoe UI',Roboto,sans-serif;\
         color:#1a1a1a;background:#fff;border:1px solid #ddd;border-radius:8px;padding:12px;\
         max-width:480px\"><div style=\"font-weight:700;margin-bottom:6px\">{}</div>",
        esc(&cfg.title)
    );
    let (fg, bg) = palette(r.headline_color);
    let _ = write!(
        b,
        "<div data-c style=\"color:{fg};background:{bg};padding:8px 10px;border-radius:6px;\
         font-weight:600\">{}</div>",
        esc(&r.headline)
    );
    if let Some(bn) = &r.banner {
        let _ = write!(
            b,
            "<p style=\"margin:8px 0\"><b>Notice:</b> {} ({})</p>",
            esc(&bn.text),
            esc(&bn.by)
        );
    }
    for i in &r.incidents {
        let u = i.latest();
        let _ = write!(
            b,
            "<p style=\"margin:8px 0\"><b>{}</b> ({}) {}</p>",
            esc(&i.title),
            stage_word(&u.stage),
            esc(&u.text)
        );
    }
    b.push_str("<ul style=\"list-style:none;margin:8px 0;padding:0\">");
    for s in &r.sites {
        let (fg, bg) = palette(s.color);
        let _ = write!(
            b,
            "<li style=\"padding:2px 0\">{} <span data-c style=\"color:{fg};background:{bg};\
             padding:0 6px;border-radius:8px;font-size:12px;font-weight:700\">{}</span></li>",
            esc(&s.label),
            s.color.word()
        );
    }
    b.push_str("</ul>");
    let text = format!("Updated {}", short_time(r.generated));
    let msg = "This status is {age} old: the mesh has stopped sending updates, so treat it as \
               unknown.";
    b.push_str(&age_note(
        r.generated,
        stale_after(cfg),
        &text,
        msg,
        "style=\"color:#666;font-size:12px;margin:0\"",
    ));
    b.push_str("</div>");
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MIN, NOW, cfg, cut_off_c, fake, report};
    use serde_json::json;

    #[test]
    fn badge_shows_the_headline_or_one_site() {
        let mut f = fake();
        cut_off_c(&mut f);
        let mut c = cfg();
        c.site_names
            .insert("never-heard".into(), "Hollister Peak".into());
        let r = report(&f, &c, "a");
        let mesh = badge_for(&r, &c, None).unwrap();
        assert!(mesh.contains(">mesh<") && mesh.contains(">DOWN<"), "{mesh}");
        assert!(mesh.contains("fill=\"#b3261e\""), "red: {mesh}");
        let a = badge_for(&r, &c, Some("A.local.mesh")).unwrap();
        assert!(a.contains(">a<") && a.contains(">OK<"), "{a}");
        assert!(a.contains("fill=\"#1e7b34\""), "green: {a}");
        let never = badge_for(&r, &c, Some("never-heard")).unwrap();
        assert!(
            never.contains(">Hollister Peak<") && never.contains(">UNKNOWN<"),
            "{never}"
        );
        assert!(badge_for(&r, &c, Some("<b>")).is_none());
    }

    #[test]
    fn fragment_escapes_what_came_from_the_mesh() {
        let mut f = fake();
        cut_off_c(&mut f);
        f.add(
            "status.incident",
            json!({"title": "<img src=x onerror=alert(1)>", "stage": "investigating"}),
            NOW - MIN,
        );
        let html = fragment(&report(&f, &cfg(), "a"), &cfg());
        assert!(
            html.contains("&lt;img src=x") && !html.contains("<img"),
            "{html}"
        );
        assert!(html.contains("(Investigating)"), "{html}");
    }
}
