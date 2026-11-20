# ridgewarden-status

[![CI](https://github.com/Londopy/ridgewarden-status/actions/workflows/ci.yml/badge.svg)](https://github.com/Londopy/ridgewarden-status/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Londopy/ridgewarden-status)](https://github.com/Londopy/ridgewarden-status/releases/latest)
[![changelog](https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/Londopy/ridgewarden-status/gh-pages/changelog-badge.json)](CHANGELOG.md)
[![License](https://img.shields.io/github/license/Londopy/ridgewarden-status)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org)
[![Linux](https://img.shields.io/badge/platform-Linux%20(Pi%20%7C%20x86__64)-555)](#building-from-source)

**"Is the mesh up?" answered in one glance, for people who have never heard of Babel.**

A ridgewarden app that serves a regional status page for served agencies (EOCs, ARES/RACES). It shows green, yellow or red per site and per key service, with a plain-language headline, signed notices, incidents and maintenance, a map, and a printable one-page layout. It is a read-only view built from the same data and code as ridgewarden-health and ridgewarden-linkdoctor, so the three never disagree.

It needs a [ridgewarden](https://github.com/Londopy/ridgewarden) core running on the same box. The core is the only part that polls the AREDN node and syncs with other sites; this app reads the core's event feed. Spec sheet 08 of the AREDN app suite.

## Features

- Headline anyone can read: "All 7 sites up." or "North site is cut off since 14:02 UTC; link down: Hilltop relay to North site."
- "Cut off" comes from the mesh's own host list as seen from your node, not a guess from partial link data. Every site serves the page, so when the mesh splits, each side shows the other as cut off.
- Old data shows as **UNKNOWN**, never green. A site that goes silent stays on the page as cut off, however long it has been quiet, until you say it's gone for good.
- Incident banner, signed by the station that set it: `ridgewarden-status banner "Hilltop on generator until Tuesday"`. Limit who may set it with `banner_authors`.
- Incidents that move through investigating, identified, monitoring and resolved, with a timeline, and a history page for the last 90 days.
- Planned maintenance notices, shown while under way and for 14 days before.
- One alert per incident to email, ntfy, Discord or a webhook, resolved when the incident is.
- Subscriptions for agency contacts: email, SMS through an email-to-SMS gateway, or a webhook, each choosing incidents, maintenance, sites cut off, or all three, for every site or only theirs.
- A map of the sites in their colors, drawn from the locations their nodes report. No map tiles, so it works with no internet.
- A wall display for a TV in an EOC, a status badge, and an HTML fragment for club websites.
- For phones in the field: a saved copy of the page that opens with no connection, and an offline cache where the browser allows one.
- An internet mirror: a read-only copy pushed as plain files to a web server off the mesh, with an example for serving it behind Anubis. The app itself never faces the internet.
- Notices, incidents and maintenance show up on AREDN node pages through the node's own message feed.
- An about page that counts how often each page is viewed (counts only, nothing about who) and whether the mirror and subscriptions are getting through.
- No JavaScript needed: loads on old phones and prints on one page.
- `/api/status` returns the same data as JSON.

## Install

On a Raspberry Pi or Linux box that already runs the [ridgewarden core](https://github.com/Londopy/ridgewarden):

```sh
curl -fsSL https://github.com/Londopy/ridgewarden-status/releases/latest/download/install.sh | sudo sh
```

The script checks the download against the release key your core already trusts. You can also take the `.deb` from the [releases page](https://github.com/Londopy/ridgewarden-status/releases/latest), but `apt install ./file.deb` checks nothing, so check it first: download `SHA256SUMS.txt` and `SHA256SUMS.txt.sig` from the same release into the same folder, run `ridgewarden verify-release SHA256SUMS.txt` and `sha256sum -c --ignore-missing SHA256SUMS.txt`, then `sudo apt install ./ridgewarden-status_<version>_arm64.deb` (`_amd64` on a PC). Either way `/etc/ridgewarden/status.toml` is written if missing, and the service starts once the core is set up. The page is on port 8793.

Public copies sit behind [Anubis](https://github.com/TecharoHQ/anubis) to keep AI scrapers off; see `deploy/mirror`.

Every release also has each download as a `.torrent`, with its GitHub download as the web seed, and signed `SHA256SUMS.txt` checksums.

The service runs as its own user, `ridgewarden-status`, in the core's `ridgewarden` group, so it can sign events through the core but can't read the station's key. Run commands that touch the app's data as that user, so files keep the right owner: `sudo -u ridgewarden-status ridgewarden-status --config /etc/ridgewarden/status.toml <command>`.

## Usage

```sh
ridgewarden-status init --config /etc/ridgewarden/status.toml
ridgewarden-status run  --config /etc/ridgewarden/status.toml
```

The page is on `http://<pi>:8793/`, with `/history`, `/map`, `/wall` and `/about` next to it. Add it to the core's dashboard with an `[[app]]` entry in `ridgewarden.toml`.

The banner, incidents and maintenance notices are signed by this site's core under its callsign, so posting them needs the core's `local.token`. Every site running ridgewarden-status shows them. When `banner_authors` is set and this site's key isn't in it, the commands refuse to post, since every page with that list would ignore the post.

### Silent sites

A site that stops being heard from stays on the page as cut off, with when it was last heard, so a dead site can't quietly drop off the list and leave the headline green. `/about` lists the sites silent for over a week. When one is gone for good:

```sh
ridgewarden-status forget tst-oldsite
```

It comes back if it is heard from again. `expected_sites` in the config lists sites to show even before they are first heard from.

### Incidents

```sh
ridgewarden-status incident open "North site is off the air" --sites tst-north --text "Checking power at the site"
ridgewarden-status incident list
ridgewarden-status incident update 7ab4d376 --stage identified --text "Generator out of fuel"
ridgewarden-status incident update 7ab4d376 --stage resolved --text "Refueled, link back up"
```

The stages are `investigating` (the default when opening), `identified`, `monitoring` and `resolved`. An id can be cut down to its first 8 characters. Open incidents show near the top of the page with their latest update, and `/history` lists the last 90 days with every update.

### Maintenance

```sh
ridgewarden-status maintenance "Tower work at the hilltop; North site may drop" \
    --from 2026-11-22T15:00:00Z --to 2026-11-22T19:00:00Z --sites tst-hilltop
ridgewarden-status maintenance list
ridgewarden-status maintenance cancel 5a2a90a6
```

### Map

`/map` places each site at the latitude and longitude its node reports, in its current color, with its links drawn solid while up and dashed red when down. Sites whose nodes report no location are listed under the map. Locations come from the polls the cores already make, so a site appears once its node's location is set in the node's own settings and a core (or the crawl) polls it.

### Wall display

`http://<pi>:8793/wall` is for a TV in an EOC: the headline, notices and large site tiles, nothing to click, reloading every 30 seconds. Up to about 20 sites fit a 1080p screen without scrolling. It follows the screen's light or dark setting.

### In the field with no connection

The status page has a "save a copy" link that downloads it as one file (`mesh-status-2026-11-20-1402.html`). The file opens on a phone with no connection, says when it was saved, and doesn't pretend to update.

Pages served over https, or from `localhost`, also register a small service worker (`/sw.js`). It saves each page as it loads, and when the network fails it shows the saved copy with a note saying how old it is. Browsers only allow service workers on https, so on the mesh's plain-http addresses this works on the internet mirror but not on the page itself; use the saved copy there. Without JavaScript the page works as before.

### Badge and embed

```html
<img src="http://tst-hilltop-pi.local.mesh:8793/badge.svg" alt="Mesh status">
<img src="http://tst-hilltop-pi.local.mesh:8793/badge.svg?site=tst-hilltop" alt="Hilltop relay status">
<iframe src="http://tst-hilltop-pi.local.mesh:8793/embed.html" title="Mesh status" width="500" height="420"></iframe>
```

The badge reads like "mesh | OK" in the headline's color, or one site's state with `?site=`. `/embed.html` is a small fragment with its own inline styles: the headline, open incidents, every site and when it was updated. Any site may load these and `/api/status`, and they're cached for a minute. Visitors' browsers fetch them straight from this app, so they only work where people can reach it; for a club website on the internet, point them at the mirror (`badge.svg`, `embed.html` and `status.json` there).

### Notices on AREDN node pages

AREDN nodes can show messages from a local server. In each node's settings set "Local Message URL" to `http://tst-hilltop-pi.local.mesh:8793/aredn` (and "Message Groups" if you use groups), or from a shell on the node:

```sh
uci set aredn.@alerts[0].localpath=http://tst-hilltop-pi.local.mesh:8793/aredn
uci set aredn.@alerts[0].groups=coast
uci commit aredn
```

Repeat those with `uci -c /etc/config.mesh` as well, or the node drops the change the next time its settings are saved. About once an hour the node fetches its own `<node>.txt`, a `<group>.txt` for each of its groups, and `all.txt`. `all.txt` has the banner, open incidents and maintenance under way; the node and group files have the incidents and maintenance (under way or coming up) that list those sites. Groups are set in `[aredn_groups]`. A file is empty when there's nothing to say, and the node then shows no message. The about page lists the nodes and groups that asked for their file in the last 30 days.

### Alerts

Each open incident is one alert to email, ntfy, Discord or a webhook. It goes out when the incident opens and again when it's resolved, and it's critical (sent even in quiet hours) if a site it lists is down. With `repeat_hours` set it repeats until someone acknowledges it from the link in the notice, which needs `public_url`. Set alerts up on one site only, or every site sends the same alert.

### Subscriptions

Agency contacts get a message when an incident is opened, updated or resolved, when maintenance is posted or called off, and when a site is cut off and when it's back. Each contact picks topics and, if they like, only their own sites:

```toml
[subscriptions]
from = "mesh-status@example.org"
link = "https://status.example.org/"         # added to every message, e.g. the mirror

[[subscriptions.contact]]
name = "County EOC duty desk"
email = ["eoc-duty@example.org"]
sms = ["5550100@sms.example.net"]            # the carrier's email-to-SMS address: one short line
topics = ["incidents", "maintenance", "sites"]

[[subscriptions.contact]]
name = "Hospital radio room"
webhook = "http://10.1.2.3:8080/sms"         # JSON, the same as alert webhooks
topics = ["sites"]
sites = ["tst-north"]                        # only news about these sites
```

Mail goes out through the box's own `sendmail`, set up the same way as for [ridgewarden-health's email alerts](https://github.com/Londopy/ridgewarden-health#email) (msmtp, or Postfix with a systemd drop-in). Check the setup with:

```sh
ridgewarden-status subscribers test                       # every contact
ridgewarden-status subscribers test "County EOC duty desk"
```

How messages go out:

- Each channel gets at most one message a round (once a minute), with everything new in it: "3 sites cut off: Hilltop relay, North site, Valley". It gets at most `max_per_hour` messages an hour (6 by default); news beyond that waits and goes out together.
- A site counts as cut off once it is red on a second host list from the node, or for 10 minutes, so one bad poll warns nobody. It counts as back once it has been seen reachable for 15 minutes, so a flapping link sends one warning and one all-clear. "Unknown" (no fresh data) sends nothing either way, and time spent unknown doesn't count toward an all-clear.
- News is judged by when this box first saw it, so a post from a station whose clock is wrong still goes out. A contact added later hears what happens from then on, not old news, and nothing this box first saw over a day ago goes out.
- Every channel sends at the same time, so a dead mail server holds up nobody else, and a channel that fails waits a minute before trying again, then twice as long each time, up to an hour. Alerts run on their own, so a slow subscription never delays one.

Set subscriptions up on one site only. The about page counts the channels and says if any are failing; `subscribers test` and the log say which and why.

The messages report network status only. The mesh is not a channel for agency business traffic.

### Internet mirror

For people who need the status but can't reach the mesh, the app can push a read-only copy to a web server off the mesh: the status page, history, map, wall, saved copy, embed, badge, `status.json` and the service worker, as plain files. Nothing on the internet ever talks to the app; the web server only holds files.

```toml
[mirror]
command = "rsync -a --delete -e 'ssh -i /etc/ridgewarden/mirror_key -o UserKnownHostsFile=/var/lib/ridgewarden-status/known_hosts' {dir}/ mirror@web.example.org:/srv/mirror/site/"
every_s = 60
```

The files are written to `/var/lib/ridgewarden-status/mirror` (or `dir`) and then `command` runs with `{dir}` replaced by that folder. Instead of a command, `put_url` sends each file with an HTTP PUT (WebDAV, for example), with a bearer token from `token_file` if set. The service may only write under `/var/lib/ridgewarden-status`; to write `dir` somewhere else, add that folder with a systemd drop-in (`ReadWritePaths=`).

The copy names only the services listed in `[mirror] services` (none by default: a service list says a lot about a network), and rounds site locations to about a kilometre. The mesh page itself shows everything.

What people see when the mesh stops pushing, because it is split or the box is off:

- The pages, the wall and the embed say when the copy was made. Once it is 10 minutes old (or five pushes, if longer), a small script turns that note into a warning ("This copy is 2 hours old: the mesh has stopped sending updates, so treat everything here as unknown") and greys every color: headline, tiles, wall blocks, pills, the map. An embed left open on a club page checks again every minute. Without JavaScript the note still says when the copy was made.
- The badge is a plain image and can't tell. `deploy/mirror/stale-badge.sh`, run each minute from cron on the web server, swaps in `badge-unknown.svg` once `status.json` is more than 15 minutes old; the next push puts the real badge back.
- `status.json` has `made`, when the copy was made (Unix milliseconds), so a program can treat an old copy as unknown.

`deploy/mirror` serves the copy with Caddy behind Anubis. Copy it to `/srv/mirror` on the web server, so the pushes above land in `/srv/mirror/site`, then:

```sh
cd /srv/mirror
cp .env.example .env    # then put `openssl rand -hex 32` in it
# set your domain in Caddyfile, and in COOKIE_DOMAIN and REDIRECT_DOMAINS in compose.yml
docker compose up -d
( crontab -l; echo "* * * * * sh /srv/mirror/stale-badge.sh /srv/mirror/site 15" ) | crontab -
```

The images are pinned by digest. The bot policy lets `badge.svg`, `embed.html` and `status.json` through without the challenge, since other sites' pages and programs load them, and Caddy lets any site's scripts read `status.json`. To copy the files once by hand, for a USB stick or a web server you upload to yourself: `ridgewarden-status export <folder>`.

## Configuration

`/etc/ridgewarden/status.toml` (`init` writes a commented one):

```toml
title = "County mesh"
key_services = ["winlink", "chat"]   # empty shows every service
banner_authors = []                  # public keys allowed to post notices and incidents
fail_after = 3                       # the same as ridgewarden-health's [probe] settings
ok_after = 2
contact = "Run by the county backbone team: mesh-status@example.org"
expected_sites = ["tst-hilltop", "tst-north"]   # always shown, even before first heard

[core]
url = "http://127.0.0.1:8790"
token_file = "/var/lib/ridgewarden/local.token"   # only needed to post

[site_names]
"tst-hilltop" = "Hilltop relay"

[aredn_groups]
coast = ["tst-hilltop", "tst-valley"]

[alerts]
ntfy = "http://10.1.2.3/mesh-alerts"
public_url = "http://tst-hilltop-pi.local.mesh:8793"
quiet_hours = "22-6"                 # UTC; only critical alerts go out then
repeat_hours = 4
```

`contact` is shown at the foot of the page and the mirror, so agencies know who to ask for what they need. `[mirror]` (with `services`, the allow-list for the public copy) and `[subscriptions]` (with `max_per_hour`) are described above.

The app believes only events from stations the core trusts: its own key, plus the keys listed under `[trust]` in `ridgewarden.toml`. With `banner_authors` set, only those stations can post or update the banner, incidents and maintenance. A push heartbeat that stops checking in (see ridgewarden-health) turns its receiving site yellow, labelled as a heartbeat.

## Limits

- Agency feedback, the spec's first change and its open question, is for people: show the page to the agencies you serve and ask what they need. The `contact` line and the view counts on the about page help, but no software can do the asking.
- The success measure (an agency referring to the page in an exercise or briefing) can't be seen from here. The about page shows the nearest proxy: how often each page, the wall and the saved copy were opened.
- Service workers only run on https or localhost, so the offline cache works on the internet mirror and on https setups, not on the mesh's plain-http pages; the saved copy covers those.
- SMS goes through carriers' email-to-SMS gateways or a webhook to a gateway you run. The app doesn't drive a GSM modem itself, and some carriers have closed their gateways, so check yours with `subscribers test`.
- Email and SMS were tested against a local webhook and with the message checks in the client crate, not through a real mail server or carrier.
- The map uses only locations the nodes themselves report; a node without one is listed under the map instead.

## Building from source

This repo builds against the core's shared client crate, so check the core out next to it:

```sh
git clone https://github.com/Londopy/ridgewarden
git clone https://github.com/Londopy/ridgewarden-health
git clone https://github.com/Londopy/ridgewarden-linkdoctor
git clone https://github.com/Londopy/ridgewarden-status
cd ridgewarden-status
cargo build --release
cargo test
```

## Contributing

Issues and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md), and add a line under `Unreleased` in [CHANGELOG.md](CHANGELOG.md) for anything a user would notice.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
