# relaycat-protocol

RelayCat 客户端（CLI / iOS / Android）与 relay 服务端之间的共享协议层。提供 v2 协议的
MessagePack 数据结构、`OuterFrame` 传输封包与 payload 帧/压缩逻辑。

`relaycat-cli` 和 `relaycat-server` 都依赖这个 crate。它是一个独立、可单独发布的 Cargo 包：
先发布 `relaycat-protocol`，再发布依赖它的 CLI 和 server。

## 构建与测试

```bash
cargo build
cargo test
```

## 文档

- 协议数据结构说明：[docs/relaycat-v2-protocol.md](docs/relaycat-v2-protocol.md)

仓库整体的开源/闭源边界见上层的 `PROJECTS.md`。
