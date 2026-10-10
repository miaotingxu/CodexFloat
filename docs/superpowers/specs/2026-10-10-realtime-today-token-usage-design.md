# 当天 Token 实时统计设计

**日期**：2026-10-10  
**状态**：用户已确认，待评审  
**范围**：取消“今日待结算”，以本机增量统计提供当天 Token 值，并保持 2 分钟自动刷新。

## 1. 目标

1. 当天 Token 不再显示“待结算”或“待更新”；
2. 当前本地日期的 Token 优先使用本机 session JSONL 的增量聚合；
3. 历史日桶继续优先使用账户 `account/usage/read` 数据；
4. 浮窗和主窗口每 120 秒更新一次快照，手动刷新继续强制刷新全部来源；
5. 当本机当天数据不可用时，显示“--”和明确的不可用原因，而不是伪造 0 或等待提示。

## 2. 数据口径

### 2.1 账户历史 + 本机实时当天（主路径）

本机 session 扫描成功时，始终用本机当天累计值替换账户日桶中同一天的值：

- 账户数据仅保留今天之前的日桶；
- 本机 `local.today` 作为今天唯一的日桶；
- `daily` 由上述历史账户日桶和本机今天日桶组成，按日期排序；
- `today`、`last7`、`last30` 都从合并后的 `daily` 重算；
- 新增 `usageSource = "account_history_local_today"`；
- 新增 `tokenCoverage = "account_history_local_today"`；
- `todayAvailable = true`。

即使账户响应已带今天，也仍优先本机今天值；账户日桶可能有结算延迟，不能称为实时值。

### 2.2 账户数据不可用

账户用量读取失败或无有效日桶时，继续沿用本机全量统计：

- `usageSource = "local"`；
- 本机扫描可用时 `tokenCoverage = "partial"` 且 `todayAvailable = true`；
- 本机扫描不可用时 `tokenCoverage = "unavailable"` 且 `todayAvailable = false`。

### 2.3 本机当天数据不可用但账户有今天

若本机扫描失败、但账户日桶含今天：

- 整组保持账户数据；
- `usageSource = "account"`；
- `tokenCoverage = "account"`；
- `todayAvailable = true`。

这是唯一允许账户日桶作为当天回退来源的情况。

### 2.4 本机当天数据不可用且账户缺少今天

若账户只有历史数据且本机当天不可用：

- 历史趋势和 7/30 天汇总保留账户历史数据；
- `today = 0`，但 `todayAvailable = false`；
- `usageSource = "account_history_today_unavailable"`；
- `tokenCoverage = "account"`；
- UI 今日数值显示“--”，来源说明为“账户历史 · 今日本机数据不可用”。

不显示“待结算”“待更新”或误导性的 0。

## 3. 刷新行为

- 两个 React 视图继续在首次挂载时请求快照，此后每 `120_000` ms 请求一次 `get_dashboard_snapshot`；
- 每次快照读取都会运行现有本机 session 增量扫描，因此当天本机值的最大自动更新间隔为 2 分钟；
- 账户用量缓存继续只影响账户历史读取，不阻止本机当天扫描；
- 用户点击刷新仍使用 `refresh_dashboard_snapshot`，强制尝试账户、额度和账号状态刷新；
- 同一窗口仍用现有 `requestActive` 防止重入，失败时保留上一份快照。

## 4. UI 与隐私

新增来源文案：

| usageSource | UI 文案 |
|---|---|
| `account_history_local_today` | 账户历史 · 本机实时今日 |
| `account_history_today_unavailable` | 账户历史 · 今日本机数据不可用 |
| `account` | 账户 Token 用量 |
| `local` + `partial` | 本机 Token 用量 · 部分覆盖 |
| `local` + `unavailable` | 本机 Token 数据不可用 |

账户 OAuth、邮箱、Cookie、原始 JSON-RPC 和 session 正文继续不进入 renderer 或持久化。

## 5. 变更边界

- 修改 Rust 的 Token 快照选择与对应单元测试；
- 修改 React Snapshot 联合类型、来源文案和今天不可用展示；
- 将 120 秒轮询提取为命名常量，语义保持为 2 分钟；
- 更新项目状态与隐私说明中的 Token 口径；
- 不新增账户 API、后台服务、设置页或第二套用量面板；
- 不改动额度刷新、账户状态缓存、清空本机统计与本机增量扫描机制。

## 6. 验收

1. 账户历史缺今天、本机扫描可用时，今天显示本机累计值而不是“待结算”；
2. 账户带今天且本机扫描可用时，今天仍显示本机累计值；
3. 合并快照的趋势、7 天和 30 天汇总不重复计算今天；
4. 本机不可用且账户有今天时，仍能显示账户今天值；
5. 两个来源都无法提供今天时，UI 显示“--”及不可用文案；
6. 自动快照刷新间隔保持 120 秒；
7. `npm test -- --run`、`npm run build`、`cargo test --manifest-path src-tauri/Cargo.toml`、`npm run tauri build -- --debug --no-bundle` 和 `git diff --check` 均通过。
