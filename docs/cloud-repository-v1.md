# Cloud Repository v1 协议（当前核对稿）

> **文档状态：当前协议与验证边界。** 本文只记录当前 Rust source 已存在的行为；不把尚未完成的 UI/manager 或未执行的集成矩阵写成已交付。本版目录树实现已接入 publisher、engine 和 cloud runtime；验证状态按已实际运行的 focused checks 记录，不替代完整回归声明。本文件不是性能或兼容性营销承诺。

## 1. 来源与范围

核对来源：

- `src-tauri/src/sync/chunker.rs`、`src-tauri/src/sync/types.rs`：FastCDC v2020 流式 chunk reader、固定 profile、buffer 独立性、64 MiB 修改复用和延迟读取测试。
- `src-tauri/src/sync/engine.rs`、`src-tauri/tests/cloud_sync_flow.rs`：engine 双向同步、编辑/删除恢复、unknown/empty、encrypted join、missing-root 行为。
- `src-tauri/src/sync/repository_types.rs`：JSON 类型、字段、大小限制、canonical decode、hex 校验。
- `src-tauri/src/sync/crypto.rs`：KDF、framing、HKDF、HMAC、XChaCha20-Poly1305、包装 master。
- `src-tauri/src/sync/repository.rs`：HEAD、对象布局、能力门槛、CAS、明文/密文对象。
- `src-tauri/src/sync/index.rs`：本地加密索引、apply journal/baseline。
- `src-tauri/src/sync/merge.rs`、`publisher.rs`：revision DAG、tombstone、投影、发布与依赖校验。
- `src-tauri/src/sync/apply.rs`、`sandbox.rs`：Linux 文件替换/恢复和 Landlock。
- `src-tauri/src/sync/*_types.rs`、`src-tauri/src/cloud/types.rs`：数据结构与能力定义。

当前明确不存在或未核实的功能不在本文伪装成已完成：完整目录扫描之外的性能优化、快照一致性保证、跨平台 safe apply。runtime commands 已接入并由 UI 使用，但这不等于全部端到端验收已完成。自动 GC/remote DELETE 是明确的非目标，不作为待实现承诺。

推荐运行路径为：**连接 → 测试连接 → 创建或加入仓库 → 映射目录 → 首次预览 → 同步 → 调度**。根目录消失或身份变更时必须暂停，人工重新选择根目录并重新预览；本版不做自动迁移。恢复区目录不会自动删除，当前目录 restore 仍为 `restoreUnsupported`，需要用户人工取回，不能宣称恢复全部。敏感信息包括 prefix/name 以及对象大小、timing、访问次数和 block equality；历史恢复区不做 GC、会占用空间，丢失密码无法恢复，模式 immutable 不能原地切换。

## 2. 冻结的 descriptor 与 JSON

`RepoDescriptor` 使用 `serde(deny_unknown_fields)`，JSON 字段为：

```json
{"version":1,"repo_id":"<32 lowercase hex>","mode":"encrypted|plain","chunker":{"name":"fastcdc","version":"5.0.0","normalization":"v2020","level":1,"min":1048576,"average":4194304,"max":8388608,"seed":0}}
```

字段顺序以 Rust `serde_json::to_vec` 的结构体声明顺序为准；wire canonical 的判定是 decode 后重新 `to_vec` 必须逐字节等于输入。未知字段、重复字段、非 canonical JSON 被拒绝。数值是 JSON number；没有另行规定浮点或 Unicode 规范化。

固定的 CDC descriptor 约束是 FastCDC 5.0.0、normalization `v2020`、level 1、min 1 MiB、average 4 MiB、max 8 MiB、seed 0；`src-tauri/src/sync/chunker.rs` 已实际使用 `fastcdc::v2020::StreamCDC`。`chunk_reader` 维持连续流状态，输出带 offset 的 owned chunk；测试覆盖空/小文件、1/4093/65536 字节短读下相同边界、24 MiB+17 固定边界向量、64 MiB 插入/删除/覆盖后的 chunk 复用，以及 64 MiB/4 GiB 延迟读取窗口。它不是独立 executor worker，也不宣称完整目录扫描之外的性能优化。

