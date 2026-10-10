# lib.rs Module Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 Rust 后端的 Provider、Token、账号、窗口和存储职责从单一 lib.rs 拆为稳定模块，同时保持全部 IPC、数据格式和行为不变。

**Architecture:** 以 models、state 和 paths 作为无业务副作用的共享基础，五个领域模块只通过明确的 crate 内接口协作。lib.rs 收敛为 Tauri 装配、命令适配和 Snapshot 编排，任何 SQLite、JSONL、HTTP、子进程或窗口事件实现都不再保留在那里。

**Tech Stack:** Rust 2021、Tauri 2、rusqlite、serde、chrono、reqwest blocking、Windows API、Vitest、Vite。

## Global Constraints

- 不修改前端 TypeScript、Tauri 命令名称或 IPC 参数。
- 保持 Snapshot 的 camelCase 序列化字段、usageSource 和 tokenCoverage 的现有取值不变。
- 保持 events、daily、cursors、window_state、quota_cache 的表结构、数据和清理语义不变。
- 保持额度刷新、账户状态刷新、账户用量刷新、本机当天 Token 替换和 120 秒前端轮询语义不变。
- 不新增 Cargo 或 npm 依赖，不引入异步运行时、后台服务或数据库迁移框架。
- 新模块内部接口使用 pub(crate)，不得暴露 OAuth、JWT、Cookie、session 正文或原始 Provider 响应。
- 每项任务必须先通过本任务验证，再提交并推送到 origin/master；不得暂存或提交 .learnings/。
- Rust 命令以仓库根目录执行并显式使用 apps/desktop/src-tauri/Cargo.toml；npm 命令在 apps/desktop 目录执行。

---

## File Structure

- Create: apps/desktop/src-tauri/src/models.rs — renderer DTO：RateWindow、Snapshot、DailyPoint、AccountStatus。
- Create: apps/desktop/src-tauri/src/paths.rs — CODEX_HOME、auth.json 和 sessions 目录解析。
- Create: apps/desktop/src-tauri/src/state.rs — Store、缓存项、刷新锁、RefreshMode。
- Create: apps/desktop/src-tauri/src/storage.rs — SQLite schema、Token 持久化操作、统计查询、额度缓存与窗口设置。
- Create: apps/desktop/src-tauri/src/token.rs — JSONL 解析、本机增量扫描、Token 快照来源选择。
- Create: apps/desktop/src-tauri/src/provider.rs — codex app-server JSON-RPC、额度和账户日桶读取。
- Create: apps/desktop/src-tauri/src/account.rs — auth.json、JWT、订阅 HTTP 和账户状态缓存。
- Create: apps/desktop/src-tauri/src/window.rs — 托盘、快捷键、尺寸位置、关闭和全屏处理。
- Modify: apps/desktop/src-tauri/src/lib.rs — 模块声明、5 个 Tauri 命令、dashboard_snapshot 和 Builder 装配。
- Modify: docs/superpowers/specs/2026-10-10-lib-rs-module-split-design.md — 将状态保持为“设计已批准，待实施”。

### Stable crate-internal interfaces

~~~rust
// models.rs
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
    pub daily: Vec<DailyPoint>,
    pub windows: Vec<RateWindow>,
    pub account: AccountStatus,
    pub rate_status: String,
    pub rate_freshness: String,
    pub token_coverage: String,
    pub usage_source: String,
    pub today_available: bool,
    pub updated_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyPoint { pub day: String, pub tokens: i64 }
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub bound: bool,
    pub plan_type: Option<String>,
    pub subscription_active_until: Option<String>,
    pub subscription_state: String,
    pub subscription_error: Option<String>,
    pub checked_at: Option<String>,
}

// paths.rs
pub(crate) fn codex_home_from(
    codex_home: Option<PathBuf>,
    user_profile: Option<PathBuf>,
    home: Option<PathBuf>,
) -> PathBuf;
pub(crate) fn codex_home() -> PathBuf;
pub(crate) fn sessions_path_from(
    codex_home: Option<&Path>,
    user_profile: Option<&Path>,
    home: Option<&Path>,
) -> PathBuf;
pub(crate) fn sessions_path() -> PathBuf;
pub(crate) fn auth_file_path() -> PathBuf;

