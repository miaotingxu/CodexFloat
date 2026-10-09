# 账户 Token 与刷新语义收口设计

**日期**：2026-10-09  
**状态**：已确认，待实施  
**范围**：收口当前未提交 patch 中的账户 Token 死代码与手动刷新语义

## 1. 目标

在不拆分现有 Rust/React 单体文件、不引入后台常驻采集服务的前提下，完成以下闭环：

1. 将现有账户 Token 读取代码接入正式快照路径；
2. 账户 Token 与本机 Token 始终整组选择，禁止静默混算；
3. 区分普通快照读取与用户主动强制刷新；
4. 强制刷新绕过正常缓存和失败退避，但继续使用 single-flight；
5. 保留当前失败降级、隐私边界和本机统计清空语义。

## 2. 非目标

- 不拆分 `apps/desktop/src-tauri/src/lib.rs`；
- 不拆分 `apps/desktop/src/main.tsx`；
- 不引入后台常驻采集线程或事件推送；
- 不新增设置页、诊断页、开机启动或安装发布能力；
- 不同时展示账户 Token 与本机 Token 两套面板；
- 不将账户统计与本机统计相加或补齐。

## 3. Token 数据口径

### 3.1 账户数据完整可用

当 `account/usage/read` 返回有效日桶且包含本地日期的今天时：

- `today`、`last7`、`last30` 和 `daily` 全部来自账户日桶；
- `usageSource = "account"`；
- `tokenCoverage = "account"`；
- `todayAvailable = true`。

### 3.2 账户历史可用但今日尚未结算

当账户日桶有效、包含历史数据但没有本地日期的今天时：

- `today = 0`，界面显示“待结算”；
- `last7`、`last30` 和 `daily` 全部来自账户日桶；
- 不使用本机今日值补齐账户数据；
- `usageSource = "account_history_pending_today"`；
- `tokenCoverage = "account"`；
- `todayAvailable = false`。

### 3.3 账户数据不可用

当账户用量请求失败、返回错误或没有有效日桶时：

- `today`、`last7`、`last30` 和 `daily` 全部回退到本机 JSONL 聚合；
- 本机扫描成功时 `usageSource = "local"`、`tokenCoverage = "partial"`、`todayAvailable = true`；
- 本机来源不可用时 `usageSource = "local"`、`tokenCoverage = "unavailable"`、`todayAvailable = false`；
- 不保留部分账户字段与本机字段组成混合快照。

### 3.4 清空本机统计

“清空统计”只删除 Codex Float 的本机 `events` 与 `daily` 聚合，不删除：

- Codex 原始 session JSONL；
- 本机扫描游标；
- OAuth 数据；
- 账户用量数据。

清空后若账户数据可用，页面仍可展示账户统计。

## 4. 刷新接口

### 4.1 内部模式

增加内部刷新模式：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshMode {
    Cached,
    Force,
}
```

快照组装逻辑接收 `RefreshMode`，两个 Tauri command 只负责选择模式：

```rust
#[tauri::command]
fn get_dashboard_snapshot(state: tauri::State<Store>) -> Snapshot;