`WrappedMaster` 字段固定为 `kdf,salt,nonce,ciphertext`；`KdfParams` 固定为 `algorithm,version,memory_kib,iterations,parallelism,output_len`。`Head` 字段为 `descriptor,generation,commit,wrapped_master,auth`；`Envelope` 字段为 `version,repo_id,kind,block_id,nonce,ciphertext`。这些结构均 deny unknown fields。

所有 hex 输入都要求小写 `0-9a-f`，长度精确；大写 hex 被拒绝。nonce、salt、ciphertext 的实际字节长度在 crypto source 中分别受 24、16、48（master wrap ciphertext）等检查约束。HEAD 上限 64 KiB，metadata 明文上限 32 MiB，block 明文上限 8 MiB。

## 3. framing、KDF 与密钥隔离

`frame(label, fields)` 的实际编码为：

1. ASCII `peerdav-s3\0`；
2. `u32` 大端版本 `1`；
3. label 长度 `u32` 大端、label bytes；
4. field 数量 `u32` 大端；
5. 每个 field 的 `u64` 大端长度、field bytes。

KDF 是 Argon2id v1.3（`0x13`），memory 65536 KiB、iterations 3、parallelism 1、output 32 bytes；salt 16 bytes。参数不是这些固定值会在昂贵派生前拒绝。KDF 使用进程内 mutex 串行化。master 是随机 32 bytes，与设备 key 独立；vault/device key 不替代 repository master。

包装 master：password → Argon2id key；XChaCha20-Poly1305，随机 24-byte nonce；AAD 为 `frame("master-wrap", [canonical(descriptor), canonical(kdf)])`。包装数据只含 kdf、salt、nonce、ciphertext，master 不写入 descriptor。错误密码无法恢复 master；**lost password 无法 recover**。

仓库密钥由 HKDF-SHA256 派生，extract salt 为 `frame("repo-extract", [repo_id])`，expand info 为 `frame("repo-key", [repo_id, purpose])`。block-id 用独立 `block-id` key 的 HMAC-SHA256，输入 `frame("block-id", [repo_id, big-endian u64 plaintext length, plaintext])`；block、tree、commit AEAD 使用分离 purpose/key。

Block AAD 为 `frame("block-aad", [repo_id, block_id, big-endian u64 plaintext length])`；tree/commit AAD 为 `frame("object-aad", [repo_id, kind])`。HEAD auth 使用独立 `head-mac` HMAC，输入 `frame("head-mac", [HEAD bytes])`。nonce、HMAC、AAD、master-wrap 均不是可互换域。`frame` 的 label/field 数量使用 big-endian u32，field 长度使用 big-endian u64；sourceFrame 的独立 Python hex 向量见下节。Envelope 的 ciphertext 是 hex 展开形式，明文上限 8 MiB（block）/32 MiB（metadata）加 16-byte AEAD tag 后再乘 2 计 hex 字符上限；nonce 是 24-byte（48 hex 字符）。

本地 `SyncIndex` 使用 vault 提供的随机 32-byte key（不是 device fingerprint 或 repository password），行 token 是 HMAC；记录使用 XChaCha20-Poly1305、24-byte nonce 和 AAD。serde JSON 明文仅在内存/Zeroizing 临时值中处理；memory zeroize 是 best effort，不是硬件保证。

## 4. 目录树对象与发布语义

目录树采用分层对象，不再要求把全库快照压进一个 flat metadata 对象。wire `TreeDto` 的版本为 `1`，字段顺序为 `version,tree`；`tree` 是内部 tagged enum，`type` 只有 `directory` 与 `recovery`。`Directory` 字段顺序为 `directory,entries,children,recovery`，其中 `entries` 保存该目录下 entity 的完整 revision history（包括 tombstone），`children` 为 `{dir,object}` 引用，`recovery` 是可选的内部 recovery object 引用；`Recovery` 字段为 `entries,children`。未知字段、未知 tag、非 canonical JSON、旧 flat v1 形态均拒绝，不做迁移，也不会把未知/空输入静默初始化为新树。借用式 serde 编码保持相同字段顺序并用于 wire hash。

