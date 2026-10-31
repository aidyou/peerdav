# Cloud append v2 格式与恢复合同

> **当前更新（2026-10-31）：用户已批准双协议及两种内容模式，不能再将 U-1 或 U-2 至 U-9 一概标为未实施。** AppendV2 的 repository、持久索引、publisher、engine、runtime、四个命令与前端入口已有生产可达路径；下述合同是实现与审阅依据，不代表所有验收矩阵完成，更不是供应商兼容通过报告。CAS v1 类型、JSON wire、revision 哈希与 generation 语义保持独立；新建默认 CAS，append 必须显式选择。

### 当前操作合同（生产可达，不等于全部验收通过）

- `cloud_append_discover`、`cloud_append_create`、`cloud_append_join`、`cloud_append_save_mapping` 已注册 Tauri 命令并由前端统一 invoke 包装调用；发现/加入不调用 CAS 写探测，CAS 旧路径继续保留能力门槛，不自动降级或迁移。
- 已批准并提供 `encrypted` 和 `authenticated_plain`；两者都需要密码包装的独立 master。authenticated_plain 不是“无需密码”或“未批准”：内容可读但需认证，不提供保密性，也不能原地切换模式。
- 加入要显式选择 anchor，提供可信指纹或明确接受 TOFU；TOFU 不证明抵御首次恶意替换。加入后还要显式选择已认证逻辑根；空仓库须显式命名根，不默认选择首项。anchor 选择与本地目录重选不是同一操作。
- vault schema 仍为 `2`；旧配置缺少 protocol 时默认 `cas_v1`，缺少 append binding 不推导成 append。仅保证新读取器兼容旧配置，不承诺旧版本读取新增字段。
- append 的恢复记录保留在持久索引中，完成 apply 后仍可查询保存原件的恢复档案；恢复到根内安全、尚不存在的相对目标，不覆盖现有文件。Prepared/Paused 安全续作先验证原持久意图、根身份和磁盘证据；存在新本地修改或证据不足时停止，不能推进基线或重造操作。目录恢复与完整硬终止矩阵不据此宣称完成。
- 普通对象浏览与仓库同步分开：仅看原始对象（含受保护同步区），不解密成虚拟文件系统。保护未知默认只读；身份确认、完整保护发现也只是允许尝试普通变更，后端仍对 CAS/append 同步区拒绝普通上传、覆盖、删除。普通对象删除不等于仓库 GC。

### 本轮已报告证据与未验证项

以下为本轮父任务/代理提供的分批运行结果，本文编辑未重新执行测试；各批次时间与代码状态不同，不猜统一总数：

- 后端代理：81 项专项测试及 24 项 runtime/schedule/manager 测试通过；双模式 HTTP runtime 端到端通过（236.71 秒，本地 fixture）。父 CAS 44 项及独立慢项 1 项（406.08 秒）通过。
- 此前 append/object 所列 `6+32+17+17+6+4+17+70+21+10+2 = 202`，分项合计确为 202。最终专项分别为 discovery 35、engine 19、index 17、publish 6、recovery 6、contract 6、repository 17；object 分组为 `70+21+10+2`，均通过。这些是不同时间的分批证据，不相加推断独立总数。
- 全部 35 个集成 test targets 已分批执行；剩余 13 targets 实质 124 项通过，另有外部验证默认空跑 1 项、七牛 ignored 1 项；bin/doc 为 0 项。lib 在最终生产修改后重跑，166 项全部通过；最终预算修改后受影响组均已重跑通过。两次单命令 `cargo test` 达到 600 秒超时，但不是目标未尝试，后续分批已覆盖全部目标；这不等于某次单命令全量成功，也不等于所有测试均在最终修改后统一重跑。
- `cargo fmt --check`、`cargo check --all-targets --offline` 通过。前端全部 21 个文件、346 项测试及 build 通过；仍有大 chunk 警告。
- 资源专项使用真实 `65 MiB + 1 byte` 文件，最大 part 为 8 MiB、peak buffer 为 8 MiB、inflight 为 1；测试耗时 4.33 秒，`/proc` 的 `VmHWM` 为 65364 kB（约 63.83 MiB）。这是该次进程高水位观测，不是 GNU time 测量或 RSS 硬限证明；256 MiB 仍只是计量分配预算。
- 窄查源码确认最终预算路径：discovery/apply/freeze 统一先持久记账 stat 请求，再按声明长度持久预留 GET 请求与 read 字节，之后 bounded stream；对象增大超声明长度立即拒绝，缩短仍保守计费、不退款。S3 stat 是对象 HEAD，故准确边界为“无 CAS 探测、不读写 `control/HEAD`，允许且计费对象 HEAD”，不是“追加式零 HEAD”。publisher 使用冻结已知长度预留 GET，无需 HEAD。持久 active unfinished run 让外层新 run id 的自动重试继续原账本，只有成功完成才开始新轮。
- 桌面/400px 视觉仍未实际渲染；真实厂商未测，本轮不读取凭据或执行远端测试。SIGKILL/掉电/磁盘满全矩阵、独立 S3 签名与供应商耐久性仍未验证；future drop、独立 SQLite 重开不等于实测断电。最终 `git diff --check` 成功；仍须保留工具 backend/worktree 差异限制，且 untracked 文件内容不能由普通 `git diff` 展示，需另行核对。