// state.rs
pub(crate) enum RefreshMode { Cached, Force }
pub(crate) struct RateCache {
    pub(crate) windows: Vec<RateWindow>,
    pub(crate) fetched_at: DateTime<Local>,
}
pub(crate) struct RateRetry {
    pub(crate) last_attempt: Option<DateTime<Local>>,
    pub(crate) consecutive_failures: u32,
}
pub(crate) struct RateState {
    pub(crate) cache: Option<RateCache>,
    pub(crate) retry: RateRetry,
}
pub(crate) struct AccountCache {
    pub(crate) cache_key: String,
    pub(crate) status: AccountStatus,
    pub(crate) fetched_at: DateTime<Local>,
}
pub(crate) struct AccountUsageCache {
    pub(crate) daily: Vec<DailyPoint>,
    pub(crate) fetched_at: DateTime<Local>,
}
pub struct Store {
    pub(crate) db: Mutex<Connection>,
    pub(crate) expanded: Mutex<bool>,
    pub(crate) click_through: Mutex<bool>,
    pub(crate) user_hidden: Mutex<bool>,
    pub(crate) rate_state: Mutex<RateState>,
    pub(crate) rate_refresh: Mutex<()>,
    pub(crate) account_cache: Mutex<Option<AccountCache>>,
    pub(crate) account_refresh: Mutex<()>,
    pub(crate) account_usage_cache: Mutex<Option<AccountUsageCache>>,
    pub(crate) account_usage_refresh: Mutex<()>,
}
impl Store {
    pub fn new(path: &Path) -> rusqlite::Result<Self>;
}

// token.rs
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
pub(crate) fn read_local_usage(state: &Store) -> LocalTokenUsage;
pub(crate) fn select_token_snapshot(
    account_daily: Option<Vec<DailyPoint>>,
    local: LocalTokenUsage,
    today: chrono::NaiveDate,
) -> TokenSnapshot;
pub(crate) fn clear_history(state: &Store) -> Result<(), String>;

// provider.rs
pub(crate) fn read_rate_limits(
    state: &Store,
    mode: RefreshMode,
) -> (Vec<RateWindow>, String, String, String);
pub(crate) fn read_account_usage(
    state: &Store,
    mode: RefreshMode,
) -> Option<Vec<DailyPoint>>;

// account.rs
pub(crate) fn query_account_status(
    state: &Store,
    mode: RefreshMode,
) -> AccountStatus;

// window.rs
pub(crate) fn save_position(state: &Store, x: i32, y: i32) -> Result<(), String>;
pub(crate) fn set_expanded(
    app: &tauri::AppHandle,
    state: &Store,
    expanded: bool,
) -> Result<(), String>;
pub(crate) fn setup(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>>;
pub(crate) fn handle_close_requested(window: &tauri::WebviewWindow);
pub(crate) fn start_fullscreen_monitor(app: tauri::AppHandle);
~~~

### Task 1: Extract shared DTO and path modules

**Files:**
- Create: apps/desktop/src-tauri/src/models.rs
- Create: apps/desktop/src-tauri/src/paths.rs
- Modify: apps/desktop/src-tauri/src/lib.rs:1-81, 400-451, 1744-1899
- Test: inline tests in models.rs and paths.rs

**Interfaces:**
- Consumes: existing RateWindow, Snapshot, DailyPoint, AccountStatus, codex_home_from and sessions_path_from implementations.
- Produces: the models.rs and paths.rs interfaces listed above; lib.rs imports the four public DTOs through crate::models.

- [ ] **Step 1: Move path-resolution assertions into the future module test boundary**

Move the existing sessions_path_prefers_custom_codex_home_and_reports_unavailable_sources test into a new paths::tests module and add the auth root assertion:

~~~rust
#[test]
fn explicit_codex_home_controls_session_and_auth_roots() {
    let root = Path::new("C:/CodexHome");
    assert_eq!(
        sessions_path_from(Some(root), None, None),
        PathBuf::from("C:/CodexHome/sessions")
    );
    assert_eq!(
        codex_home_from(Some(root.to_path_buf()), None, None),
        PathBuf::from("C:/CodexHome")
    );
}
~~~

- [ ] **Step 2: Run the focused path test before implementing the modules**

Run:

~~~powershell
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml paths::tests::explicit_codex_home_controls_session_and_auth_roots
~~~

Expected: FAIL because the paths module does not exist yet.

- [ ] **Step 3: Add models.rs and paths.rs, then replace duplicate declarations in lib.rs**

Move only these definitions without changing fields, derives, serde configuration or logic:

~~~rust
// lib.rs
mod models;
mod paths;
pub use models::{AccountStatus, DailyPoint, RateWindow, Snapshot};

// paths.rs
pub(crate) fn sessions_path() -> PathBuf {
    let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from);
    let user_profile = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    sessions_path_from(
        codex_home.as_deref(),
        user_profile.as_deref(),
        home.as_deref(),
    )
}
~~~

