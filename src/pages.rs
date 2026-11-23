//! The pages as HTML, so the app can serve them and the internet mirror can copy them to
//! files. Server-rendered with the shared CSS. The only scripts are optional extras: one
//! registers the offline copy, and on the mirror one says when the copy has gone stale.

use std::fmt::Write as _;

use ridgewarden_client::ui::{CSS, Color, ICON, Page, pill};
use ridgewarden_client::util::{esc, human_duration, ms_to_rfc3339, norm_node, short_time};

use crate::Sent;
use crate::config::Config;
use crate::incidents::{Incident, stage_color, stage_word};
use crate::map;
use crate::power::Monitor;
use crate::status::{Report, SiteTile, fresh_ms, label, labels};
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
    age_note(
        made,
        stale_after(ctx.cfg),
        &text,
        STALE_COPY,
        "class=\"mute small\"",
    )
}

const STALE_COPY: &str = "This copy is {age} old: the mesh has stopped sending updates, so \
                          treat everything here as unknown.";

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
        "<div class=\"wrap svc\"><table><tr><th>Service</th><th>Site</th><th>State</th><th></th></tr>",
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

/// When a site was last heard and what its power reads at `now`, as a line of `class` for
/// under its tile; `when` words the time, as HTML.
fn facts(s: &SiteTile, class: &str, now: i64, when: impl Fn(i64) -> String) -> String {
    let mut f = vec![];
    if let Some(t) = s.last_seen {
        f.push(when(t));
    }
    if let Some(p) = &s.power {
        f.push(format!("<span class=pw>{}</span>", esc(&p.line(now))));
    }
    if f.is_empty() {
        return String::new();
    }
    format!("<div class=\"{class}\">{}</div>", f.join(" "))
}

/// "08:32 UTC" on the same day as `now`, otherwise with the date.
fn heard_time(t: i64, now: i64) -> String {
    let (at, today) = (short_time(t), short_time(now));
    if at[..10] == today[..10] {
        at[11..].to_string()
    } else {
        at
    }
}

// Tiles break a long word from the mesh or the site monitor rather than run out of the box.
// On paper the page has to fit one sheet for briefings and binders, so it gets smaller type,
// tight spacing, tiles flowed down four columns so a tall tile doesn't stretch a whole row,
// and services in two columns of plain rows. The shared print rules (no nav, black on white)
// still apply.
const PAGE_CSS: &str = "<style>.tile{overflow-wrap:anywhere}@media print{\
body{font-size:9pt;line-height:1.3}h1{font-size:13pt;margin:0 0 3pt}\
h2{font-size:10.5pt;margin:5pt 0 2pt}.headline{font-size:11pt;padding:3pt 6pt;margin:3pt 0 4pt}\
.banner{padding:2pt 6pt;margin:2pt 0;border-width:1px}.tiles{display:block;columns:4;column-gap:4pt}\
.tile{display:block;padding:2pt 5pt;margin:0 0 3pt;break-inside:avoid}.ago{display:none}\
.pill{padding:0 4pt;font-size:7pt}th,td{padding:1pt 4pt}ul.plain{margin:2pt 0}p{margin:3pt 0}\
.pw{display:block}.svc{columns:2;column-gap:14pt;overflow:visible}\
.svc table,.svc tbody{display:block}.svc tr{display:flex;flex-wrap:wrap;break-inside:avoid;\
border-bottom:1px solid #999}.svc tr:first-child,.svc td:empty{display:none}\
.svc td{border:0;padding:1pt 3pt}.svc td:first-child{flex:1 1 45%}.svc td:nth-child(2){flex:1 1 30%}\
.svc td:nth-child(4){flex:1 1 100%;padding-top:0}}</style>";

