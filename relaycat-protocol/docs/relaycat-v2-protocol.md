# RelayCat Protocol 数据结构说明

本文档按当前代码实现整理协议的数据结构与字段含义。

协议中有两个独立的版本号，注意区分：

- **Secure transport protocol**：当前为 `relaycat-v3`（domain separator 常量，见
  `relaycat-cli/crates/crypto/src/lib.rs` 的 `PROTOCOL_VERSION`）。覆盖
  pairing proof、relay admission、session key 派生和 AEAD AAD。v3 将双方
  per-connection salt 设为 mandatory（无 salt 即拒绝，杜绝降级）。v3 与 v2
  的密钥派生上下文不同，因此 v3 端**无法**与 v2 端互通。
- **Terminal state protocol**：当前为 `TERMINAL_STATE_PROTOCOL_V2 = 2`，通过
  加密通道内的 `hello_v2`/`hello_ack_v2` 协商，决定终端状态同步语义。

主要来源：

- `relaycat-protocol/src/lib.rs`
- `relaycat-cli/crates/crypto/src/lib.rs`
- `relaycat-cli/crates/cli/src/secure.rs`
- `android/RelayCat/app/src/main/java/dev/blueclaw/relaycat/protocol/*`
- `ios/RelayCat/Sources/RelayCatCore/Protocol*.swift`

## 1. 编码总览

RelayCat v2 运行在 WebSocket 上，业务消息使用 MessagePack 编码。

协议分两层：

1. `OuterFrame`：WebSocket 直接承载的外层帧。`join`、`peer_joined` 等连接控制帧是明文；`data` 帧承载加密后的业务消息。
2. `PlainMsg`：加密通道里的明文业务消息。发送前先 MessagePack 编码，再用 ChaCha20-Poly1305 加密，放入 `OuterFrame::Data.ciphertext`。

MessagePack 形状采用“单键 tag + body”的枚举编码：

```text
{ "<kind>": { ...fields } }

# 无 body 的帧/消息可编码为字符串：
"ping"
"pong"
"evicted"
"heartbeat"
```

字段名统一使用 `snake_case`。Rust 端使用 `rmp_serde::to_vec_named`；Android/iOS 手写 codec 与该结构对齐。二进制字段在 Rust 端通常是 MessagePack binary；移动端 decoder 兼容 binary 和 byte array。

## 2. 基础枚举

```text
Role = "cli" | "app"

Direction = "cli_to_app" | "app_to_cli"

ProtocolCapabilityV2 =
  "terminal_state"          # 终端状态快照/补丁
| "snapshot_recovery"       # 丢补丁或重连时请求快照恢复
| "exactly_once_input"      # input_stream_id + input_seq 输入去重
| "incremental_scrollback"  # patch 可增量携带滚出屏幕的 scrollback
| "terminal_transcript"     # 独立 transcript 分页历史
| "cli_metadata"            # CLI 上报会话元数据（如 project path）
| "compression"             # PlainMsg 明文在加密前做 raw DEFLATE 压缩
| "incremental_attrs"       # patch 只携带自上次以来新增的 attr 表尾部
```

各 capability 的协商属性：

| capability | mandatory | 对端缺失时的 fallback | 首次可用边界 |
|---|---|---|---|
| `terminal_state` | 是 | 无 —— 发送 `protocol_reject_v2` 拒绝 | — |
| `snapshot_recovery` | 是 | 无 —— 发送 `protocol_reject_v2` 拒绝 | — |
| `exactly_once_input` | 否 | 输入不做 (stream, seq) 去重，重连重发可能重复执行 | `hello_ack_v2` 之后 |
| `incremental_scrollback` | 否 | scrollback 只随全量 snapshot 更新 | `hello_ack_v2` 之后 |
| `terminal_transcript` | 否 | 无 transcript 分页历史 | `hello_ack_v2` 之后 |
| `cli_metadata` | 否 | 不发送 `cli_metadata` 消息 | `hello_ack_v2` 之后 |
| `compression` | 否 | 明文不压缩发送 | `hello_ack_v2` 之后 |
| `incremental_attrs` | 否 | attr 表增长时重发完整表 | `hello_ack_v2` 之后 |

