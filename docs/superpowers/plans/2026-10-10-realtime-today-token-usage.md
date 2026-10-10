# 当天 Token 实时统计 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不等待账户日桶结算的前提下，显示每两分钟更新的本机当天 Token 累计，并以账户日桶保留历史统计。

**Architecture:** Rust 的纯 Token 选择函数按日期合并来源：账户只提供今天之前的历史，本机增量扫描提供今天；随后从合并日序列重算今天、7 天和 30 天。React 只渲染新的明确来源状态，两个窗口继续使用同一个 120 秒快照节奏和现有手动强制刷新 command。

**Tech Stack:** Rust、Tauri 2、chrono、rusqlite、React 19、TypeScript、Vitest、Vite。

## Global Constraints

- 自动刷新间隔必须保持精确的 `120_000` ms（2 分钟）。
- 本机可用时，今天必须优先于账户日桶，即使账户响应已包含今天。
- 账户历史和本机今天的合并必须按日期替换，不能把同一天相加或重复计数。
- 本机今天不可用时，只能回退到账户今天；两个来源都无今天时显示 `--`，不得显示“待结算”“待更新”或伪造 0。
- 不新增账户 API、后台常驻服务、依赖、设置页或第二套用量面板。
- 不向 renderer、SQLite 或日志传递邮箱、OAuth 凭据、Cookie、原始 JSON-RPC 或 session 正文。
- 保留现有额度刷新、账户状态缓存、清空本机 Token 统计和手动强制刷新语义。
- 不暂存或提交现有未跟踪的 `.learnings/`。

---

### Task 1: 按日期合并账户历史与本机当天

