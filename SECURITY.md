# Security Policy

## Scope

Codex Float 是本地优先的非官方桌面工具。安全边界包括：

- Codex app-server 子进程与 JSON-RPC；
- `~/.codex/sessions` 的只读增量扫描；
- 本地 SQLite 聚合和设置；
- Rust 后端到 WebView renderer 的白名单 IPC DTO。

## Data handling

Codex Float 不应收集、记录、展示或上传：

- OAuth Token、API Key、Cookie 或 Authorization Header；
- `auth.json` 内容；
- 账号邮箱；
- prompt、回复、工具输出或会话正文；
- 项目文件内容和原始 app-server 流。

允许进入 UI 的数据仅限额度百分比、窗口时长、重置时间、Token 聚合、覆盖状态、更新时间和脱敏错误码。

## Reporting a vulnerability

请不要在公开 Issue 中附加：

- `~/.codex` 原始文件；
- app-server 原始输出；
- 包含账号信息的截图；
- 访问令牌或 Cookie。

报告应包含 Codex Float 版本、Windows 版本、Codex CLI/Desktop 版本、脱敏复现步骤和脱敏日志。正式公开仓库建立后，应通过仓库的私密安全报告渠道提交漏洞。

## Unsupported behavior

以下行为不在安全支持范围：

- 修改或绕过 OpenAI 额度规则；
- 复制、导出或共享认证材料；
- 向非 loopback 地址暴露无认证 app-server；
- 使用调试注入读取 Codex Desktop DOM 或原始协议流。
