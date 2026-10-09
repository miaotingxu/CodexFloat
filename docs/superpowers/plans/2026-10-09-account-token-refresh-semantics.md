# Account Token and Refresh Semantics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Make account Token usage a real, clearly labeled dashboard source and make manual refresh force every source without breaking cache, fallback, or single-flight behavior.

**Architecture:** Keep ordinary snapshot reads and add a dedicated force-refresh command. Add RefreshMode plus in-memory account-status and account-usage caches in the existing Rust module. Construct each dashboard Token section from either account buckets or local JSONL aggregates, never a mixture.

**Tech Stack:** Rust, Tauri 2, rusqlite, chrono, React 19, TypeScript, Vitest, Vite.

## Global Constraints

- Preserve unrelated dirty-worktree changes.
- Do not expose credentials, email, raw JSON-RPC messages, or raw account responses to the renderer.
- Account and local Token totals are mutually exclusive source groups.
- Cached reads retain the 120-second quota cadence and 5/15/30-second retry policy.
- Force reads bypass old cache and retry delay but concurrent requests share one underlying refresh per source.
- Do not add a background collector, dependency, settings page, or module split.

---

### Task 1: Add force-refresh cache primitives

**Files:**
- Modify: apps/desktop/src-tauri/src/lib.rs
- Test: apps/desktop/src-tauri/src/lib.rs

**Interfaces:**
- Produce RefreshMode::Cached and RefreshMode::Force.
- Change read_rate_limits_with and read_rate_limits to accept RefreshMode.
- Add account status and account usage cache state plus separate refresh mutexes to Store.
- Produce closure-testable helpers for account status and account usage cache selection.

- [ ] **Step 1: Write failing Rust tests**

    Add tests proving:
    - Cached quota reads reuse fresh cache without invoking the provider closure.
    - Force quota reads invoke the provider despite fresh cache.
    - Force quota reads bypass a pending retry delay.
    - Concurrent force quota reads invoke one provider closure.
    - Cached account usage reuses its 120-second cache, while force bypasses it.
    - Account status force bypasses its 30-minute cache and a failed force fetch preserves the last good bound status plus an error.

- [ ] **Step 2: Run tests to establish the red state**

    Run: cargo test --manifest-path src-tauri/Cargo.toml force

    Expected: compile failure because RefreshMode and the new cache helpers do not exist.

- [ ] **Step 3: Implement RefreshMode and Store state**

    Add:

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RefreshMode { Cached, Force }

    Add AccountUsageCache with daily: Vec<DailyPoint> and fetched_at: DateTime<Local>. Add account_refresh, account_usage_cache, and account_usage_refresh mutexes to Store and initialize them in Store::new.

- [ ] **Step 4: Implement quota force behavior**

    Record request_started before attempting rate_refresh. Cached mode preserves existing cache interval and retry checks. Force mode skips those old-cache checks. After acquiring the mutex, reuse only a cache whose fetched_at is not earlier than request_started; otherwise fetch and save. On a failed fetch, retain stale cache.

- [ ] **Step 5: Implement account cache helpers**

    Extract account-status remote work into a Result<AccountStatus, String> closure path. Cached status accepts a matching key younger than 30 minutes. Force accepts only an entry refreshed after request_started. Protect remote work with account_refresh. On a failure, clone the last matching successful AccountStatus and update only subscription_error.

    Wrap read_account_usage_uncached in a helper with the same request-started coalescing rule and a 120-second cache. Cache only non-empty normalized daily points.

- [ ] **Step 6: Run Rust tests**

    Run: cargo test --manifest-path src-tauri/Cargo.toml

    Expected: all old and new cache tests pass.

---

### Task 2: Select one Token source per snapshot and expose both commands

**Files:**
- Modify: apps/desktop/src-tauri/src/lib.rs
- Test: apps/desktop/src-tauri/src/lib.rs

