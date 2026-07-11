# relaycat-gui

RelayCat 的桌面 GUI（Tauri 2 + Vite 前端 + xterm.js 终端）。底层会话逻辑几乎全部复用
`relaycat-cli`：PTY 启动、会话定义（`TargetCommand` / `SessionKind`）、配置（与 CLI 共用
`~/.config/relaycat/config.json`）、配对二维码。

GUI 不重写底层逻辑：CLI 的 `run_interactive` 把 PTY 绑定到系统 stdin/stdout/SIGWINCH，
而 GUI 把同一套 PTY 启动逻辑抽成前端无关的 `PtySession`
（`relaycat-cli/crates/cli/src/session.rs`，CLI 与 GUI 共用），由前端的 xterm.js 负责渲染。

## 界面

- 顶部多标签：每个标签 = 一个独立 PTY 子进程，状态色区分 本地（灰）/ 等待配对（黄）/
  已配对（绿）/ 已退出（红）。
- 下方 xterm.js 终端：原生渲染 VT 序列，输入直接转发到 PTY，窗口 resize 同步到 PTY。
- 新建会话面板：选工具 → 选项目（收藏置顶）→ 填 Relay（留空 = 仅本机运行）。
- 配对二维码弹窗：Relay 模式下用 CLI 实际生成的配对 URL 渲染二维码；手机扫码、建立
  端到端加密会话后，标签与状态条自动转为「已配对」（绿）。点击状态条的配对标记可重新打开二维码。
- 设置页：默认工具 / 默认 Relay / 字号 / 自定义工具（增删）/ 收藏项目（删除），均与 CLI
  共用 `~/.config/relaycat/config.json`。
- 顶部更新提示横幅：启动时检查 CLI 升级清单，有新版本时提示。
- 底部状态条 + 空态引导（最近会话快捷入口）。

## 开发

需要 Node.js、Rust（edition 2024）以及 Tauri 的系统依赖（Linux 上为
`libwebkit2gtk-4.1-dev`、`build-essential`、`pkg-config` 等），还有 `tauri-cli`：

```bash
cargo install tauri-cli --version "^2.0" --locked
cd relaycat-gui
npm install
cargo tauri dev      # 启动 Vite + Tauri 开发窗口
```

构建发布产物：

```bash
cd relaycat-gui
cargo tauri build
```

## Relay 模式

本机模式直接在 PTY 中启动目标工具。Relay 模式则在 PTY 中启动 `relaycat` CLI 本身
（`relaycat <tool> --relay <url>`）：端到端加密配对、终端镜像、手机输入回传这套
协议敏感逻辑由 CLI 原样承担，GUI 不重新实现，只做终端宿主——本地用 xterm.js 渲染 CLI、
转发按键，并通过观察 CLI 输出与日志把配对二维码 / 状态呈现到原生界面：

- `session://pairing`：从 CLI 输出里抓到的真实 `relaycat://pair?...` URL，用于渲染二维码。
- `session://relay`：通过 tail `<project>/.relaycat/cli.log` 中的
  `secure session established` 判定手机已配对。

GUI 通过以下顺序定位 `relaycat` 可执行文件：`RELAYCAT_BIN` 环境变量 → 与 GUI 可执行文件
同目录（打包布局）→ `relaycat-cli/target/{release,debug}/relaycat`（开发布局）→ `PATH`。
开发时请先在 `relaycat-cli` 下 `cargo build --release` 生成该二进制，或设置 `RELAYCAT_BIN`。

## 与 relaycat-cli 的关系

`src-tauri` 是独立的 Cargo workspace，通过
`relaycat-cli = { path = "../../relaycat-cli/crates/cli" }` 复用 CLI 库，并在
`[patch.crates-io]` 中复制了 CLI 的本地 `vt100` 补丁。后端只是一层薄桥：把 CLI 的能力暴露
成 Tauri command / event，并把 PTY 输出流式推送到 webview。

### 后端 Tauri command

`list_tools` · `get_config` / `save_config` · `default_project` · `list_recents` /
`forget_recent` · `create_session` · `write_session` · `resize_session` /
`close_session` · `render_qr`

### 后端 → 前端事件

- `session://output` `{ id, data }`：PTY 原始字节，前端直接喂给 xterm.js。
- `session://status` `{ id, state, code }`：会话退出时上报退出码。
- `session://pairing` `{ id, url }`：Relay 会话抓到的真实配对 URL。
- `session://relay` `{ id, state }`：Relay 配对状态（`paired`）。

## 当前范围

已实现：多标签 + 本地 PTY + xterm.js I/O + resize + 新建/设置/空态；Relay 模式（复用 CLI 的
端到端加密配对 + 终端镜像 + 手机输入回传，配对二维码 / 已配对状态实时呈现）；最近会话快捷
入口、收藏管理、自定义工具增删、设置完善、更新提示。

后续可选：分屏、会话持久化、安装包分发（`cargo tauri build` 已可出包）。
