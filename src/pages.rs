//! The pages as HTML, so the app can serve them and the internet mirror can copy them to
//! files. Server-rendered with the shared CSS. The only scripts are optional extras: one
//! registers the offline copy, and on the mirror one says when the copy has gone stale.

use std::fmt::Write as _;

use ridgewarden_client::ui::{CSS, Color, Page, pill};
use ridgewarden_client::util::{esc, human_duration, ms_to_rfc3339, short_time};

use crate::Sent;
use crate::config::Config;
use crate::incidents::{Incident, stage_color, stage_word};
use crate::map;
use crate::status::{Report, label, labels};
use crate::usage::{PAGES, Usage};

pub const SERVICE_WORKER: &str = include_str!("sw.js");

/// Where the pages live: served by the app, or as files in the internet mirror.
pub struct Site {
    pub status: &'static str,
    pub history: &'static str,
    pub map: &'static str,
    pub copy: &'static str,
    pub sw: &'static str,
    pub mirror: bool,
}

pub const APP: Site = Site {
    status: "/",
    history: "/history",
    map: "/map",
    copy: "/copy",
    sw: "/sw.js",
    mirror: false,
};

pub const MIRROR: Site = Site {
    status: "index.html",
    history: "history.html",
    map: "map.html",
    copy: "copy.html",
    sw: "sw.js",
    mirror: true,
};

/// What a page needs to know about where it is shown.
pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub site: &'a Site,
    /// this site's node: the pages show the network from its side
    pub anchor: &'a str,
}

impl Ctx<'_> {
    pub fn page(&self, active: &str, title: &str, body: &str, refresh: bool) -> String {
        let s = self.site;
        let mut who = label(self.cfg, self.anchor);
        if s.mirror {
            who = match who.as_str() {
                "" => "a copy".into(),
                w => format!("a copy from {w}"),
            };
        }
        let page = Page {
            app: &self.cfg.title,
            nav: &[
                (s.status, "Status"),
                (s.history, "Incident history"),
                (s.map, "Map"),
            ],
            active,
            who: &who,
            refresh,
        };
        page.render(title, body)
    }
}

// A copy that stopped updating must not go on looking current. Once the copy is older than
// data-old it says so and every color in it turns to "unknown": classes on the pages,
// style variables in the map, inline colors (data-c) in the embed. It looks only inside the
// element marked data-copy, if there is one, so a pasted embed can't touch its host page,
// and it checks again every minute for pages that stay open.
const AGE_SCRIPT: &str = "<script>(function(){function c(){\
var p=document.getElementById('copy-age');if(!p)return;var a=Date.now()-p.dataset.t;\
if(a<=p.dataset.old)return;var m=Math.round(a/60000),r=p.closest('[data-copy]')||document;\
p.className='banner';p.style.fontWeight='bold';p.textContent=p.dataset.msg.replace('{age}',\
m<120?m+' minutes':Math.round(m/60)+' hours');\
r.querySelectorAll('.headline,.tile,.site,.pill').forEach(function(e){\
e.className=e.className.replace(/\\b(green|yellow|red)\\b/,'unknown')});\
r.querySelectorAll('[style*=\"--\"]').forEach(function(e){\
e.setAttribute('style',e.getAttribute('style').replace(/--(green|yellow|red)\\b/g,'--unknown'))});\
r.querySelectorAll('[data-c]').forEach(function(e){e.style.color='#555';\
e.style.background='#ececec'})}document.addEventListener('DOMContentLoaded',c);\
setInterval(c,60000)})()</script>";

/// How old a copy may get before it says it is stale: five pushes, and at least 10 minutes.
pub fn stale_after(cfg: &Config) -> i64 {
    (cfg.mirror.every_s as i64 * 5_000).max(10 * 60_000)
}

/// A note saying when a copy was made, which turns into a warning and greys every color
/// once the copy is more than `old` ms old. `msg` has "{age}" where the age goes; `attrs`
/// are the note's own attributes, such as its class or inline style.
pub fn age_note(made: i64, old: i64, text: &str, msg: &str, attrs: &str) -> String {
    format!(
        "<p id=copy-age {attrs} data-t={made} data-old={old} data-msg=\"{}\">{}</p>{AGE_SCRIPT}",
        esc(msg),
        esc(text)
    )
}