Keep auth_file_path as paths::codex_home().join("auth.json"). Do not move Token parser records, cache records or Store in this task.

- [ ] **Step 4: Verify model serialization and path behavior**

Add this model regression test, then run both focused test groups:

~~~rust
#[test]
fn snapshot_keeps_camel_case_renderer_fields() {
    let encoded = serde_json::to_value(Snapshot {
        expanded: false,
        today: 0,
        last7: 0,
        last30: 0,
        daily: vec![],
        windows: vec![],
        account: AccountStatus::default(),
        rate_status: "unavailable".into(),
        rate_freshness: "unavailable".into(),
        token_coverage: "unavailable".into(),
        usage_source: "local".into(),
        today_available: false,
        updated_at: String::new(),
    })
    .unwrap();
    assert!(encoded.get("rateStatus").is_some());
    assert!(encoded.get("todayAvailable").is_some());
    assert!(encoded.get("usage_source").is_none());
}
~~~

Run:

~~~powershell
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml paths::tests
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml models::tests
~~~

Expected: both test groups PASS.

- [ ] **Step 5: Commit and push the shared-foundation extraction**

~~~powershell
git add apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/src/models.rs apps/desktop/src-tauri/src/paths.rs
git commit -m "refactor: extract shared backend foundations"
git push origin master
~~~

### Task 2: Extract state and SQLite storage

**Files:**
- Create: apps/desktop/src-tauri/src/state.rs
- Create: apps/desktop/src-tauri/src/storage.rs
- Modify: apps/desktop/src-tauri/src/lib.rs:82-394, 793-820, 1201-1399, 1471-1528
- Test: inline tests in state.rs and storage.rs

**Interfaces:**
- Consumes: models::DailyPoint, models::RateWindow and the existing SQLite schema.
- Produces: Store::new plus storage functions digest, initialize, load_window_state, save_window_position, save_window_expanded, load_rate_cache, save_rate_cache, delete_rate_cache, clear_token_data, sums, series and cleanup_old_daily.

- [ ] **Step 1: Add regression tests for restored state and persisted quota cache**

Move normalized_rate_cache_survives_database_reopen and cleanup_removes_only_data_older_than_thirty_days to storage::tests. Keep clear_token_data_removes_events_daily_and_cursors in the Token task because it asserts scan-baseline semantics. Add a Store restoration assertion:

~~~rust
#[test]
fn store_restores_expanded_flag_from_window_state() {
    let db = tempfile::NamedTempFile::new().unwrap();
    let connection = Connection::open(db.path()).unwrap();
    initialize(&connection).unwrap();
    save_window_expanded(&connection, true).unwrap();
    drop(connection);

    let store = Store::new(db.path()).unwrap();
    assert!(*store.expanded.lock().unwrap());
}
~~~

- [ ] **Step 2: Run the focused storage test before extracting storage**

Run:

~~~powershell
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml storage::tests::store_restores_expanded_flag_from_window_state
~~~

Expected: FAIL because state.rs and storage.rs do not exist yet.

- [ ] **Step 3: Move storage queries and cache/state records behind narrow functions**

Create storage.rs with the unchanged SQL schema and these signatures:

~~~rust
pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()>;
pub(crate) fn digest(input: &str) -> String;
pub(crate) fn load_window_state(db: &Connection) -> Option<(i32, i32, bool)>;
pub(crate) fn save_window_position(
    db: &Connection,
    x: i32,
    y: i32,
    expanded: bool,
) -> rusqlite::Result<()>;
pub(crate) fn save_window_expanded(
    db: &Connection,
    expanded: bool,
) -> rusqlite::Result<()>;
pub(crate) fn load_rate_cache(
    db: &Connection,
) -> Option<(Vec<RateWindow>, DateTime<Local>)>;
pub(crate) fn save_rate_cache(
    db: &Connection,
    windows: &[RateWindow],
    fetched_at: DateTime<Local>,
) -> rusqlite::Result<()>;
pub(crate) fn delete_rate_cache(db: &Connection) -> rusqlite::Result<usize>;
pub(crate) fn clear_token_data(db: &Connection) -> rusqlite::Result<()>;
pub(crate) fn sums(db: &Connection, days: i64) -> rusqlite::Result<i64>;
pub(crate) fn series(db: &Connection) -> rusqlite::Result<Vec<DailyPoint>>;
~~~

Create state.rs containing Store, AccountCache, RateCache, RateRetry, RateState, AccountUsageCache and RefreshMode. Convert the tuple returned by storage::load_rate_cache into RateCache inside Store::new so storage.rs never depends on state.rs.

~~~rust
let rate_cache = storage::load_rate_cache(&db).map(|(windows, fetched_at)| RateCache {
    windows,
    fetched_at,
});
~~~

Implement storage::save_window_expanded with the current ON CONFLICT update that changes only expanded and preserves the saved x and y coordinates.

Keep Token scan implementation temporarily in lib.rs during this task, but route its existing sums, series, clear and quota-cache calls through storage.rs.

- [ ] **Step 4: Verify persistence behavior and all Rust tests**

Run:

~~~powershell
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml storage::tests
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml
~~~

Expected: every existing Rust test still PASS; no database schema migration is created.

- [ ] **Step 5: Commit and push state/storage extraction**

~~~powershell
git add apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/src/state.rs apps/desktop/src-tauri/src/storage.rs
git commit -m "refactor: extract state and storage"
git push origin master
~~~

### Task 3: Extract Token parsing, scanning and source selection

**Files:**
- Create: apps/desktop/src-tauri/src/token.rs
- Modify: apps/desktop/src-tauri/src/storage.rs
- Modify: apps/desktop/src-tauri/src/lib.rs:49-71, 239-394, 443-450, 1110-1150, 1471-1475, 1790-1900, 2270-2400, 2467-2571
- Test: inline tests in token.rs and storage.rs

**Interfaces:**
- Consumes: paths::sessions_path, state::Store, storage statistics and persistence functions, models::DailyPoint.
- Produces: LocalTokenUsage, TokenSnapshot, read_local_usage, select_token_snapshot and clear_history for lib.rs.

- [ ] **Step 1: Relocate all Token behavior tests before implementation**

Move these existing tests into token::tests without weakening their assertions:

~~~text
aggregation_uses_cumulative_total_not_last_snapshot_field
duplicate_snapshot_is_idempotent
counter_reset_uses_new_total
second_scan_and_append_are_idempotent
event_timestamp_controls_the_daily_bucket
account_history_with_local_today_prefers_local_today_and_recalculates_totals
account_history_without_today_uses_local_today
unavailable_local_usage_uses_account_today_when_available
account_history_without_local_today_marks_today_unavailable
unavailable_account_usage_falls_back_to_one_local_group
unavailable_local_usage_marks_today_unavailable
clear_token_data_removes_events_daily_and_cursors
clearing_statistics_only_counts_tokens_added_after_the_clear
session_scan_does_not_retain_data_older_than_thirty_days
session_scan_retries_an_incomplete_trailing_json_line
session_scan_rebuilds_a_truncated_file_without_double_counting
~~~

Add a module-local source-selection assertion that preserves the special realtime source:

~~~rust
#[test]
fn local_today_replaces_account_today_without_duplicating_totals() {
    let result = select_token_snapshot(
        Some(vec![
            DailyPoint { day: "2026-10-09".into(), tokens: 10 },
            DailyPoint { day: "2026-10-10".into(), tokens: 20 },
        ]),
        LocalTokenUsage {
            today: 30,
            last7: 40,
            last30: 40,
            daily: vec![
                DailyPoint { day: "2026-10-09".into(), tokens: 10 },
                DailyPoint { day: "2026-10-10".into(), tokens: 30 },
            ],
            coverage: "partial".into(),
        },
        chrono::NaiveDate::from_ymd_opt(2026, 10, 10).unwrap(),
    );
    assert_eq!(result.usage_source, "account_history_local_today");
    assert_eq!(result.today, 30);
    assert_eq!(result.last7, 40);
}
~~~