向前兼容：解码 `hello_v2`/`hello_ack_v2` 时，本端不认识的 capability 字符串
直接丢弃而不报错（Rust `CapabilityOrUnknown`、Android/iOS `mapNotNull`/
`compactMap`）。因此新版本可以安全地新增 optional capability；协商结果永远
是双方已知集合的交集。

当前终端状态协议版本常量：

```text
TERMINAL_STATE_PROTOCOL_V2 = 2
```

## 3. 外层帧 OuterFrame

### 3.1 join

客户端进入 room 时发送。

```text
{
  "join": {
    "room_id": string,                  # 房间 ID；同一 CLI/App 会话共享
    "role": "cli" | "app",              # 当前连接身份
    "device_pubkey": bytes[32],         # X25519 公钥
    "pairing_token_proof": bytes[32]?,  # App 证明自己持有 pairing token；CLI 为空
    "relay_admission": bytes[32]?,      # relay HTTP/WS 侧的 room 准入证明
    "connection_salt": bytes[32]?,      # 本连接的随机 salt；v3 下语义上 mandatory
    "supports_join_accepted": bool      # 缺省 false；见 join_accepted
  }
}
```

约定：

- CLI join：`role="cli"`，`pairing_token_proof=null`，`relay_admission` 有值。
- App join：`role="app"`，`pairing_token_proof` 和 `relay_admission` 都有值。
- `connection_salt` 每次（重）连接都必须用 CSPRNG 重新生成；v3 对端会拒绝
  缺失 salt 的 `peer_joined`。字段在 wire 上标为 optional 仅为解码旧帧兼容。
- `supports_join_accepted=true` 表示该客户端理解显式的 `join_accepted` 确认
  帧；老客户端不发送该字段，解码缺省为 false，relay 就不会发确认帧。

### 3.2 peer_joined

relay 通知已有 peer：另一端已经加入。

```text
{
  "peer_joined": {
    "role": "cli" | "app",              # 新加入 peer 的角色
    "device_pubkey": bytes[32],         # 新 peer 的 X25519 公钥
    "pairing_token_proof": bytes[32]?,  # App 加入时携带，用于 CLI 验证
    "connection_salt": bytes[32]?       # relay 原样转发新 peer 的 salt
  }
}
```

CLI 收到 `role="app"` 后会验证 `pairing_token_proof`（proof 覆盖 salt，见
§4.1），再派生会话密钥。v3 下双方都必须校验 `connection_salt` 非空：缺 salt
的 `peer_joined` 直接按握手失败处理，不允许退回无 salt 派生。

### 3.2b join_accepted

relay 对 `supports_join_accepted=true` 的客户端在准入通过后立即发送的显式
确认帧，先于任何排队的 `peer_joined` 通知：

```text
"join_accepted"
```

- 新客户端在收到 `join_accepted` 前应保持“正在加入”状态；收到即视为准入
  成功，无需再依赖时间探测。
- 老 relay 不发送该帧；客户端在探测窗口内未收到任何帧时退回原有的
  时间探测判定。
- 老客户端（`supports_join_accepted` 缺省 false）不会收到该帧，行为不变。

### 3.3 peer_left

```text
{
  "peer_left": {
    "role": "cli" | "app"   # 离开的 peer
  }
}
```

### 3.4 data

加密数据帧。所有 `PlainMsg` 都通过这个帧承载。

```text
{
  "data": {
    "room_id": string,          # room 绑定；参与 AEAD AAD
    "direction": Direction,     # 决定使用哪条方向密钥和 nonce 前缀
    "seq": uint64,              # 每个方向独立递增，从 1 开始
    "nonce": bytes[12],         # prefix[4] + seq_be[8]
    "ciphertext": bytes         # ChaCha20-Poly1305 ciphertext || tag[16]
  }
}
```

`nonce` 规则：

```text
direction == "cli_to_app" -> "RCCI" || u64_be(seq)
direction == "app_to_cli" -> "RCIC" || u64_be(seq)
```

### 3.5 ack

当前代码定义了外层 `ack`，结构如下：

```text
{
  "ack": {
    "room_id": string,
    "direction": Direction,
    "seq": uint64
  }
}
```

注意：v2 终端渲染确认主要使用加密内层的 `render_ack_v2`，输入确认使用 `input_ack_v2`。

### 3.6 其他外层帧