/// On the mirror: when the copy was made, said loudly once it is old.
fn mirror_note(ctx: &Ctx, made: i64) -> String {
    if !ctx.site.mirror {
        return String::new();
    }
    let text = format!(
        "A copy of the mesh's status page, made {}. It is updated every few minutes while \
         the mesh can reach the internet.",
        short_time(made)
    );
    let msg = "This copy is {age} old: the mesh has stopped sending updates, so treat \
               everything here as unknown.";
    age_note(
        made,
        stale_after(ctx.cfg),
        &text,
        msg,
        "class=\"mute small\"",
    )
}

/// Registers the service worker that keeps a copy for when there is no connection. Browsers
/// only allow it on https and on localhost; elsewhere this does nothing.
fn offline(ctx: &Ctx) -> String {
    format!(
        "<script>if('serviceWorker' in navigator)navigator.serviceWorker.register('{}')\
         .catch(function(){{}})</script>",
        ctx.site.sw
    )
}

fn contact(cfg: &Config) -> String {
    if cfg.contact.trim().is_empty() {
        return String::new();
    }
    format!("<p class=\"mute small\">{}</p>", esc(&cfg.contact))
}

/// "mesh-status-2026-11-19-1402.html", for a saved copy.
pub fn copy_name(generated: i64) -> String {
    let t = ms_to_rfc3339(generated);
    format!("mesh-status-{}-{}{}.html", &t[..10], &t[11..13], &t[14..16])
}

fn incident_box(b: &mut String, cfg: &Config, i: &Incident) {
    let u = i.latest();
    let _ = write!(
        b,
        "<div class=banner><b>{}</b> {}",
        esc(&i.title),
        pill(stage_color(&u.stage), &stage_word(&u.stage))
    );
    if !u.text.is_empty() {
        let _ = write!(b, "<br>{}", esc(&u.text));
    }
    let mut meta = format!("Updated {} by {}.", short_time(u.at), u.by);
    if !i.sites.is_empty() {
        meta = format!("Sites: {}. {meta}", labels(cfg, &i.sites));
    }
    let _ = write!(b, "<div class=\"mute small\">{}</div></div>", esc(&meta));
}

fn maintenance_list(b: &mut String, cfg: &Config, r: &Report) {
    if r.maintenance.is_empty() {
        return;
    }
    b.push_str("<h2>Planned maintenance</h2><ul class=plain>");
    for n in &r.maintenance {
        let state = if n.active(r.generated) {
            pill(Color::Yellow, "Under way")
        } else {
            pill(Color::Unknown, "Planned")
        };
        let mut meta = n.when(r.generated);
        if !n.sites.is_empty() {
            let _ = write!(meta, "; {}", labels(cfg, &n.sites));
        }
        let _ = write!(
            b,
            "<li>{state} {} <span class=\"mute small\">({}; posted by {})</span></li>",
            esc(&n.text),
            esc(&meta),
            esc(&n.by)
        );
    }
    b.push_str("</ul>");
}

fn services_table(b: &mut String, r: &Report) {
    if r.services.is_empty() {
        b.push_str("<p class=mute>No services to show.</p>");
        return;
    }
    b.push_str(
        "<div class=wrap><table><tr><th>Service</th><th>Site</th><th>State</th><th></th></tr>",
    );
    for s in &r.services {
        let kind = if s.heartbeat {
            " <span class=\"mute small\">heartbeat</span>"
        } else {
            ""
        };
        let _ = write!(
            b,
            "<tr><td>{}{kind}</td><td>{}</td><td>{}</td><td class=\"mute small\">{}</td></tr>",
            esc(&s.name),
            esc(&s.site),
            pill(s.color, s.words),
            esc(&s.reason)
        );
    }
    b.push_str("</table></div>");
}

