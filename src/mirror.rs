//! The internet mirror: a read-only copy of the pages, written as static files and pushed
//! to a web server off the mesh, for people who need the status but can't reach the mesh.
//! The app itself never faces the internet; the web server only ever holds files.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::header::CONTENT_TYPE;

use ridgewarden_client::ui::Color;

use crate::View;
use crate::config::Config;
use crate::embed;
use crate::pages::{self, Ctx, MIRROR, SERVICE_WORKER};
use crate::status::Report;

/// The report as the public may see it: only the services `[mirror] services` allows,
/// locations rounded to about a kilometre, and no power readings unless `[mirror] power`.
fn public(cfg: &Config, r: &Report) -> Report {
    let mut r = r.clone();
    let allowed = |name: &str| {
        let name = name.to_ascii_lowercase();
        cfg.mirror
            .services
            .iter()
            .any(|s| name.contains(&s.to_ascii_lowercase()))
    };
    r.services.retain(|s| allowed(&s.name));
    let round = |x: f64| (x * 100.0).round() / 100.0;
    for s in &mut r.sites {
        s.pos = s.pos.map(|(lat, lon)| (round(lat), round(lon)));
        if !cfg.mirror.power {
            s.power = None;
        }
    }
    r
}

/// Every file in the mirror, as (name, contents). `made` is when the copy is made; it goes
/// into status.json so programs can tell an old copy from a current one.
pub fn files(cfg: &Config, v: &View, made: i64) -> Vec<(&'static str, String)> {
    let r = public(cfg, &v.report);
    let ctx = Ctx {
        cfg,
        site: &MIRROR,
        anchor: &v.anchor,
    };
    let mut json = r.to_json();
    json["made"] = made.into();
    vec![
        ("index.html", pages::status(&ctx, &r)),
        (
            "history.html",
            pages::history(&ctx, &v.history, r.generated),
        ),
        ("map.html", pages::map(&ctx, &r)),
        ("wall.html", pages::wall(&ctx, &r)),
        ("copy.html", pages::copy(cfg, &r)),
        ("embed.html", embed::fragment(&r, cfg)),
        ("badge.svg", embed::badge("mesh", r.headline_color)),
        // what the web server shows instead once the mesh stops pushing; see deploy/mirror
        ("badge-unknown.svg", embed::badge("mesh", Color::Unknown)),
        ("status.json", json.to_string()),
        ("sw.js", SERVICE_WORKER.to_string()),
    ]
}

/// Write the files into `dir`, each under a temporary name first, so a web server reading
/// the folder never serves half a file.
pub fn write(dir: &Path, files: &[(&str, String)]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, body) in files {
        let tmp = dir.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, dir.join(name))
            .with_context(|| format!("replacing {name} in {}", dir.display()))?;
    }
    Ok(())
}

fn mime(name: &str) -> &'static str {
    match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        _ => "text/javascript",
    }
}

/// Send the written files on: run the configured command, then PUT each file, if set.
pub async fn push(http: &reqwest::Client, cfg: &Config, files: &[(&str, String)]) -> Result<()> {
    let m = &cfg.mirror;
    if let Some(cmd) = &m.command {
        run(cmd, &cfg.mirror_dir()).await?;
    }
    let Some(url) = &m.put_url else {
        return Ok(());
    };
    let token = match &m.token_file {
        Some(p) => Some(
            std::fs::read_to_string(p)
                .with_context(|| format!("reading the mirror token {}", p.display()))?
                .trim()
                .to_string(),
        ),
        None => None,
    };
    for (name, body) in files {
        let mut req = http
            .put(format!("{}/{name}", url.trim_end_matches('/')))
            .header(CONTENT_TYPE, mime(name))
            .body(body.clone());
        if let Some(t) = &token {
            req = req.bearer_auth(t);
        }
        // the URL may carry credentials, so it stays out of the error
        req.send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.without_url())
            .with_context(|| format!("uploading {name}"))?;
    }
    Ok(())
}