## 1. 协议与内容模式

新建默认 `CasV1`；用户显式选择 `AppendV2`。不能因条件写失败自动切换。AppendV2 普通打开使用 GET/LIST 及计费的对象 stat/HEAD，不做 CAS 探测；发布只使用 `PutCondition::Any`，不读写 v1 HEAD，不执行 CAS，不 DELETE/GC。anchor 初始化一次后不可修改，普通打开必须读取并验证所选 anchor。

内容模式是独立选择：`encrypted` 使用 XChaCha20-Poly1305；`authenticated_plain` 保持块内容可读，但仍需要密码封装的独立 master、认证元数据和 HMAC 块身份。master 不公开存储。共享 master 证明仓库成员认证，不提供设备不可抵赖签名。

## 2. Canonical 编码与身份

v2 使用项目现有 `serde_json` 的紧凑 UTF-8 编码，struct 按下列声明字段顺序，禁止未知/重复字段；所有 optional 必须编码为值或 null，不省略。所有集合使用排序数组（不得用无序 map）；解码后重新编码必须与输入逐字节相同。不改变 v1 的任何编解码。hex 一律小写：repo/operation 为16字节，摘要/逻辑块id为32字节，nonce为24字节；整数为非负 JSON 整数，限制在目标 u32/u64 内。

认证域使用无歧义 frame：`UTF8("peerdav-append/v2/") || UTF8(label) || 0x00 || repeated(u64_be(field_bytes.len) || field_bytes)`。字段本身用上述 canonical JSON，原始密钥、nonce、摘要按原始 bytes。SHA-256 为 wire 摘要；逻辑 revision ID 仍由现有 `merge::revision` 计算，locator 不进入 `LocalChange` 或旧 `BlockRef`。v2 不重定义 v1 revision frame。

所有对象引用 `ObjectRef { kind, digest }`，kind 为 `block|tree|commit`，digest 为完整 wire SHA-256。key 为仓库根下 `v2/<repo_id>/<kind>s/<digest>`；commit key 为 `v2/<repo_id>/commits/<operation_id>-<digest>`，因此 commit 引用额外使用 `CommitRef { operation_id, digest }`。不把 endpoint/bucket/prefix 或物理路径放入逻辑 revision；仓库 binding 保存物理根。相同逻辑块不同随机密文有不同 digest，均合法。禁止先把 wire digest 放进生成该 wire 的 AAD，避免循环依赖。

## 3. Anchor、密钥与信任

`Descriptor { version:2, repo_id, mode, chunker }`，chunker 使用现有固定 FastCDC profile 字段并验证参数。

`WrappedMaster { kdf, salt, nonce, ciphertext }` 使用现有 Argon2id 参数验证、16字节随机 salt、24字节随机 nonce及32字节随机 master；KDF 输出32字节。包装 AEAD 的 AAD 为 frame(`wrap`, canonical descriptor, canonical kdf, salt)。禁止任意远端参数触发无界 KDF 分配，沿用当前 crypto 参数上限。

