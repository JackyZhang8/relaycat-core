# RelayCat Core

RelayCat Core is the open desktop and relay layer for RelayCat: CLI, desktop GUI,
relay server, and shared protocol. It lets a mobile app take over local terminal
AI coding sessions such as Claude Code, Codex, OpenCode, Gemini CLI, Aider, or a
regular shell, while the actual work continues to run on your own machine.

RelayCat Core 是 RelayCat 的开源桌面端与中继层，包含 CLI、桌面 GUI、Relay 服务和共享协议。
它让手机 App 可以接管本地终端里的 AI 编程会话，比如 Claude Code、Codex、OpenCode、
Gemini CLI、Aider 或普通 Shell；真正的代码执行仍然发生在你自己的电脑上。

| GUI | TUI |
| --- | --- |
| 【GUI截图】 | 【TUI截图】 |

| Flow | App |
| --- | --- |
| 【流程图】 | 【APP截图】 |

## 中文

### RelayCat 是什么？

RelayCat 不是云 IDE，也不是把代码上传到远程服务器执行的平台。它更像一个本地 AI Coding
Agent 的手机遥控器：

- 电脑负责运行终端、项目、AI 编程工具和本地命令。
- 手机负责查看输出、输入提示词、审批继续、发送快捷键和中断任务。
- Relay 只负责 WebSocket 房间和密文转发，不参与解密终端内容。

这让你可以在午饭、通勤、开会间隙或离开工位时继续跟进本地 AI Agent 的长任务，而不用一直守在电脑前。

### 核心特性

- **本地执行**：Claude Code、Codex、OpenCode、Gemini CLI、Aider 和 Shell 仍在本机运行。
- **手机接管**：通过 RelayCat App 扫码后，在手机上查看终端、输入内容、审批操作和中断任务。
- **CLI + GUI 双入口**：终端用户用 `relaycat` CLI，首次上手或偏好可视化的用户用桌面 GUI。
- **端到端加密模型**：桌面端和手机端之间传输加密数据，Relay 只转发密文。
- **可自建 Relay**：可以使用默认 Relay，也可以部署自己的 `relaycat-relay`。
- **多平台发布**：CLI 和 GUI 分别独立发版，macOS 区分 `arm64` / `x86_64`，Windows 和 Linux 提供通用 x86_64 包。

### 仓库结构

```text
relaycat-core/
  relaycat-cli/        # CLI/TUI desktop bridge, binary: relaycat
  relaycat-gui/        # Tauri desktop GUI, reuses relaycat-cli session logic
  relaycat-server/     # WebSocket relay server, binary: relaycat-relay
  relaycat-protocol/   # Shared protocol types and frame encoding
```

移动端 App 源码不在这个仓库中。这个仓库只负责开源的 core 层：桌面入口、Relay 服务和协议。

### 典型工作流

1. 在电脑上安装 RelayCat CLI 或 RelayCat GUI。
2. 在电脑上启动本地 AI 编程工具。
3. RelayCat 生成配对二维码。
4. 手机 App 扫码连接。
5. 手机端接管这个本地终端会话。

CLI 示例：

```bash
relaycat claude
relaycat codex
relaycat opencode
relaycat gemini
relaycat aider
relaycat shell
```

GUI 流程：

```text
打开 RelayCat GUI -> 选择项目目录 -> 选择工具 -> 启动会话 -> 手机扫码
```

### 快速开始

#### 使用 CLI

先确保目标 AI 工具已经在本机终端中可用，例如：

```bash
claude --version
codex --version
```

然后在项目目录中启动：

```bash
relaycat claude
# 或
relaycat codex
```

如果使用自建 Relay：

```bash
relaycat codex --relay wss://relay.example.com
```

#### 使用 GUI

安装 GUI 后打开 RelayCat：

1. 检查本机工具是否可用。
2. 选择项目目录。
3. 选择 Claude Code、Codex、OpenCode、Gemini CLI、Aider 或 Shell。
4. 点击启动。
5. 用手机 App 扫描二维码。

GUI 底层复用 `relaycat-cli` 的会话逻辑，负责项目选择、工具检测、二维码展示和状态可视化。

