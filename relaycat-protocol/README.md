# RelayCat Protocol

`relaycat-protocol` 是 RelayCat CLI、GUI、移动端 App 与 Relay 服务共享的 Rust 协议 crate。它定义 MessagePack 消息、`OuterFrame` 传输封包、payload 压缩、终端状态同步，以及文件、Git 和共享 Shell 的工作区消息。

需要特别区分两个版本概念：源码中的 `HelloV2`、`TerminalSnapshotV2` 等名称表示当前消息结构版本；这些消息运行在 RelayCat 当前的 **protocol v3 安全会话** 内。v3 负责配对证明、密钥派生、方向密钥和加密帧，二者不是互相替代的协议版本。

## 协议能力

### 握手与兼容性

- `HelloV2` / `HelloAckV2` 协商双方支持的版本区间和 capability。
- `ProtocolRejectV2` 在版本无交集或缺少强制能力时返回结构化拒绝原因。
- 未知 optional capability 可被旧端忽略，便于向前兼容。
- CLI、App 和 Relay 发布版本可独立演进，不要求产品版本号完全一致。

### 终端同步

- 全量终端快照、增量 patch、输入、resize、控制消息和渲染确认。
- 终端 transcript、replay/resume 和丢帧后的快照恢复。
- 大型 screen frame 按行边界分片，并保留逻辑 frame 标识与分片坐标。
- 对 terminal payload 进行尺寸预算，避免构造超过 Relay 单帧限制的数据。

### 工作区协议

- 带 `request_id`、`project_id`、deadline 和幂等键的请求封装。
- 文件目录分页、搜索和文本、图片、压缩包、数据库等预览结果。
- Git 状态、Diff、历史、引用、暂存、提交、分支、标签和远程操作。
- 共享 Shell 的创建、输入、resize、快照、输出事件和退出事件。
- 结构化错误码，包括 `GitNotInstalled`、`NotGitRepository`、`PathOutsideProject`、`TooLarge`、`Timeout` 和 `Cancelled` 等。
- 工作区单 payload 预算为 768 KiB；目录默认每页 100 项，Git 历史默认每页 20 项。

### 编码与传输边界

- 消息使用 MessagePack 编码。
- `OuterFrame` 携带 Relay 路由所需的角色、方向、序号和 payload。
- 外层帧最大为 1 MiB（`MAX_OUTER_FRAME_BYTES`）。
- payload 可在加密前使用 raw DEFLATE 压缩；小消息保留 identity framing。
- 解压后设置 16 MiB 硬上限，防止畸形压缩数据造成过量内存占用。
- 兼容旧的无 payload frame header 的 MessagePack 消息。

## 构建与测试

```bash
cargo build --locked
cargo test --locked
cargo fmt --check
```

如果要作为 crates.io 包发布，应先发布 `relaycat-protocol`，再发布依赖它的 CLI 和 Relay。

## 项目结构

```text
relaycat-protocol/
  src/lib.rs          # 握手、终端消息、OuterFrame 与编码辅助函数
  src/compression.rs  # payload framing、DEFLATE 与解压安全限制
  src/workspace.rs    # 文件、Git、共享 Shell 请求、响应和事件
  docs/               # 协议设计说明
```

更完整的数据结构说明见 [RelayCat v2 protocol 文档](docs/relaycat-v2-protocol.md)。文件名保留 `v2`，对应当前消息结构命名。

上层使用方包括 [`relaycat-cli`](../relaycat-cli/README.md)、[`relaycat-gui`](../relaycat-gui/README.md) 和 [`relaycat-server`](../relaycat-server/README.md)。

## 参与贡献

问题反馈、协议讨论和 Pull Request 请前往 [relaycat-core](https://github.com/JackyZhang8/relaycat-core/)。协议改动应优先保持向前/向后兼容，并补充编码往返、未知字段、尺寸边界和恶意输入测试。

## License

本项目基于 [Apache License 2.0](LICENSE) 开源。