```text
"evicted"  # 新 App 连接占用同一房间 App 槽位，旧 App 应提示“另一台设备已连接”，不可自动重试
"ping"
"pong"
{ "error": { "message": string, "code": RelayErrorCode? } }
```

```text
RelayErrorCode =
  "invalid_room_id"          # 不可重试
| "server_at_capacity"       # 可重试
| "join_timeout"             # 可重试
| "frame_too_large"          # 不可重试
| "invalid_join"             # 不可重试
| "join_room_role_mismatch"  # 不可重试
| "peer_not_registered"      # 可重试
| "admission_rejected"       # 不可重试（token 错误）
| "join_notification_failed" # 可重试
| "heartbeat_timeout"        # 可重试
| "rate_limited"             # 可重试
| "room_expired"             # 不可重试（需重新扫码配对）
```

`error.code` 是稳定的机器可读错误码（P2-10），客户端据此分类而不解析
`message` 文本；旧 relay 不发送 `code`，未知 code 解码为 null 并退回
message 分类。此外 relay 过载踢出 slow consumer 时使用 WebSocket close
code `1013`（reason 含 `slow_consumer retryable`），客户端按可重试瞬态失败
处理。

## 4. 加密与认证（secure transport v3）

以下所有构造中的版本串均为 `"relaycat-v3"`。v3 与 v2 的关键差异：双方的
per-connection salt 是 mandatory，并同时绑定进 pairing proof 与密钥派生。
这保证 (a) 恶意 relay 无法剥离/替换 salt（proof 校验会失败），(b) 重连后
seq 计数器从 1 重置时派生出的方向密钥仍然不同，杜绝 ChaCha20-Poly1305
nonce 复用。

### 4.1 pairing_token_proof

join 时证明发送方持有 pairing token，并把本连接的 salt 绑定进 transcript。

```text
HMAC-SHA256(
  key = pairing_token,
  msg =
    "relaycat-v3"
    || u32_be(len(room_id)) || room_id
    || role                # "cli" 或 "app"
    || device_pubkey       # bytes[32]
    || connection_salt     # bytes[32]，本连接的 salt
)
```

接收方用它拿到的 salt 重算 proof；salt 被篡改则 proof 校验失败，握手拒绝。

### 4.2 relay_admission

relay 准入证明，不绑定设备公钥，只绑定 token 与 room。

```text
HMAC-SHA256(
  key = pairing_token,
  msg =
    "relaycat-v3"
    || "relay-admission"
    || u32_be(len(room_id)) || room_id
)
```

### 4.3 session keys

双方用 X25519 得到 shared secret，再用 HKDF-SHA256 派生两个方向密钥。双方
salt 按固定 (cli, app) 顺序拼接，两端派生出相同上下文。

```text
pairing_token_hash = SHA256(pairing_token)

salt =
  "relaycat-v3"
  || u32_be(len(room_id)) || room_id
  || cli_public_key                # bytes[32]
  || app_public_key                # bytes[32]
  || pairing_token_hash            # bytes[32]
  || "relaycat-v3-connection-salt"
  || cli_connection_salt           # bytes[32]
  || app_connection_salt           # bytes[32]

cli_to_app_key = HKDF-SHA256(shared_secret, salt, "relaycat-v2-session-key cli_to_app", 32)
app_to_cli_key = HKDF-SHA256(shared_secret, salt, "relaycat-v2-session-key app_to_cli", 32)
```

（HKDF expand 的 info 标签沿用历史的 `relaycat-v2-session-key` 前缀；版本
隔离由 salt 中的 `relaycat-v3` 提供。）

每次（重）连接都必须重新生成 salt（`generate_connection_salt()`，OS
CSPRNG，32 字节）。

### 4.4 PlainMsg AEAD

加密算法：ChaCha20-Poly1305。

```text
plaintext = MessagePack(PlainMsg)
key       = direction == cli_to_app ? cli_to_app_key : app_to_cli_key
nonce     = direction_prefix || u64_be(seq)
aad       =
  "relaycat-v3"
  || u32_be(len(room_id)) || room_id
  || direction_raw              # "cli_to_app" 或 "app_to_cli"
  || u64_be(seq)
  || u32_be(len(plain_msg_type)) || plain_msg_type

ciphertext = ChaCha20-Poly1305-Seal(plaintext, key, nonce, aad)
```

