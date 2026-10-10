use chrono::{DateTime, Local};
use rusqlite::Connection;
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{mpsc, Mutex},
    thread,
    time::Duration,
};

use crate::{
    models::{DailyPoint, RateWindow},
    state::{AccountUsageCache, RateCache, RateRetry, RateState, RefreshMode, Store},
    storage,
};

const ACCOUNT_USAGE_CACHE_MAX_AGE_SECS: i64 = 120;
const RATE_REFRESH_INTERVAL_SECS: i64 = 120;
const RATE_LIVE_MAX_AGE_SECS: i64 = 180;
pub(crate) const RATE_CACHE_MAX_AGE_SECS: i64 = 15 * 60;

pub(crate) fn recv_response(
    messages: &mpsc::Receiver<serde_json::Value>,
    id: i64,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let message = messages.recv_timeout(remaining).ok()?;
        if message.get("id").and_then(|value| value.as_i64()) == Some(id) {
            return Some(message);
        }
    }
}

pub(crate) fn first_existing_path(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|path| path.is_file())
}

pub(crate) fn parse_rate_limits_response(
    response: &serde_json::Value,
) -> Result<Vec<RateWindow>, &'static str> {
    if response.get("error").is_some() {
        return Err("protocol_error");
    }
    let rate_limits = response
        .get("result")
        .and_then(|value| value.get("rateLimits"))
        .and_then(serde_json::Value::as_object)
        .ok_or("invalid_response")?;
    let mut windows = rate_limits
        .iter()
        .filter_map(|(key, value)| {
            let value = value.as_object()?;
            let used = value.get("usedPercent")?.as_f64()?;
            let resets_at = value.get("resetsAt").and_then(|value| value.as_i64());
            let minutes = value
                .get("windowDurationMins")
                .and_then(|value| value.as_i64());
            let label = match minutes {
                Some(300) => "5 小时".to_string(),
                Some(10_080) => "7 天".to_string(),
                Some(value) if value > 0 && value % 1_440 == 0 => {
                    format!("{} 天", value / 1_440)
                }
                Some(value) if value > 0 && value % 60 == 0 => {
                    format!("{} 小时", value / 60)
                }
                Some(value) if value > 0 => format!("{value} 分钟"),
                _ => key.to_string(),
            };
            Some(RateWindow {
                label,
                used_percent: Some(used),
                remaining_percent: Some((100.0 - used).clamp(0.0, 100.0)),
                resets_at,
                window_minutes: minutes,
            })
        })
        .collect::<Vec<_>>();
    windows.sort_by_key(|window| window.window_minutes.unwrap_or(i64::MAX));
    if windows.is_empty() {
        return Err("empty_snapshot");
    }
    Ok(windows)
}

fn resolve_codex_executable() -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(value) = std::env::var_os("CODEX_EXE") {
        candidates.push(PathBuf::from(value));
    }
    candidates.push(PathBuf::from(
        r"D:\nvm4w\nodejs\node_modules\@openai\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe",
    ));
    candidates.push(PathBuf::from(r"D:\nvm4w\nodejs\codex.cmd"));
    if let Some(root) = std::env::var_os("APPDATA").map(PathBuf::from) {
        candidates.push(
            root.join("npm/node_modules/@openai/codex/node_modules/@openai/codex-win32-x64")
                .join("vendor/x86_64-pc-windows-msvc/bin/codex.exe"),
        );
        candidates.push(root.join("npm/codex.cmd"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            candidates.push(directory.join("codex.exe"));
            candidates.push(directory.join("codex.cmd"));
        }
    }
    first_existing_path(candidates).unwrap_or_else(|| PathBuf::from("codex.cmd"))
}

fn read_rate_limits_uncached() -> (Vec<RateWindow>, String) {
    let executable = resolve_codex_executable();

    let mut command = if executable.extension().and_then(|value| value.to_str()) == Some("cmd") {
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/c"]).arg(executable);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command
    } else {
        Command::new(executable)
    };
    let mut child = match command
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
    let (messages_tx, messages_rx) = mpsc::channel::<serde_json::Value>();
    thread::spawn(move || {
        let reader = BufReader::new(output);
        for line in reader.lines().flatten() {
            if let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) {
                if messages_tx.send(message).is_err() {
                    break;
                }
            }
        }
    });

    if writeln!(
        input,
        "{}",
        r#"{"id":1,"method":"initialize","params":{"clientInfo":{"name":"codex-float","title":"Codex Float","version":"0.1.0"}}}"#
    )
    .and_then(|_| input.flush())
    .is_err()
    {
        let _ = child.kill();
        return (vec![], "unavailable".into());
    }
    let initialized = recv_response(&messages_rx, 1, Duration::from_secs(4))
        .is_some_and(|message| message.get("error").is_none());
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

    let response = recv_response(&messages_rx, 2, Duration::from_secs(8));
    let _ = child.kill();
    let Some(response) = response else {
        return (vec![], "unavailable".into());
    };
    match parse_rate_limits_response(&response) {
        Ok(windows) => (windows, "live".into()),
        Err(_) => (vec![], "unavailable".into()),
    }
}