`Anchor { descriptor, wrapped_master, auth }`；auth 为 HMAC-SHA256(anchor-key, frame(`anchor`, canonical descriptor, canonical wrapped_master))。anchor-key 从 master 经 HKDF-SHA256 派生，salt 为 repo_id，info 为 `peerdav-append/v2/anchor`。对象 key 为 `v2/anchors/<repo_id>-<SHA256(anchor_wire)>`。binding 持久保存完整相对 key、repo_id、wire 指纹、mode、信任来源。

可信指纹优先且必须验证；没有带外指纹时必须由用户显式接受 TOFU，不能悄悄接受。并发多个根全部保留并要求选择，不按时间、LIST顺序或可解密码选赢家。错密码、错指纹、anchor 暂缺不初始化空库。

## 4. 对象与认证

从 master 使用独立 HKDF info 派生 `block-id`、`encryption`、`authentication` 三把32字节密钥，salt 为 repo_id，info 前缀为 `peerdav-append/v2/`。块逻辑身份为 HMAC-SHA256(block-id-key, frame(`block-id`, repo_id, plaintext))。

`Envelope { version:2, repo_id, kind, mode, logical_id, plaintext_len, nonce, payload, auth }`：encrypted 的 nonce 为24字节随机值、payload 为 AEAD ciphertext+tag（hex）、auth 为 null；authenticated_plain 的 nonce 为 null、payload 为原始内容 bytes 的 hex、auth 为 HMAC-SHA256(authentication-key, frame(`plain`, aad, plaintext))。plain 的 wire 可直接解码内容，不声称隐藏长度或文件内容。AAD 为 frame(`object`, version, repo_id, kind, mode, logical_id, plaintext_len)，不含物理 locator。读取先检查 wire digest，再检查域、认证、长度和逻辑身份；encrypted 不另加自制确定性 nonce。

块 logical_id 为上述块 HMAC。tree/commit 的 logical_id 为 HMAC-SHA256(authentication-key, frame(kind, canonical plaintext))。一次 prepare 随机生成 wire 后必须冻结，不允许同 operation 重加密。

`BlockBinding { block_id, len, object }`；`Tree { version:2, repo_id, directory, entries, bindings }`。entries 使用现有逻辑 MergeSet 的完整 revision 记录以及子目录引用 `{ directory, object }`，分别按实体/revision id和目录id排序；bindings 按 block_id/len/digest 排序。每个活跃或历史文件块引用必须有认证 binding；同id/len多个合法locator允许，缺失/不匹配拒绝。引用的每个子树和块均需完整认证，不能只信摘要或 LIST。未变目录与已验证块可以复用；不使用共享可变去重表。

`Commit { version:2, repo_id, operation_id, parents, root }`；parents 为排序去重的精确 CommitRef 数组，root 为树 ObjectRef。树携带完整因果 MergeSet 历史，子快照必须包含每个父的所有逻辑 revision（同 ID 同内容）。不能仅凭 parents 声明代替父 GET 和历史包含验证。

## 5. 发现、状态与恢复

known 是分记录持久候选并集，状态 `observed|pending|validated|invalid`；LIST 遗漏不删除 known。全分页失败不得返回完整结果。父按精确引用 GET；暂缺是 `PendingDependency`，认证失败为 invalid，网络失败保留原始 source。迭代 DFS 验证环/深度/闭包、全部父历史和同 revision ID 同内容。只有完整验证的 R 才允许发布或破坏性物化；按时间戳选赢家禁止。

发布状态为 `Preparing -> Prepared -> Publishing -> PublishedUnknown|Published -> ApplyPending -> Applied`。Prepared 的单个 SQLite 事务发布全部分记录加密冻结对象、父 frontier、意图和请求预算账本；任意首次 PUT 必须在此后。上传按 blocks→trees→commit；commit 请求前持久 Publishing。丢响应、取消或重启在精确 GET 校验后确认，必要时同键同字节重传；LIST 缺席不证明未发布。同 operation 不改父、树、nonce或字节。

