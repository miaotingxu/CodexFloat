use chrono::{DateTime, Local};
use rusqlite::Connection;
use serde::Deserialize;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
};

use crate::{
    models::DailyPoint,
    paths,
    state::Store,
    storage::{self, SessionEvent},
};

#[derive(Debug, Deserialize)]
struct TokenInfo {
    total_token_usage: Option<TotalUsage>,
}

#[derive(Debug, Deserialize)]
struct TotalUsage {
    total_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct LinePayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    info: Option<TokenInfo>,
}

#[derive(Debug, Deserialize)]
struct EventLine {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<LinePayload>,
}

pub(crate) struct LocalTokenUsage {
    pub(crate) today: i64,
    pub(crate) last7: i64,
    pub(crate) last30: i64,
    pub(crate) daily: Vec<DailyPoint>,
    pub(crate) coverage: String,
}

pub(crate) struct TokenSnapshot {
    pub(crate) today: i64,
    pub(crate) last7: i64,
    pub(crate) last30: i64,
    pub(crate) daily: Vec<DailyPoint>,
    pub(crate) token_coverage: String,
    pub(crate) usage_source: String,
    pub(crate) today_available: bool,
}

fn file_id(path: &Path) -> String {
    storage::digest(&path.to_string_lossy())
}

pub(crate) fn scan_file(db: &Connection, path: &Path) -> rusqlite::Result<usize> {
    let id = file_id(path);
    let current_len = match fs::metadata(path) {
        Ok(metadata) => metadata.len() as i64,
        Err(_) => return Ok(0),
    };
    let (old_offset, last_total) = storage::load_cursor(db, &id);
    let truncated = old_offset > current_len;
    if truncated {
        storage::remove_file_events(db, &id)?;
    }
    let start = if truncated { 0 } else { old_offset };
    let file = fs::File::open(path)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let mut reader = BufReader::new(file);
    reader
        .seek_relative(start)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let mut offset = start;
    let mut previous = if start > 0 { last_total } else { 0 };
    let mut count = 0;
    loop {
        let event_offset = offset;
        let mut line = Vec::new();
        let bytes = reader
            .read_until(b'\n', &mut line)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        if bytes == 0 {
            break;
        }
        let complete_line = line.last() == Some(&b'\n');
        if complete_line {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }
        let parsed = serde_json::from_slice::<EventLine>(&line);
        if parsed.is_err() && !complete_line {
            break;
        }
        offset += bytes as i64;
        let Ok(event) = parsed else {
            continue;
        };
        if event.kind.as_deref() != Some("event_msg") {
            continue;
        }
        let day = event
            .timestamp
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Local).date_naive().to_string())
            .unwrap_or_else(|| Local::now().date_naive().to_string());
        let Some(payload) = event.payload else {
            continue;
        };
        if payload.kind.as_deref() != Some("token_count") {
            continue;
        }
        let Some(info) = payload.info else {
            continue;
        };
        let Some(total_tokens) = info.total_token_usage.and_then(|usage| usage.total_tokens) else {
            continue;
        };
        let delta_tokens = if total_tokens == previous {
            0
        } else if total_tokens < previous {
            total_tokens.max(0)
        } else {
            (total_tokens - previous).max(0)
        };
        previous = total_tokens;
        let event_id = storage::digest(&format!("{id}:{event_offset}:{total_tokens}"));
        if storage::record_session_event(
            db,
            &SessionEvent {
                event_id,
                file_id: id.clone(),
                event_offset,
                total_tokens,
                delta_tokens,
                day,
            },
        )? {
            count += 1;
        }
    }
    storage::save_cursor(db, &id, &id, offset, previous)?;
    Ok(count)
}

pub fn scan_sessions(db: &Connection, root: &Path) -> rusqlite::Result<usize> {
    if !root.is_dir() {
        return Ok(0);
    }
    storage::with_immediate_transaction(db, |db| {
        fn walk(db: &Connection, path: &Path, count: &mut usize) -> rusqlite::Result<()> {
            for entry in fs::read_dir(path)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?
            {
                let path = entry
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?
                    .path();
                if path.is_dir() {
                    walk(db, &path, count)?;
                } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                    *count += scan_file(db, &path)?;
                }
            }
            Ok(())
        }

        let mut count = 0;
        walk(db, root, &mut count)?;
        storage::cleanup_old_daily(db, 30)?;
        Ok(count)
    })
}

pub(crate) fn token_coverage(root_exists: bool, scan_succeeded: bool) -> &'static str {
    if root_exists && scan_succeeded {
        "partial"
    } else {
        "unavailable"
    }
}