fn read_account_usage_uncached() -> Option<Vec<DailyPoint>> {
    let executable = resolve_codex_executable();
    let mut command = if executable.extension().and_then(|value| value.to_str()) == Some("cmd") {
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/c"]).arg(executable);
        command
    } else {
        Command::new(executable)
    };
    let mut child = command
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut input = child.stdin.take()?;
    let output = child.stdout.take()?;
    let (messages_tx, messages_rx) = mpsc::channel::<serde_json::Value>();
    thread::spawn(move || {
        for line in BufReader::new(output).lines().flatten() {
            if let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) {
                if messages_tx.send(message).is_err() {
                    break;
                }
            }
        }
    });
    writeln!(
        input,
        r#"{{"id":1,"method":"initialize","params":{{"clientInfo":{{"name":"codex-float","title":"Codex Float","version":"0.1.0"}}}}}}"#
    )
    .ok()?;
    input.flush().ok()?;
    if recv_response(&messages_rx, 1, Duration::from_secs(5)).is_none() {
        let _ = child.kill();
        return None;
    }
    writeln!(input, r#"{{"method":"initialized"}}"#).ok()?;
    writeln!(input, r#"{{"id":2,"method":"account/usage/read"}}"#).ok()?;
    input.flush().ok()?;
    let response = recv_response(&messages_rx, 2, Duration::from_secs(15))?;
    let buckets = response
        .get("result")?
        .get("dailyUsageBuckets")?
        .as_array()?;
    let mut daily = Vec::new();
    for bucket in buckets {
        let day = bucket
            .get("startDate")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let tokens = bucket
            .get("tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        if !day.is_empty() {
            daily.push(DailyPoint {
                day: day.to_string(),
                tokens,
            });
        }
    }
    let _ = child.kill();
    (!daily.is_empty()).then_some(daily)
}

pub(crate) fn cached_account_usage(
    cache: &Mutex<Option<AccountUsageCache>>,
    mode: RefreshMode,
    request_started: DateTime<Local>,
    now: DateTime<Local>,
) -> Option<Vec<DailyPoint>> {
    let slot = cache.lock().ok()?;
    let cached = slot.as_ref()?;
    let use_cached = match mode {
        RefreshMode::Cached => {
            let age = (now - cached.fetched_at).num_seconds();
            (0..ACCOUNT_USAGE_CACHE_MAX_AGE_SECS).contains(&age)
        }
        RefreshMode::Force => cached.fetched_at >= request_started,
    };
    use_cached.then(|| cached.daily.clone())
}

pub(crate) fn read_account_usage_with<F>(
    cache: &Mutex<Option<AccountUsageCache>>,
    refresh: &Mutex<()>,
    mode: RefreshMode,
    fetch: F,
) -> Option<Vec<DailyPoint>>
where
    F: FnOnce() -> Option<Vec<DailyPoint>>,
{
    let request_started = Local::now();
    if let Some(daily) = cached_account_usage(cache, mode, request_started, Local::now()) {
        return Some(daily);
    }
    let _refresh_guard = refresh.lock().ok()?;
    if let Some(daily) = cached_account_usage(cache, mode, request_started, Local::now()) {
        return Some(daily);
    }
    let daily = fetch()?;
    if daily.is_empty() {
        return None;
    }
    if let Ok(mut slot) = cache.lock() {
        *slot = Some(AccountUsageCache {
            daily: daily.clone(),
            fetched_at: Local::now(),
        });
    }
    Some(daily)
}

pub(crate) fn read_account_usage(state: &Store, mode: RefreshMode) -> Option<Vec<DailyPoint>> {
    read_account_usage_with(
        &state.account_usage_cache,
        &state.account_usage_refresh,
        mode,
        read_account_usage_uncached,
    )
}

pub(crate) fn rate_retry_delay_secs(failures: u32) -> i64 {
    match failures {
        0 => 0,
        1 => 5,
        2 => 15,
        _ => 30,
    }
}

pub(crate) fn rate_cache_status(
    cache: Option<&RateCache>,
    now: DateTime<Local>,
) -> Option<&'static str> {
    let cache = cache?;
    if cache.windows.is_empty() {
        return None;
    }
    let age = (now - cache.fetched_at).num_seconds();
    if age < 0 || age > RATE_CACHE_MAX_AGE_SECS {
        return None;
    }
    if age < RATE_LIVE_MAX_AGE_SECS {
        Some("live")
    } else {
        Some("stale")
    }
}

pub(crate) fn discard_expired_rate_cache(
    state: &Mutex<RateState>,
    db: &Mutex<Connection>,
    now: DateTime<Local>,
) {
    let expired = if let Ok(mut state) = state.lock() {
        let expired = state
            .cache
            .as_ref()
            .is_some_and(|cache| rate_cache_status(Some(cache), now).is_none());
        if expired {
            state.cache = None;
        }
        expired
    } else {
        false
    };
    if expired {
        if let Ok(db) = db.lock() {
            let _ = storage::delete_rate_cache(&db);
        }
    }
}

