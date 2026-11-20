# Changelog

All notable changes to ridgewarden-status are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The section for a version is what appears on its GitHub release page. Validated in CI by [patchnotes](https://github.com/Londopy/patchnotes).

## [Unreleased]

## [0.1.0] - 2026-11-20

### Added

- Runs as its own system user, `ridgewarden-status`, in the core's `ridgewarden` group: it can sign events through the core but not read the station key.
- Headline anyone can read: "All 7 sites up." or "North site is cut off since 14:02 UTC; link down: Hilltop relay to North site."
- "Cut off" comes from the mesh's own host list as seen from your node, not a guess from partial link data. Polls from this site's own core decide it, so another station can't speak for your node.
- Old data shows as UNKNOWN, never green.
- A site that goes silent stays on the page as cut off until `ridgewarden-status forget <node>`, instead of dropping off the list after a week. `expected_sites` lists sites to show before they are first heard from.
- Incidents with stages (investigating, identified, monitoring, resolved) and a timeline, posted with `ridgewarden-status incident`. Open ones show near the top of the page, and `/history` lists the last 90 days.
- One alert per open incident to email, ntfy, Discord or a webhook, resolved when the incident is, with an acknowledge link.
- Planned maintenance notices with `ridgewarden-status maintenance`, shown while under way and for 14 days before.
- The posting commands refuse when this site's key isn't in `banner_authors`, and the app warns at startup when subscriptions are set up but anyone may post.
- `/wall`, a full-screen view for a TV in an EOC.
- `/badge.svg` and `/embed.html` for club websites.
- `/aredn/` feed, so the banner, incidents and maintenance show on AREDN node pages as local messages, per node and per message group. Names that are neither a site nor a group get an empty file and aren't recorded.
- `fail_after` and `ok_after` settings, so services are judged the same way ridgewarden-health judges them.
- `/map`: the sites in their colors at the locations their nodes report, with their links, drawn without map tiles.
- Subscriptions for agency contacts by email, email-to-SMS gateway or webhook, for incidents, maintenance and sites cut off, all sites or only their own. Each channel gets one message a round with everything new in it, at most `max_per_hour` an hour, and backs off when it fails without holding up the others. A site warning waits for a second host list or 10 minutes, and news is judged by when this box first saw it. `ridgewarden-status subscribers test` checks the setup.
- A "save a copy" link that downloads the page as one file to read with no connection, and a service worker that shows the last copy, marked with its age, when the network fails on https.
- The internet mirror: a read-only copy of the pages pushed as static files by a command (rsync) or HTTP PUT. Every page, the wall, the embed and the saved copy turn grey with a warning once the copy is old; `status.json` says when it was made; `deploy/mirror` serves it behind Anubis, with a cron script that swaps in an UNKNOWN badge when the pushes stop. The copy names only allow-listed services and rounds locations. `ridgewarden-status export` writes the same files once.
- `/about`: page views per page (counts only), which AREDN nodes fetch their message files, silent sites, and whether the mirror and subscriptions are getting through, without naming contacts.
- `contact`, a line at the foot of the page saying who runs it and how to reach them.
- A push heartbeat that stops checking in turns its site yellow, labelled as a heartbeat. Links that only measure below their plan don't.
- Pages come from a view rebuilt in the background every 20 seconds, so a page never waits for the analysis.

[Unreleased]: https://github.com/Londopy/ridgewarden-status/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Londopy/ridgewarden-status/releases/tag/v0.1.0
