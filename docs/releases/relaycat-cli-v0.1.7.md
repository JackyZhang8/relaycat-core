# RelayCat CLI v0.1.7

发布日期：2026-11-11 · Tag：`relaycat-cli-v0.1.7` · 对比版本：v0.1.6

本次更新重点改善长任务运行后的手机重连与数据同步，避免配对凭证到期或工作区异常影响主终端连接。

## 长任务配对与重连

- 取消 24 小时配对凭证过期限制；已有配对凭证不再因时间经过而失效，运行多天的会话仍可重连或重新扫码。
- 保留 token 证明、加密握手与协议版本校验。

## 工作区稳定性

- 工作区共享 Shell 的控制与输出异常单独处理，避免错误中断主终端及 Relay 消息处理。
- 取消请求绕过普通工作区请求的并发额度，队列满载时仍可处理取消操作；保留项目访问校验。

## 升级提示

- 从 [GitHub Releases](https://github.com/JackyZhang8/relaycat-core/releases) 或 [RelayCat 下载站](https://cdn.relaycat.cn/) 获取对应平台安装包，安装后运行 `relaycat --version` 确认版本为 `0.1.7`。
- 已运行的旧版进程仍使用旧的配对与工作区逻辑；完成或妥善保存当前任务后，用新版重新启动会话以应用修复。
- Relay server 保持 `0.1.3`，本次未变更协议格式。
- 自行构建与发布的更新签名配置见 [CLI README](../../relaycat-cli/README.md#cli-update-signing)。