#### 自建 Relay

构建并运行 relay server：

```bash
cargo build --release --manifest-path relaycat-server/Cargo.toml
./relaycat-server/target/release/relaycat-relay --listen 127.0.0.1:8787
```

建议通过 Nginx、Caddy 或云负载均衡配置 TLS 和 WebSocket 反向代理，对外提供 `wss://` 地址。

### 从源码构建

#### 测试 Rust 项目

```bash
cargo test --locked --manifest-path relaycat-protocol/Cargo.toml
cargo test --locked --manifest-path relaycat-cli/Cargo.toml
cargo test --locked --manifest-path relaycat-server/Cargo.toml
```

#### 构建 CLI

```bash
cargo build --release --locked \
  --manifest-path relaycat-cli/Cargo.toml \
  -p relaycat-cli
```

产物：

```text
relaycat-cli/target/release/relaycat
```

#### 构建 Relay

```bash
cargo build --release --locked \
  --manifest-path relaycat-server/Cargo.toml
```

产物：

```text
relaycat-server/target/release/relaycat-relay
```

#### 构建 GUI

GUI 使用 Tauri 2、Vite 和 xterm.js。

```bash
cd relaycat-gui
npm ci
npm run build
npx tauri build
```

Linux 构建 Tauri 需要额外系统依赖，例如 `libwebkit2gtk-4.1-dev`、`libgtk-3-dev`、
`libayatana-appindicator3-dev`、`librsvg2-dev`、`patchelf` 等。

### 发版模型

CLI 和 GUI 在同一个 Git 仓库中独立发版：

```text
relaycat-cli-v0.1.4
relaycat-gui-v0.1.5
```

对应 workflow：

```text
.github/workflows/release-cli.yml
.github/workflows/release-gui.yml
```

CLI 发版只校验并发布 CLI；GUI 发版只校验并发布 GUI。两个产品版本不需要同步。

### 安全边界

RelayCat 的核心边界是：

```text
本地电脑执行任务
手机端远程控制会话
Relay 只转发加密数据
用户负责审批和最终代码审查
```

RelayCat 不托管你的代码，不运行云端 AI Agent，不代管第三方 AI 工具账号。使用 Claude Code、
Codex、OpenCode、Gemini CLI 或其他工具时，它们自身的账号、模型、网络请求和隐私策略仍然由对应工具负责。

对删除文件、部署、数据库变更、生产操作等高风险命令，建议回到电脑上完整检查后再执行。

### 相关文档

- CLI: [`relaycat-cli/README.md`](relaycat-cli/README.md)
- GUI: [`relaycat-gui/README.md`](relaycat-gui/README.md)
- Relay server: [`relaycat-server/README.md`](relaycat-server/README.md)
- Protocol: [`relaycat-protocol/README.md`](relaycat-protocol/README.md)
- Relay deployment: [`relaycat-server/docs/cli-relay-quickstart.md`](relaycat-server/docs/cli-relay-quickstart.md)

---

## English

### What is RelayCat?

RelayCat is not a cloud IDE and does not move your code execution into a hosted
environment. It is a mobile remote control for local AI coding agents.

Your desktop keeps running the terminal, project, shell, and AI coding tool. Your
phone can watch output, type prompts, approve next steps, send shortcuts, and
interrupt the session. The relay server only routes encrypted WebSocket frames.

### Key Features

- **Local execution**: Claude Code, Codex, OpenCode, Gemini CLI, Aider, and Shell
  still run on your own machine.
- **Mobile takeover**: scan a QR code with the RelayCat app and control the local
  terminal session from your phone.
- **CLI and GUI entry points**: use `relaycat` from the terminal or launch the
  Tauri desktop GUI for project picking, tool detection, and visible session
  state.
- **End-to-end encryption model**: terminal payloads are encrypted between the
  desktop and the phone; the relay only forwards ciphertext.
- **Self-hostable relay**: use the hosted relay for quick starts or run your own
  `relaycat-relay`.
- **Independent releases**: CLI and GUI are versioned and released separately.

### Repository Layout

