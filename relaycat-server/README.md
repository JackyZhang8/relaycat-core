# relaycat-server

RelayCat 的 relay 中转服务。基于 Axum 的房间式 WebSocket 服务，让 CLI 主机和移动端在网络两侧
配对、转发端到端加密的终端流量。二进制名为 `relaycat-relay`。

这是一个独立的 Cargo 项目，依赖共享协议 crate `relaycat-protocol`（通过 `version + path`）。
relay 只转发密文，不参与解密。

## 构建

```bash
cargo build --release         # 产物：target/release/relaycat-relay
```

## 运行

```bash
target/release/relaycat-relay --listen 127.0.0.1:8787
# 可选配置文件
target/release/relaycat-relay --listen 127.0.0.1:8787 --config /etc/relaycat/config.yaml
```

## 测试

```bash
cargo test
cargo fmt
```

## 文档与部署

- CLI + Relay 最简教程、systemd/Nginx 部署示例：[docs/cli-relay-quickstart.md](docs/cli-relay-quickstart.md)
- xfyun rtasr 签名鉴权方案：[docs/xfyun-rtasr-relay-auth-plan.md](docs/xfyun-rtasr-relay-auth-plan.md)
- Nginx 配置示例：[conf.d/](conf.d/)

仓库整体的开源/闭源边界见上层的 `PROJECTS.md`。