**Interfaces:**
- Produce internal TokenSnapshot { today, last7, last30, daily, token_coverage, usage_source, today_available }.
- Produce dashboard_snapshot(state, mode), get_dashboard_snapshot, and refresh_dashboard_snapshot.

- [ ] **Step 1: Write failing source-selection tests**

    Add fixed-date tests that assert:
    - Account buckets containing today supply every displayed Token field and yield account coverage/source.
    - Account history without today yields today = 0, today_available = false, and account_history_pending_today without using local today.
    - Account failure selects every local aggregate field as a group.
    - Unavailable local data yields zero values and unavailable coverage.

- [ ] **Step 2: Run a focused test to establish the red state**

    Run: cargo test --manifest-path src-tauri/Cargo.toml account_usage_with_today

    Expected: compile failure because TokenSnapshot and source selection do not exist.

- [ ] **Step 3: Implement pure Token selection**

    Add TokenSnapshot and select_token_snapshot(account_daily, local_usage, local_day). Change usage_totals so it never receives or substitutes local_today. Account buckets containing today use account source. Account buckets missing today retain only account history and mark today pending. Missing account data uses local values only.

- [ ] **Step 4: Wire snapshot commands**

    Extract dashboard_snapshot(state, mode). It scans local sessions, reads account usage with mode, selects TokenSnapshot, reads quota with mode, reads account status with mode, and maps all values into the existing renderer Snapshot.

    Register:

    get_dashboard_snapshot -> dashboard_snapshot(Cached)
    refresh_dashboard_snapshot -> dashboard_snapshot(Force)

    in tauri::generate_handler!.

- [ ] **Step 5: Run complete Rust suite**

    Run: cargo test --manifest-path src-tauri/Cargo.toml

    Expected: the two account-usage functions are no longer dead code and all tests pass.

---

### Task 3: Route both UI refresh buttons to force refresh and validate

**Files:**
- Modify: apps/desktop/src/main.tsx
- Test: apps/desktop/src/format.test.ts only if a pure formatting assertion is needed
- Modify: docs/PROJECT-STATUS.md only if an existing status claim must change

**Interfaces:**
- Startup and the 120-second timers call get_dashboard_snapshot.
- Manual refresh buttons call refresh_dashboard_snapshot.

- [ ] **Step 1: Update both refresh callbacks**

    Change each refresh callback to accept force: boolean. Choose command with force ? refresh_dashboard_snapshot : get_dashboard_snapshot. Initial load and timers call refresh(false); the two button handlers call refresh(true). Preserve requestActive, disabled buttons, refreshFailed, and previous successful snapshot state.

- [ ] **Step 2: Correct source labels**

    Replace the Dashboard label 账户实时 with 账户 Token. Render account trend coverage as 账户日桶, pending history as 账户历史 · 今日待结算, and keep local/unavailable labels. Update the floating-window title so account coverage is not described as unavailable local data.

- [ ] **Step 3: Run frontend and full-project validation**

    Run:

    npm test -- --run
    npm run build
    cargo test --manifest-path src-tauri/Cargo.toml
    npm run tauri build -- --debug --no-bundle
    git diff --check

    Expected: every command exits zero and the Debug executable is generated under apps/desktop/src-tauri/target/debug/codex-float.exe.

- [ ] **Step 4: Inspect staging and commit only this feature**

    Stage only the intended hunks in lib.rs, main.tsx, and PROJECT-STATUS.md if it changed. Do not stage unrelated existing edits in PRIVACY.md, format.ts, format.test.ts, style.css, capability files, or untracked user files.

## Plan Self-Review

- Spec coverage: Task 1 implements force/cache semantics and last-known-good account status; Task 2 implements exclusive account/local Token selection and both commands; Task 3 updates both UIs and validates the desktop build.
- Placeholder scan: no TBD, TODO, or implicit test instruction remains.
- Type consistency: RefreshMode flows through quota, account status, account usage, and dashboard_snapshot; TokenSnapshot maps into the existing Snapshot DTO; refresh_dashboard_snapshot is registered before the UI invokes it.