B 为已核验物化 epoch，L 必须完整扫描；本地 revision 父只来自 B，未知B/部分扫描不推断删除。R 是完整认证历史 join。apply journal 完成并复核目标之后，事务切换 B/frontier；pending 不推进 B。仅拉取不强制提交，稳态零 PUT。不把文件系统操作假装成数据库事务。

## 6. 预算与错误

沿用 block 8MiB、manifest 32MiB、tree明文总量512MiB、索引单记录32MiB、entities 100000、revisions 200000、revision parents/frontier 64。追加 DAG节点及known各100000、深度4096、commit parents/frontier各64；每轮最多1000实际LIST页/100000原始条目、20000请求（重试计入，单请求最多3次）、读写各2GiB、并发4、最多2块缓冲、operation对象100000、mapping staging8GiB、计量分配256MiB（不是RSS硬上限）。分配前检查；不截断历史、淘汰known或靠后台重启刷新累计预算。页边界必须来自实际适配器，不能按条目数虚构。

错误至少区分 `PendingDependency`、`Authentication`、`InvalidGraph`、`IncompleteListing`、`ResourceLimit`、`PublishedUnknown` 与原始网络/磁盘source。只在命令边界映射 AppError，code稳定、message一层、detail/source保留，敏感凭据不出现在错误或日志中。

## 7. 验证边界

### 7.1 U4 固定格式向量与复用边界

`append_repository.rs` 的模块内 `cfg(test)` 固定向量由独立 Python 3 标准库 `hashlib`、`hmac`、`json` 计算，不调用 Rust 生产编码器生成期望值：master 为 `00..1f`，repo_id 为 `00112233445566778899aabbccddeeff`，明文为 hex `00617070656e642d76320a`（11 bytes）。HKDF 使用 RFC5869 extract（repo_id 为 salt）和单块 expand（info 后接 `01`）；JSON 使用紧凑分隔符并保留声明顺序。固定比较 frame（包含空字段和 NUL）、四个派生密钥、块 HMAC、完整 AAD、authenticated_plain 完整 envelope bytes/key，以及排序去重后的 tree 明文 bytes 和 tree HMAC；tree 向量的引用仅用于格式验证，不代表已验证其引用闭包。

- block logical_id：`edd1f428c0adf21b1dcb56e7e735158db52353e5ad7fa5eebfe28e7c33c40697`
- plain block wire SHA-256：`c9ab99aff13090d572fe7d6c9815f36968e6c929c696b35019e0eba196281aa1`
- tree canonical plaintext SHA-256：`5c34238f180d8782ba470e4fb07da977a366251cf2415342930d7d37b11e94b6`
- tree logical_id：`4f5de3f3d1289b1cd52160fbb3e5447b83c25ff5535f9ad4db4e2f63baa7e81b`

同一仓库/master 下，plain 相同块重新 prepare 的 logical_id/key/wire 相同。encrypted 相同块逻辑身份相同而随机 nonce 导致 locator 不同；locator 改变不进入 v1 revision hash。未变树的复用指保留已经认证的 ObjectRef 和冻结 metadata bytes，并在后续 commit 中重复引用；encrypted 重新 seal 不保证相同 bytes/ref，不声称确定性加密。测试验证同 id/len 多个已认证 locator、多个文件和文件内重复块引用可共存；不证明 publisher 自动发现或选择复用对象。

`append_common` 是纯本地故障 fake，非 S3 签名、耐久性或厂商证明；重开共享 fake 仅模拟客户端重连，不代替 SQLite 硬终止。当前已有生产路径的格式、持久发布、恢复、预算和 runtime 回归，不能再统称 U-2 至 U-9 未实施；各测试层证据见页首。后续仍需完整 SIGKILL/掉电/磁盘满矩阵及独立外部验收；资源专项已有 `/proc` VmHWM 观测（见页首），不证明 RSS 硬限。future drop、独立 SQLite 重开不等于实测断电，局部故障注入不等于完成全部合同。未授权不读取凭据、不执行远端 ignored 测试、不删除远端对象。