解密端会用方向对应的候选 `plain_msg_type` 列表尝试 AEAD；解出后还会确认解码出的消息类型与 AAD 使用的类型一致。

## 5. 内层消息 PlainMsg

### 5.1 hello_v2

```text
{
  "hello_v2": {
    "protocol_versions": uint16[],       # 发送方支持的协议版本，例如 [2]
    "capabilities": ProtocolCapabilityV2[]
  }
}
```

### 5.2 hello_ack_v2

```text
{
  "hello_ack_v2": {
    "selected_protocol_version": uint16, # 协商选中的版本
    "capabilities": ProtocolCapabilityV2[]
  }
}
```

协商规则（P2-2）：CLI 取双方版本交集中的最高版本；交集为空或对端缺少
mandatory capability（`terminal_state`、`snapshot_recovery`）时回复
`protocol_reject_v2` 而不是静默降级。App 收到 `hello_ack_v2` 后校验
selected 版本在自己支持列表内且 mandatory capabilities 齐全。Hello/HelloAck
是屏障：`resume_v2`/`resize_event_v2`/`input_event_v2` 必须等屏障打开后才
能发送。

### 5.2b protocol_reject_v2

```text
{
  "protocol_reject_v2": {
    "reason": string,             # 面向升级提示的原因
    "supported_versions": uint16[]
  }
}
```

收到方应呈现“协议不兼容，请升级较旧一侧”的终态错误，不自动重试。

### 5.3 resume_v2

App 重连后告诉 CLI 自己已有的终端状态和输入 ack 进度。

```text
{
  "resume_v2": {
    "terminal_run_id": string?,          # App 已持有的终端运行 ID；未知则 null
    "last_applied_state_seq": uint64,    # App 已成功应用的 terminal state seq
    "last_snapshot_id": uint64?,         # App 当前基于哪个 snapshot
    "input_stream_id": string,           # App 当前输入流；缺省兼容为 "legacy"
    "last_input_ack": uint64             # CLI 已确认的最高连续 input_seq
  }
}
```

CLI 会尝试从 retained patches 补齐；如果 base 不匹配、补丁不连续、补丁过大，则返回完整 snapshot。

### 5.3b resume_accepted_v2

CLI 对 `resume_v2` 的显式回复，先于随后的补丁/快照发送：

```text
{
  "resume_accepted_v2": {
    "mode": "up_to_date" | "replaying_patches" | "sending_snapshot",
    "target_state_seq": uint64   # 追平后 App 应达到的 state seq
  }
}
```

App 的 reconnect overlay 以 `target_state_seq` 是否已应用为收敛条件，而不是
固定时长。

### 5.4 process_exit

```text
{
  "process_exit": {
    "code": int32?   # 子进程退出码；未知为 null
  }
}
```

### 5.5 terminal_snapshot_v2

完整终端状态。

```text
{
  "terminal_snapshot_v2": {
    "terminal_run_id": string,          # CLI 端一次终端运行的稳定 ID
    "snapshot_id": uint64,              # 当前快照 ID；resize/新快照会推进
    "state_seq": uint64,                # 该快照包含到的状态序号
    "cols": uint16,
    "rows": uint16,
    "title": string,
    "cursor": CursorState,
    "modes": TerminalModes,
    "palette": PaletteState,
    "attrs": CellAttr[],                # 完整 attr 表；TerminalRow 通过 attr_id 引用
    "reset_app_cache": bool,            # true 时 App 应清掉旧缓存/历史合并状态
    "scrollback_window": TerminalRow[], # 当前保留的 scrollback 窗口
    "screen_rows": TerminalRow[]        # 当前可见屏幕行，通常长度为 rows
  }
}
```

### 5.6 terminal_patch_v2

基于某个 snapshot 的增量终端状态更新。

```text
{
  "terminal_patch_v2": {
    "terminal_run_id": string,
    "base_snapshot_id": uint64,       # patch 依赖的 snapshot_id
    "from_state_seq": uint64,         # 必须等于 App 当前 state_seq + 1
    "to_state_seq": uint64,           # 应用后状态序号
    "attrs": CellAttr[],              # attr 表增长时发送完整表；空数组表示沿用已有表
    "ops": PatchOp[]
  }
}
```

App 校验规则：

```text
base_snapshot_id == current.snapshot_id
from_state_seq == current.state_seq + 1
to_state_seq >= from_state_seq
patch 中引用的 attr_id 必须存在
```

