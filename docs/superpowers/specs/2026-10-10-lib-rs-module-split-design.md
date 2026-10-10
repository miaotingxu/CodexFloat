# `lib.rs` 职责拆分设计

**日期**：2026-10-10
**状态**：用户已确认，待评审
**范围**：将桌面端 Rust 后端中 Provider、Token、账号、窗口和存储职责从 `apps/desktop/src-tauri/src/lib.rs` 拆出；保持 IPC、持久化格式和用户可见行为不变。

## 1. 目标

1. 将当前 `lib.rs` 中混杂的 Tauri 装配、SQLite、session JSONL、Codex app-server、账户订阅和窗口生命周期拆为单一职责模块；
2. 保持现有命令、`Snapshot` JSON 字段、SQLite 表结构、缓存时效和刷新语义不变；
3. 将现有 Rust 单元测试按职责迁移，继续覆盖本地 Token 增量、账户缓存、Provider 协议和窗口可见性规则；
4. 让后续新增 Provider、用量来源或窗口行为时不再需要修改一个大型中心文件。

## 2. 非目标

- 不修改 React 前端、Tauri 命令名称或 IPC 参数；
- 不调整 `codex app-server` 请求协议、账户 HTTP 接口或 Token 数据口径；
- 不引入新依赖、后台服务、异步运行时或新的数据库迁移框架；
- 不借此次重构改变刷新间隔、缓存 TTL、错误提示或隐私边界。

## 3. 目标目录结构

```text
apps/desktop/src-tauri/src/
├── lib.rs        # Tauri 装配、命令入口、仪表盘快照编排
├── models.rs     # 对 renderer 可序列化的共享 DTO
├── state.rs      # Store、缓存、刷新锁和刷新模式
├── paths.rs      # CODEX_HOME、认证文件和 sessions 路径解析
├── storage.rs    # SQLite 初始化、扫描持久化、查询和本地设置
├── token.rs      # JSONL 解析、本机用量采集和账户/本地用量合并
├── provider.rs   # Codex app-server 启动、JSON-RPC 收发和用量/额度读取
├── account.rs    # auth.json、JWT 声明、订阅 API 与账户状态读取
└── window.rs     # 悬浮窗、托盘、快捷键、关闭和全屏可见性处理
```

`models.rs`、`state.rs` 和 `paths.rs` 是避免五个业务模块互相循环依赖的支撑模块，不承担新的业务能力。

## 4. 模块边界

| 模块 | 负责内容 | 不负责内容 |
|---|---|---|
| `models` | `RateWindow`、`Snapshot`、`DailyPoint`、`AccountStatus` 等共享数据模型 | 读取文件、网络、SQL 或 Tauri API |
| `state` | `Store`、Rate/账号/账户用量缓存、刷新锁、`RefreshMode` | Provider 协议解析、SQL 细节或窗口事件 |
| `paths` | `CODEX_HOME` 覆盖、默认 Codex 根目录、`auth.json` 和 sessions 目录定位 | 文件内容解析 |
| `storage` | 数据库建表、游标/事件/日桶写入、统计查询、额度缓存和窗口设置读写 | session JSON 语义、账户接口和 Tauri 窗口控制 |
| `token` | session JSONL 行解析、累计值差分、本地用量采集、当天本机值与账户历史合并 | SQL 表设计、Provider 子进程和账户 HTTP 请求 |
| `provider` | 可执行文件定位、app-server 生命周期、JSON-RPC 收发、额度与账户用量响应解析 | session 扫描、订阅状态和窗口操作 |
| `account` | OAuth 文件读取、JWT 解析、订阅接口请求、账户状态回退与缓存使用 | Provider 子进程、Token 日桶合并 |
| `window` | 浮窗恢复/保存、展开状态、托盘、快捷键、关闭拦截和全屏监测 | 账户、Token 和 Provider 业务规则 |
| `lib` | 组装模块、保留现有 Tauri 命令、调用各领域模块生成快照 | 领域实现细节 |

