# Codex Float M2 稳定化到首个预览版开发计划

> **For Hermes:** 实施时加载 `subagent-driven-development`，逐任务执行；每个任务必须先验证修改范围，再完成测试、Windows 回归和独立 Git 提交。

**目标**：把当前可运行的 M2 内部 Alpha 收敛为可安装、可诊断、可回归验证的 Windows 预览版。

**架构**：保留 Tauri 2 + React + Rust + SQLite 技术栈。先建立不可绕过的回归门禁，再将单体 `lib.rs` 和 `main.tsx` 按 Provider、Token、窗口状态、存储、UI 组件拆分。额度采集改为后端单实例服务，renderer 只读取标准化快照，不直接触发无限制子进程启动。

**技术栈**：Tauri 2、React 19、TypeScript、Vite、Vitest、Rust、rusqlite、serde、chrono、Windows 11 x64、NSIS。

---

## 1. 交付原则

### 1.1 每个 patch 的强制流程

每个小功能或修复必须按以下顺序完成：

1. 写明本次允许修改的文件和明确非目标；
2. `git status --short` 确认起始工作区；
3. 先补失败测试或可复现步骤；
4. 实现最小改动；
5. 运行本任务测试；
6. 运行完整自动化回归；
7. 构建唯一 canonical exe；
8. 执行 Windows smoke checklist；
9. `git diff --check`；
10. 独立提交一个 commit；
11. 再次确认工作区干净。

### 1.2 固定自动化门禁

```bash
cd /f/CodexFloat/apps/desktop
npm test -- --run
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri build -- --debug --no-bundle
```

预期：

- 前端测试全部通过；
- Rust 测试全部通过；
- TypeScript/Vite 构建成功；
- Tauri Debug exe 生成成功。

### 1.3 固定 Windows smoke checklist

每个影响运行时的 patch 至少检查：

- 冷启动 10 秒内显示额度；
- 5 小时和 7 天窗口按 Provider 实际结果显示；
- 今天/7 天/30 天 Token 正常；
- 点击展开、再次点击收起；
- 真实鼠标拖动；
- 刷新按钮不触发拖动/收起；
- 关闭后隐藏到托盘；
- 托盘显示和退出；
- `Ctrl+Shift+Space` 可恢复鼠标操作；
- 没有可见 CMD 窗口；
- 退出后没有 Codex Float 所属孤儿 app-server。

## 2. 里程碑

| 里程碑 | 目标 | 完成门槛 |
|---|---|---|
| M2.1 稳定性基线 | 防止额度和窗口交互互相回归 | 回归矩阵、Provider 诊断、持久化缓存、single-flight |
| M2.2 Windows 完整交互 | 多屏/DPI/隐藏/穿透/托盘可解释 | Windows 测试矩阵通过，状态持久化正确 |
| M2.3 设置与可诊断性 | 用户可自行配置和排障 | 设置页、开机启动、诊断页、恢复默认位置 |
| M3 Preview | 普通用户可安装和升级 | NSIS、升级/卸载、签名策略、版本化发布产物 |

## 3. 实施任务

### Task 1：建立回归合同与验证脚本

**目标**：把当前已验证能力固定成每次 patch 都要执行的门禁。

**文件**：

- Create: `docs/WINDOWS-QA-MATRIX.md`
- Create: `tools/verify_project.py`
- Modify: `README.md`

**步骤**：

1. 在 QA 矩阵中列出额度、Token、展开/收起、拖动、托盘、全屏隐藏、穿透、位置恢复的前置条件、操作、预期和证据状态。
2. `verify_project.py` 顺序执行前端测试、Vite 构建、Rust 测试和 Tauri Debug 构建，保留每一步真实退出码；禁止用管道或 `|| true` 掩盖失败。
3. 脚本输出明确区分：自动化通过、需要真实鼠标、需要重启、需要多显示器。
4. 在 README 增加统一验证命令。

**验证**：

```bash
python tools/verify_project.py
```

预期：任一子命令失败时脚本非零退出；全部成功时输出 canonical exe 路径。

**提交**：

```bash
git add docs/WINDOWS-QA-MATRIX.md tools/verify_project.py README.md
git commit -m "test: add Codex Float regression gate"
```

### Task 2：无行为变化地拆分 Rust 核心

**目标**：降低修改额度时误伤窗口、修改窗口时误伤 Token 的风险。

**文件**：