校验失败时 App 发送 `request_snapshot_v2`。

### 5.7 render_ack_v2

App 确认自己已经渲染到某个状态序号，CLI 可据此丢弃 retained patches。

```text
{
  "render_ack_v2": {
    "terminal_run_id": string,
    "snapshot_id": uint64,
    "applied_state_seq": uint64
  }
}
```

### 5.8 request_snapshot_v2

App 请求 CLI 发送完整快照。

```text
{
  "request_snapshot_v2": {
    "terminal_run_id": string?,       # 已知 terminal_run_id；未知可为 null
    "reason": SnapshotRequestReason,
    "cols": uint16,
    "rows": uint16
  }
}

SnapshotRequestReason =
  "seq_gap"        # patch 序列断档
| "missing_base"   # snapshot_id/base 不匹配
| "renderer_reset" # App 渲染状态需要重置
| "memory_pressure"
| "reconnect"
```

### 5.9 transcript 分页

transcript 是独立于终端 scrollback 的历史记录，用于保留 TUI/alt-screen 帧。

请求：

```text
{
  "request_transcript_v2": {
    "terminal_run_id": string,
    "before_entry_id": uint64?,  # 返回 entry_id 严格小于该值的页；null 表示最新页
    "max_entries": uint32
  }
}
```

响应：

```text
{
  "transcript_chunk_v2": {
    "terminal_run_id": string,
    "before_entry_id": uint64?,
    "entries": TerminalTranscriptEntryV2[],
    "attrs": CellAttr[],         # entries 中 TerminalRow 使用的 attr 表
    "has_more": bool
  }
}
```

entry：

```text
TerminalTranscriptEntryV2 = {
  "entry_id": uint64,
  "terminal_run_id": string,
  "state_seq": uint64,
  "kind": "normal_scrollback" | "alt_screen_frame" | "screen_frame",
  "cols": uint16,
  "rows": TerminalRow[],
  "captured_at_unix_ms": uint64,
  "frame_fragment"?: {
    "frame_id": uint64,          # 该逻辑 frame 的首个 entry_id
    "fragment_index": uint32,    # 从 0 开始
    "fragment_count": uint32
  }
}
```

`frame_fragment` 只用于超过单 entry 预算的 `screen_frame` /
`alt_screen_frame`。同一逻辑 frame 的 fragments 按 `entry_id` 连续排列，App
可跨 transcript pages 按 `frame_id` 和 index 无损重组；缺少任一 fragment
时不得把剩余部分当成完整 frame。该字段是可选 map 字段，旧 V2 App 会忽略
它并继续正常解码 entry。

### 5.10 input_event_v2

App 向 CLI 发送用户输入字节。

```text
{
  "input_event_v2": {
    "input_stream_id": string,  # App 启动/会话输入流 UUID；缺省兼容为 "legacy"
    "input_seq": uint64,        # 流内递增，从 1 开始
    "bytes": bytes              # 原始 PTY 输入，如按键、paste 内容
  }
}
```

CLI 侧按 `input_stream_id` 和 `input_seq` 做 exactly-once 去重：

```text
input_seq <= highest_contiguous_input_seq -> duplicate
input_seq == highest_contiguous_input_seq + 1 -> accept
否则 -> gap
```

**Exactly-once 边界（P2-9）**：该去重能力的准确名字是
`reconnect_deduplicated_input` —— 它只覆盖**同一 App 进程内、跨 transport
重连**的场景。pending inputs（stream id、seq、字节、ack 水位）只保存在内存
中，不做持久化：

- transport 断开重连：App 重发内存中未 ACK 的相同 (stream, seq) 信封，
  CLI 按序去重，输入恰好执行一次。
- App 进程被杀 / 重启：新进程生成新的随机 `input_stream_id`，pending 集为
  空。上一进程未 ACK 的输入要么丢失（CLI 未收到），要么已执行但 App 无
  记录（ACK 在途中）。无法跨进程去重，因此**不得**将其描述为持久
  exactly-once。UI 在用户主动断开/删除 session 且仍有 pending 字节时给出
  警告（`PendingInputWarningPolicy`）。
- 若未来需要跨进程语义，需实现加密持久化 outbox（持久化 stream id、seq、
  payload、ack 水位，重启后继续 Resume，并定义 session 删除 / CLI run 变更
  时的丢弃规则）。

