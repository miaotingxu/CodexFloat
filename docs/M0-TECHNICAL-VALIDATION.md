# Codex Float M0 技术验证报告

- 日期：2026-08-23
- 目标平台：Windows 11 x64
- 结论：**M0 通过，可以进入核心 MVP 开发**
- 隐私说明：验证工具只输出白名单统计字段；未输出或保存邮箱、OAuth Token、Cookie、会话正文和原始协议流。

## 1. 结论摘要

| 验证项 | 结果 | 结论 |
|---|---|---|
| Codex 本地额度读取 | 通过 | app-server 可直接读取真实额度快照 |
| 今日 Token 总量 | 通过 | 本地 JSONL 中有可聚合、可去重的 Token 事件 |
| Codex Desktop 覆盖 | 通过 | Desktop 会话写入统一的 `~/.codex/sessions` 数据源 |
| CLI/Desktop 来源识别 | 通过 | `session_meta.payload.originator` 可区分来源 |
| 账户历史用量 | 部分通过 | app-server 提供账户日桶，但当前日不一定出现，且与本地口径不同 |
| Tauri 悬浮窗 | 通过 | 已成功构建、启动并目视验证透明圆角置顶窗口 |
| Windows 安装包 | 未在 M0 打包 | Tauri 编译链已通过；NSIS 放入发布阶段验证 |

## 2. 验证环境

- Codex CLI：`0.147.0`
- Codex Desktop Appx：`OpenAI.Codex 26.818.5229.0 x64`
- Desktop 内部运行时在本机会话元数据中出现多个 alpha 版本，最高观察到 `0.149.0-alpha.4.1`
- Rust/Cargo：`1.96.0`
- Node.js：`24.13.0`
- npm：`11.6.2`
- Tauri：构建时解析为 `2.11.x` 组件

## 3. 额度 Provider 验证

### 3.1 已验证协议

通过本地 `codex app-server --stdio` 完成 JSON-RPC 初始化，并成功调用只读方法：

- `account/read`
- `account/rateLimits/read`
- `account/usage/read`

生成的实验性 JSON Schema 明确包含：

- `GetAccountRateLimitsResponse`
- `GetAccountTokenUsageResponse`
- `AccountRateLimitsUpdatedNotification`
- `ThreadTokenUsageUpdatedNotification`

### 3.2 实际额度响应

验证时成功得到：

- 账户类型：ChatGPT
- 套餐类型：Plus
- `limitId`：`codex`
- 主窗口时长：`10080` 分钟（7 天）
- 已用比例：`75%`
- 剩余比例：`25%`
- 有效重置时间戳
- `secondary`：当前为 `null`

### 3.3 产品影响

当前环境只返回一个周窗口，没有返回固定的“5 小时 + 周”双窗口。因此：

1. UI 必须按 Provider 实际返回的窗口动态渲染；
2. 不得把 `primary` 永久命名为“5 小时”；
3. 只有时长等于已知窗口时才使用友好标签，否则显示实际时长；
4. 额度读取可以直接启动独立 app-server，不需要 CDP，也不必注入 Codex Desktop；
5. Windows 上 `app-server daemon` 生命周期命令不可用，但 `--stdio` 独立子进程可用，MVP 应管理该子进程的启动、超时和退出。

## 4. Token 数据源验证

### 4.1 本地数据盘点

`~/.codex/sessions` 中验证到：

- JSONL 会话文件：87 个
- 含 Token 事件的文件：84 个
- `token_count` 事件：3470 条
- 重复累计快照：67 条
- 会话内累计计数回退/重置：4 次
- 缺少 `info` 的 Token 通知：2 条

来源分布：

- Codex Desktop：82 个会话文件
- Codex TUI：2 个
- 其他本地 Codex 调用来源：3 个

这证明 Codex Desktop 与 CLI/TUI 在本机共享统一的会话根目录，第一版无需分别扫描两个私有数据库。

### 4.2 字段语义

Token 事件包含：

- `total_token_usage`：会话累计快照
- `last_token_usage`：本次增量
- `input_tokens`
- `cached_input_tokens`
- `cache_write_input_tokens`
- `output_tokens`
- `reasoning_output_tokens`
- `total_tokens`

在 3470 条事件中，绝大多数非重复事件满足：

```text
当前 total_token_usage.total_tokens
- 上一条 total_token_usage.total_tokens
= 当前 last_token_usage.total_tokens
```

67 条重复快照表现为累计值不变，但 `last_token_usage` 重复出现；直接求和会重复统计。4 次累计值下降时，新累计值等于新的 `last_token_usage`，说明需要处理会话内计数器重置。

### 4.3 推荐聚合算法

对每个 JSONL 文件按行增量读取：

1. 保存文件身份、已读 byte offset 和上一条累计 Token；
2. 遇到 `token_count` 后读取累计值和增量值；
3. 如果累计值与上一条相同，视为重复快照，不累计；
4. 如果累计值增加，累计本条 `last_token_usage.total_tokens`；
5. 如果累计值下降，视为计数器重置，仍累计本条增量并记录诊断事件；
6. 使用事件 UTC 时间转换为当前 Windows 本地时区后归属自然日；
7. 使用 session ID、文件身份、byte offset 和事件时间生成幂等游标；
8. 不保存会话正文、文件路径、工作目录或账号邮箱。

