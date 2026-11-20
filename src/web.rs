//! The app's HTTP side: the pages from `pages`, embeds for other sites, the files AREDN
//! nodes show as messages, the offline service worker, and alert acknowledgements. Pages
//! come from the view the background task keeps current, so a request never waits for the
//! analysis, and the little database work left runs off the async threads.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ridgewarden_client::alerts;
use ridgewarden_client::util::{esc, now_ms};

use crate::pages::{self, APP, Ctx, SERVICE_WORKER};
use crate::{App, Sent, View, aredn_feed, embed, subscribe, usage};

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(status_page))
        .route("/status", get(status_page))
        .route("/history", get(history_page))
        .route("/map", get(map_page))
        .route("/wall", get(wall_page))
        .route("/copy", get(copy_page))
        .route("/about", get(about_page))
        .route("/sw.js", get(service_worker))
        .route("/badge.svg", get(badge_svg))
        .route("/embed.html", get(embed_html))
        .route("/aredn/{name}", get(aredn))
        .route("/ack/{token}", get(ack_page).post(ack_post))
        .route("/api/status", get(status_json))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(app)
}

struct Fail(anyhow::Error);

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        tracing::error!("request failed: {:#}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for Fail {
    fn from(e: E) -> Self {
        Fail(e.into())
    }
}

fn ctx<'a>(app: &'a App, v: &'a View) -> Ctx<'a> {
    Ctx {
        cfg: &app.cfg,
        site: &APP,
        anchor: &v.anchor,
    }
}

/// Run a small piece of database work off the async threads. It is bookkeeping, so a
/// failure is logged and the page served anyway.
fn record(
    app: &Arc<App>,
    what: &'static str,
    work: impl FnOnce(&App) -> anyhow::Result<()> + Send + 'static,
) {
    let app = app.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = work(&app) {
            tracing::warn!("{what}: {e:#}");
        }
    });
}

fn seen(app: &Arc<App>, page: &'static str) {
    record(app, "counting a page view", move |a| {
        usage::count(&a.db(), page, now_ms())
    });
}

/// For what other sites embed or fetch: anyone may load it, and it keeps for a minute.
fn shared(body: impl IntoResponse) -> Response {
    (
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::CACHE_CONTROL, "max-age=60"),
        ],
        body,
    )
        .into_response()
}

async fn status_page(State(app): State<Arc<App>>) -> Result<Html<String>, Fail> {
    let v = app.view().await?;
    seen(&app, "status");
    Ok(Html(pages::status(&ctx(&app, &v), &v.report)))
}

async fn history_page(State(app): State<Arc<App>>) -> Result<Html<String>, Fail> {
    let v = app.view().await?;
    seen(&app, "history");
    Ok(Html(pages::history(
        &ctx(&app, &v),
        &v.history,
        v.report.generated,
    )))
}

async fn map_page(State(app): State<Arc<App>>) -> Result<Html<String>, Fail> {
    let v = app.view().await?;
    seen(&app, "map");
    Ok(Html(pages::map(&ctx(&app, &v), &v.report)))
}

async fn wall_page(State(app): State<Arc<App>>) -> Result<Html<String>, Fail> {
    let v = app.view().await?;
    seen(&app, "wall");
    Ok(Html(pages::wall(&ctx(&app, &v), &v.report)))
}

async fn copy_page(State(app): State<Arc<App>>) -> Result<Response, Fail> {
    let v = app.view().await?;
    seen(&app, "copy");
    let file = format!(
        "attachment; filename=\"{}\"",
        pages::copy_name(v.report.generated)
    );
    Ok((
        [(header::CONTENT_DISPOSITION, file)],
        Html(pages::copy(&app.cfg, &v.report)),
    )
        .into_response())
}