`gap` 不得写入 PTY，也不得推进 `highest_contiguous_input_seq`。CLI 应重建
relay transport，使 App 在 `PeerJoined(cli)` 后按序重发仍未 ACK 的输入；否则
执行 gap 后的字节再累计确认会静默丢弃缺失输入。

### 5.11 input_ack_v2

CLI 确认已经连续处理到哪个输入序号。

```text
{
  "input_ack_v2": {
    "input_stream_id": string,  # 该 ACK 所属输入流；缺省兼容为 "legacy"
    "highest_contiguous_input_seq": uint64
  }
}
```

App 用它清理 pending inputs，并在 resize/resume 中带上 `last_input_ack`。

### 5.12 resize_event_v2 / resize_ack_v2

App 通知 CLI 终端视口尺寸变化。

```text
{
  "resize_event_v2": {
    "resize_seq": uint64,
    "cols": uint16,
    "rows": uint16,
    "input_stream_id": string, # 与输入流绑定
    "last_input_ack": uint64   # App 已知 CLI 确认的最高输入 seq
  }
}
```

CLI 响应：

```text
{
  "resize_ack_v2": {
    "resize_seq": uint64
  }
}
```

resize 通常会触发 CLI 发送新的 `terminal_snapshot_v2`，因为终端网格尺寸变化会改变 screen rows。

### 5.13 heartbeat

```text
"heartbeat"
```

加密通道里的轻量保活消息。

### 5.14 cli_status

CLI 周期性上报运行状态，App 顶部诊断 UI 使用。

```text
{
  "cli_status": {
    "cpu_percent_x10": uint16,      # CPU 百分比乘以 10；例如 123 表示 12.3%
    "memory_bytes": uint64,
    "rx_bytes_per_sec": uint64,
    "tx_bytes_per_sec": uint64,
    "process_name": string,
    "collected_at_unix_ms": uint64
  }
}
```

## 6. 终端状态基础结构

### 6.1 TerminalColor

```text
TerminalColor =
  "default"
| { "indexed": uint16 }                 # xterm 0..255 或其他索引
| { "rgb": { "r": uint8, "g": uint8, "b": uint8 } }
```

### 6.2 CellAttr

```text
CellAttr = {
  "fg": TerminalColor,
  "bg": TerminalColor,
  "bold": bool,
  "italic": bool,
  "underline": bool,
  "inverse": bool,
  "strikethrough": bool,
  "dim": bool
}
```

`attrs` 是表，`TerminalRow` 通过 `attr_id` 引用，避免每个 cell 重复传完整样式。

### 6.3 TerminalCell / CellRun / TerminalRow

```text
TerminalCell = {
  "text": string,   # 一个终端 cell 或宽字符文本
  "width": uint8    # 显示宽度；宽字符通常为 2
}

CellRun = {
  "attr_id": uint32,        # 引用 attrs[attr_id]
  "cells": TerminalCell[]   # 连续同样 attr 的 cell
}

TerminalRow = {
  "line_id": uint64,        # 行身份，用于 scrollback 合并/去重
  "wrapped": bool,          # 是否由终端自动换行延续
  "cells": CellRun[]
}
```

### 6.4 CursorState / TerminalModes / PaletteState

```text
CursorState = {
  "row": uint16,
  "col": uint16,
  "visible": bool,
  "style": "block" | "bar" | "underline"
}

TerminalModes = {
  "alt_screen": bool,
  "bracketed_paste": bool,
  "application_cursor": bool
}

PaletteState = {
  "default_fg": TerminalColor,
  "default_bg": TerminalColor,
  "cursor": TerminalColor,
  "ansi": TerminalColor[]   # 通常 16 色 ANSI palette
}
```

## 7. PatchOp

```text
PatchOp =
  { "put_cells": {
      "row": uint16,
      "col": uint16,
      "cells": CellRun[]
    }}

| { "clear_range": {
      "row": uint16,
      "col_start": uint16,
      "col_end": uint16,
      "attr_id": uint32
    }}

| { "replace_row": {
      "row": uint16,
      "line": TerminalRow
    }}

| { "scroll_region": {
      "top": uint16,
      "bottom": uint16,
      "delta": int16
    }}

| { "set_cursor": CursorState }
| { "set_title": string }
| { "set_palette": PaletteState }
| { "set_mode": {
      "mode": "bracketed_paste" | "application_cursor",
      "enabled": bool
    }}
| { "switch_alt_screen": bool }
| "bell"
| { "append_scrollback": {
      "rows": TerminalRow[]   # 刚从 live screen 顶部滚出的行，按 oldest-first 排列
    }}
```

