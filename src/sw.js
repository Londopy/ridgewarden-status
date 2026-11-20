// Keeps the status pages readable with no connection: every page is saved as it loads,
// and when the network fails the saved copy is shown with a note saying how old it is.
const CACHE = "mesh-status";

self.addEventListener("install", (e) => {
  self.skipWaiting();
  e.waitUntil(live(new Request(self.registration.scope)).catch(() => {}));
});

self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

self.addEventListener("fetch", (e) => {
  const r = e.request;
  if (r.method !== "GET" || r.mode !== "navigate" || new URL(r.url).pathname.includes("/ack/")) {
    return;
  }
  e.respondWith(live(r).catch(() => saved(r)));
});

async function live(req) {
  const stop = new AbortController();
  const timer = setTimeout(() => stop.abort(), 15000);
  try {
    const res = await fetch(req, { signal: stop.signal });
    const body = res.ok ? await res.clone().text() : "";
    // only our own pages, never an error page or a proxy's challenge page
    if (body.includes("serviceWorker.register")) {
      const copy = new Response(body, {
        headers: { "content-type": "text/html; charset=utf-8", "x-saved": String(Date.now()) },
      });
      await (await caches.open(CACHE)).put(req, copy);
    }
    return res;
  } finally {
    clearTimeout(timer);
  }
}

// only the page that was asked for: another page in its place would say the wrong thing
async function saved(req) {
  const html = "text/html; charset=utf-8";
  const res = await caches.match(req);
  if (!res) {
    return new Response(
      "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\">" +
        "<title>No connection</title><p>No connection, and this page hasn't been opened on " +
        "this device before, so there is no saved copy of it.</p>",
      { status: 503, headers: { "content-type": html } },
    );
  }
  const at = Number(res.headers.get("x-saved"));
  const mins = Math.max(0, Math.round((Date.now() - at) / 60000));
  const age = mins < 120 ? mins + " min" : Math.round(mins / 60) + " h";
  const when = new Date(at).toISOString().slice(0, 16).replace("T", " ") + " UTC";
  const note =
    "<div class=banner>No connection. This copy was saved " + age + " ago (" + when +
    "), so it may be out of date.</div>";
  const body = (await res.text()).replace("<main>", "<main>" + note);
  return new Response(body, { headers: { "content-type": html } });
}
