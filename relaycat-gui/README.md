# RelayCat GUI

RelayCat GUI 是 RelayCat 的开源桌面客户端，当前对外发布版本为 **0.1.7**。它使用 Tauri 2、Vite、TypeScript 和 xterm.js，将本地 AI Coding Agent、项目文件、Git 工作流和手机远程控制整合在一个桌面界面中。

GUI 不会把项目上传到云端执行。Claude Code、Codex、OpenCode、Gemini CLI、Aider、Shell 和自定义工具仍在本机 PTY 中运行；手机通过 RelayCat 的端到端加密会话查看和控制终端。

## 功能特性

### 终端与会话

- 多项目、多标签 PTY 会话，每个标签对应独立的本地进程。
- 终端分屏、标签切换、窗口 resize、快捷键和右键菜单。
- xterm.js 终端渲染，并在 WebGL 上下文异常后自动恢复。
- 支持 Claude Code、Codex、OpenCode、Gemini CLI、Aider、Shell 和自定义工具。
- 扫码配对、连接状态显示、二维码重新打开和断线重连。
- 每个已配对会话可同时使用 App 共享终端和 GUI 本地辅助终端。

### 工作区与 Git

- 浏览项目目录，并分页加载大型目录。
- 预览文本、代码和 Markdown；对二进制或过大文件给出明确提示。
- 查看 Git 状态、文件 Diff、提交 Diff 和历史记录。
- 支持暂存、取消暂存、按 hunk 或选中行操作、提交和修订提交。
- 支持创建、切换、重命名和删除分支，以及创建标签。
- 支持 fetch、pull、push、提交并推送等远程操作。
- 提供 cherry-pick、revert 和不同模式的 reset，并在危险操作前确认。
- Windows 未安装 Git 时，明确提示“未检测到 Git”，不会混淆为普通工作区请求失败。

### 桌面体验

- 项目收藏、最近会话、默认工具、默认 Relay 和自定义工具配置。
- 自动检查、下载和安装新版本。
- 系统托盘与开机自动启动。
- 配对设备管理、会话状态与运行诊断。
- 导出经过脱敏处理的诊断包，过滤主目录、配对材料、密钥和 IP 等敏感信息。
- 中英文界面。

## 架构关系

GUI 复用 `relaycat-cli` 的 PTY、会话、Relay 和加密实现，并复用 `relaycat-workspace` 处理项目文件、Git 和共享 Shell。Tauri 后端负责把这些本地能力以 command 和 event 暴露给前端，xterm.js 负责终端显示与输入。

Relay 模式下，GUI 会为 CLI 启动一个带随机 token 的本机 loopback TCP 状态桥。CLI 通过认证后的结构化状态快照通知 GUI 当前处于等待配对、同步、在线、断开或错误状态；GUI 不再依赖 tail 日志判断是否配对。

```text
Vite / TypeScript / xterm.js
            |
      Tauri command/event
            |
relaycat-cli + relaycat-workspace
            |
     encrypted WebSocket
            |
      Relay -> Mobile App
```

GUI 会按以下顺序定位 `relaycat` 可执行文件：

1. `RELAYCAT_BIN` 环境变量；
2. GUI 可执行文件同目录中的打包版本；
3. 源码工作区中的 CLI release/debug 产物；
4. 系统 `PATH`。

## 开发

需要 Node.js、Rust stable、Tauri 2 所需的系统依赖，以及一个可用的 `relaycat` CLI 构建产物。

```bash
cd ../relaycat-cli
cargo build --release -p relaycat-cli

cd ../relaycat-gui
npm install
npm run tauri dev
```

也可以设置 CLI 路径：

```bash
RELAYCAT_BIN=/absolute/path/to/relaycat npm run tauri dev
```

Linux 通常还需要 `libwebkit2gtk-4.1-dev`、`libgtk-3-dev`、`libayatana-appindicator3-dev`、`librsvg2-dev`、`build-essential`、`pkg-config` 和 `patchelf` 等系统依赖。

## 构建与测试

构建前端：

```bash
npm ci
npm run build
```

构建桌面安装包：

```bash
npm run tauri build
```

运行前端和文档测试：

```bash
npm test
```

运行 Tauri 后端测试：

```bash
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

正式版本通过仓库中的 GitHub Actions 独立发布，tag 格式为 `relaycat-gui-v<version>`。桌面安装包可在 [GitHub Releases](https://github.com/JackyZhang8/relaycat-core/releases) 或 [RelayCat 下载站](https://cdn.relaycat.cn/) 获取。

## 项目结构

```text
relaycat-gui/
  src/             # TypeScript 前端、终端、工作区和本地化
  src-tauri/       # Rust/Tauri 后端、Git、会话和桌面集成
  tests/           # 前端行为、发布清单和 README 链接测试
  package.json     # Vite、测试与 Tauri 命令
```

共享协议见 [`relaycat-protocol`](../relaycat-protocol/README.md)，本地会话核心见 [`relaycat-cli`](../relaycat-cli/README.md)，Relay 部署见 [`relaycat-server`](../relaycat-server/README.md)。

## 参与贡献

问题反馈、功能建议和 Pull Request 请前往 [relaycat-core](https://github.com/JackyZhang8/relaycat-core/)。提交前请运行前端测试、构建检查和相关 Rust 测试；涉及更新、Git 或进程管理的改动应同时覆盖失败路径和用户提示。

## License

本项目基于 [Apache License 2.0](LICENSE) 开源。