语义说明：

- `put_cells`：从指定 row/col 写入若干 cell runs。
- `clear_range`：清空半开区间 `[col_start, col_end)`，使用指定 attr。
- `replace_row`：整行替换。
- `scroll_region`：滚动指定区域；正负 delta 的方向由端实现处理。
- `append_scrollback`：补齐 patch streaming 期间产生的 scrollback，不必等待下次全量 snapshot。
- `bell`：终端响铃事件，不改变屏幕文字，但 App 可累计 bell count。

## 8. 连接生命周期

典型 App 连接流程：

```text
App -> Relay:
  join(role=app, device_pubkey=app_pub, pairing_token_proof, relay_admission)

Relay -> CLI:
  peer_joined(role=app, device_pubkey=app_pub, pairing_token_proof)

CLI:
  verify pairing_token_proof
  derive session keys

App:
  derive session keys from QR pairing material + app private key + cli public key

之后双方通过 data 帧交换加密 PlainMsg。
```

典型状态同步流程：

```text
CLI -> App:
  terminal_snapshot_v2          # 首次全量状态

CLI -> App:
  terminal_patch_v2             # 后续增量状态

App -> CLI:
  render_ack_v2                 # 成功应用到某个 state_seq

App -> CLI:
  request_snapshot_v2           # patch 缺口/base 不匹配/渲染重置时请求全量

App reconnect -> CLI:
  resume_v2                     # 带 last_snapshot_id + last_applied_state_seq

CLI -> App:
  terminal_patch_v2[] 或 terminal_snapshot_v2(reset_app_cache=true)
```

输入与 resize：

```text
App -> CLI:
  input_event_v2(input_stream_id, input_seq, bytes)

CLI -> App:
  input_ack_v2(input_stream_id, highest_contiguous_input_seq)

App -> CLI:
  resize_event_v2(resize_seq, cols, rows, input_stream_id, last_input_ack)

CLI -> App:
  resize_ack_v2(resize_seq)
  terminal_snapshot_v2          # 通常随后发送新的全量状态
```

## 9. 兼容和默认值

- `input_stream_id` 缺省为 `"legacy"`，用于兼容旧消息。
- `ResizeEventV2.last_input_ack` 缺省为 `0`。
- `TerminalSnapshotV2.reset_app_cache` 缺省为 `false`。
- `Join.relay_admission` 缺省可为 `null`，但受保护 room 会校验。
- `Join.connection_salt`/`PeerJoined.connection_salt` wire 上 optional（旧帧
  解码为 null），但 v3 对端在握手时要求非空。
- `Join.supports_join_accepted` 缺省为 `false`（老客户端不发送该字段）。
- `Error.code` 缺省为 `null`；未知 code 解码为 `null` 并退回 message 分类。
- 未知 `ProtocolCapabilityV2` 字符串解码时被忽略。
- `TerminalPatchV2.attrs=[]` 表示 attr 表未变化，App 应复用当前 attr 表。

跨端一致性由各端 codec 的 wire-hex 测试保证（Rust `tests/frames.rs`、
Android `ProtocolCodecTest.kt`、iOS `ProtocolCodecTests.swift` 对同一帧断言
相同的 MessagePack 十六进制），新增字段/帧时三端需同步更新这组向量。

## 10. 当前方向消息策略

实现上解密时会按方向优先尝试常见消息类型，避免每次遍历所有类型成本过高。

常见 CLI -> App：

```text
terminal_patch_v2
terminal_snapshot_v2
transcript_chunk_v2
input_ack_v2
resize_ack_v2
heartbeat
cli_status
process_exit
```

常见 App -> CLI：

```text
input_event_v2
render_ack_v2
resize_event_v2
request_snapshot_v2
request_transcript_v2
heartbeat
resume_v2
```

注意：消息类型字符串是 AEAD AAD 的一部分，新增 `PlainMsg` 时必须同步更新三端的 type label 列表和方向策略，否则解密会失败。
