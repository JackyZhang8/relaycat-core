# relaycat-cli

RelayCat 的电脑端命令行工具。负责启动 shell 或 AI 编程工具（Codex、Claude Code、OpenCode 等），
管理 PTY 会话，并通过 relay 把终端会话端到端加密地中转给手机端。二进制名为 `relaycat`。

这是一个独立的 Cargo workspace：

- `crates/cli`：`relaycat` 二进制
- `crates/crypto`：CLI 私有的会话加密逻辑（`relaycat-crypto`，不单独发布）
- `third_party/vt100`：带宽字符 resize 补丁的本地 vt100

依赖共享协议 crate `relaycat-protocol`（通过 `version + path`，见根 `Cargo.toml`）。

## 构建

```bash
./build.sh release            # 产物：target/release/relaycat
# 或
cargo build --release -p relaycat-cli
```

Windows 交叉编译见 [build-win.sh](build-win.sh)。

## 测试

```bash
cargo test
cargo fmt
```

## 打包

桌面快捷方式和安装脚本见 [packaging/README.md](packaging/README.md)。

仓库整体的开源/闭源边界见上层的 `PROJECTS.md`。