async fn about_page(State(app): State<Arc<App>>) -> Result<Html<String>, Fail> {
    let v = app.view().await?;
    let a = app.clone();
    let (usage, delivered) = tokio::task::spawn_blocking(move || {
        let db = a.db();
        anyhow::Ok((usage::summary(&db, now_ms())?, subscribe::health(&db)?))
    })
    .await??;
    let mirror = app
        .cfg
        .mirror
        .enabled()
        .then(|| *app.pushed.lock().unwrap_or_else(|e| e.into_inner()));
    let subs: Vec<Sent> = subscribe::targets(&app.cfg.subscriptions)
        .iter()
        .map(|t| delivered.get(&t.id).copied().unwrap_or_default())
        .collect();
    let page = pages::about(&ctx(&app, &v), &v.report, &usage, mirror, &subs, now_ms());
    Ok(Html(page))
}

async fn service_worker() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        SERVICE_WORKER,
    )
        .into_response()
}

async fn badge_svg(
    State(app): State<Arc<App>>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, Fail> {
    let v = app.view().await?;
    seen(&app, "badge");
    Ok(
        match embed::badge_for(&v.report, &app.cfg, q.get("site").map(String::as_str)) {
            Some(svg) => shared(([(header::CONTENT_TYPE, "image/svg+xml")], svg)),
            None => (StatusCode::NOT_FOUND, "site must be a node name").into_response(),
        },
    )
}

async fn embed_html(State(app): State<Arc<App>>) -> Result<Response, Fail> {
    let v = app.view().await?;
    seen(&app, "embed");
    Ok(shared(Html(embed::fragment(&v.report, &app.cfg))))
}

/// A node or group asks for its message file. Names that are neither a site nor a group
/// get the empty answer at once and leave no trace, so anonymous requests can't fill the
/// database.
async fn aredn(State(app): State<Arc<App>>, Path(name): Path<String>) -> Result<Response, Fail> {
    let Some(base) = aredn_feed::base(&name) else {
        return Ok((
            StatusCode::NOT_FOUND,
            "the feed has all.txt, <node>.txt and <group>.txt, in lowercase",
        )
            .into_response());
    };
    let text = |t: String| ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], t);
    let v = app.view().await?;
    if !aredn_feed::known(&v.report, &app.cfg, base) {
        return Ok(text(String::new()).into_response());
    }
    seen(&app, "aredn");
    if base != "all" {
        let name = base.to_string();
        record(&app, "noting who asked for a message file", move |a| {
            usage::feed_asked(&a.db(), &name, now_ms())
        });
    }
    Ok(text(aredn_feed::text(&v.report, &app.cfg, base)).into_response())
}

async fn ack_page(State(app): State<Arc<App>>) -> Result<Html<String>, Fail> {
    let v = app.view().await?;
    // a GET only shows the button, so link previews in chat apps can't acknowledge anything;
    // the form posts back to this same address
    let b = "<h1>Acknowledge alert</h1><p>Acknowledging stops repeats of this alert. You'll \
             still hear when it's resolved.</p><form method=post><button type=submit>Acknowledge\
             </button></form>";
    Ok(Html(ctx(&app, &v).page("", "Acknowledge", b, false)))
}

async fn ack_post(
    State(app): State<Arc<App>>,
    Path(token): Path<String>,
) -> Result<Response, Fail> {
    let v = app.view().await?;
    let a = app.clone();
    let done =
        tokio::task::spawn_blocking(move || alerts::ack(&a.db(), &token, now_ms())).await??;
    let (code, b) = match done {
        Some(title) => (
            StatusCode::OK,
            format!(
                "<h1>Acknowledged</h1><p>{}</p><p><a href=\"/\">Status</a></p>",
                esc(&title)
            ),
        ),
        None => (
            StatusCode::NOT_FOUND,
            "<h1>Nothing to acknowledge</h1><p>That alert is already resolved, or the link is wrong.</p>"
                .to_string(),
        ),
    };
    let page = ctx(&app, &v).page("", "Acknowledge", &b, false);
    Ok((code, Html(page)).into_response())
}

async fn status_json(State(app): State<Arc<App>>) -> Result<Response, Fail> {
    let v = app.view().await?;
    seen(&app, "api");
    Ok(shared(Json(v.report.to_json())))
}
