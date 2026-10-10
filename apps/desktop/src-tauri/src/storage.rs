use chrono::{DateTime, Local};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::models::{DailyPoint, RateWindow};

pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS cursors(file_id TEXT PRIMARY KEY,file_hash TEXT NOT NULL,offset INTEGER NOT NULL,last_total INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS events(event_id TEXT PRIMARY KEY,file_id TEXT NOT NULL,event_offset INTEGER NOT NULL,total_tokens INTEGER NOT NULL,delta_tokens INTEGER NOT NULL,day TEXT NOT NULL); CREATE TABLE IF NOT EXISTS daily(day TEXT PRIMARY KEY,tokens INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS window_state(id INTEGER PRIMARY KEY CHECK(id=1),x INTEGER NOT NULL,y INTEGER NOT NULL,expanded INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS quota_cache(id INTEGER PRIMARY KEY CHECK(id=1),windows_json TEXT NOT NULL,fetched_at TEXT NOT NULL);",
    )
}

pub(crate) fn load_window_state(db: &Connection) -> Option<(i32, i32, bool)> {
    db.query_row(
        "SELECT x,y,expanded FROM window_state WHERE id=1",
        [],
        |row| {
            Ok((
                row.get::<_, i32>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, i64>(2)? != 0,
            ))
        },
    )
    .ok()
}

pub(crate) fn save_window_position(
    db: &Connection,
    x: i32,
    y: i32,
    expanded: bool,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO window_state(id,x,y,expanded) VALUES(1,?,?,?) ON CONFLICT(id) DO UPDATE SET x=excluded.x,y=excluded.y,expanded=excluded.expanded",
        params![x, y, expanded as i64],
    )?;
    Ok(())
}

pub(crate) fn save_window_expanded(db: &Connection, expanded: bool) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO window_state(id,x,y,expanded) VALUES(1,0,0,?) ON CONFLICT(id) DO UPDATE SET expanded=excluded.expanded",
        [expanded as i64],
    )?;
    Ok(())
}

pub(crate) fn load_rate_cache(db: &Connection) -> Option<(Vec<RateWindow>, DateTime<Local>)> {
    let (windows_json, fetched_at): (String, String) = db
        .query_row(
            "SELECT windows_json,fetched_at FROM quota_cache WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;
    let windows = serde_json::from_str(&windows_json).ok()?;
    let fetched_at = DateTime::parse_from_rfc3339(&fetched_at)
        .ok()?
        .with_timezone(&Local);
    Some((windows, fetched_at))
}

pub(crate) fn save_rate_cache(
    db: &Connection,
    windows: &[RateWindow],
    fetched_at: DateTime<Local>,
) -> rusqlite::Result<()> {
    let windows_json = serde_json::to_string(windows)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    db.execute(
        "INSERT INTO quota_cache(id,windows_json,fetched_at) VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET windows_json=excluded.windows_json,fetched_at=excluded.fetched_at",
        params![windows_json, fetched_at.to_rfc3339()],
    )?;
    Ok(())
}

pub(crate) fn delete_rate_cache(db: &Connection) -> rusqlite::Result<usize> {
    db.execute("DELETE FROM quota_cache WHERE id=1", [])
}

pub(crate) fn cleanup_old_daily(db: &Connection, keep_days: i64) -> rusqlite::Result<usize> {
    let cutoff = (Local::now().date_naive()
        - chrono::Days::new(keep_days.saturating_sub(1) as u64))
    .to_string();
    db.execute("DELETE FROM events WHERE day < ?", [&cutoff])?;
    db.execute("DELETE FROM daily WHERE day < ?", [cutoff])
}

pub(crate) fn clear_token_data(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("DELETE FROM events; DELETE FROM daily;")
}

pub(crate) fn digest(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(crate) fn sums(db: &Connection, days: i64) -> rusqlite::Result<i64> {
    let since =
        (Local::now().date_naive() - chrono::Days::new(days.saturating_sub(1) as u64)).to_string();
    db.query_row(
        "SELECT COALESCE(SUM(tokens),0) FROM daily WHERE day>=?",
        [since],
        |row| row.get(0),
    )
}

pub(crate) fn series(db: &Connection) -> rusqlite::Result<Vec<DailyPoint>> {
    let today = Local::now().date_naive();
    (0..7)
        .rev()
        .map(|offset| {
            let day = (today - chrono::Days::new(offset)).to_string();
            db.query_row(
                "SELECT COALESCE(tokens,0) FROM daily WHERE day=?",
                [&day],
                |row| {
                    Ok(DailyPoint {
                        day: day.clone(),
                        tokens: row.get(0)?,
                    })
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{cleanup_old_daily, initialize, load_rate_cache, save_rate_cache};
    use crate::models::RateWindow;
    use chrono::{Duration, Local};
    use rusqlite::Connection;
    use tempfile::tempdir;

    fn live_window() -> RateWindow {
        RateWindow {
            label: "5 小时".into(),
            used_percent: Some(25.0),
            remaining_percent: Some(75.0),
            resets_at: Some(123),
            window_minutes: Some(300),
        }
    }

    #[test]
    fn cleanup_removes_only_data_older_than_thirty_days() {
        let connection = Connection::open_in_memory().unwrap();
        initialize(&connection).unwrap();
        connection
            .execute("INSERT INTO daily(day,tokens) VALUES('2000-01-01',10)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO daily(day,tokens) VALUES(?,20)",
                [Local::now().date_naive().to_string()],
            )
            .unwrap();

        cleanup_old_daily(&connection, 30).unwrap();

        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM daily", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn normalized_rate_cache_survives_database_reopen() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cache.sqlite");
        let fetched_at = Local::now() - Duration::seconds(1);
        {
            let connection = Connection::open(&path).unwrap();
            initialize(&connection).unwrap();
            save_rate_cache(&connection, &[live_window()], fetched_at).unwrap();
        }

        let connection = Connection::open(&path).unwrap();
        initialize(&connection).unwrap();
        let (windows, loaded_at) = load_rate_cache(&connection).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].remaining_percent, Some(75.0));
        assert_eq!(loaded_at.timestamp(), fetched_at.timestamp());
    }
}