/// The headline, notices, incidents, maintenance, sites and services.
fn report_body(cfg: &Config, r: &Report) -> String {
    let mut b = format!("<h1>{}</h1>", esc(&cfg.title));
    if let Some(bn) = &r.banner {
        let _ = write!(
            b,
            "<div class=banner><b>Notice:</b> {} <span class=\"mute small\">({}, {})</span></div>",
            esc(&bn.text),
            esc(&bn.by),
            short_time(bn.at)
        );
    }
    let _ = write!(
        b,
        "<div class=\"headline {}\">{}</div>",
        r.headline_color.css(),
        esc(&r.headline)
    );
    if !r.incidents.is_empty() {
        b.push_str("<h2>Incidents</h2>");
        for i in &r.incidents {
            incident_box(&mut b, cfg, i);
        }
    }
    maintenance_list(&mut b, cfg, r);
    b.push_str("<h2>Sites</h2><div class=tiles>");
    for s in &r.sites {
        let _ = write!(
            b,
            "<div class=\"tile {}\"><div class=name>{} {}</div><div class=why>{}</div></div>",
            s.color.css(),
            esc(&s.label),
            pill(s.color, s.color.word()),
            esc(&s.reason)
        );
    }
    b.push_str("</div><h2>Services</h2>");
    services_table(&mut b, r);
    b
}

pub fn status(ctx: &Ctx, r: &Report) -> String {
    let mut b = mirror_note(ctx, r.generated);
    b.push_str(&report_body(ctx.cfg, r));
    let _ = write!(
        b,
        "<p class=\"mute small\">Updated {}. This page shows whether the amateur radio mesh network \
         is working; it is not a channel for messages. <span class=noprint>Print it for a one-page \
         briefing, or <a href=\"{}\" download=\"{}\">save a copy</a> to read with no connection.</span></p>",
        short_time(r.generated),
        ctx.site.copy,
        copy_name(r.generated)
    );
    b.push_str(&contact(ctx.cfg));
    if !ctx.site.mirror {
        b.push_str("<p class=\"mute small noprint\"><a href=\"/about\">About this page</a></p>");
    }
    b.push_str(&offline(ctx));
    ctx.page(ctx.site.status, "Status", &b, true)
}

/// The status page as one file to keep on a phone: no links back, no reloading, and a note
/// that it does not update, which greys its colors once it is 10 minutes old.
pub fn copy(cfg: &Config, r: &Report) -> String {
    let text = format!(
        "Saved copy from {}. It does not update; open the live page for the current state.",
        short_time(r.generated)
    );
    let msg = "Saved {age} ago. It does not update, so treat its colors as unknown and open \
               the live page for the current state.";
    let mut b = age_note(r.generated, 10 * 60_000, &text, msg, "class=banner");
    b.push_str(&report_body(cfg, r));
    b.push_str(&contact(cfg));
    let page = Page {
        app: &cfg.title,
        nav: &[],
        active: "",
        who: "saved copy",
        refresh: false,
    };
    page.render("Saved copy", &b)
}

pub fn history(ctx: &Ctx, list: &[Incident], generated: i64) -> String {
    let mut b = mirror_note(ctx, generated);
    b.push_str(
        "<h1>Incident history</h1><p class=\"mute small\">Incidents from the last 90 days and any \
         still open, newest first, with every update.</p>",
    );
    if list.is_empty() {
        b.push_str("<p class=mute>No incidents in the last 90 days.</p>");
    }
    for i in list {
        let stage = &i.latest().stage;
        let _ = write!(
            b,
            "<h2>{} {}</h2>",
            esc(&i.title),
            pill(stage_color(stage), &stage_word(stage))
        );
        if !i.sites.is_empty() {
            let _ = write!(
                b,
                "<p class=\"mute small\">Sites: {}</p>",
                esc(&labels(ctx.cfg, &i.sites))
            );
        }
        b.push_str("<ul class=plain>");
        for u in i.updates.iter().rev() {
            let _ = write!(
                b,
                "<li><b>{}</b> {} <span class=\"mute small\">({}, {})</span></li>",
                stage_word(&u.stage),
                esc(&u.text),
                short_time(u.at),
                esc(&u.by)
            );
        }
        b.push_str("</ul>");
    }
    b.push_str(&offline(ctx));
    ctx.page(ctx.site.history, "Incident history", &b, false)
}

