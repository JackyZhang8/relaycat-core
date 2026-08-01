# RelayCat Relay

`relaycat-relay` 是 RelayCat 的开源 WebSocket 中继服务，当前版本为 **0.1.2**。它基于 Axum 和 Tokio，为 CLI/GUI 与移动端 App 提供房间配对、连接管理和帧转发。

Relay 只负责转发公开握手数据和端到端加密后的 payload，不持有客户端私钥，也不参与解密终端、项目文件、Git 或共享 Shell 内容。

## 功能特性

- Axum WebSocket 服务，连接入口为 `/ws`。
- CLI 与 App 按 room 和 role 配对，双向转发 `OuterFrame`。
- 根路径健康信息包含 Relay 版本、协议版本以及最低 GUI/CLI 兼容版本。
- 全局连接数、单 IP 连接数、入站消息速率、入站字节速率和单帧大小限制。
- 每个连接使用有界出站队列；慢消费者导致队列溢出时会被驱逐，使用 WebSocket `1013 Try Again Later` 和可重试原因提示客户端重连。
- 初始 Join 帧超时、周期 ping/pong 存活检测、失效房间清理和运行统计。
- 可选 YAML 配置文件，适合官方服务、自建服务器或可信内网部署。

## 构建与运行

需要 Rust stable；本项目使用 Rust edition 2024。

```bash
cargo build --release --locked
target/release/relaycat-relay --listen 127.0.0.1:8787
```

指定配置文件：

```bash
target/release/relaycat-relay \
  --listen 127.0.0.1:8787 \
  --config /etc/relaycat/config.yaml
```

未传 `--config` 时，如果当前目录存在 `config.yaml`，Relay 会自动加载；否则使用默认配置。

## 传输限制

所有 YAML 字段都可选，缺失字段使用下列默认值：

```yaml
limits:
  max_concurrent_connections: 4096
  max_connections_per_ip: 24
  inbound_messages_per_second: 120
  inbound_bytes_per_second: 2097152
  outbound_channel_capacity: 64
  max_binary_frame_bytes: 1048576
```

这些限制分别控制全局连接、单来源 IP 连接、每连接每秒消息数、每连接每秒字节数、出站队列容量和单个二进制帧大小。生产环境应根据实例内存、文件描述符上限、网络带宽和客户端规模调整，并在反向代理层增加连接和请求保护。

## 客户端连接

开发环境或可信内网可直接连接：

```bash
relaycat codex --relay ws://192.168.1.12:8787
```

公网环境建议只让 Relay 监听本机或内网地址，由 Nginx、Caddy 或负载均衡器终止 TLS，对客户端暴露 `wss://relay.example.com`。

Nginx 的关键配置如下：

```nginx
location /ws {
    proxy_pass http://127.0.0.1:8787;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 3600s;
    proxy_send_timeout 3600s;
}
```

完整示例见 [conf.d](conf.d/)。

## 安全部署建议

- 不要把未启用 TLS 的 `ws://` 服务直接暴露到公网。
- 限制 Relay 进程权限，只开放必要端口和目录。
- 在反向代理和云防火墙中设置连接数、带宽和请求频率限制。
- 监控连接数、慢消费者驱逐、内存、CPU 和异常断连。
- 定期升级 Relay、CLI、GUI 和 App，并关注根路径返回的兼容性元数据。
- E2EE 保护消息内容，但 Relay 仍可观察 IP、连接时间、room 和流量大小等传输元数据。

## 测试

```bash
cargo test --locked
cargo fmt --check
```

测试覆盖房间生命周期、连接限制、帧限制、WebSocket 行为、慢消费者处理和配置解析。

Relay 使用 [`relaycat-protocol`](../relaycat-protocol/README.md) 的共享帧结构；桌面接入见 [`relaycat-cli`](../relaycat-cli/README.md) 和 [`relaycat-gui`](../relaycat-gui/README.md)。

## 参与贡献

问题反馈、部署建议和 Pull Request 请前往 [relaycat-core](https://github.com/JackyZhang8/relaycat-core/)。涉及公网输入、资源限制或队列行为的改动，应补充边界测试并说明安全影响。

## License

本项目基于 [Apache License 2.0](LICENSE) 开源。
