# RelayCat CLI

RelayCat CLI 是 RelayCat 的电脑端命令行入口，当前版本为 **0.1.3**，二进制名为 `relaycat`。它在本机启动 Claude Code、Codex、OpenCode、Gemini CLI、Aider、Shell 或自定义命令，管理 PTY 终端，并通过 Relay 将终端会话端到端加密地连接到 RelayCat App。

代码、凭据、项目目录和 Agent 进程始终留在本机；Relay 只转发握手信息和密文。

## 功能特性

- CLI 与交互式 TUI 两种使用方式；不带子命令时直接进入 TUI。
- 内置支持 Claude Code、Codex、OpenCode、Gemini CLI、Aider 和 Shell。
- 支持将任意本地命令注册为自定义工具。
- 原生 PTY、终端窗口 resize、控制键和手机输入回传。
- 扫码配对，以及基于 X25519、HKDF-SHA256 和 ChaCha20-Poly1305 的端到端加密。
- 网络波动后自动重连，并通过 replay、快照和增量 patch 恢复终端状态。
- 大终端画面按行边界分片，避免单帧超过传输预算。
- 通过 `HelloV2` / `HelloAckV2` 协商协议版本与能力；不兼容时返回明确的升级提示。
- 与 GUI 共用会话、配置、工作区和加密实现，避免维护两套协议逻辑。

## 快速开始

先确认需要使用的 Agent 已能在本机终端中正常启动：

```bash
claude --version
codex --version
```

在当前项目启动 Relay 会话：

```bash
relaycat claude --relay wss://001.relaycat.cn
relaycat codex --relay wss://001.relaycat.cn
```

指定项目目录或自建 Relay：

```bash
relaycat codex \
  --project /path/to/project \
  --relay wss://relay.example.com
```

其他内置工具：

```bash
relaycat opencode --relay wss://001.relaycat.cn
relaycat gemini --relay wss://001.relaycat.cn
relaycat aider --project /path/to/project --relay wss://001.relaycat.cn
relaycat shell --relay wss://001.relaycat.cn
```

启动自定义命令：

```bash
relaycat tool my-agent \
  --cmd my-agent \
  --project /path/to/project \
  --relay wss://relay.example.com \
  -- --model flash
```

不传 `--relay` 时只创建本地会话，不生成手机配对二维码。需要重新显示已有会话的二维码时：

```bash
relaycat qr --project /path/to/project --kind codex
```

完整参数可通过以下命令查看：

```bash
relaycat --help
relaycat <command> --help
```

## 构建与测试

需要 Rust stable；本项目使用 Rust edition 2024。

```bash
./build.sh release
# 或
cargo build --release -p relaycat-cli
```

产物位于 `target/release/relaycat`。Windows 交叉编译可参考 [build-win.sh](build-win.sh)，桌面快捷方式和安装脚本见 [packaging/README.md](packaging/README.md)。

运行检查：

```bash
cargo test --locked
cargo fmt --check
```

## 项目结构

```text
relaycat-cli/
  crates/cli/        # relaycat 二进制、PTY、TUI、Relay 会话与终端同步
  crates/crypto/     # 会话密钥派生和双向 AEAD 加密
  crates/workspace/  # 文件、Git 和共享 Shell 工作区后端
  third_party/vt100/ # 带宽字符 resize 修复的本地 vt100
  packaging/         # 安装和桌面集成脚本
```

CLI 依赖同仓库的 [`relaycat-protocol`](../relaycat-protocol/README.md) 共享消息结构；[`relaycat-gui`](../relaycat-gui/README.md) 直接复用 CLI 与 workspace crate；[`relaycat-server`](../relaycat-server/README.md) 负责 WebSocket 房间和密文转发。

## 参与贡献

问题反馈、功能建议和 Pull Request 请前往 [relaycat-core](https://github.com/JackyZhang8/relaycat-core/)。提交前建议运行格式检查与完整测试，并避免在日志、截图或测试数据中提交配对 token、密钥和真实项目内容。

## License

本项目基于 [Apache License 2.0](LICENSE) 开源。