/// `s` quoted for sh, so a folder with spaces or quotes in its name stays one word.
fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The configured command, through the shell, with {dir} replaced by the files' folder.
async fn run(cmd: &str, dir: &Path) -> Result<()> {
    let line = cmd.replace("{dir}", &quoted(&dir.display().to_string()));
    let out = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&line)
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(300), out)
        .await
        .context("the mirror command ran for over 5 minutes")?
        .context("starting the mirror command with sh")?;
    if !out.status.success() {
        bail!(
            "the mirror command failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::power::{Power, Reading};
    use crate::testing::{MIN, NOW, cfg, cut_off_c, fake, poll_at, view};
    use serde_json::json;

    #[test]
    fn the_copy_is_static_files_that_say_how_old_they_are() {
        let mut f = fake();
        cut_off_c(&mut f);
        let c = cfg();
        let v = view(&f, &c, "a");
        let files = files(&c, &v, NOW + MIN);
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &files).unwrap();
        let read = |n: &str| std::fs::read_to_string(dir.path().join(n)).unwrap();

        let index = read("index.html");
        assert!(index.contains("c is cut off"), "the real headline");
        assert!(
            index.contains("href=\"history.html\"") && !index.contains("href=\"/history\""),
            "links stay inside the copy"
        );
        assert!(!index.contains("/ack/"));
        // every page a person might leave open greys itself once the pushes stop
        for page in [
            "index.html",
            "history.html",
            "map.html",
            "wall.html",
            "copy.html",
            "embed.html",
        ] {
            let html = read(page);
            assert!(
                html.contains(&format!("data-t={}", v.report.generated))
                    && html.contains("data-old=")
                    && html.contains("id=copy-age"),
                "{page} can't tell it is stale"
            );
        }
        let json: serde_json::Value = serde_json::from_str(&read("status.json")).unwrap();
        assert_eq!(json["headline_color"], "DOWN");
        assert_eq!(json["made"], NOW + MIN, "programs can judge its age");
        assert!(read("badge.svg").contains(">DOWN<"));
        assert!(read("badge-unknown.svg").contains(">UNKNOWN<"));
        assert!(!dir.path().join(".index.html.tmp").exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), files.len());
    }

    #[test]
    fn the_public_copy_names_only_allowed_services_and_rough_locations() {
        let mut f = fake();
        poll_at(
            &mut f,
            NOW - 5 * MIN,
            "a",
            &[],
            &["a"],
            Some((35.123456, -120.654321)),
        );
        let ads = json!([
            {"host": "a", "name": "Winlink Gateway", "link": "http://a:8080/", "protocol": "tcp"},
            {"host": "a", "name": "Camera 3", "link": "http://a:8081/", "protocol": "tcp"},
        ]);
        let hash = ridgewarden_client::event::list_hash(&ads);
        f.add(
            "mesh.list",
            json!({"hash": hash, "list": ads}),
            NOW - 4 * MIN,
        );
        f.add(
            "node.poll",
            json!({"node": "a", "ok": true, "via": "lan", "ads_hash": hash}),
            NOW - 4 * MIN,
        );
        let mut c = cfg();
        c.mirror.services = vec!["winlink".into()];
        let v = view(&f, &c, "a");
        assert_eq!(v.report.services.len(), 2, "the mesh page shows both");
        let files = files(&c, &v, NOW);
        let json: serde_json::Value =
            serde_json::from_str(&files.iter().find(|f| f.0 == "status.json").unwrap().1).unwrap();
        let names: Vec<&str> = json["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Winlink Gateway"]);
        assert_eq!(json["sites"][0]["lat"], 35.12);
        assert_eq!(json["sites"][0]["lon"], -120.65);
        assert!(files.iter().all(|f| !f.1.contains("Camera 3")));
    }

    #[test]
    fn the_public_copy_leaves_power_out_unless_asked() {
        let mut f = fake();
        cut_off_c(&mut f);
        let mut c = cfg();
        let mut v = view(&f, &c, "a");
        let a = v.report.sites.iter_mut().find(|s| s.node == "a").unwrap();
        a.power = Some(Power::Reading(Reading {
            text: "11.9 V".into(),
            at: NOW - MIN,
            alarms: vec!["Battery low: 11.9 V".into()],
        }));
        let has = |c: &Config, text: &str| files(c, &v, NOW).iter().any(|f| f.1.contains(text));
        assert!(
            !has(&c, "11.9 V"),
            "no reading or alarm anywhere in the copy"
        );
        let json: serde_json::Value = serde_json::from_str(
            &files(&c, &v, NOW)
                .into_iter()
                .find(|f| f.0 == "status.json")
                .unwrap()
                .1,
        )
        .unwrap();
        assert!(
            json["sites"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["power"].is_null())
        );

        c.mirror.power = true;
        assert!(has(&c, "Power 11.9 V: Battery low: 11.9 V."));
    }

    #[test]
    fn the_folder_stays_one_word_for_the_shell() {
        assert_eq!(quoted("/srv/mesh status"), "'/srv/mesh status'");
        assert_eq!(quoted("/srv/it's"), "'/srv/it'\\''s'");
    }
}