每个 tree object 的明文上限为 32 MiB；一次遍历的 tree 明文总量上限为 512 MiB，最多 100,002 个对象、100,000 个 entity、200,000 个 revision。目录树构建与加载使用迭代、有界遍历；单目录 history 超过 32 MiB 在 tree PUT 前拒绝。local `SyncIndex` 的单条记录仍受 32 MiB 限制，因此不能据此宣称本地索引已支持超过 32 MiB 的端到端运行；超过 32 MiB 的聚合测试仅验证 tree codec/repository 路径。

canonical owner 选择 entity 历史中 revision ID 最大的 live revision 所引用的目录；有效 root 父目录仍归根目录，缺失或非目录父节点、自引用及历史目录环成员归入内部 Recovery。Recovery 是清单元数据，不是本地恢复区，也不是用户可见目录名。ownership 仅决定历史数据的存放位置，不决定因果 winner；移动 entity 的全部 revision 和 tombstone 均保留。

发布先写子目录清单，再写父目录、commit，最后执行 HEAD CAS。已认证读取的清单可通过明文摘要和逐字节比较复用现有密文对象 ID，不重新加密、不改变随机 nonce 策略；同一加密快照重写时零 tree PUT。只修改一个一级子目录时，测试验证仅上传该子目录和根节点两个对象，未变化兄弟目录的密文 ID 不变。复用缓存上限为 512 MiB，仅影响上传效率，不修改历史对象。

## 5. 发布、对象布局与 merge 语义

当前实际 key：`control/HEAD`、`blocks/<block-id>`、`trees/<sha256 wire bytes>`、`commits/<sha256 wire bytes>`；创建前 list 必须只看到允许的 probe 对象，否则 prefix 非空。创建/join 要求 provider 能力 `create_only + conditional_replace + range_read + conditional_writes_verified` 全部为真；否则 capability probe/使用失败，不 fallback 到不安全写入。

Encrypted 仓库存 Envelope；Plain 仓库存明文 block/metadata。明文 mode 风险固定：内容、文件名/metadata、大小与访问时序不提供加密隐藏，且**不能原地翻转 mode**。HEAD 使用 generation、commit 和 auth；发布先上传 blocks、校验 candidate DAG 与所有 block 依赖，再 metadata，最后带 etag 的 HEAD CAS。CAS 后 transport 丢响应时，publisher 按 operation id 沿祖先有限查找恢复。

Merge 是有界 revision DAG：父 revision 必须存在且同 entity；frontier 保留并列分支；投影对大小写冲突使用确定性后缀；tombstone 是逻辑历史记录。Unknown baseline 不允许 delete，非 complete scan 不允许 delete。publisher 当前写入 `PublishedPendingApply` journal，未证明本地 apply 已完成。

Delete association 不做 remote DELETE。当前 repository/publisher source 没有自动 GC；tombstone/history logical delete 不释放远端空间，恢复会付出扫描/下载/重建成本。对象大小、timing、访问次数、block equality、prefix 等侧信道仍可能敏感，Encrypted 也不能隐藏这些元数据；不要向用户承诺“完全隐藏”。

## 6. 恢复与 safe apply 边界

`SyncIndex` 的 apply intent 与 baseline 以 SQLite journal/WAL/FULL synchronous 保存；apply 使用 staging/recovery 路径、identity/dev+inode、digest、size、父目录同步和 no-replace rename。它是 crash recovery 记录，不是 snapshot-consistent view，也不保证旧文件 handle 的快照一致性。

Linux safe apply 仅在 Landlock ABI v3 且 ruleset status 为 `FullyEnforced`、`no_new_privs` 成功时继续；能力不足返回 Unsupported，**不 fallback rename**。Windows/macOS 当前 `run_confined` 明确 Unsupported，不能伪称已实现 safe apply。Root loss 或 root identity 改变会暂停/失败；重选 root 后只能 manual preview，未知状态进入 safe union，不自动覆盖或删除。当前没有新目录迁移功能。

## 7. 已存在的静态测试向量与测试事实

`src-tauri/src/sync/crypto.rs` 的 `framing_hkdf_and_hmac_match_independent_python_vectors` 已有独立 Python 计算向量：