- [ ] **Step 2: Run a moved Token test before adding token.rs**

Run:

~~~powershell
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml token::tests::local_today_replaces_account_today_without_duplicating_totals
~~~

Expected: FAIL because the token module is not implemented.

- [ ] **Step 3: Separate JSONL semantics from SQLite persistence**

Move TokenInfo, TotalUsage, LinePayload and EventLine into token.rs. Keep parsing, cumulative-delta calculation, date attribution and directory traversal in token.rs. Add persistence records and functions to storage.rs so token.rs does not embed SQL:

~~~rust
// storage.rs
pub(crate) struct SessionEvent {
    pub event_id: String,
    pub file_id: String,
    pub event_offset: i64,
    pub total_tokens: i64,
    pub delta_tokens: i64,
    pub day: String,
}
pub(crate) fn load_cursor(
    db: &Connection,
    file_id: &str,
) -> rusqlite::Result<(i64, i64)>;
pub(crate) fn remove_file_events(db: &Connection, file_id: &str) -> rusqlite::Result<()>;
pub(crate) fn record_session_event(
    db: &Connection,
    event: &SessionEvent,
) -> rusqlite::Result<bool>;
pub(crate) fn save_cursor(
    db: &Connection,
    file_id: &str,
    file_hash: &str,
    offset: i64,
    last_total: i64,
) -> rusqlite::Result<()>;
pub(crate) fn with_immediate_transaction<T>(
    db: &Connection,
    work: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T>;
~~~

Implement token::scan_sessions by preserving the existing incomplete-line, truncation, idempotency and 30-day cleanup behavior. Implement read_local_usage as the only caller that locks Store::db, scans paths::sessions_path and reads storage::sums and storage::series.

~~~rust
pub(crate) fn clear_history(state: &Store) -> Result<(), String> {
    let db = state.db.lock().map_err(|_| "database lock")?;
    storage::clear_token_data(&db).map_err(|error| error.to_string())
}
~~~

- [ ] **Step 4: Verify the Token module and full backend suite**

Run:

~~~powershell
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml token::tests
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml
~~~

Expected: all Token source, local scan, clearing and session recovery tests PASS with their original test count retained.

- [ ] **Step 5: Commit and push Token extraction**

~~~powershell
git add apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/src/storage.rs apps/desktop/src-tauri/src/token.rs
git commit -m "refactor: extract token collection"
git push origin master
~~~

### Task 4: Extract Codex Provider communication and rate/account-usage refresh

**Files:**
- Create: apps/desktop/src-tauri/src/provider.rs
- Modify: apps/desktop/src-tauri/src/lib.rs:821-836, 840-1109, 1197-1402, 1901-2050, 2199-2269, 2402-2410
- Modify: apps/desktop/src-tauri/src/storage.rs
- Test: inline tests in provider.rs

**Interfaces:**
- Consumes: models::DailyPoint and RateWindow, state::{RateCache, RateRetry, RateState, RefreshMode, Store}, storage rate-cache functions.
- Produces: read_rate_limits and read_account_usage with the stable signatures in the file-structure section.

- [ ] **Step 1: Move Provider protocol and cache tests into provider::tests**

Move these tests intact:

~~~text
first_existing_path_prefers_the_first_real_candidate
recv_response_filters_by_id
recv_response_times_out_without_matching_id
cache_is_available_until_fifteen_minutes
cache_expires_after_fifteen_minutes
rate_cache_status_changes_after_three_and_fifteen_minutes
rate_limits_parser_rejects_protocol_errors_and_empty_snapshots
rate_limits_parser_keeps_all_windows_and_sorts_by_duration
rate_retry_delay_uses_five_fifteen_then_thirty_seconds
expired_rate_cache_is_removed_from_memory_and_database
concurrent_forced_rate_refreshes_share_one_provider_call
forced_rate_refresh_ignores_a_fresh_cache
forced_rate_refresh_bypasses_retry_delay
forced_account_usage_ignores_a_fresh_cache
cached_account_usage_reuses_a_fresh_value
concurrent_forced_account_usage_refreshes_share_one_provider_call
future_cache_timestamp_is_not_accepted
~~~

- [ ] **Step 2: Run the focused parser test before provider.rs exists**

Run:

~~~powershell
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml provider::tests::rate_limits_parser_keeps_all_windows_and_sorts_by_duration
~~~

Expected: FAIL because the provider module is absent.

- [ ] **Step 3: Move app-server lifecycle and cache orchestration unchanged**

Move recv_response, first_existing_path, parse_rate_limits_response, resolve_codex_executable, read_rate_limits_uncached and read_account_usage_uncached into provider.rs. Retain the current command startup, initialize notification, timeouts, child cleanup and Windows hidden-process flag.

Implement cache functions against Store rather than passing six separate Mutex references:

~~~rust
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

pub(crate) fn read_account_usage(
    state: &Store,
    mode: RefreshMode,
) -> Option<Vec<DailyPoint>> {
    read_account_usage_with(
        &state.account_usage_cache,
        &state.account_usage_refresh,
        mode,
        read_account_usage_uncached,
    )
}
~~~

Use storage::save_rate_cache and storage::delete_rate_cache in place of inline SQL. Do not change the 120-second live refresh, 180-second live freshness, 15-minute stale cache, or 5/15/30 second retry delay constants.

- [ ] **Step 4: Verify Provider behavior without invoking a real account**

Run:

~~~powershell
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml provider::tests
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml
~~~

Expected: parser, retry, persisted cache and concurrent forced-refresh tests PASS; no test launches the actual Codex executable.

- [ ] **Step 5: Commit and push Provider extraction**

~~~powershell
git add apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/src/provider.rs apps/desktop/src-tauri/src/storage.rs
git commit -m "refactor: extract codex provider"
git push origin master
~~~

### Task 5: Extract account identity, subscription and cache behavior

**Files:**
- Create: apps/desktop/src-tauri/src/account.rs
- Modify: apps/desktop/src-tauri/src/lib.rs:395-399, 451-792, 2135-2198, 2412-2466
- Modify: apps/desktop/src-tauri/src/state.rs
- Test: inline tests in account.rs

**Interfaces:**
- Consumes: models::AccountStatus, paths::auth_file_path, state::{AccountCache, RefreshMode, Store}, storage::digest.
- Produces: query_account_status(state, mode) and no public credential-bearing types.

- [ ] **Step 1: Move account behavior tests into account::tests**

Move these tests intact:

~~~text
forced_account_status_preserves_the_last_success_on_failure
cached_account_status_reuses_a_fresh_value
subscription_state_uses_subscription_date_not_token_expiry
account_status_serialization_never_exposes_email
account_claims_keep_plan_and_subscription_date_separate
jwt_payload_decoder_rejects_malformed_tokens
~~~

Add an explicit fallback assertion:

~~~rust
#[test]
fn missing_auth_returns_unbound_status_without_access_token() {
    let cache = Mutex::new(None);
    let status = account_status_fallback(&cache, None, false, "missing auth".into());
    assert!(!status.bound);
    assert_eq!(status.subscription_state, "unknown");
    assert_eq!(status.subscription_error.as_deref(), Some("missing auth"));
}
~~~

- [ ] **Step 2: Run the malformed JWT test before account.rs exists**

Run:

~~~powershell
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml account::tests::jwt_payload_decoder_rejects_malformed_tokens
~~~

Expected: FAIL because account.rs has not been added.

- [ ] **Step 3: Move identity and subscription code without broadening data exposure**

Move SUBSCRIPTION_CHECK_URL, SUBSCRIPTIONS_URL, ACCOUNT_CACHE_MAX_AGE_SECS, decode_jwt_payload, json_string, find_key, subscription_state, normalize_plan_type, normalize_subscription_date, auth_plan_type, auth_subscription_active_until, auth_identity, subscription_request, cached_account_status, account_status_fallback, read_account_status_with and fetch_account_status into account.rs.

Keep cache identity hashed exactly as before and use the shared state locks:

~~~rust
pub(crate) fn query_account_status(
    state: &Store,
    mode: RefreshMode,
) -> AccountStatus {
    let (access, account_id, token_plan, token_active_until) = match auth_identity() {
        Ok(identity) => identity,
        Err(error) => {
            return account_status_fallback(&state.account_cache, None, false, error);
        }
    };
    let cache_identity = account_id.as_deref().unwrap_or(&access);
    let cache_key = storage::digest(cache_identity);
    let fetch_account_id = account_id.clone();
    read_account_status_with(
        &state.account_cache,
        &state.account_refresh,
        &cache_key,
        mode,
        move || fetch_account_status(
            &access,
            fetch_account_id.as_deref(),
            token_plan,
            token_active_until,
        ),
    )
}
~~~

- [ ] **Step 4: Verify account privacy and fallback contracts**

Run:

~~~powershell
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml account::tests
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml
~~~

Expected: all account cache and JWT tests PASS; AccountStatus serialization has no email, access token or raw subscription response field.

- [ ] **Step 5: Commit and push account extraction**

~~~powershell
git add apps/desktop/src-tauri/src/account.rs apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/src/state.rs apps/desktop/src-tauri/src/storage.rs
git commit -m "refactor: extract account status"
git push origin master
~~~

### Task 6: Extract window, tray and fullscreen behavior

**Files:**
- Create: apps/desktop/src-tauri/src/window.rs
- Modify: apps/desktop/src-tauri/src/lib.rs:176-199, 1476-1743, 2441-2459
- Modify: apps/desktop/src-tauri/src/storage.rs
- Test: inline tests in window.rs

**Interfaces:**
- Consumes: state::Store and storage window-setting functions.
- Produces: setup, save_position, set_expanded, handle_close_requested and start_fullscreen_monitor for Tauri wiring.

- [ ] **Step 1: Move the fullscreen visibility regression test**

Move fullscreen_visibility_preserves_user_visibility_choice into window::tests and keep the complete transition matrix:

~~~rust
#[test]
fn fullscreen_visibility_preserves_user_visibility_choice() {
    assert_eq!(
        fullscreen_visibility_transition(true, false, false),
        (true, VisibilityAction::Hide)
    );
    assert_eq!(
        fullscreen_visibility_transition(false, true, true),
        (false, VisibilityAction::None)
    );
    assert_eq!(
        fullscreen_visibility_transition(false, false, true),
        (false, VisibilityAction::Show)
    );
}
~~~

- [ ] **Step 2: Run the focused window test before window.rs exists**

Run:

~~~powershell
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml window::tests::fullscreen_visibility_preserves_user_visibility_choice
~~~

Expected: FAIL because the window module is absent.

- [ ] **Step 3: Extract all Tauri window behavior with the same labels and dimensions**

Move VisibilityAction, fullscreen_visibility_transition, start_fullscreen_monitor, the Ctrl+Shift+Space shortcut, tray creation, saved position restoration, CloseRequested handling, save_window_position and set_window_expanded into window.rs.

Use storage methods rather than inline SQL:

~~~rust
pub(crate) fn save_position(state: &Store, x: i32, y: i32) -> Result<(), String> {
    let expanded = *state.expanded.lock().map_err(|_| "lock")?;
    let db = state.db.lock().map_err(|_| "database lock")?;
    storage::save_window_position(&db, x, y, expanded).map_err(|error| error.to_string())
}

pub(crate) fn handle_close_requested(window: &tauri::WebviewWindow) {
    if window.label() == "float" {
        let state = window.app_handle().state::<Store>();
        if let Ok(mut hidden) = state.user_hidden.lock() {
            *hidden = true;
        }
    }
    let _ = window.hide();
}
~~~

Preserve labels main and float; menu IDs show-main, show-float and quit; sizes 250x64, 320x280 and restored expanded size 320x300; the 500 ms fullscreen polling cadence; and close-to-tray behavior.

- [ ] **Step 4: Verify all platform-agnostic window behavior and compile the desktop backend**

Run:

~~~powershell
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml window::tests
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml
npm --prefix apps/desktop run tauri build -- --debug --no-bundle
~~~

Expected: window state test and full Rust suite PASS; the debug backend build succeeds without launching a window.

- [ ] **Step 5: Commit and push window extraction**

~~~powershell
git add apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/src/storage.rs apps/desktop/src-tauri/src/window.rs
git commit -m "refactor: extract window runtime"
git push origin master
~~~

### Task 7: Reduce lib.rs to composition root and run the full verification gate

**Files:**
- Modify: apps/desktop/src-tauri/src/lib.rs:all production code and tests
- Modify: all newly created Rust modules only where imports or test visibility require cleanup
- Test: full frontend and Rust test suites

**Interfaces:**
- Consumes: every stable crate-internal interface declared above.
- Produces: unchanged Tauri commands get_dashboard_snapshot, refresh_dashboard_snapshot, clear_token_history, save_window_position and set_window_expanded; unchanged public run().

- [ ] **Step 1: Write the cross-module compile-time command adapters**

Replace domain implementation in lib.rs with only module declarations, imports, snapshot composition, the five command wrappers and Builder composition:

~~~rust
fn dashboard_snapshot(state: &Store, mode: RefreshMode) -> Snapshot {
    let tokens = token::select_token_snapshot(
        provider::read_account_usage(state, mode),
        token::read_local_usage(state),
        Local::now().date_naive(),
    );
    let (windows, status, freshness, rate_updated_at) =
        provider::read_rate_limits(state, mode);
    Snapshot {
        expanded: *state.expanded.lock().unwrap(),
        today: tokens.today,
        last7: tokens.last7,
        last30: tokens.last30,
        daily: tokens.daily,
        windows,
        account: account::query_account_status(state, mode),
        rate_status: status,
        rate_freshness: freshness,
        token_coverage: tokens.token_coverage,
        usage_source: tokens.usage_source,
        today_available: tokens.today_available,
        updated_at: if rate_updated_at.is_empty() {
            Local::now().to_rfc3339()
        } else {
            rate_updated_at
        },
    }
}
~~~

Keep command names and argument order exact. Delegate clear_token_history to token::clear_history, position saving to window::save_position, expansion to window::set_expanded, setup to window::setup and CloseRequested to window::handle_close_requested.

- [ ] **Step 2: Run formatting and static diff checks before the full suite**

Run:

~~~powershell
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml -- --check
git diff --check
$remaining = rg -n 'Command::new|serde_json::from_slice::<EventLine>|CREATE TABLE|subscription_request|TrayIconBuilder' apps/desktop/src-tauri/src/lib.rs
if ($LASTEXITCODE -eq 0) { throw "lib.rs still contains domain implementation: $remaining" }
if ($LASTEXITCODE -ne 1) { exit $LASTEXITCODE }
~~~

Expected: formatting and diff checks PASS; the search returns no domain implementation in lib.rs.

- [ ] **Step 3: Run the complete regression matrix**

Run:

~~~powershell
Push-Location apps/desktop
npm test -- --run
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri build -- --debug --no-bundle
Pop-Location
~~~

Expected: frontend tests PASS, frontend build succeeds, every Rust unit test PASS and Tauri debug build succeeds. Do not claim native click-through or tray interaction testing unless a visible application window is actually exercised.

- [ ] **Step 4: Inspect the final source and working tree**

Run:

~~~powershell
(Get-Content apps/desktop/src-tauri/src/lib.rs).Count
git status --short
git diff --check
~~~

Expected: lib.rs contains composition only, domain modules contain their matching tests, and .learnings/ remains untracked and unstaged.

- [ ] **Step 5: Commit and push the completed refactor**

~~~powershell
git add apps/desktop/src-tauri/src
git commit -m "refactor: split desktop backend modules"
git push origin master
~~~

## Final Acceptance Checklist

- [ ] Provider、Token、账号、窗口、存储各有独立 Rust 模块。
- [ ] lib.rs 没有 SQL、session JSONL 解析、app-server 子进程、订阅 HTTP 或窗口事件实现。
- [ ] 五个 Tauri 命令及其序列化输出保持兼容。
- [ ] 今日本机 Token 优先、账户历史回退和缓存刷新语义保持不变。
- [ ] cargo fmt check、cargo test、npm test、npm build、Tauri debug build 和 git diff check 全部通过。
- [ ] 每个已验证任务已提交并推送，.learnings/ 未进入任何提交。
