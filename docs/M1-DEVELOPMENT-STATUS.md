# Codex Float M1 开发状态

- 日期：2026-08-23
- 阶段：M1 核心 MVP 垂直切片
- 状态：**可运行、可构建；基础额度与 Token 主链路已闭环**

## 已实现

### 正式桌面工程

路径：`apps/desktop`

- Tauri 2 Windows 壳；
- React 19 + TypeScript + Vite；
- Rust 本地数据后端；
- 透明、无边框、始终置顶、跳过任务栏；
- 默认 250 × 64 DIP 折叠态；
- 展开目标 320 × 300 DIP；
- 首次启动定位至当前显示器工作区右下角安全边距。

### 额度 Provider

Rust 后端直接管理本机原生 `codex.exe app-server --stdio` 子进程，并按初始化顺序执行：

1. `initialize`；
2. 等待初始化响应；
3. `initialized`；
4. `account/rateLimits/read`；
5. 只映射白名单额度字段。

UI 不硬编码“5 小时 + 周”双窗口，而是根据 `windowDurationMins` 动态显示零个、一个或多个窗口。

白名单 DTO 字段：

- `label`；
- `usedPercent`；
- `remainingPercent`；
- `resetsAt`；
- `windowMinutes`。

不向 renderer 传递账号、邮箱、Token、Cookie 或原始协议对象。

### Token Provider

Rust 后端只读扫描 `~/.codex/sessions/**/*.jsonl` 的 Token 事件：

- 使用事件中的本轮增量；
- 忽略重复累计快照；
- 处理会话内累计计数器下降；
- 使用事件时间戳转换为本地自然日；
- 保存按文件哈希生成的游标；
- SQLite 使用幂等事件键避免二次计数；
- 输出今天、最近 7 天、最近 30 天和 7 天日序列。

本地数据库：

```text
%LOCALAPPDATA%\CodexFloat\codex-float.sqlite
```

数据库不保存原始 JSONL 路径、prompt、回复或会话正文。

### UI

折叠态已实际显示并目视验证：

- 动态额度窗口；
- 剩余百分比；
- 今日本机 Token；
- 额度实时状态点；
- 透明圆角深色卡片；
- 无文字裁切、乱码或占位值。

展开态前端已实现：

- 全部额度窗口；
- 重置时间；
- 今天 / 7 天 / 30 天 Token；
- 最近 7 天极简柱状趋势；
- 部分覆盖声明；
- 更新时间；
- 手动刷新。

## 实际验证结果

### 前端测试

```text
Test Files  1 passed (1)
Tests       4 passed (4)
```

覆盖：

- K/M Token 格式；
- 非法数值保护；
- 动态额度窗口格式；
- Unix 秒级重置时间格式。

### Rust 测试

```text
running 4 tests
4 passed; 0 failed
```

覆盖：

- 重复累计快照幂等；
- 累计计数器重置；
- 二次扫描与追加事件；
- 事件本地自然日归属；
- SQLite 文件标识不保存原始路径。

### 构建

以下命令均已真实执行成功：

```bash
npm test -- --run
npm run build
cargo test
npm run tauri build -- --debug --no-bundle
```

生成物：

```text
apps/desktop/src-tauri/target/debug/codex-float.exe
```

### Windows 运行验证

- 应用进程启动成功；
- 实际窗口可见；
- 实际物理窗口约 265 × 72 px，对应系统缩放后的 250 × 64 DIP；
- `WS_EX_TOPMOST` 验证为真；
- 启动位置位于当前显示器工作区右下角；
- 额度实际返回并显示；
- 今日 Token 实际读取并显示；
- 最近每日聚合已按原事件日期恢复，不再把历史事件错误归入今天。

### 隐私检查

```text
source_email_literals       0
source_credential_literals  0
sqlite_raw_path_values      0
```

临时全屏验证截图在检查后已经删除。

## 当前诚实限制

以下能力尚未实现或尚未形成真实验收证据，不应标记完成：

1. 拖动和靠边吸附；
2. 鼠标穿透及全局快捷键恢复；
3. 全屏应用自动隐藏；
4. 多显示器位置持久化和 DPI 切换恢复；
5. 睡眠/唤醒及虚拟桌面边界；
6. app-server 明确超时与取消机制；
7. SQLite 最近 30 天自动清理及一键清空；
8. CLI/Desktop 本地账号指纹一致性；
9. NSIS 安装、升级保留数据和卸载；
10. 开机启动设置；
11. 自动化点击展开验证（当前环境拒绝未经用户同意的鼠标控制，因此未绕过限制）。

## 下一步

M1 下一增量应优先完成：

1. app-server 超时、错误码和缓存新鲜度；
2. 30 天清理、一键清空；
3. 窗口拖动、位置保存、吸附和安全恢复；
4. 鼠标穿透 + 全局快捷键；
5. 全屏隐藏；
6. NSIS 安装验证。
