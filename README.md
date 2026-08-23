# Codex Float

Codex Float 是一个面向 Windows 11 的本地优先 Codex 状态悬浮窗，目标是用一个小而安静的桌面组件回答：

- 还能使用多少额度？
- 今日本机已经使用多少 Token？
- 当前额度何时重置？

> Codex Float 是非官方开源项目，与 OpenAI 无隶属或官方支持关系。

## 当前阶段

项目已完成 M1 核心 MVP 的首个可运行垂直切片。M0 技术验证与额度/Token 主链路已经闭环：

- 本地 app-server 额度读取已验证；
- Codex Desktop 与 CLI/TUI 的本地 Token 事件已验证；
- Tauri 2 透明置顶窗口已验证。

详见：

- `docs/PRD.md`
- `docs/M0-TECHNICAL-VALIDATION.md`
- `docs/M1-DEVELOPMENT-STATUS.md`
- `docs/plans/2026-08-23-m1-core-mvp.md`

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