- Create: `apps/desktop/src-tauri/src/models.rs`
- Create: `apps/desktop/src-tauri/src/storage.rs`
- Create: `apps/desktop/src-tauri/src/token_usage.rs`
- Create: `apps/desktop/src-tauri/src/quota.rs`
- Create: `apps/desktop/src-tauri/src/window.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs`

**步骤**：

1. 先记录当前 12 个 Rust 测试名称和结果。
2. 只移动类型与函数，不改变 SQL、超时、UI DTO 和 Tauri command 名称。
3. 每移动一个模块就运行对应测试。
4. 最后运行完整回归和 Windows smoke checklist。

**验收**：

- renderer DTO JSON 形状不变；
- SQLite 路径和表结构不变；
- 额度与 Token 数值和改造前一致；
- Rust 测试数不减少。

**提交**：

```bash
git commit -m "refactor: split desktop core modules"
```

### Task 3：为额度 Provider 增加结构化诊断与子进程生命周期

**目标**：把“额度暂不可用”拆成可排查且不泄露隐私的内部状态。

**文件**：

- Modify: `apps/desktop/src-tauri/src/quota.rs`
- Modify: `apps/desktop/src-tauri/src/models.rs`
- Test: `apps/desktop/src-tauri/src/quota.rs`
- Modify: `tools/app_server_probe.py`

**测试先行行为**：

- 找不到 Codex：`runtime_not_found`；
- 子进程启动失败：`spawn_failed`；
- initialize 超时：`initialize_timeout`；
- rateLimits 超时：`rate_limit_timeout`；
- JSON-RPC error：`protocol_error`；
- 返回窗口为空：`empty_snapshot`；
- 成功和失败路径都执行 kill/wait，不留下子进程；
- 错误对象不包含用户名、路径、邮箱、token 或原始响应。

**实现要求**：

- UI 默认仍使用简短中文，不直接展示内部错误；
- 诊断页未来可读取错误码和时间；
- `app_server_probe.py` 将额度探针与可选 usage 探针拆开，usage 超时不能让额度探针整体失败。

**验证**：

```bash
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml quota
python tools/app_server_probe.py --timeout 30
```

**提交**：

```bash
git commit -m "feat: add safe quota provider diagnostics"
```

### Task 4：持久化额度缓存和数据库迁移

**目标**：程序重启后仍能短期显示明确标记的 stale 快照。

**文件**：

- Modify: `apps/desktop/src-tauri/src/storage.rs`
- Modify: `apps/desktop/src-tauri/src/quota.rs`
- Modify: `apps/desktop/src-tauri/src/models.rs`
- Test: `apps/desktop/src-tauri/src/storage.rs`

**数据表建议**：

```sql
CREATE TABLE quota_cache (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  snapshot_json TEXT NOT NULL,
  fetched_at INTEGER NOT NULL,
  provider_version TEXT
);
```

只保存标准化窗口字段，不保存原始响应、账户、邮箱或凭据。

**状态规则**：

- 读取成功：`live`；
- 最近成功超过 3 分钟但不超过 15 分钟：`stale`；
- 超过 15 分钟或没有有效快照：`unavailable`；
- 未来时间戳：拒绝使用。

**测试先行行为**：

- 重启模拟后可读取 stale 快照；
- 15 分钟后不再返回旧百分比；
- 损坏 JSON 安全降级；
- schema migration 可重复执行；
- 清理 Token 统计不删除额度缓存，清空全部数据命令才删除。

**提交**：

```bash
git commit -m "feat: persist normalized quota cache"
```

### Task 5：后端 single-flight、重试和重置时间刷新

**目标**：避免重复点击或定时器并发启动多个 app-server。

**文件**：

- Modify: `apps/desktop/src-tauri/src/quota.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs`
- Modify: `apps/desktop/src/main.tsx`
- Test: `apps/desktop/src-tauri/src/quota.rs`

**行为**：

- 同一时刻最多一个额度刷新；
- 后续请求复用进行中的刷新结果；
- 失败按 5 秒、15 秒、30 秒退避；
- 已知重置时间后约 2 秒触发刷新；
- 手动刷新可请求立即重试，但不并发启动第二个子进程；
- renderer 只查询快照，不负责 Provider 生命周期。

**验收**：

- 连续点击刷新 10 次，只产生一个受管 app-server；
- 失败恢复后 UI 从 stale/unavailable 回到 live；
- 退出应用后无所属孤儿进程。