pub(crate) fn read_local_usage(state: &Store) -> LocalTokenUsage {
    let sessions = paths::sessions_path();
    let sessions_exist = sessions.is_dir();
    let db = state.db.lock().unwrap();
    let scan_succeeded = sessions_exist && scan_sessions(&db, &sessions).is_ok();
    let coverage = token_coverage(sessions_exist, scan_succeeded);
    if coverage == "unavailable" {
        LocalTokenUsage {
            today: 0,
            last7: 0,
            last30: 0,
            daily: Vec::new(),
            coverage: coverage.into(),
        }
    } else {
        LocalTokenUsage {
            today: storage::sums(&db, 1).unwrap_or(0),
            last7: storage::sums(&db, 7).unwrap_or(0),
            last30: storage::sums(&db, 30).unwrap_or(0),
            daily: storage::series(&db).unwrap_or_default(),
            coverage: coverage.into(),
        }
    }
}

pub(crate) fn clear_history(state: &Store) -> Result<(), String> {
    let db = state.db.lock().map_err(|_| "database lock")?;
    storage::clear_token_data(&db).map_err(|error| error.to_string())
}

fn usage_totals(daily: &[DailyPoint], today: chrono::NaiveDate) -> (i64, i64, i64, bool) {
    let today_key = today.to_string();
    let today_available = daily.iter().any(|point| point.day == today_key);
    let today_value = daily
        .iter()
        .filter(|point| point.day == today_key)
        .map(|point| point.tokens)
        .sum();
    let since7 = (today - chrono::Days::new(6)).to_string();
    let since30 = (today - chrono::Days::new(29)).to_string();
    let last7 = daily
        .iter()
        .filter(|point| point.day >= since7 && point.day <= today_key)
        .map(|point| point.tokens)
        .sum();
    let last30 = daily
        .iter()
        .filter(|point| point.day >= since30 && point.day <= today_key)
        .map(|point| point.tokens)
        .sum();
    (today_value, last7, last30, today_available)
}

pub(crate) fn account_history_with_local_today(
    account_daily: Vec<DailyPoint>,
    local_today: i64,
    today: chrono::NaiveDate,
) -> Vec<DailyPoint> {
    let today_key = today.to_string();
    let mut merged: Vec<DailyPoint> = account_daily
        .into_iter()
        .filter(|point| point.day.as_str() < today_key.as_str())
        .collect();
    merged.push(DailyPoint {
        day: today_key,
        tokens: local_today,
    });
    merged.sort_by(|left, right| left.day.cmp(&right.day));
    merged
}

pub(crate) fn select_token_snapshot(
    account_daily: Option<Vec<DailyPoint>>,
    local: LocalTokenUsage,
    today: chrono::NaiveDate,
) -> TokenSnapshot {
    if let Some(daily) = account_daily.filter(|daily| !daily.is_empty()) {
        let (today_tokens, last7, last30, today_available) = usage_totals(&daily, today);
        if local.coverage != "unavailable" {
            let daily = account_history_with_local_today(daily, local.today, today);
            let (today, last7, last30, _) = usage_totals(&daily, today);
            return TokenSnapshot {
                today,
                last7,
                last30,
                daily,
                token_coverage: "account_history_local_today".into(),
                usage_source: "account_history_local_today".into(),
                today_available: true,
            };
        }
        return TokenSnapshot {
            today: today_available.then_some(today_tokens).unwrap_or(0),
            last7,
            last30,
            daily,
            token_coverage: "account".into(),
            usage_source: if today_available {
                "account".into()
            } else {
                "account_history_today_unavailable".into()
            },
            today_available,
        };
    }
    let today_available = local.coverage != "unavailable";
    TokenSnapshot {
        today: local.today,
        last7: local.last7,
        last30: local.last30,
        daily: local.daily,
        token_coverage: local.coverage,
        usage_source: "local".into(),
        today_available,
    }
}

#[cfg(test)]
mod tests {
    use super::{select_token_snapshot, LocalTokenUsage};
    use crate::models::DailyPoint;

    #[test]
    fn local_today_replaces_account_today_without_duplicating_totals() {
        let result = select_token_snapshot(
            Some(vec![
                DailyPoint {
                    day: "2026-10-09".into(),
                    tokens: 10,
                },
                DailyPoint {
                    day: "2026-10-10".into(),
                    tokens: 20,
                },
            ]),
            LocalTokenUsage {
                today: 30,
                last7: 40,
                last30: 40,
                daily: vec![
                    DailyPoint {
                        day: "2026-10-09".into(),
                        tokens: 10,
                    },
                    DailyPoint {
                        day: "2026-10-10".into(),
                        tokens: 30,
                    },
                ],
                coverage: "partial".into(),
            },
            chrono::NaiveDate::from_ymd_opt(2026, 10, 10).unwrap(),
        );

        assert_eq!(result.usage_source, "account_history_local_today");
        assert_eq!(result.today, 30);
        assert_eq!(result.last7, 40);
    }
}