#[tauri::command]
fn refresh_dashboard_snapshot(state: tauri::State<Store>) -> Snapshot;
```

- 启动和 120 秒定时轮询调用 `get_dashboard_snapshot`；
- 用户点击刷新调用 `refresh_dashboard_snapshot`。

### 4.2 额度刷新

`Cached` 模式：

- 120 秒内允许直接返回额度缓存；
- 遵守 5、15、30 秒失败退避；
- 过期但仍在 15 分钟有效期内的缓存按 `stale` 返回。

`Force` 模式：

- 绕过 120 秒缓存命中；
- 绕过失败退避并立即尝试 Provider；
- 继续获取 `rate_refresh` 锁，多个强制请求只执行一次实际 Provider 调用；
- 请求在等待锁之前记录开始时间；取得锁后如果缓存抓取时间不早于该开始时间，直接复用前一个并发请求刚写入的结果；
- Provider 失败时仍可返回 15 分钟内的 stale 缓存；
- 不因强制刷新失败删除最近有效缓存。

### 4.3 账户状态刷新

`Cached` 模式继续使用 30 分钟账户状态缓存。

`Force` 模式绕过旧缓存并重新读取账号与订阅状态。账户状态增加独立刷新锁，请求在等待锁之前记录开始时间；取得锁后如果缓存抓取时间不早于该开始时间，则复用前一个并发请求刚写入的结果。刷新失败时：

- 优先保留最近成功的绑定状态、套餐和订阅日期；
- 记录脱敏错误文本；
- 不把已经绑定的账号瞬间降级成“未绑定”；
- 不向 renderer 传递邮箱、OAuth Token、Cookie 或原始响应。

### 4.4 账户 Token 缓存与 single-flight

在 `Store` 中增加独立的账户用量缓存和刷新锁。缓存保存标准化的 `Vec<DailyPoint>` 及抓取时间，不保存原始响应。

- `Cached` 模式在 120 秒内复用账户用量缓存；
- `Force` 模式绕过缓存年龄；
- 所有实际账户用量读取均受 single-flight 锁保护；
- 请求在等待锁之前记录开始时间；取得锁后如果缓存抓取时间不早于该开始时间，复用前一个并发请求刚写入的结果；
- 读取失败时不产生混合数据，快照整组回退本机统计。

### 4.5 本机 Token 扫描

普通读取和强制刷新都继续执行现有幂等增量扫描。扫描使用 byte offset、事件主键和事务避免重复累计，因此无需额外的 `force` 分支。

## 5. 前端行为

浮窗和主窗口共用以下规则：

- 启动与定时器调用 `get_dashboard_snapshot`；
- 刷新按钮调用 `refresh_dashboard_snapshot`；
- 同一窗口已有请求进行中时忽略重复触发；
- 刷新期间禁用按钮并显示刷新状态；
- 强制刷新失败时保留上一次成功快照，显示“刷新失败”，不清空页面。

来源文案：

| `usageSource` | UI 文案 |
|---|---|
| `account` | 账户 Token 用量 |
| `account_history_pending_today` | 账户历史用量 · 今日待结算 |
| `local` + `partial` | 本机 Token 用量 · 部分覆盖 |
| `local` + `unavailable` | 本机 Token 数据不可用 |

不使用“账户实时”文案，因为账户日桶可能存在服务端结算延迟。

## 6. 失败处理

- 账户 Token 失败不影响额度、账号状态和本机 Token 的返回；
- 额度失败不清空 Token 数据；
- 本机扫描失败时，若账户数据可用，仍可返回完整账户 Token 快照；
- 任一失败不得将凭据、邮箱、原始 JSON-RPC 或响应正文写入 UI；
- 锁中毒或数据库锁失败时保持当前安全降级，不 panic；
- 子进程生命周期的完整进程树治理留给后续 Provider 稳定化任务，本轮不扩大范围。

## 7. 测试要求

Rust 单元测试至少覆盖：

1. 新鲜额度缓存下，`Cached` 不调用 Provider；
2. 新鲜额度缓存下，`Force` 仍调用 Provider；
3. `Force` 绕过失败退避；
4. 并发强制刷新仍只执行一次实际 Provider 调用；
5. 账户日桶包含今天时整组使用账户数据；
6. 账户日桶缺少今天时今日不可用，历史不混入本机值；
7. 账户读取失败时整组回退本机数据；
8. 账户用量缓存的普通命中和强制绕过；
9. 账号状态强制刷新绕过 30 分钟缓存；
10. 账号状态刷新失败保留最近成功状态。

完整验证命令：

```powershell
Set-Location D:\Code\github\CodexFloat\apps\desktop
npm test -- --run
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri build -- --debug --no-bundle
Set-Location D:\Code\github\CodexFloat
git diff --check
```

自动测试和 Debug 构建通过不等同于 Windows 真实交互验收。本轮至少还应手动确认两个窗口的刷新按钮会显示加载状态、不会清空旧数据，并且来源文案与实际快照一致。

## 8. 完成标准

- `read_account_usage_uncached` 与 `usage_totals` 不再是死代码；
- Rust 构建不再产生这两个函数的 `dead_code` 警告；
- 普通读取和手动强制刷新具有不同且可测试的语义；
- 账户与本机 Token 从不混算；
- 两个窗口都使用新的强制刷新命令；
- 现有额度缓存、退避、stale 降级和本机 Token 幂等语义不回归；
- 前端测试、Rust 测试、Vite 构建和 Tauri Debug 构建全部通过。
