# Changelog

All notable changes to ridgewarden-status are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The section for a version is what appears on its GitHub release page. Validated in CI by [patchnotes](https://github.com/Londopy/patchnotes).

## [Unreleased]

### Added

- Each site's tile, the wall and the map say when the site was last heard, and so do subscription messages about a site cut off. The mirror's wall gives the time of day as well, since a copy is read long after it is made, and says when the copy was made in its header, leaving the sites their room.
- Power at each site from a sitemon hub, set up under `[power]`: the reading on each tile, on the wall and in `/api/status`, and a battery alarm from the hub on the tile's power line, turning the tile yellow. An alarm keeps the tile yellow once its reading is older than `fresh_s`, saying how old; any other old reading, or a hub this box can't reach, shows as "Power unknown". The hub's text is cut short and stripped of control and direction-changing characters, and a group that fails keeps its sites' last readings.
- `/about` says whether the site monitor's hub answers and which mapped sites it gives no reading for; the log says so too, once each time that list changes.
- `[mirror] power`: the public copy leaves out power readings and alarms unless it is set.

### Changed

- Breaking: an empty `banner_authors` now means nobody may post, instead of any station the core trusts. A site upgraded with an empty list stops showing its banner, incidents and maintenance, and the alerts of its open incidents go out as resolved, until the posting stations' keys are listed. The posting commands say so and print this site's key to add, and the app won't start with an entry that isn't a key (64 hex characters).
- The page prints on one sheet of Letter or A4 with 20 sites, a few notices and up to about 28 services: smaller type, tiles in four columns, services in two, and times without "min ago".
- A cut-off site's `reason` no longer carries when it was last heard; `last_seen` has it, and the tile shows it once.
- Services take their colors from ridgewarden-health, so a service listed but never seen working is yellow here as it is there.
- The wall shows at most five lines of notices, the last saying how many more are on the status page.

### Fixed

- The wall no longer loses its bottom row of sites when the notices or reasons run long, or on a 4:3 screen: the rows shrink to share the screen, and a tile hides whole lines instead of cutting them through the middle.

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
- `/badge.svg` and `/embed.html` for other websites.
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
