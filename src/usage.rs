//! How much the page is used: views per page and day, and which AREDN nodes fetch their
//! message files. Counts only, nothing about who looked. It is the evidence the spec's
//! success measure asks for: whether anyone opens this during an exercise or briefing.

use anyhow::Result;
use rusqlite::{Connection, params};

const DAY: i64 = 86_400_000;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS views (
    day  INTEGER NOT NULL,
    page TEXT NOT NULL,
    n    INTEGER NOT NULL,
    PRIMARY KEY (day, page)
);
CREATE TABLE IF NOT EXISTS feed_names (
    name TEXT PRIMARY KEY,
    last INTEGER NOT NULL
);";

/// The pages that are counted, and what the about page calls them.
pub const PAGES: [(&str, &str); 9] = [
    ("status", "Status page"),
    ("history", "Incident history"),
    ("map", "Map"),
    ("wall", "Wall display"),
    ("copy", "Saved copies"),
    ("embed", "Embedded on other sites"),
    ("badge", "Badge"),
    ("api", "JSON for other programs"),
    ("aredn", "AREDN message files"),
];

pub struct Usage {
    /// (page, views in the last 7 days, views in the last 30), busiest first
    pub views: Vec<(String, i64, i64)>,
    /// node and group message files asked for in the last 30 days, with the latest time
    pub feed: Vec<(String, i64)>,
}

pub fn count(conn: &Connection, page: &str, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO views (day, page, n) VALUES (?1, ?2, 1)
         ON CONFLICT(day, page) DO UPDATE SET n = n + 1",
        params![now - now.rem_euclid(DAY), page],
    )?;
    Ok(())
}

/// A node or group asked for its message file; all.txt says nothing about who.
pub fn feed_asked(conn: &Connection, name: &str, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO feed_names (name, last) VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET last = excluded.last",
        params![name, now],
    )?;
    Ok(())
}

pub fn summary(conn: &Connection, now: i64) -> Result<Usage> {
    let today = now - now.rem_euclid(DAY);
    let mut st = conn.prepare(
        "SELECT page, sum(CASE WHEN day > ?1 THEN n ELSE 0 END), sum(n) FROM views
         WHERE day > ?2 GROUP BY page ORDER BY 3 DESC, page",
    )?;
    let views = st
        .query_map(params![today - 7 * DAY, today - 30 * DAY], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut st = conn.prepare("SELECT name, last FROM feed_names WHERE last > ?1 ORDER BY name")?;
    let feed = st
        .query_map([now - 30 * DAY], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(Usage { views, feed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_add_up_by_page_and_age_out() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        let now = 1_800_000_000_000;
        for _ in 0..3 {
            count(&c, "wall", now).unwrap();
        }
        count(&c, "status", now - 10 * DAY).unwrap();
        count(&c, "map", now - 40 * DAY).unwrap();
        feed_asked(&c, "tst-hilltop", now - DAY).unwrap();
        feed_asked(&c, "coast", now - 31 * DAY).unwrap();
        let u = summary(&c, now).unwrap();
        assert_eq!(
            u.views,
            vec![("wall".to_string(), 3, 3), ("status".to_string(), 0, 1)]
        );
        assert_eq!(u.feed, vec![("tst-hilltop".to_string(), now - DAY)]);
    }
}
