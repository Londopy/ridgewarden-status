#!/bin/sh
# Run every minute from cron on the web server:
#
#   * * * * * sh /srv/mirror/stale-badge.sh /srv/mirror/site 15
#
# When the mesh hasn't pushed a copy for that many minutes, the badge turns UNKNOWN instead
# of showing the last state for ever. The next push from the mesh puts the real one back.
# The pages and the embed say so by themselves; a badge is a plain image and can't.
set -eu

site=${1:-/srv/mirror/site}
minutes=${2:-15}

if [ -n "$(find "$site/status.json" -mmin +"$minutes")" ] &&
    ! cmp -s "$site/badge-unknown.svg" "$site/badge.svg"; then
    cp "$site/badge-unknown.svg" "$site/.badge.svg.tmp"
    mv "$site/.badge.svg.tmp" "$site/badge.svg"
fi