**Files:**
- Modify: `apps/desktop/src-tauri/src/lib.rs`
- Test: `apps/desktop/src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `select_token_snapshot(account_daily: Option<Vec<DailyPoint>>, local: LocalTokenUsage, today: NaiveDate) -> TokenSnapshot`。
- Produces: `usage_source` 值 `account_history_local_today`、`account_history_today_unavailable`、`account` 或 `local`。
- Produces: `token_coverage` 值 `account_history_local_today`、`account`、`partial` 或 `unavailable`。
- Keeps: `usage_totals(daily, today)` 是唯一计算 today、last7、last30 的函数。

- [ ] **Step 1: 先改写会失败的 Rust 选择测试**

  用现有 `point` 测试辅助函数替换旧的“账户优先整组”断言，并新增以下测试：

  ```rust
  #[test]
  fn account_history_with_local_today_prefers_local_today_and_recalculates_totals() {
      let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
      let selected = select_token_snapshot(
          Some(vec![point("2026-10-08", 30), point("2026-10-09", 70)]),
          LocalTokenUsage {
              today: 999,
              last7: 1_999,
              last30: 2_999,
              daily: vec![point("2026-10-09", 999)],
              coverage: "partial".into(),
          },
          today,
      );

      assert_eq!(selected.today, 999);
      assert_eq!(selected.last7, 1_029);
      assert_eq!(selected.last30, 1_029);
      assert_eq!(selected.daily.last().unwrap().tokens, 999);
      assert_eq!(selected.usage_source, "account_history_local_today");
      assert_eq!(selected.token_coverage, "account_history_local_today");
      assert!(selected.today_available);
  }
  ```

  另加两项固定日期测试：账户只有历史但本机今天可用时同样生成本机今天；本机不可用时，账户带今天则保持 `account`，账户缺今天则产生 `account_history_today_unavailable` 且 `today_available = false`。

- [ ] **Step 2: 运行聚焦测试，确认旧逻辑不符合新口径**

  Run:

  ```powershell
  Set-Location D:\Code\github\CodexFloat\apps\desktop
  cargo test --manifest-path src-tauri/Cargo.toml account_history_with_local_today
  ```

  Expected: FAIL，因为当前逻辑保留账户当天值或把缺今天的状态标为 `account_history_pending_today`。

- [ ] **Step 3: 实现按日合并 helper 和新回退状态**

  在 `usage_totals` 与 `select_token_snapshot` 附近添加一个纯 helper：

  ```rust
  fn account_history_with_local_today(
      account_daily: Vec<DailyPoint>,
      local_today: i64,
      today: chrono::NaiveDate,
  ) -> Vec<DailyPoint> {
      let today_key = today.to_string();
      let mut merged: Vec<DailyPoint> = account_daily
          .into_iter()
          .filter(|point| point.day < today_key)
          .collect();
      merged.push(DailyPoint {
          day: today_key,
          tokens: local_today,
      });
      merged.sort_by(|left, right| left.day.cmp(&right.day));
      merged
  }
  ```

  在 `select_token_snapshot` 中按以下顺序选择：

  1. 账户日桶非空且 `local.coverage != "unavailable"`：调用 helper，使用合并日序列重算全部 totals，返回 `account_history_local_today`。
  2. 账户日桶非空且本机不可用、账户含今天：保持完整账户日序列，返回 `account`。
  3. 账户日桶非空且本机不可用、账户缺今天：保留账户历史 totals，返回 `account_history_today_unavailable` 和 `today_available = false`。
  4. 账户日桶为空：保留现有本机全量回退。

  删除 `account_history_pending_today` 的生成路径。不要以 `local.daily`、`local.last7` 或 `local.last30` 补齐账户历史，避免引入重复或过期的本机历史。

- [ ] **Step 4: 运行完整 Rust 测试与格式检查**

  Run:

  ```powershell
  Set-Location D:\Code\github\CodexFloat\apps\desktop
  cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
  cargo test --manifest-path src-tauri/Cargo.toml
  ```

  Expected: 所有既有测试和新的按日来源测试通过，Rust 不再引用旧的 pending source 字符串。

- [ ] **Step 5: 提交该可验证的 Rust 变更**

  ```powershell
  Set-Location D:\Code\github\CodexFloat
  git add -- apps/desktop/src-tauri/src/lib.rs
  git commit -m "fix: use local tokens for today"
  ```

---

### Task 2: 统一两个窗口的 2 分钟显示与来源文案

**Files:**
- Modify: `apps/desktop/src/main.tsx`

**Interfaces:**
- Consumes: renderer Snapshot 的新 `usageSource` 和 `tokenCoverage` 字面量。
- Produces: 浮窗和主窗口对 `account_history_local_today` 与 `account_history_today_unavailable` 的一致文案。
- Keeps: `refresh(false)` 调用 `get_dashboard_snapshot`，`refresh(true)` 调用 `refresh_dashboard_snapshot`。

- [ ] **Step 1: 更新 Snapshot 联合类型和刷新常量**

  在 `main.tsx` 顶部定义：

  ```ts
  const SNAPSHOT_REFRESH_INTERVAL_MS = 120_000;
  ```

  将类型收敛为：

  ```ts
  tokenCoverage: "partial" | "unavailable" | "account" | "account_history_local_today";
  usageSource:
    | "account"
    | "account_history_local_today"
    | "account_history_today_unavailable"
    | "local";
  ```

  把 `FloatApp` 与 `MainWindow` 的两个 `window.setInterval(..., 120_000)` 均替换为 `SNAPSHOT_REFRESH_INTERVAL_MS`；不得改成 5 分钟，也不得调用强制刷新 command。

- [ ] **Step 2: 移除“待结算”显示并映射新的来源**

  在浮窗 totals 区和主窗口“今日 Token”卡片中统一采用：

  ```tsx
  {snapshot.todayAvailable ? formatTokens(snapshot.today) : "--"}
  ```

  将原本判断 `account_history_pending_today` 的位置替换为：

  - `account_history_local_today`：`账户历史 · 本机实时今日`；
  - `account_history_today_unavailable`：`账户历史 · 今日本机数据不可用`；
  - `account`：`账户 Token` 或 `账户 Token 用量`；
  - `local`：保持现有本机回退或不可用文案。

  更新浮窗 footer、主窗口今日卡片标签和趋势标签。源码中不应再出现 `待结算` 或 `account_history_pending_today`。

- [ ] **Step 3: 运行前端静态验证**

  Run:

  ```powershell
  Set-Location D:\Code\github\CodexFloat\apps\desktop
  npm test -- --run
  npm run build
  ```

  Expected: Vitest 既有格式化测试通过，TypeScript 正确接受新的联合类型，Vite 生产构建成功。

- [ ] **Step 4: 提交 UI 变更**

  ```powershell
  Set-Location D:\Code\github\CodexFloat
  git add -- apps/desktop/src/main.tsx
  git commit -m "fix: label realtime today token usage"
  ```

---

### Task 3: 使产品状态和隐私说明与新口径一致

**Files:**
- Modify: `docs/PROJECT-STATUS.md`
- Modify: `PRIVACY.md`

**Interfaces:**
- Documents: 历史账户日桶 + 本机当天的显式按日合并、2 分钟刷新和不可用回退。
- Must not document: “账户与本机 Token 从不混算”或“今日待结算”作为现行产品行为。

- [ ] **Step 1: 更新项目状态的 Token 口径和测试覆盖说明**

  在 `docs/PROJECT-STATUS.md` 的 “3.4 Token 数据口径与本机统计” 中改为说明：

  - 账户只提供今天之前的历史日桶；
  - 本机 JSONL 增量扫描每 120 秒提供今天实时累计；
  - 所有聚合从这份按日期替换后的日序列计算；
  - 本机不可用时才使用账户今天，两个来源都无今天时显示不可用而非等待；
  - 手动刷新仍会强制刷新全部 Provider。

  在 Rust 测试覆盖列表中，将“今日待结算的互斥数据口径”改为“账户历史与本机实时今天的按日替换、无重复计数与不可用回退”。

- [ ] **Step 2: 更新 Privacy 的 Coverage 表述**

  把 `PRIVACY.md` 的 Coverage 段落改为说明：当天本机 session 聚合可与账户的历史日桶共同显示，但二者按日期替换而非相加；页面每 2 分钟重新扫描本机记录；该指标不是账单或精确计费数据。

- [ ] **Step 3: 检查文档与未跟踪文件边界**

  Run:

  ```powershell
  Set-Location D:\Code\github\CodexFloat
  rg -n "待结算|待更新|account_history_pending_today" apps/desktop/src docs/PROJECT-STATUS.md PRIVACY.md
  git diff --check
  git status --short
  ```

  Expected: 第一个命令没有命中；空白检查通过；`.learnings/` 仍未暂存。

- [ ] **Step 4: 提交文档变更**

  ```powershell
  git add -- docs/PROJECT-STATUS.md PRIVACY.md
  git commit -m "docs: clarify realtime today token coverage"
  ```

---

### Task 4: 完整构建验证与收口

**Files:**
- Verify only: `apps/desktop/src-tauri/src/lib.rs`
- Verify only: `apps/desktop/src/main.tsx`
- Verify only: `docs/PROJECT-STATUS.md`
- Verify only: `PRIVACY.md`

**Interfaces:**
- Verifies: `get_dashboard_snapshot` 保持 cached 语义，两个前端窗口每 2 分钟读取一次并展示本机当天数据。
- Verifies: `refresh_dashboard_snapshot` 的手动 force 语义不变。

- [ ] **Step 1: 运行全套自动化与 Debug 打包**

  Run:

  ```powershell
  Set-Location D:\Code\github\CodexFloat\apps\desktop
  npm test -- --run
  npm run build
  cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
  cargo test --manifest-path src-tauri/Cargo.toml
  npm run tauri build -- --debug --no-bundle
  ```

  Expected: 所有命令退出码为 0，Debug 可执行文件位于 `apps/desktop/src-tauri/target/debug/codex-float.exe`。

- [ ] **Step 2: 做源码级回归核对**

  Run:

  ```powershell
  Set-Location D:\Code\github\CodexFloat
  rg -n "SNAPSHOT_REFRESH_INTERVAL_MS|account_history_local_today|account_history_today_unavailable" apps/desktop/src/main.tsx apps/desktop/src-tauri/src/lib.rs
  rg -n "account_history_pending_today|待结算|待更新" apps/desktop/src docs/PROJECT-STATUS.md PRIVACY.md
  git diff --check
  git status --short --branch
  ```

  Expected: 第一条包含新常量和两个来源状态；第二条无命中；空白检查通过；除 `.learnings/` 外无未提交产品文件。

- [ ] **Step 3: 仅在窗口句柄可用时做手动冒烟**

  启动已有的 Debug 可执行文件，在浮窗与主窗口确认：

  1. 今日 Token 显示数字或 `--`，绝不显示“待结算”；
  2. 账户历史 + 本机当天的来源文案为“账户历史 · 本机实时今日”；
  3. 两个刷新按钮仍会出现加载态并保留旧快照。

  若 UI 自动化环境无法枚举原生窗口，记录该限制，不将启动成功误报为已完成点击验收。

## Plan Self-Review

- Spec coverage: Task 1 实现按日期替换、两个不可用回退和 totals 重算；Task 2 保持两窗口 2 分钟 cadence 并删除等待文案；Task 3 同步状态与隐私说明；Task 4 覆盖自动化、Debug 构建与可选原生 UI 验收。
- Placeholder scan: 每个实现任务都指定了文件、接口、命令和预期结果，且没有未定义工作项。
- Type consistency: Rust 输出的 `account_history_local_today` 和 `account_history_today_unavailable` 均加入 TypeScript 联合类型；前端间隔常量固定为 `120_000`；`todayAvailable` 控制 `--` 显示。
