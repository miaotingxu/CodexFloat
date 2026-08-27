# Codex Float

Codex Float 是一个面向 Windows 11 的本地优先 Codex 状态悬浮窗，目标是用一个小而安静的桌面组件回答：

- 还能使用多少额度？
- 今日本机已经使用多少 Token？
- 当前额度何时重置？

> Codex Float 是非官方开源项目，与 OpenAI 无隶属或官方支持关系。

## 当前阶段

项目已完成 M1 核心 MVP，并实现了多数 M2 窗口交互能力。目前定位为 **可运行的 Windows 内部 Alpha**：额度、Token、历史趋势、拖动、折叠、位置保存、托盘、鼠标穿透和全屏隐藏已进入正式运行路径；Provider 发布级容错、设置、完整 Windows 验收和安装发布仍在开发中。

当前事实基线与后续方案：

- `docs/PROJECT-STATUS.md`
- `docs/plans/2026-08-26-m2-to-release.md`

基础产品与历史验证文档：

- `docs/PRD.md`
- `docs/M0-TECHNICAL-VALIDATION.md`
- `docs/M1-DEVELOPMENT-STATUS.md`（历史快照）
- `docs/plans/2026-08-23-m1-core-mvp.md`（历史计划）

## 开发运行

```bash
cd apps/desktop
npm install
npm test -- --run
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri build -- --debug --no-bundle
```

Windows Debug 可执行文件生成于：

```text
apps/desktop/src-tauri/target/debug/codex-float.exe
```

## 产品边界

- 仅在本机处理数据；
- 不提供默认遥测；
- 不统计价格；
- 不按模型拆分；
- 不把额度百分比反推为 Token；
- 不读取或上传聊天正文、邮箱、OAuth Token 或 Cookie；
- 本机统计不等同于 OpenAI 全账户账单。

## M0 探针

```bash
python tools/token_usage_probe.py
python tools/app_server_probe.py --timeout 30
```

探针输出经过白名单过滤。不要将原始 `auth.json`、会话 JSONL 或 app-server 原始流附加到 Issue。

## 许可证

MIT