**提交**：

```bash
git commit -m "fix: serialize quota refresh lifecycle"
```

### Task 6：Token 收集器边界和迁移测试

**目标**：把当前已工作的 JSONL 聚合提升为发布级容错。

**文件**：

- Modify: `apps/desktop/src-tauri/src/token_usage.rs`
- Modify: `apps/desktop/src-tauri/src/storage.rs`
- Test: `apps/desktop/src-tauri/src/token_usage.rs`

**测试先行行为**：

- 文件截断；
- 文件替换或轮转；
- 最后一行不完整；
- 单行损坏但后续行有效；
- 超大历史库分批处理；
- 同一 session 的不同文件不能重复计数；
- 跨午夜后新事件进入新自然日；
- 清空后重新扫描的产品语义明确且测试固定。

**非目标**：不增加模型、价格、输入/输出拆分。

**提交**：

```bash
git commit -m "test: harden local token collection edges"
```

### Task 7：拆分 React UI 并增加交互测试

**目标**：让窗口交互、额度展示和操作按钮可独立测试。

**文件**：

- Create: `apps/desktop/src/components/CompactView.tsx`
- Create: `apps/desktop/src/components/ExpandedView.tsx`
- Create: `apps/desktop/src/components/QuotaCard.tsx`
- Create: `apps/desktop/src/hooks/useDashboard.ts`
- Create: `apps/desktop/src/ipc.ts`
- Modify: `apps/desktop/src/main.tsx`
- Test: `apps/desktop/src/components/*.test.tsx`

**测试先行行为**：

- 单击摘要行展开；
- 单击标题栏收起；
- 刷新按钮不触发展开/收起；
- 不可用状态不显示伪造百分比；
- stale 状态明确标记；
- 多额度窗口动态渲染；
- 刷新进行中禁用重复按钮；
- Token 文案始终声明本机覆盖。

**提交**：

```bash
git commit -m "refactor: split dashboard UI components"
```

### Task 8：统一窗口可见性与交互状态机

**目标**：消除用户隐藏、全屏隐藏、托盘显示和鼠标穿透之间的冲突。

**文件**：

- Modify: `apps/desktop/src-tauri/src/window.rs`
- Modify: `apps/desktop/src-tauri/src/storage.rs`
- Test: `apps/desktop/src-tauri/src/window.rs`

**状态至少区分**：

- `visible`；
- `user_hidden`；
- `fullscreen_hidden`；
- `click_through`；
- `expanded`。

**行为**：

- 用户主动关闭到托盘后，退出全屏不能擅自显示；
- 托盘“显示窗口”退出 `user_hidden`，并提供恢复鼠标操作的路径；
- 全屏结束只恢复由全屏逻辑隐藏的窗口；
- 状态变化可测试，不依赖无限循环里的隐式布尔值。

**提交**：

```bash
git commit -m "fix: unify floating window visibility state"
```

### Task 9：设置页与开机启动最小闭环

**目标**：交付 PRD 中真正必要的用户设置，不一次性扩展主题系统。

**文件**：

- Create: `apps/desktop/src/components/SettingsView.tsx`
- Create: `apps/desktop/src-tauri/src/settings.rs`
- Modify: `apps/desktop/src-tauri/Cargo.toml`
- Modify: `apps/desktop/package.json`
- Modify: `apps/desktop/src-tauri/src/storage.rs`

**首版设置范围**：

- 开机启动，默认关闭；
- 鼠标穿透开关；
- 全屏自动隐藏开关；
- 恢复默认位置；
- 刷新间隔预设；
- 清空 Token 统计；
- 清空全部 Codex Float 本地数据；
- 关于、隐私和非官方声明。

**非目标**：首版不做复杂主题编辑器和云同步。

**验收**：

- 设置持久化并重启恢复；
- 开机启动注册/取消真实生效；
- 清空全部数据不删除 `%USERPROFILE%\.codex`；
- 恢复位置后窗口在当前主屏工作区内。

**提交**：

```bash
git commit -m "feat: add essential desktop settings"
```

### Task 10：Windows 多场景验收

**目标**：把“代码已实现”提升为“Windows 行为已验收”。

**文件**：

- Modify: `docs/WINDOWS-QA-MATRIX.md`
- Create: `docs/reports/M2-WINDOWS-ACCEPTANCE.md`

**矩阵**：