```text
relaycat-core/
  relaycat-cli/        # CLI/TUI desktop bridge, binary: relaycat
  relaycat-gui/        # Tauri desktop GUI, reuses relaycat-cli session logic
  relaycat-server/     # WebSocket relay server, binary: relaycat-relay
  relaycat-protocol/   # Shared protocol types and frame encoding
```

The mobile app source is not part of this repository. This repository contains
the open core layer: desktop entry points, relay server, and shared protocol.

### Typical Workflow

1. Install RelayCat CLI or RelayCat GUI on your desktop.
2. Start a local AI coding tool.
3. RelayCat displays a pairing QR code.
4. Scan it with the mobile app.
5. Continue controlling the local terminal session from your phone.

CLI examples:

```bash
relaycat claude
relaycat codex
relaycat opencode
relaycat gemini
relaycat aider
relaycat shell
```

GUI flow:

```text
Open RelayCat GUI -> choose project -> choose tool -> start session -> scan QR
```

### Quick Start

#### CLI

First make sure the target AI tool works in your local terminal:

```bash
claude --version
codex --version
```

Then start a session from your project directory:

```bash
relaycat claude
# or
relaycat codex
```

With a self-hosted relay:

```bash
relaycat codex --relay wss://relay.example.com
```

#### GUI

Install and open RelayCat GUI:

1. Check whether local tools are available.
2. Pick a project directory.
3. Choose Claude Code, Codex, OpenCode, Gemini CLI, Aider, or Shell.
4. Start the session.
5. Scan the QR code with the mobile app.

The GUI reuses the CLI session logic and focuses on tool detection, project
selection, pairing QR display, and visible session state.

#### Self-hosted Relay

Build and run the relay server:

```bash
cargo build --release --manifest-path relaycat-server/Cargo.toml
./relaycat-server/target/release/relaycat-relay --listen 127.0.0.1:8787
```

For production use, put it behind Nginx, Caddy, or a cloud load balancer with TLS
and WebSocket forwarding, then use a `wss://` URL from CLI or GUI.

### Build From Source

Run Rust tests:

```bash
cargo test --locked --manifest-path relaycat-protocol/Cargo.toml
cargo test --locked --manifest-path relaycat-cli/Cargo.toml
cargo test --locked --manifest-path relaycat-server/Cargo.toml
```

Build CLI:

```bash
cargo build --release --locked \
  --manifest-path relaycat-cli/Cargo.toml \
  -p relaycat-cli
```

Build relay:

```bash
cargo build --release --locked \
  --manifest-path relaycat-server/Cargo.toml
```

Build GUI:

```bash
cd relaycat-gui
npm ci
npm run build
npx tauri build
```

On Linux, Tauri requires system packages such as `libwebkit2gtk-4.1-dev`,
`libgtk-3-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, and `patchelf`.

### Release Model

The CLI and GUI live in the same Git repository but are released independently:

```text
relaycat-cli-v0.1.4
relaycat-gui-v0.1.5
```

Release workflows:

```text
.github/workflows/release-cli.yml
.github/workflows/release-gui.yml
```

The CLI release validates and publishes only the CLI. The GUI release validates
and publishes only the GUI. Their versions do not need to move together.

### Security Boundary

RelayCat's boundary is intentionally narrow:

```text
The desktop executes.
The phone controls.
The relay forwards encrypted frames.
The user reviews and approves real changes.
```

RelayCat does not host your code, run a cloud AI agent, or manage accounts for
third-party AI tools. Claude Code, Codex, OpenCode, Gemini CLI, and other tools
keep their own account, model, network, and privacy behavior.

For destructive commands, deployments, database changes, and production
operations, review the full context on your desktop before approving.

### Related Docs

- CLI: [`relaycat-cli/README.md`](relaycat-cli/README.md)
- GUI: [`relaycat-gui/README.md`](relaycat-gui/README.md)
- Relay server: [`relaycat-server/README.md`](relaycat-server/README.md)
- Protocol: [`relaycat-protocol/README.md`](relaycat-protocol/README.md)
- Relay deployment: [`relaycat-server/docs/cli-relay-quickstart.md`](relaycat-server/docs/cli-relay-quickstart.md)