pub(crate) fn cached_rate_limits(
    cache: Option<&RateCache>,
    now: DateTime<Local>,
) -> Option<(Vec<RateWindow>, String)> {
    let cached = cache?;
    rate_cache_status(Some(cached), now)?;
    Some((cached.windows.clone(), cached.fetched_at.to_rfc3339()))
}

pub(crate) fn cached_rate_response(
    state: &RateState,
    now: DateTime<Local>,
) -> Option<(Vec<RateWindow>, String, String, String)> {
    let (windows, fetched_at) = cached_rate_limits(state.cache.as_ref(), now)?;
    let status = rate_cache_status(state.cache.as_ref(), now)?;
    Some((windows, status.into(), status.into(), fetched_at))
}

pub(crate) fn rate_unavailable() -> (Vec<RateWindow>, String, String, String) {
    (
        vec![],
        "unavailable".into(),
        "unavailable".into(),
        String::new(),
    )
}

pub(crate) fn retry_is_due(retry: &RateRetry, now: DateTime<Local>) -> bool {
    let delay = rate_retry_delay_secs(retry.consecutive_failures);
    if delay == 0 {
        return true;
    }
    retry
        .last_attempt
        .map(|last| (now - last).num_seconds() >= delay)
        .unwrap_or(true)
}

pub(crate) fn read_rate_limits_with<F>(
    state: &Mutex<RateState>,
    refresh: &Mutex<()>,
    db: &Mutex<Connection>,
    mode: RefreshMode,
    fetch: F,
) -> (Vec<RateWindow>, String, String, String)
where
    F: FnOnce() -> (Vec<RateWindow>, String),
{
    let request_started = Local::now();
    discard_expired_rate_cache(state, db, request_started);
    if matches!(mode, RefreshMode::Cached) {
        if let Ok(state) = state.lock() {
            if let Some(cache) = state.cache.as_ref() {
                let age = (request_started - cache.fetched_at).num_seconds();
                if (0..RATE_REFRESH_INTERVAL_SECS).contains(&age) {
                    if let Some(snapshot) = cached_rate_response(&state, request_started) {
                        return snapshot;
                    }
                }
            }
            if !retry_is_due(&state.retry, request_started) {
                return cached_rate_response(&state, request_started)
                    .unwrap_or_else(rate_unavailable);
            }
        }
    }

    let Ok(_refresh_guard) = refresh.lock() else {
        return rate_unavailable();
    };
    let attempt_at = Local::now();
    if let Ok(mut state) = state.lock() {
        match mode {
            RefreshMode::Cached => {
                if let Some(cache) = state.cache.as_ref() {
                    let age = (attempt_at - cache.fetched_at).num_seconds();
                    if (0..RATE_REFRESH_INTERVAL_SECS).contains(&age) {
                        if let Some(snapshot) = cached_rate_response(&state, attempt_at) {
                            return snapshot;
                        }
                    }
                }
                if !retry_is_due(&state.retry, attempt_at) {
                    return cached_rate_response(&state, attempt_at)
                        .unwrap_or_else(rate_unavailable);
                }
            }
            RefreshMode::Force => {
                if state
                    .cache
                    .as_ref()
                    .is_some_and(|cache| cache.fetched_at >= request_started)
                {
                    if let Some(snapshot) = cached_rate_response(&state, attempt_at) {
                        return snapshot;
                    }
                }
            }
        }
        state.retry.last_attempt = Some(attempt_at);
    } else {
        return rate_unavailable();
    }

    let (windows, status) = fetch();
    let fetched_at = Local::now();
    if status == "live" && !windows.is_empty() {
        let cache = RateCache {
            windows: windows.clone(),
            fetched_at,
        };
        if let Ok(mut state) = state.lock() {
            state.cache = Some(cache);
            state.retry = RateRetry {
                last_attempt: Some(fetched_at),
                consecutive_failures: 0,
            };
        }
        if let Ok(db) = db.lock() {
            let _ = storage::save_rate_cache(&db, &windows, fetched_at);
        }
        return (
            windows,
            "live".into(),
            "live".into(),
            fetched_at.to_rfc3339(),
        );
    }

    let now = Local::now();
    if let Ok(mut state) = state.lock() {
        state.retry.consecutive_failures = state.retry.consecutive_failures.saturating_add(1);
        state.retry.last_attempt = Some(now);
        return cached_rate_response(&state, now).unwrap_or_else(rate_unavailable);
    }
    rate_unavailable()
}

pub(crate) fn read_rate_limits(
    state: &Store,
    mode: RefreshMode,
) -> (Vec<RateWindow>, String, String, String) {
    read_rate_limits_with(
        &state.rate_state,
        &state.rate_refresh,
        &state.db,
        mode,
        read_rate_limits_uncached,
    )
}
