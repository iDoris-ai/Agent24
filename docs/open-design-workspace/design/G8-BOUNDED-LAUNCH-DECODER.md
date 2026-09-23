# G8：Bounded Launch Decoder 规格

> 状态：Design frozen / implementation not started
>
> 基线：sidecar actor state `d45fb17`；`serde_json 1.0.151`
>
> 范围：替换不受信 control request 的内部解码路径；公开 wire schema/signature 不变

## 1. 为什么不能事后校验

现有 `decode_request` 直接把 JSON 解成含 `Vec`/`BTreeMap` 的 `Request`。Serde 的
`BTreeMap` 会让重复 key 后写覆盖前写，事后无法证明原始 env 没有重复；internally-tagged
enum derive 在 `type` 不先出现时还可能先缓冲内容。64KiB frame 有界也不能阻止大量短
argv/env 项放大 heap。

因此 actor 的不受信入口禁止先调用旧 decoder，也禁止先构造 `serde_json::Value` 或
Serde `Content`。使用手写 `MapAccess`、`SeqAccess` 和 `DeserializeSeed`，在 materialize
前拒绝超额或重复数据。

## 2. API 与提交顺序

公开入口保持：

```rust
pub fn decode_request(
    bytes: &[u8],
    sequence: &mut RequestSequence,
) -> Result<Request, ProtocolError>;
```

内部顺序固定为：

```text
frame checks
-> serde_json::Deserializer::from_slice(body)
-> RequestSeed::deserialize
-> deserializer.end()
-> RequestSequence::accept(&request)
-> Request
```

只有 framing、bounded visitor、字段组装和 `end()` 全部成功后才推进 sequence。任何失败
保持原 sequence；actor 仍必须 poison 整条 control connection 并清理 owner，不能因 ID
未推进就在同一连接重试或跳过坏 frame。

建议私有模块：`bounded/{mod,text,argv,env,request}.rs`。Decode context 只记录首个固定
错误分类；Serde custom error 使用静态消息，禁止带 raw key/value。

## 3. 字符串与字段 key

`TextSeed` 使用 `deserialize_str`，在复制前验证解码后 UTF-8 bytes `<=4096` 和既有
control-character 规则；随后才 `try_reserve_exact` 并复制。未转义字符串可 borrow；
转义字符串会使用 serde_json 内部 scratch，因此 4096 是输出 String 限制，不能宣称
parser scratch 也只有 4096；scratch 仍由 64KiB frame 上限约束。

argv item/env value 可空；executable、cwd、env key 不可空。decoded env key 还必须在
native tracker/value 前拒绝任意 `=`（含 `\u003d`），避免 spawn 时被重解释；映射为
InvalidMessage。本 slice 不新增 ASCII-only key 规则或其他未冻结平台限制。共享
`validate_request_data` 与 `encode_request` 必须执行相同 key/quota 规则，不能让本地
encoder 产生 peer 必拒 frame。

`FieldSeed` 不分配 String，直接映射八个 key：`type/version/request_id/executable/cwd/
argv/env/force`。`KindSeed` 同样映射 `launch/signal/is_empty`。escaped key 必须与其解码
结果相同，例如 `"\u0074ype"` 与 `"type"` 算重复。

## 4. Top-level visitor

维护 `u8 seen_mask` 和每字段 `Option<T>`。`type` 可在任意位置；每个 key 在读取 value
之前检查 bit，重复立即失败，不能 materialize 第二个 value。

结束时必须满足精确字段集合：

| kind | 必须且只能存在 |
| --- | --- |
| launch | type, version, request_id, executable, cwd, argv, env |
| signal | type, version, request_id, force |
| is_empty | type, version, request_id |

缺字段、null、未知/跨 variant 字段和错误类型都拒绝；不得默认空 argv/env。version 用
`u8`、request ID 用 `u64`、force 用 bool，不做字符串/浮点/负数宽松转换。

## 5. argv 与 env quota

argv 最多 128 项，不信任 size hint：前 128 项逐项用 `TextSeed`；达到 128 后用一个立即
拒绝的 `RejectSeed` 探测第 129 项。该 seed 不读取或构造值，因此巨大字符串/嵌套对象
也不能在超限点 materialize。Vec 只用 fallible bounded reserve。

env 最多 64 项。达到 64 后用 `RejectSeed` 探测第 65 个 key，不分配 key、不读 value。
在限额内，顺序是：解 key -> 检查 exact duplicate -> 检查 native equivalent duplicate
-> 解 value -> insert。所有平台 exact duplicate 都拒绝，不能 last-write-wins。

Windows 不得用 `eq_ignore_ascii_case` 冒充系统语义。用一个永不 spawn 的私有
`std::process::Command` 做最多 64 项的 native key tracker：`env_clear()`，每个 key 调
`env(key, "")`，若 `get_envs().count()` 未增加则是 native-equivalent duplicate。原始
key/value 仍进入 BTreeMap，不 lowercase。Unix 允许 `PATH` 与 `Path`；Windows 拒绝。

## 6. 错误映射与 redaction

| 条件 | ProtocolError |
| --- | --- |
| frame 超限/缺 newline/内部 newline | 现有 TooLarge/MissingNewline/TrailingData |
| JSON、类型、字段集合、重复 key、错误 kind | InvalidJson |
| quota、字符串/control、reserve 失败 | InvalidMessage |
| version/ID/path 数据规则 | 现有 InvalidMessage |
| env key 为空、含 `=` 或 control | InvalidMessage |
| sequence 规则 | WrongSequence |

重复 env 不新增 wire ErrorCode。错误 Display/Debug、日志和测试失败输出都不得包含
executable、cwd、env、token 或 raw frame sentinel。

## 7. 必测矩阵

- argv 0/128 成功、129 失败，且第129项不进入 value visitor；
- env 0/64 成功、65 失败，且第65 key 不 materialize；
- 解码后 4096/4097 bytes、多字节/escape/surrogate；
- exact/escaped env duplicate；八个 top-level key逐一 duplicate；
- Unix/Windows 都拒绝 raw/escaped `=` env key；encoder/decoder validation 保持对称；
- type 首/中/尾；缺/null/unknown/cross-variant 字段；
- 失败后合法 Launch 证明 sequence 未推进；成功后坏 Signal 不推进更高 ID；
- raw error/Debug/Display 不泄漏 sentinel；
- mock access 证明 duplicate/quota 后不读 value，capacity 不增长；
- Unix native 允许 PATH/Path；Windows native 拒绝并含非 ASCII 对照。

结构/限额测试可在 Unix 跑；Windows key 等价必须由真实 Windows CI 验证，ASCII mock
不能代替平台门禁。

## 8. 按执行门禁拆分实现切片

1. DecodeContext + TextSeed/RejectSeed 与字符串边界；
2. FieldSeed/KindSeed 与 escaped/unknown key；
3. ArgvSeed 与 128/129 不读取证明；
4. EnvSeed exact duplicate/64 quota；
5. native env tracker 与 Unix/Windows tests；
6. top-level visitor/seen mask；
7. 精确字段集合与 Request 组装；
8. 切换唯一公开 `decode_request`、`end()` 和 sequence 回归；
9. 组合攻击、capacity/redaction、Windows native aggregate gate。

前七层只增加私有组件，第八层才切换入口；不得同时保留两个公开“安全 decoder”供调用者
任选。协议 owner 独占 `bounded/**` 和第八层 `lib.rs`；不改已通过的 frame reader、host
actor、平台 owner、desktop 或 Cargo 依赖。最终 SOL 必须检查 derive 未被不受信入口调用、
超限元素未先分配、Windows test 不是 ASCII 模拟。