/// The headline, notices, incidents, maintenance, sites and services.
fn report_body(cfg: &Config, r: &Report) -> String {
    let mut b = format!("{PAGE_CSS}<h1>{}</h1>", esc(&cfg.title));
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
        // "4 min ago" means nothing on paper, so print leaves it out
        let seen = facts(s, "mute small", r.generated, |t| {
            format!(
                "Last heard {}<span class=ago> ({} ago)</span>.",
                heard_time(t, r.generated),
                human_duration(r.generated - t)
            )
        });
        let _ = write!(
            b,
            "<div class=\"tile {}\"><div class=name>{} {}</div><div class=why>{}</div>{seen}</div>",
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

// Sized in units of the screen's short side, so 20 sites fit a 1080p TV without scrolling.
// Past that the rows shrink to share the screen rather than drop off its bottom edge, and a
// tile's lines that no longer fit wrap into a second column out of sight, whole, last first.
const WALL_CSS: &str = "
html{font-size:2vmin}body{overflow:hidden;line-height:1.2}
main{max-width:none;height:100vh;padding:1.2rem 1.6rem;display:flex;flex-direction:column;gap:.9rem}
header{display:flex;justify-content:space-between;align-items:baseline;gap:1rem}
h1{font-size:1.8rem;margin:0}header span{font-size:1.1rem;color:var(--mute)}
header #copy-age{font-size:1.1rem;margin:0;text-align:right}
.headline{font-size:2.4rem;margin:0;padding:.8rem 1.2rem}
.notes{list-style:none;margin:0;padding:0;font-size:1.3rem}
.notes li{white-space:nowrap;overflow:hidden;text-overflow:ellipsis;border-left:.4rem solid var(--yellow);padding:.1rem .6rem;margin:.3rem 0}
.sites{display:grid;grid-template-columns:repeat(auto-fit,minmax(16rem,1fr));gap:.8rem;flex:1;min-height:0;grid-auto-rows:minmax(0,max-content)}
.site{border-radius:.5rem;padding:.6rem .9rem;overflow:hidden;display:flex;flex-flow:column wrap;align-content:flex-start;column-gap:2rem}
.site>*{width:100%}
.site .name{font-size:1.7rem;font-weight:700;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.site .why{font-size:1rem;margin-top:.3rem;display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical;overflow:hidden;overflow-wrap:anywhere}
.site .seen{font-size:.9rem;margin-top:.25rem;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.green{background:var(--green);color:var(--bg)}.yellow{background:var(--yellow);color:var(--bg)}
.red{background:var(--red);color:var(--bg)}.unknown{background:var(--unknown);color:var(--bg)}
";

/// The banner, open incidents and maintenance under way, one line each, for the wall: five
/// lines at most, so the sites keep the screen.
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
    let max = 5;
    if notes.len() > max {
        let more = notes.len() - (max - 1);
        notes.truncate(max - 1);
        notes.push(format!("And {more} more on the status page."));
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
         <meta http-equiv=refresh content=30>{ICON}<title>Wall - {}</title><style>{CSS}{WALL_CSS}</style>\
         </head><body><main>",
        esc(&cfg.title)
    );
    let mut when = short_time(r.generated);
    if !ctx.anchor.is_empty() {
        let _ = write!(when, " at {}", label(cfg, ctx.anchor));
    }
    // on a copy the header says when it was made, and warns once that was long ago, so the
    // note takes no line of its own away from the sites
    let side = if ctx.site.mirror {
        let text = format!("Copy made {when}; it updates while the mesh can reach the internet.");
        age_note(
            r.generated,
            stale_after(cfg),
            &text,
            STALE_COPY,
            "class=mute",
        )
    } else {
        format!("<span>Updated {}</span>", esc(&when))
    };
    let _ = write!(
        b,
        "<header><h1>{}</h1>{side}</header><div class=\"headline {}\">{}</div>",
        esc(&cfg.title),
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
        let seen = facts(s, "seen", r.generated, |t| {
            let ago = human_duration(r.generated - t);
            // a copy is read long after it was made, so it gives the time as well
            if ctx.site.mirror {
                format!("Heard {} ({ago} ago).", heard_time(t, r.generated))
            } else {
                format!("Heard {ago} ago.")
            }
        });
        let _ = write!(
            b,
            "<div class=\"site {}\"><div class=name>{}</div><div class=why><b>{}</b> {}</div>{seen}</div>",
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

/// Whether the hub answers, how many mapped sites have a fresh reading, and which it gives
/// no reading for.
fn monitor_state(m: &Monitor, cfg: &Config, now: i64) -> String {
    let when = |t: i64| format!("{} ({} ago)", short_time(t), human_duration(now - t));
    let hub = esc(cfg.power.hub.as_deref().unwrap_or_default());
    let mut out = match (m.read.ok, m.read.failed) {
        (_, Some(f)) if failing(&m.read) => format!(
            "{} reading {hub} failed at {}; the log says why.",
            pill(Color::Red, "failing"),
            when(f)
        ),
        (Some(o), _) => format!("{} read {hub} {}.", pill(Color::Green, "working"), when(o)),
        _ => format!("{} {hub} not read yet.", pill(Color::Unknown, "starting")),
    };
    let fresh_from = now - fresh_ms(cfg);
    let fresh = cfg
        .power
        .sites
        .values()
        .filter(|n| {
            m.readings
                .get(&norm_node(n))
                .is_some_and(|r| r.at >= fresh_from)
        })
        .count();
    let _ = write!(
        out,
        " {fresh} of {} mapped sites have a fresh reading.",
        cfg.power.sites.len()
    );
    if !m.gaps.is_empty() {
        let _ = write!(out, " {}", esc(&m.gaps.words(&cfg.power.metric)));
    }
    out
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
    power: Option<&Monitor>,
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
    b.push_str("<h2>Site monitor</h2>");
    match power {
        None => b.push_str(
            "<p class=mute>Off. Set up [power] in the config to show each site's power from a \
             sitemon hub.</p>",
        ),
        Some(m) => {
            let _ = write!(b, "<p>{}</p>", monitor_state(m, ctx.cfg, now));
        }
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
    use crate::power::{Reading, Readings};
    use crate::status::{build, snapshot};
    use crate::testing::{MIN, NOW, cfg, cut_off_c, fake, report};
    use serde_json::json;

    #[test]
    fn tiles_say_when_each_site_was_last_heard_and_what_its_power_reads() {
        let mut f = fake();
        cut_off_c(&mut f);
        let mut c = cfg();
        c.power.hub = Some("http://127.0.0.1:9/".into());
        c.power.sites.insert("hill".into(), "a".into());
        c.power.sites.insert("ridge".into(), "b".into());
        let readings = Readings::from([(
            "a".to_string(),
            Reading {
                text: "12.6 V".into(),
                at: NOW - MIN,
                alarms: vec![],
            },
        )]);
        let snap = snapshot(&f.conn, &c, NOW, "a").unwrap();
        let r = build(&f.conn, &c, &snap, "a", "", &readings).unwrap();
        let ctx = Ctx {
            cfg: &c,
            site: &APP,
            anchor: "a",
        };
        let html = status(&ctx, &r);
        let tile = |name: &str| {
            html.split("<div class=\"tile ")
                .find(|t| t.starts_with(&format!("{}\"><div class=name>{name} ", color(&r, name))))
                .unwrap_or_else(|| panic!("no tile for {name}: {html}"))
        };
        assert!(
            tile("c").contains("Last heard 07:30 UTC<span class=ago> (30 min ago)</span>."),
            "{}",
            tile("c")
        );
        assert!(tile("c").contains("Not reachable."), "cut off");
        assert!(
            tile("a").contains("(5 min ago)</span>. <span class=pw>Power 12.6 V.</span>"),
            "{}",
            tile("a")
        );
        assert!(
            tile("b").contains("Power unknown: no recent reading"),
            "{}",
            tile("b")
        );
        assert!(!tile("c").contains("Power"), "c has no site monitor");

        let wall = wall(&ctx, &r);
        assert!(wall.contains("Heard 30 min ago.</div>"), "{wall}");
        assert!(
            wall.contains("Heard 5 min ago. <span class=pw>Power 12.6 V.</span></div>"),
            "{wall}"
        );
    }

    fn color(r: &Report, node: &str) -> &'static str {
        r.sites.iter().find(|s| s.node == node).unwrap().color.css()
    }

    #[test]
    fn a_time_from_another_day_keeps_its_date() {
        let day = 24 * 60 * MIN;
        assert_eq!(heard_time(NOW - 30 * MIN, NOW), "07:30 UTC");
        assert_eq!(heard_time(NOW - 10 * day, NOW), "2027-01-05 08:00 UTC");
        assert_eq!(
            heard_time(NOW - 9 * 60 * MIN, NOW),
            "2027-01-14 23:00 UTC",
            "yesterday"
        );
    }

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
        // the tab icon is an inline data URI, so it needs no connection either
        assert!(
            !html.replace(ICON, "").contains("href="),
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
        let page = |m| about(&ctx, &r, &u, Some(m), &subs, None, now);
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

    #[test]
    fn the_about_page_says_whether_the_site_monitor_answers() {
        let f = fake();
        let mut c = cfg();
        c.power.hub = Some("http://hub.local.mesh:8470".into());
        c.power.sites.insert("hill".into(), "a".into());
        c.power.sites.insert("ridge".into(), "b".into());
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
        let mut m = Monitor::default();
        m.read.ok = Some(NOW - MIN);
        let fresh = Reading {
            text: "12.6 V".into(),
            at: NOW - MIN,
            alarms: vec![],
        };
        m.readings.insert("a".into(), fresh);
        m.gaps.unlisted.insert("ridge".into());
        let page = |m: &Monitor| about(&ctx, &r, &u, None, &[], Some(m), NOW);
        let html = page(&m);
        assert!(html.contains(">working<"), "{html}");
        assert!(
            html.contains(
                "1 of 2 mapped sites have a fresh reading. Not listed by the hub: ridge."
            ),
            "{html}"
        );
        m.read.failed = Some(NOW);
        assert!(page(&m).contains(">failing<"));
        assert!(about(&ctx, &r, &u, None, &[], None, NOW).contains("Off. Set up [power]"));
    }

    #[test]
    fn the_wall_shows_five_notes_at_most() {
        let notes = |n: usize| {
            let mut f = fake();
            cut_off_c(&mut f);
            for i in 0..n {
                f.add(
                    "status.incident",
                    json!({"title": format!("Problem {i}"), "stage": "investigating"}),
                    NOW - MIN,
                );
            }
            wall_notes(&report(&f, &cfg(), "a"))
        };
        assert_eq!(notes(5).len(), 5, "five fit");
        let seven = notes(7);
        assert_eq!(seven.len(), 5);
        assert_eq!(seven[4], "And 3 more on the status page.");
    }

    #[test]
    fn the_mirror_wall_gives_clock_times() {
        let mut f = fake();
        cut_off_c(&mut f);
        let c = cfg();
        let r = report(&f, &c, "a");
        let shown = |site| {
            wall(
                &Ctx {
                    cfg: &c,
                    site,
                    anchor: "a",
                },
                &r,
            )
        };
        let copy = shown(&MIRROR);
        assert!(copy.contains("Heard 07:30 UTC (30 min ago).</div>"));
        assert!(
            copy.contains("<header><h1>Mesh status</h1><p id=copy-age class=mute"),
            "the copy's age goes in the header, not on a line of its own: {copy}"
        );
        assert!(shown(&APP).contains("Heard 30 min ago.</div>"));
    }
}