## 5. 依赖方向

依赖只朝共享基础设施和模型方向流动，避免模块之间形成环：

```text
models   paths   storage
   ^       ^       ^
   │       │       │
 state ────┴───────┘
   ^        ^       ^
   │        │       │
account  provider token    window
   \        |      /         /
    \       |     /         /
     └──────lib.rs─────────┘
```

- `state` 可调用 `storage` 初始化 `Store`，但 `storage` 不引用 `state`；
- `token` 通过 `storage` 的窄接口读取和写入本地统计，不直接操作其它领域缓存；
- `provider` 只产出额度窗口或账户日桶；账户日桶与本机当天值的选择仍由 `token` 完成；
- `lib.rs` 是唯一的跨领域编排点：读取额度、账户状态、账户历史与本机 Token 后构建 `Snapshot`；
- `window` 仅依赖应用状态和持久化设置，不参与快照生成。

## 6. 兼容性约束

以下项目必须字节级或语义上保持兼容：

1. Tauri 命令：`get_dashboard_snapshot`、`refresh_dashboard_snapshot`、`clear_token_history`、`save_window_position`、`set_window_expanded`；
2. renderer 使用的 `Snapshot` 字段、camelCase 序列化名和 `usageSource` / `tokenCoverage` 枚举值；
3. SQLite 的 `events`、`daily`、`cursors`、`rate_cache`、`settings` 等既有表及数据清理策略；
4. 强制刷新绕过缓存、并发强制刷新共用单次 Provider 调用、失败回退到最近成功数据的现有规则；
5. 当前“账户历史 + 本机实时当天”的 Token 合并口径和 120 秒前端自动刷新节奏；
6. 账户身份、JWT、Cookie、session 正文和原始 app-server 响应均不向 renderer 暴露。

## 7. 测试迁移

测试和实现同处，避免 `lib.rs` 成为私有实现的唯一测试入口：

- `storage`：增量扫描写入、游标重建、清空、30 天清理和 SQLite 缓存；
- `token`：累计差分、日桶归属、本机与账户历史合并、当天不可用语义；
- `provider`：响应按 ID 筛选、额度响应解析、重试/缓存和账户日桶读取；
- `account`：JWT 解码、订阅日期与 Token 过期时间区分、缓存与失败回退；
- `window`：全屏状态变化不覆盖用户主动隐藏选择；
- `lib`：仅保留跨模块快照编排的少量断言，或以现有单元测试间接覆盖。

重构结束后，测试数量不应减少；若因为相邻测试重组发生合并，需保持等价断言覆盖。

## 8. 实施顺序

1. 抽取共享模型、路径和应用状态，先保持函数签名与行为不变；
2. 抽取存储层，并让 `Store::new` 通过存储模块初始化数据库和已保存缓存；
3. 抽取 Token 解析与本地统计，再迁移对应测试；
4. 抽取 Provider 和账户读取，保留现有缓存及并发刷新语义；
5. 抽取窗口/托盘/快捷键处理；
6. 将 `lib.rs` 收敛为命令和快照编排，迁移剩余测试；
7. 执行格式化、前端测试/构建、Rust 测试、Tauri debug 构建和 `git diff --check`。

每一步均要求可编译；不提交仅靠大范围移动且无法独立验证的中间状态。

## 9. 验收标准

1. `lib.rs` 不再包含 Provider 子进程、JSONL 解析、订阅 HTTP、SQLite SQL 或窗口事件实现；
2. 五个指定领域均有独立模块，职责与本设计一致；
3. 现有 IPC 与前端无需修改即可构建运行；
4. 当前 Token 今日实时覆盖、账户回退和缓存刷新测试仍通过；
5. `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`、`cargo test --manifest-path src-tauri/Cargo.toml`、`npm test -- --run`、`npm run build`、`npm run tauri build -- --debug --no-bundle` 和 `git diff --check` 均通过；
6. 除本设计及后续实现文件外，不包含 `.learnings/` 等本地诊断产物。