pub fn map(ctx: &Ctx, r: &Report) -> String {
    let mut b = mirror_note(ctx, r.generated);
    b.push_str("<h1>Map</h1>");
    match map::svg(r) {
        Some(svg) => {
            b.push_str(
                "<p class=\"mute small\">Each site whose node reports a location, in its current \
                 state. Lines are links: solid while up, red and dashed when down. North is up.</p>",
            );
            b.push_str(&svg);
            let key: Vec<String> = [Color::Green, Color::Yellow, Color::Red, Color::Unknown]
                .into_iter()
                .map(|c| pill(c, c.word()))
                .collect();
            let _ = write!(b, "<p>{}</p>", key.join(" "));
        }
        None => b.push_str(
            "<p class=mute>No site reports a location yet. A node reports one once its latitude \
             and longitude are set in its own settings.</p>",
        ),
    }
    let unplaced: Vec<&str> = r
        .sites
        .iter()
        .filter(|s| s.pos.is_none())
        .map(|s| s.label.as_str())
        .collect();
    if !unplaced.is_empty() {
        let _ = write!(
            b,
            "<p class=\"mute small\">Not on the map, because their nodes report no location: {}.</p>",
            esc(&unplaced.join(", "))
        );
    }
    let _ = write!(
        b,
        "<p class=\"mute small\">Updated {}.</p>",
        short_time(r.generated)
    );
    b.push_str(&offline(ctx));
    ctx.page(ctx.site.map, "Map", &b, true)
}