- `frame("test", ["a","bc"])` hex：`706565726461762d7333000000000100000004746573740000000200000000000000016100000000000000026263`
- master/repo 为 `00..1f` 时 `expand(...,"block-id")`：`bc9340205b52a46032c1eb45371e4cb2ce7922a7d382bcad3340d49d1e701de1`
- `block_id("hello")`：`4f3504276a511dd696dbaf00db9d764f718ec0a7e9f924d8fa47fe571d802b8d`

因此没有另行发明向量。随机 AEAD nonce/ciphertext 没有固定向量；若需跨语言固定密文向量，当前 source 为 pending。KDF bounds、wrong-password、未初始化随机设备 key、device/repository key 分离及 logical path metadata 风险仍须按 source 约束理解。

`src-tauri/tests/cloud_sync_flow.rs` 的 engine fixture 已覆盖：空设备物化首个文件、第二设备编辑回传、unknown/empty 不删除远端、missing root 不推进 generation、已知删除本地物化并保留 recovery 内容、encrypted devices 使用同一密码 join。它们使用进程内 local fake object store，不是真实多 part/AWS/R2 provider 集成。Linux Landlock 测试真实覆盖 held parent fd：root/a 移出后 private→held-a 的 INSTALL denied、held-a→private 的 preservation denied，以及正常 cross-directory rename；应用层 all-rename gate 已接入。Windows/macOS 是 unsupported。

## 8. 验证状态与 pending

最终代码验证已通过：Rust 共 344 项测试，含 127 项 lib、12 项 tree、13 项 publisher、16 项 sync，以及既有 peer/server/vault 回归；前端 19 个文件、292 项测试及 `pnpm build` 通过（主 bundle 大于 500 kB 的构建提示仍存在）。`cargo fmt --check`、`cargo check --all-targets --offline` 和 `git diff --check` 通过，无新增 Rust 警告。

单次全量 Rust 命令超过工具 600 秒限额后，采用两段执行而非忽略慢用例：`cargo test --manifest-path src-tauri/Cargo.toml --offline -- --skip local_plaintext_chunks_reduce_apply_download_after_middle_insertion` 全部通过，随后 `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_sync_flow local_plaintext_chunks_reduce_apply_download_after_middle_insertion --offline -- --exact` 单独通过（约 364 秒）。12 项目录树测试覆盖总元数据超过 32 MiB 的 40 个实际目录、41 个对象的精确往返；加密清单认证读取后的零 PUT 重写；单子目录变化时兄弟密文 ID 不变；完整历史、移动、tombstone 和目录环保存；缺失/重复/身份不符引用、未知版本、旧 flat v1、取消及单目录超限失败关闭。发布中途取消另有回归，确认 HEAD 不推进且取消语义不变。

HTTP fixture 的两设备测试已贯穿不同 vault 的创建/加入、预览、正反向修改、删除恢复及关联移除零远端 DELETE，但 fixture 不独立验证签名，不代表真实 AWS/R2。`active_intent`/`source_scan` 已接入恢复，覆盖 CAS 后取消及同操作重启、恢复期间本地修改保留、安装完成但日志未落盘核验；完整硬终止/磁盘满矩阵仍未覆盖。64 MiB 中间插入 4 KiB 的应用阶段下载量低于原文件的 25%；本地复用块必须重新验证内容地址。发布前完整性验证流量不包含在此断言中，不能称整个同步网络量低于 25%。

静态 CDC/framing/HKDF/HMAC 向量保持与源码一致。nested empty directory 物化、空目录删除以及根目录失联后同 inode 返回仍暂停已有本地运行时测试；重新选择并确认人工预览后才恢复同步资格。

尚未验证：no-provider 实测、400px 视觉检查、磁盘满条件下所有阶段故障注入、AWS/R2 真实授权及端到端 capability/TLS/proxy/CAS。Linux safe apply 仅 Landlock ABI v3、`FullyEnforced`、`no_new_privs` 可继续；Windows/macOS unsupported，不 fallback。root loss/identity change 只允许暂停、人工重选和预览；本版不迁移新目录。remote DELETE/自动 GC 是非目标，历史/恢复对象持续占用空间。