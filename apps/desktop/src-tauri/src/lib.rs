use chrono::{DateTime, Local};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RateWindow {
    pub label: String,
    pub used_percent: Option<f64>,
    pub remaining_percent: Option<f64>,
    pub resets_at: Option<i64>,
    pub window_minutes: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub expanded: bool,
    pub today: i64,
    pub last7: i64,
    pub last30: i64,
    pub daily: Vec<i64>,
    pub windows: Vec<RateWindow>,
    pub rate_status: String,
    pub token_coverage: String,
    pub updated_at: String,
}
#[derive(Debug, Deserialize)]
struct TokenInfo {
    total_token_usage: Option<TotalUsage>,
    last_token_usage: Option<TotalUsage>,
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

pub struct Store {
    pub db: Mutex<Connection>,
    pub expanded: Mutex<bool>,
}
impl Store {
    pub fn new(path: &Path) -> rusqlite::Result<Self> {
        let c = Connection::open(path)?;
        init(&c)?;
        Ok(Self {
            db: Mutex::new(c),
            expanded: Mutex::new(false),
        })
    }
}
fn init(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS cursors(file_id TEXT PRIMARY KEY,file_hash TEXT NOT NULL,offset INTEGER NOT NULL,last_total INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS events(event_id TEXT PRIMARY KEY,file_id TEXT NOT NULL,event_offset INTEGER NOT NULL,total_tokens INTEGER NOT NULL,delta_tokens INTEGER NOT NULL,day TEXT NOT NULL); CREATE TABLE IF NOT EXISTS daily(day TEXT PRIMARY KEY,tokens INTEGER NOT NULL);")
}
fn digest(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}
fn file_id(path: &Path) -> String {
    digest(&path.to_string_lossy())
}
fn scan_file(c: &Connection, path: &Path) -> rusqlite::Result<usize> {
    let id = file_id(path);
    let current_len = match fs::metadata(path) {
        Ok(metadata) => metadata.len() as i64,
        Err(_) => return Ok(0),
    };
    let (_old_hash, old_offset, last_total): (Option<String>, i64, i64) = c
        .query_row(
            "SELECT file_hash,offset,last_total FROM cursors WHERE file_id=?",
            [&id],
            |r| Ok((Some(r.get(0)?), r.get(1)?, r.get(2)?)),
        )
        .unwrap_or((None, 0, 0));
    let start = if old_offset <= current_len {
        old_offset
    } else {
        0
    };
    let f =
        fs::File::open(path).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let mut reader = BufReader::new(f);
    reader
        .seek_relative(start)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let mut offset = start;
    let mut previous = if start > 0 {
        last_total
    } else {
        0
    };
    let mut count = 0;
    for line in reader.lines() {
        let line = line.map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let bytes = line.as_bytes().len() as i64 + 1;
        let at = offset;
        offset += bytes;
        let Ok(ev) = serde_json::from_str::<EventLine>(&line) else {
            continue;
        };
        if ev.kind.as_deref() != Some("event_msg") {
            continue;
        }
        let event_day = ev
            .timestamp
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Local).date_naive().to_string())
            .unwrap_or_else(|| Local::now().date_naive().to_string());
        let Some(p) = ev.payload else { continue };
        if p.kind.as_deref() != Some("token_count") {
            continue;
        }
        let Some(info) = p.info else { continue };
        let Some(total) = info.total_token_usage.and_then(|x| x.total_tokens) else {
            continue;
        };
        let increment = info
            .last_token_usage
            .and_then(|x| x.total_tokens)
            .unwrap_or(0);
        let delta = if total == previous {
            0
        } else if increment > 0 {
            increment
        } else if total < previous {
            total.max(0)
        } else {
            (total - previous).max(0)
        };
        previous = total;
        let day = event_day;
        let eid = digest(&format!("{id}:{at}:{total}"));
        let inserted=c.execute("INSERT OR IGNORE INTO events(event_id,file_id,event_offset,total_tokens,delta_tokens,day) VALUES(?,?,?,?,?,?)",params![eid,id,at,total,delta,day])?;
        if inserted > 0 && delta > 0 {
            c.execute("INSERT INTO daily(day,tokens) VALUES(?,?) ON CONFLICT(day) DO UPDATE SET tokens=tokens+excluded.tokens",params![day,delta])?;
            count += 1;
        }
    }
    c.execute("INSERT INTO cursors(file_id,file_hash,offset,last_total) VALUES(?,?,?,?) ON CONFLICT(file_id) DO UPDATE SET file_hash=excluded.file_hash,offset=excluded.offset,last_total=excluded.last_total",params![id,id,offset,previous])?;
    Ok(count)
}
pub fn scan_sessions(c: &Connection, root: &Path) -> rusqlite::Result<usize> {
    let mut n = 0;
    if !root.exists() {
        return Ok(0);
    }
    fn walk(c: &Connection, p: &Path, n: &mut usize) -> rusqlite::Result<()> {
        for e in
            fs::read_dir(p).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
        {
            let p = e
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
                .path();
            if p.is_dir() {
                walk(c, &p, n)?
            } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                *n += scan_file(c, &p)?;
            }
        }
        Ok(())
    }
    walk(c, root, &mut n)?;
    Ok(n)
}
fn sums(c: &Connection, days: i64) -> rusqlite::Result<i64> {
    let since = (Local::now().date_naive()
        - chrono::Days::new(days.saturating_sub(1) as u64))
    .to_string();
    c.query_row(
        "SELECT COALESCE(SUM(tokens),0) FROM daily WHERE day>=?",
        [since],
        |r| r.get(0),
    )
}
fn series(c: &Connection) -> rusqlite::Result<Vec<i64>> {
    let today = Local::now().date_naive();
    (0..7)
        .rev()
        .map(|i| {
            let d = (today - chrono::Days::new(i)).to_string();
            c.query_row(
                "SELECT COALESCE(tokens,0) FROM daily WHERE day=?",
                [d],
                |r| r.get(0),
            )
        })
        .collect()
}
fn read_rate_limits() -> (Vec<RateWindow>, String) {
    let executable = std::env::var_os("CODEX_EXE")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            let candidate = PathBuf::from(
                r"D:\nvm4w\nodejs\node_modules\@openai\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe",
            );
            candidate.is_file().then_some(candidate)
        })
        .or_else(|| {
            std::env::var_os("APPDATA").map(PathBuf::from).and_then(|root| {
                let candidate = root
                    .join("npm/node_modules/@openai/codex/node_modules/@openai/codex-win32-x64")
                    .join("vendor/x86_64-pc-windows-msvc/bin/codex.exe");
                candidate.is_file().then_some(candidate)
            })
        });

    let Some(executable) = executable else {
        return (vec![], "unavailable".into());
    };
    let mut child = match Command::new(executable)
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return (vec![], "unavailable".into()),
    };
    let Some(mut input) = child.stdin.take() else {
        let _ = child.kill();
        return (vec![], "unavailable".into());
    };
    let Some(output) = child.stdout.take() else {
        let _ = child.kill();
        return (vec![], "unavailable".into());
    };
    let mut reader = BufReader::new(output);

    let initialized = writeln!(
        input,
        r#"{{"id":1,"method":"initialize","params":{{"clientInfo":{{"name":"codex-float","title":"Codex Float","version":"0.1.0"}}}}}}"#
    )
    .and_then(|_| input.flush())
    .is_ok()
        && (&mut reader)
            .lines()
            .take(100)
            .flatten()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
            .any(|message| message.get("id") == Some(&serde_json::json!(1)) && message.get("error").is_none());
    if !initialized {
        let _ = child.kill();
        return (vec![], "unavailable".into());
    }

    if writeln!(input, r#"{{"method":"initialized"}}"#).is_err()
        || writeln!(input, r#"{{"id":2,"method":"account/rateLimits/read"}}"#).is_err()
        || input.flush().is_err()
    {
        let _ = child.kill();
        return (vec![], "unavailable".into());
    }

    let response = reader
        .lines()
        .take(100)
        .flatten()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
        .find(|message| message.get("id") == Some(&serde_json::json!(2)));
    let _ = child.kill();
    let Some(response) = response else {
        return (vec![], "unavailable".into());
    };
    let result = response
        .get("result")
        .and_then(|value| value.get("rateLimits"))
        .unwrap_or(&serde_json::Value::Null);
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let Some(value) = result.get(key).and_then(|value| value.as_object()) else {
            continue;
        };
        let used = value.get("usedPercent").and_then(|value| value.as_f64());
        let resets_at = value.get("resetsAt").and_then(|value| value.as_i64());
        let minutes = value
            .get("windowDurationMins")
            .and_then(|value| value.as_i64());
        let label = match minutes {
            Some(300) => "5 小时".to_string(),
            Some(10_080) => "7 天".to_string(),
            Some(value) if value % 1_440 == 0 => format!("{} 天", value / 1_440),
            Some(value) if value % 60 == 0 => format!("{} 小时", value / 60),
            Some(value) => format!("{value} 分钟"),
            None => key.to_string(),
        };
        windows.push(RateWindow {
            label,
            used_percent: used,
            remaining_percent: used.map(|value| (100.0 - value).clamp(0.0, 100.0)),
            resets_at,
            window_minutes: minutes,
        });
    }
    (windows, "live".into())
}
#[tauri::command]
fn get_dashboard_snapshot(state: tauri::State<Store>) -> Snapshot {
    let db = state.db.lock().unwrap();
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_default()
        .join(".codex/sessions");
    let token_coverage = match scan_sessions(&db, &home) {
        Ok(_) => "partial",
        Err(_) => "unavailable",
    };
    let today = sums(&db, 1).unwrap_or(0);
    let last7 = sums(&db, 7).unwrap_or(0);
    let last30 = sums(&db, 30).unwrap_or(0);
    let daily = series(&db).unwrap_or_default();
    let expanded = *state.expanded.lock().unwrap();
    let (windows, status) = read_rate_limits();
    Snapshot {
        expanded,
        today,
        last7,
        last30,
        daily,
        windows,
        rate_status: status,
        token_coverage: token_coverage.into(),
        updated_at: Local::now().to_rfc3339(),
    }
}
#[tauri::command]
fn set_window_expanded(
    app: tauri::AppHandle,
    state: tauri::State<Store>,
    expanded: bool,
) -> Result<(), String> {
    *state.expanded.lock().map_err(|_| "lock")? = expanded;
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("main") {
        w.set_size(tauri::LogicalSize::new(
            if expanded { 320.0 } else { 250.0 },
            if expanded { 280.0 } else { 64.0 },
        ))
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let db = data.join("CodexFloat").join("codex-float.sqlite");
    let _ = fs::create_dir_all(db.parent().unwrap());
    let store = Store::new(&db).expect("database");
    tauri::Builder::default()
        .manage(store)
        .setup(|app| {
            use tauri::Manager;
            if let Some(window) = app.get_webview_window("main") {
                if let Some(monitor) = window.current_monitor()? {
                    let work_area = monitor.work_area();
                    let size = window.outer_size()?;
                    let margin = (16.0 * monitor.scale_factor()).round() as i32;
                    let x = work_area.position.x + work_area.size.width as i32
                        - size.width as i32
                        - margin;
                    let y = work_area.position.y + work_area.size.height as i32
                        - size.height as i32
                        - margin;
                    window.set_position(tauri::PhysicalPosition::new(x, y))?;
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_dashboard_snapshot,
            set_window_expanded
        ])
        .run(tauri::generate_context!())
        .expect("error running app");
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        c
    }
    fn line(total: i64, inc: i64) -> String {
        serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":total},"last_token_usage":{"total_tokens":inc}}}}).to_string()
    }
    fn line_at(total: i64, inc: i64, timestamp: &str) -> String {
        serde_json::json!({"timestamp":timestamp,"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":total},"last_token_usage":{"total_tokens":inc}}}}).to_string()
    }
    #[test]
    fn duplicate_snapshot_is_idempotent() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        fs::write(&p, format!("{}\n{}\n", line(10, 10), line(10, 0))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        scan_file(&c, &p).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 10)
    }
    #[test]
    fn counter_reset_uses_new_total() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        fs::write(&p, format!("{}\n", line(100, 100))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        fs::write(&p, format!("{}\n{}\n", line(100, 100), line(3, 3))).unwrap();
        scan_file(&c, &p).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 103)
    }
    #[test]
    fn second_scan_and_append_are_idempotent() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        fs::write(&p, format!("{}\n", line(4, 4))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(format!("{}\n", line(9, 5)).as_bytes())
            .unwrap();
        scan_file(&c, &p).unwrap();
        scan_file(&c, &p).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 9)
    }
    #[test]
    fn event_timestamp_controls_the_daily_bucket() {
        let d = tempdir().unwrap();
        let p = d.path().join("private-session.jsonl");
        let timestamp = "2026-08-10T23:30:00Z";
        fs::write(&p, format!("{}\n", line_at(12, 12, timestamp))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        let expected_day = DateTime::parse_from_rfc3339(timestamp)
            .unwrap()
            .with_timezone(&Local)
            .date_naive()
            .to_string();
        let (stored_day, tokens): (String, i64) = c
            .query_row("SELECT day,tokens FROM daily", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!((stored_day, tokens), (expected_day, 12));
        let stored_file_id: String = c
            .query_row("SELECT file_id FROM cursors", [], |row| row.get(0))
            .unwrap();
        assert!(!stored_file_id.contains("private-session"));
    }
}