// sized in units of the screen's short side, so 20 sites fit a 1080p TV without scrolling
const WALL_CSS: &str = "
html{font-size:2vmin}body{overflow:hidden;line-height:1.2}
main{max-width:none;height:100vh;padding:1.2rem 1.6rem;display:flex;flex-direction:column;gap:.9rem}
header{display:flex;justify-content:space-between;align-items:baseline;gap:1rem}
h1{font-size:1.8rem;margin:0}header span{font-size:1.1rem;color:var(--mute)}
.headline{font-size:2.4rem;margin:0;padding:.8rem 1.2rem}
.notes{list-style:none;margin:0;padding:0;font-size:1.3rem}
.notes li{white-space:nowrap;overflow:hidden;text-overflow:ellipsis;border-left:.4rem solid var(--yellow);padding:.1rem .6rem;margin:.3rem 0}
.sites{display:grid;grid-template-columns:repeat(auto-fit,minmax(16rem,1fr));gap:.8rem}
.site{border-radius:.5rem;padding:.6rem .9rem;overflow:hidden}
.site .name{font-size:1.7rem;font-weight:700;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.site .why{font-size:1rem;margin-top:.3rem;display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical;overflow:hidden}
.green{background:var(--green);color:var(--bg)}.yellow{background:var(--yellow);color:var(--bg)}
.red{background:var(--red);color:var(--bg)}.unknown{background:var(--unknown);color:var(--bg)}
";

/// The banner, open incidents and maintenance under way, one line each, for the wall.
fn wall_notes(r: &Report) -> Vec<String> {
    let mut notes = vec![];
    if let Some(bn) = &r.banner {
        notes.push(format!("Notice: {} ({})", bn.text, bn.by));
    }
    for i in &r.incidents {
        let u = i.latest();
        let mut n = format!("{} ({})", i.title, stage_word(&u.stage));
        if !u.text.is_empty() {
            let _ = write!(n, ": {}", u.text);
        }
        notes.push(n);
    }
    for n in r.maintenance.iter().filter(|n| n.active(r.generated)) {
        notes.push(format!("Maintenance {}: {}", n.when(r.generated), n.text));
    }
    notes
}

/// A full-screen page for a TV: the headline, notices and big site tiles, solid colors,
/// reloading every 30 s.
pub fn wall(ctx: &Ctx, r: &Report) -> String {
    let cfg = ctx.cfg;
    let mut b = String::new();
    let _ = write!(
        b,
        "<!doctype html><html lang=en><head><meta charset=utf-8>\
         <meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <meta http-equiv=refresh content=30><title>Wall - {}</title><style>{CSS}{WALL_CSS}</style>\
         </head><body><main>{}",
        esc(&cfg.title),
        mirror_note(ctx, r.generated)
    );
    let mut updated = format!("Updated {}", short_time(r.generated));
    if !ctx.anchor.is_empty() {
        let _ = write!(updated, " at {}", label(cfg, ctx.anchor));
    }
    let _ = write!(
        b,
        "<header><h1>{}</h1><span>{}</span></header><div class=\"headline {}\">{}</div>",
        esc(&cfg.title),
        esc(&updated),
        r.headline_color.css(),
        esc(&r.headline)
    );
    let notes = wall_notes(r);
    if !notes.is_empty() {
        b.push_str("<ul class=notes>");
        for n in &notes {
            let _ = write!(b, "<li>{}</li>", esc(n));
        }
        b.push_str("</ul>");
    }
    b.push_str("<div class=sites>");
    for s in &r.sites {
        let _ = write!(
            b,
            "<div class=\"site {}\"><div class=name>{}</div><div class=why><b>{}</b> {}</div></div>",
            s.color.css(),
            esc(&s.label),
            s.color.word(),
            esc(&s.reason)
        );
    }
    b.push_str("</div></main></body></html>");
    b
}

fn failing(s: &Sent) -> bool {
    s.failed.is_some_and(|f| s.ok.is_none_or(|o| f > o))
}

/// The mirror is working only while its pushes are recent: a push that worked hours ago
/// says nothing about now.
fn mirror_state(s: Sent, every_ms: i64, now: i64) -> String {
    let when = |t: i64| format!("{} ({} ago)", short_time(t), human_duration(now - t));
    match (s.ok, s.failed) {
        (_, Some(f)) if failing(&s) => format!(
            "{} the last push, at {}, failed; the log says why.",
            pill(Color::Red, "failing"),
            when(f)
        ),
        (Some(o), _) if now - o <= 3 * every_ms => {
            format!("{} last pushed {}.", pill(Color::Green, "working"), when(o))
        }
        (Some(o), _) => format!(
            "{} last pushed {}, and not since.",
            pill(Color::Yellow, "stalled"),
            when(o)
        ),
        _ => format!("{} nothing pushed yet.", pill(Color::Unknown, "starting")),
    }
}

/// Subscriptions in numbers, without naming anyone: this page is open to the whole mesh.
fn subs_state(subs: &[Sent]) -> String {
    let bad = subs.iter().filter(|s| failing(s)).count();
    let last = subs.iter().filter_map(|s| s.ok).max();
    let mut out = format!(
        "{} channel{} set up.",
        subs.len(),
        if subs.len() == 1 { "" } else { "s" }
    );
    if bad > 0 {
        let _ = write!(
            out,
            " {} {bad} failing; `ridgewarden-status subscribers test` and the log say which and why.",
            pill(Color::Red, "failing")
        );
    }
    match last {
        Some(t) => {
            let _ = write!(out, " The last message went out {}.", short_time(t));
        }
        None => out.push_str(" No message has gone out yet."),
    }
    out
}

/// For the people who run the page: how much it is used, and whether the mirror and the
/// subscriptions are getting through. Counts only; nothing about who looked.
pub fn about(
    ctx: &Ctx,
    r: &Report,
    u: &Usage,
    mirror: Option<Sent>,
    subs: &[Sent],
    now: i64,
) -> String {
    let mut b = String::from("<h1>About this page</h1>");
    b.push_str(&contact(ctx.cfg));
    b.push_str(
        "<h2>How it is used</h2><p class=\"mute small\">Views counted per page and day, with \
         nothing about who looked.</p>",
    );
    if u.views.is_empty() {
        b.push_str("<p class=mute>No views counted yet.</p>");
    } else {
        b.push_str(
            "<div class=wrap><table><tr><th>Page</th><th>Last 7 days</th><th>Last 30 days</th></tr>",
        );
        for (key, week, month) in &u.views {
            let name = PAGES
                .iter()
                .find(|p| p.0 == key)
                .map_or(key.as_str(), |p| p.1);
            let _ = write!(
                b,
                "<tr><td>{}</td><td>{week}</td><td>{month}</td></tr>",
                esc(name)
            );
        }
        b.push_str("</table></div>");
    }
    b.push_str("<h2>AREDN nodes showing these notices</h2>");
    if u.feed.is_empty() {
        b.push_str(
            "<p class=mute>No node has asked for its message file in the last 30 days. The \
             README shows how to point a node at this page.</p>",
        );
    } else {
        let names: Vec<String> = u
            .feed
            .iter()
            .map(|(n, t)| format!("{} ({})", esc(n), short_time(*t)))
            .collect();
        let _ = write!(
            b,
            "<p>Asked for their message file in the last 30 days, with the latest time: {}.</p>",
            names.join(", ")
        );
    }
    b.push_str("<h2>Internet mirror</h2>");
    match mirror {
        None => b.push_str(
            "<p class=mute>Off. Set up [mirror] in the config to keep a copy of this page on a web \
             server off the mesh.</p>",
        ),
        Some(s) => {
            let every = ctx.cfg.mirror.every_s as i64 * 1000;
            let _ = write!(b, "<p>{}</p>", mirror_state(s, every, now));
        }
    }
    b.push_str("<h2>Subscriptions</h2>");
    if subs.is_empty() {
        b.push_str("<p class=mute>No contacts are set up under [subscriptions].</p>");
    } else {
        let _ = write!(b, "<p>{}</p>", subs_state(subs));
    }
    let silent: Vec<String> = r
        .sites
        .iter()
        .filter_map(|s| {
            let t = s.last_seen.filter(|t| now - t > 7 * 86_400_000)?;
            Some(format!(
                "{} ({}, last heard {})",
                s.label,
                s.node,
                short_time(t)
            ))
        })
        .collect();
    if !silent.is_empty() {
        let _ = write!(
            b,
            "<h2>Silent sites</h2><p>Not heard from for over a week: {}. They stay on the page as \
             cut off until you run <code>ridgewarden-status forget &lt;node&gt;</code>.</p>",
            esc(&silent.join("; "))
        );
    }
    let _ = write!(
        b,
        "<p class=\"mute small\">ridgewarden-status {}</p>",
        env!("CARGO_PKG_VERSION")
    );
    ctx.page("/about", "About", &b, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{cfg, cut_off_c, fake, report};

    #[test]
    fn a_saved_copy_stands_alone() {
        let mut f = fake();
        cut_off_c(&mut f);
        let c = cfg();
        let r = report(&f, &c, "a");
        let html = copy(&c, &r);
        assert!(html.contains(&format!("Saved copy from {}", short_time(r.generated))));
        assert!(html.contains("c is cut off"));
        assert!(
            !html.contains("http-equiv=refresh"),
            "a file can't reload itself"
        );
        assert!(
            !html.contains("href="),
            "nothing to follow with no connection"
        );
        assert!(
            html.contains(&format!("data-t={} data-old=600000", r.generated)),
            "greys itself after 10 minutes: {html}"
        );
    }

    #[test]
    fn the_about_page_counts_channels_and_says_when_the_mirror_stalls() {
        let f = fake();
        let mut c = cfg();
        c.mirror.dir = Some("/srv/x".into());
        let r = report(&f, &c, "a");
        let ctx = Ctx {
            cfg: &c,
            site: &APP,
            anchor: "a",
        };
        let u = Usage {
            views: vec![],
            feed: vec![],
        };
        let now = 1_800_000_000_000;
        let minute = 60_000;
        let pushed = |ok: i64| Sent {
            ok: Some(now - ok),
            failed: None,
        };
        let subs = [pushed(minute), Sent::default()];
        let page = |m| about(&ctx, &r, &u, Some(m), &subs, now);
        assert!(page(pushed(minute)).contains(">working<"));
        assert!(
            page(pushed(60 * minute)).contains(">stalled<"),
            "an hour-old push isn't working"
        );
        let failed = Sent {
            ok: Some(now - 5 * minute),
            failed: Some(now - minute),
        };
        assert!(page(failed).contains(">failing<"));
        let html = page(pushed(minute));
        assert!(html.contains("2 channels set up."), "{html}");
    }
}
