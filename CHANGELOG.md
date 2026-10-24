# Changelog

本文档记录 RelayCat Core（`relaycat-gui`、`relaycat-cli`、`relaycat-server` 与 `relaycat-protocol`）的版本变更说明。

## [Relay v0.1.3] - 2026-10-24

- 新增所有连接共享的排队 payload 字节预算，默认 256 MiB，可通过 `limits.max_queued_bytes` 配置。
- 默认全局并发连接上限降为 512，每连接出站队列降为 32 条，降低高并发和慢消费者场景的内存压力。
- WebSocket 数据帧及心跳写入增加 10 秒超时，避免慢连接长期占用发送任务。
- 更新依赖锁定版本，修复严格 Clippy 检查诊断。

独立发布说明：[Relay v0.1.3](docs/releases/relaycat-relay-v0.1.3.md)。

## [GUI v0.2.0 / CLI v0.1.6] - 2026-10-24

### GUI

- 文件树支持路径搜索及跳转到 Git 改动；图片预览支持缩放、平移与适应窗口，文本预览增加行号、换行和查看全文入口。
- Git 改动按目录分组，显示文件增删统计；提交区常驻底部，补齐提交信息辅助、冲突状态、危险操作说明和分类错误提示。
- 切换视图时保留工作区状态，补齐键盘操作、入口提示、空状态引导和窄面板布局。
- 修复过期预览响应、跨项目引用残留、重命名统计、Git 钩子区域设置与超时进程清理；加固 SVG 预览和提交信息临时文件。
- 限制远程终端网格尺寸，更新前端及 Tauri TLS/XML 等依赖。

### CLI / 共享核心

- 正式发布构建支持 Ed25519 更新签名，绑定版本、平台和摘要；自定义清单仅下载校验，自替换采用同目录原子重命名。
- 执行 24 小时配对凭证有效期，拒绝过期握手与旧凭证恢复；加固配对 URL、握手公钥与协议版本校验。
- 配对材料、配置和最近会话改用原子写入；收紧私有文件权限及 Windows ACL，拒绝日志符号链接并脱敏输入和密钥调试输出。
- 加固符号链接、文件读取和嵌套仓库访问边界，限制扫描、解码、删除、队列、终端缓冲及并发请求预算。
- Git 取消覆盖排队和执行阶段，超时回收子进程组；修复幂等缓存、操作编号与锁中毒错误处理。

独立发布说明：[CLI v0.1.6](docs/releases/relaycat-cli-v0.1.6.md) · [GUI v0.2.0](docs/releases/relaycat-gui-v0.2.0.md)。

## [GUI v0.1.9 / CLI v0.1.5] - 2026-10-16

### Added / Changed
- **Codex 启动兼容**：内置 `codex` 默认启动参数更新为 `["--no-alt-screen", "--no-daemon"]`，并在加载 `recent.json` 历史记录时自动为旧版 `codex` 会话补齐 `--no-daemon`，解决新版 Codex CLI 报后台守护进程错误的问题。
- **精简内置工具列表**：在 `relaycat-cli` 和 `relaycat-gui` 中移除内置 `aider` 子命令与预设项，聚焦主流内置工具（`shell`、`codex`、`claude`、`opencode`、`gemini`）与自定义工具支持。
- **自定义工具参数解析**：GUI 新增自定义工具时支持带单/双引号及转义空格的参数解析（`splitToolArgs`）。
- **TUI 单行输入体验**：CLI 交互式启动器（TUI）单行输入框自动过滤粘贴内容中的换行与控制字符，并支持 `Ctrl+U`（清空整行）、`Ctrl+W`（删除前词）、`Ctrl+H`（退格）。

### Fixed / Security
- **GUI 退出进程清理**：在托盘退出与窗口退出（`RunEvent::Exit`）时统一清理所有活动会话及其子进程组，防止后台残留孤儿 PTY / Relay 进程。
- **内嵌终端输入法兼容**：将主终端的中文输入法标点（`keyCode === 229`）修复逻辑复用到右侧内嵌终端（共享 Shell 与本地 Shell）。
- **GUI 异步竞态与弹窗一致性**：修复 `reopenTab` 重开标签时的异步关闭竞态及新建会话弹窗的工具状态刷新竞态；将配对设备解绑与工具校验提示统一为应用内 `confirmDialog`。
- **登录 Shell 环境变量提取加固**：在 macOS / Linux 桌面启动提取登录 Shell 环境变量时增加分隔符与合法变量名校验，避免 `.zshrc` / `.bashrc` 启动输出污染环境。
- **本地 PTY 会话环境一致性**：为 `relaycat` 本地非中继模式补齐 UTF-8 Locale、`TERM=xterm-256color` 以及 Windows `.cmd` / `.bat` 脚本包装。
- **工作区共享 Shell 进程组回收**：关闭工作区共享 Shell 时一并清理整个子进程组，避免后台子进程持有 PTY 句柄导致读取线程阻塞。
- **工作区 Git 健壮性与安全加固**：为工作区 `GitService` 增加非交互式 SSH 配置（`BatchMode=yes`）、120 秒超时控制、2 MiB 输出上限，并对历史查询 `reference` 执行严格校验与 `--end-of-options` 参数隔离。
- **CLI 升级与存储加固**：CLI `update` 模块强制校验 `https://` 协议并隔离 `curl` 参数；`pairing_store` 改用 `serde_json` 标准序列化。

## [GUI v0.1.8 / CLI v0.1.4] - 2026-09-18

- 优化中继服务端连接发送队列上限与 WebSocket 写入超时控制。
- 收紧工作区路径解析、文件读取与终端输入/粘贴字节边界，防止截断破坏 UTF-8 字符。
- 串行化主终端与内嵌终端输入队列，提升多会话稳定性。