验证脚本按该算法成功输出今天、7 天和 30 天聚合结果。

## 5. 本地统计与账户统计的差异

`account/usage/read` 成功返回：

- `lifetimeTokens`
- `peakDailyTokens`
- 连续使用天数
- `dailyUsageBuckets`

但验证时存在两个重要差异：

1. 返回了此前 7 个完整日期的日桶，没有返回当前自然日；
2. 账户日桶与本地 JSONL 按日聚合值并不完全相等。

可能原因包括：

- 账户接口为服务端账号口径，本地 JSONL 为本机可观察口径；
- 日期边界或结算延迟不同；
- 服务端包含其他设备或排除部分内部 Token；
- 本地上下文/缓存 Token 与服务端统计定义不同。

因此 MVP 不应把两种来源静默相加，也不能用账户历史桶替换“今日本机”实时总量。

建议：

- 主界面“今日本机已用 Token”使用本地 JSONL 增量聚合；
- `account/usage/read` 仅作为可选诊断、覆盖校验或未来的“账户统计”功能；
- 如果未来展示账户口径，必须单独命名，不与本机口径混为一谈。

## 6. Codex Desktop 兼容性

- Desktop Appx 正在运行，主进程为 `ChatGPT.exe`，并启动包内 `codex.exe`。
- 82/87 个会话元数据的 `originator` 为 `Codex Desktop`，且包含 Token 事件。
- Desktop 包内 `codex.exe` 受 WindowsApps 权限保护，从外部直接执行返回 `WinError 5`；MVP 不应依赖直接执行包内二进制。
- 使用全局 Codex CLI 的 app-server 可正常读取同一登录环境的额度与账户用量。

兼容策略：

1. 优先发现已安装的全局 Codex CLI；
2. 检测最低协议能力，而不是只判断版本字符串；
3. 额度 Provider 失败时，可用最近 JSONL 中的额度快照作短期降级，但必须标记为过期数据；
4. 不复制或重新分发 Codex Desktop 包内二进制。

## 7. Tauri 窗口验证

已创建并运行：

`spikes/window-probe`

验证内容：

- Tauri 2 Windows 工程成功安装依赖；
- Debug 可执行文件成功编译；
- WebView2/Tauri Windows 原生依赖编译通过；
- 程序成功启动并创建可见窗口；
- 窗口配置为 250 × 64 DIP、无原生装饰、不可缩放、始终置顶、跳过任务栏；
- 实际截图目视确认：窗口小巧、圆角、透明背景、内容清晰，无明显渲染错误；
- 实际物理像素约为 265 × 72，符合当前系统 DPI 缩放后的 DIP 换算。

首次构建发现 Tauri Windows Resource 强制需要 `icons/icon.ico`，补充图标后构建成功。正式工程必须从初始化阶段加入完整 Windows 图标集。

M0 尚未覆盖：

- 多显示器坐标恢复；
- 靠边吸附；
- 全屏自动隐藏；
- 鼠标穿透与全局恢复快捷键；
- NSIS 安装包和代码签名。

这些属于 M1/M3 工程验收，不阻塞架构成立。

## 8. 建议架构

```text
Tauri/React UI
  ↓ 白名单 IPC DTO
Rust Core
  ├─ AppServerQuotaProvider
  │    ├─ account/rateLimits/read
  │    └─ 额度通知/轮询
  ├─ JsonlTokenProvider
  │    ├─ 增量文件扫描
  │    ├─ 去重和计数器重置处理
  │    └─ 本地自然日聚合
  ├─ AccountUsageProbe（诊断/可选）
  │    └─ account/usage/read
  └─ SQLite
       ├─ 文件游标
       ├─ 每日聚合
       ├─ Provider 状态
       └─ 用户设置
```

存储建议从候选方案收敛为 SQLite。原因是需要事务化保存文件游标和日聚合，避免应用崩溃后重复累计；单一 JSON 文件的幂等恢复成本更高。

## 9. Go / No-Go

### Go

- 独立悬浮窗方案成立；
- 额度读取路径成立；
- CLI + Desktop 本机 Token 覆盖路径成立；
- 不需要 CDP、DOM 注入或远程中转；
- 可以进入核心 MVP 开发。

### 仍需工程化解决

- app-server 协议版本探测；
- JSONL 文件监听、轮转和损坏恢复；
- 本地与账户统计口径差异提示；
- 当前只返回一个额度窗口时的动态 UI；
- Windows 多屏/DPI/鼠标穿透测试矩阵；
- NSIS 和发布签名。

## 10. 可复现验证命令

```bash
cd /f/CodexFloat
python tools/token_usage_probe.py
python tools/app_server_probe.py --timeout 30

cd /f/CodexFloat/spikes/window-probe
npm install
npm run tauri build -- --debug --no-bundle
```

所有验证输出均经过白名单过滤。不要在 Issue、日志或截图中附加原始 `auth.json`、完整 app-server 流或会话 JSONL。