- 100%、125%、150%、175% DPI；
- 单屏、双屏、主副屏互换；
- 混合 DPI；
- 显示器断开和重连；
- 分辨率变化；
- 任务栏四个方向；
- 睡眠/唤醒；
- 远程桌面；
- 虚拟桌面；
- 浏览器视频全屏；
- 无边框游戏全屏；
- 鼠标穿透恢复；
- Explorer 重启后的托盘；
- 连续运行 8 小时的进程和 CPU/内存检查。

**验收规则**：每一项记录环境、步骤、结果和证据；未执行不得写“通过”。

**提交**：

```bash
git commit -m "docs: record M2 Windows acceptance"
```

### Task 11：NSIS 安装、升级和卸载

**目标**：生成普通 Windows 用户可使用的安装包。

**文件**：

- Modify: `apps/desktop/src-tauri/tauri.conf.json`
- Modify/Create: Windows 图标资源
- Create: `docs/INSTALLATION.md`
- Create: `docs/reports/INSTALL-UPGRADE-UNINSTALL.md`

**步骤**：

1. 只生成 NSIS x64 目标；
2. 构建版本化安装包；
3. 在干净测试用户或 VM 安装；
4. 验证开始菜单、托盘和首次启动；
5. 写入测试 SQLite 数据；
6. 覆盖升级并确认数据保留；
7. 卸载并确认程序文件删除；
8. 明确 `%LOCALAPPDATA%\CodexFloat` 的保留/删除策略；
9. 检查 app-server 和 Codex Float 残留进程。

**命令**：

```bash
cd /f/CodexFloat/apps/desktop
npm run tauri build -- --bundles nsis
```

**提交**：

```bash
git commit -m "build: add Windows NSIS packaging"
```

### Task 12：预览版发布工程

**目标**：形成可重复、可校验、可回滚的首个 Preview。

**文件**：

- Create: `.github/workflows/ci.yml`
- Create: `.github/ISSUE_TEMPLATE/bug_report.yml`
- Create: `CONTRIBUTING.md`
- Create: `CHANGELOG.md`
- Modify: `README.md`
- Modify: `SECURITY.md`

**行为**：

- CI 运行前端测试、Rust 测试和构建；
- 发布产物包含 NSIS、SHA-256 checksum、版本和提交 SHA；
- Bug 模板禁止上传 auth.json、原始 JSONL、token、邮箱或完整协议流；
- README 包含安装、卸载、隐私、已知限制和故障诊断；
- 发布说明明确“非 OpenAI 官方”。

**发布前验收**：

- 工作区干净；
- tag 指向已验收提交；
- 安装包可在干净 Windows 11 x64 安装；
- 额度、Token、拖动、收起、托盘 smoke 全通过；
- checksum 与实际文件一致。

**提交**：

```bash
git commit -m "docs: prepare first Codex Float preview"
```

## 4. 推荐执行顺序

严格按以下顺序，不并行修改相互依赖的运行时核心：

1. Task 1 回归门禁；
2. Task 2 无行为重构；
3. Task 3 Provider 诊断；
4. Task 4 持久化缓存；
5. Task 5 single-flight 与重试；
6. Task 6 Token 边界；
7. Task 7 UI 拆分与测试；
8. Task 8 窗口状态机；
9. Task 9 设置和开机启动；
10. Task 10 Windows 验收；
11. Task 11 NSIS；
12. Task 12 Preview 发布。

## 5. 暂缓事项

首个 Preview 前暂不开发：

- 多账号切换；
- 云同步；
- 价格和成本；
- 按模型或输入/输出拆分；
- Codex Desktop 注入；
- 自动更新；
- macOS/Linux；
- 复杂主题系统；
- 远程遥测。

这些功能会扩大隐私、兼容性和发布范围，不应阻塞 Windows 单用户本地版闭环。

## 6. 完成定义

首个 Preview 只有同时满足以下条件才算完成：

- 核心自动化测试全部通过；
- 固定 Windows smoke checklist 全通过；
- 多屏/DPI/睡眠/全屏矩阵有真实记录；
- Provider 失败后能显示 stale/unavailable 并自动恢复；
- 没有孤儿 app-server 或可见 CMD；
- 设置和开机启动可用；
- NSIS 安装、覆盖升级和卸载真实通过；
- 数据保留和隐私边界有文档；
- 版本化安装包和 checksum 已生成；
- 每项功能有独立 Git 提交，最终工作区干净。
