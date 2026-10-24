# RelayCat Relay v0.1.3

发布日期：2026-10-24 · Tag：`relaycat-relay-v0.1.3` · 对比版本：v0.1.2

本次更新重点控制高并发与慢消费者场景下的内存占用，并改善 WebSocket 发送任务的退出行为。

## 资源限制与稳定性

- 新增全局排队 payload 字节预算，所有连接共享，默认 **256 MiB**，可通过 `limits.max_queued_bytes` 调整。该预算限制排队数据，不代表进程总内存上限。
- 默认全局并发连接上限调整为 **512**，每连接出站队列容量调整为 **32 条**，降低大量连接与大帧积压造成的内存压力。
- WebSocket 数据帧及心跳写入增加 **10 秒超时**，避免网络阻塞时发送任务长期挂起。
- 保留有界队列与慢消费者驱逐机制，队列溢出时通过 WebSocket `1013 Try Again Later` 提示客户端稍后重连。
- 更新依赖锁定版本，修复严格 Clippy 检查诊断。

## 配置与升级

```yaml
limits:
  max_concurrent_connections: 512
  outbound_channel_capacity: 32
  max_queued_bytes: 268435456
```

- 未显式设置上述字段时采用新版默认值；已有 YAML 中显式配置的连接上限和队列容量继续生效。
- 请根据实例内存、带宽及客户端规模检查现有配置，尤其是沿用较大连接上限或队列容量的部署。
- 替换服务端程序并重启服务后，通过根路径健康信息确认 Relay 版本为 `0.1.3`。
- 完整构建、配置和部署方式见 [Server README](../../relaycat-server/README.md)。
