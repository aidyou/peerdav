# Cloud Storage / S3 连接说明（当前核对稿）

> **文档状态：当前协议与验证边界。** 本文只描述当前 source 已实现或明确拒绝的边界，不把未执行的真实 provider、硬终止/磁盘满矩阵或 UI 检查写成已通过，也不作 provider 性能、可用性或兼容性营销承诺。

## 1. S3-only 与 endpoint

当前连接模型是 S3-compatible object store：配置结构 `S3Config` 的实际字段为 `endpoint,bucket,region,prefix,path_style,access_key,secret_key,session_token,allow_insecure_http,proxy`。endpoint 必须由用户显式提供；source 不提供隐式 provider endpoint，也没有证据证明 AWS 或 Cloudflare R2 已通过真实 integration。不要声称 “AWS/R2 supported/passed”。

仓库依赖 object store 的 create-only PUT、conditional replace/CAS、range read、已验证 conditional writes 四项能力；缺一即 `Unsupported`，不降级成普通 overwrite。对象 key 必须位于配置 prefix；HEAD/CAS 是 `control/HEAD`，数据对象是 `blocks/`、`trees/`、`commits/`。创建时 prefix 必须为空（允许的 owned probe 除外）。

S3/对象存储不是远端文件系统：上传的是独立 block 和分层、有界 tree/commit metadata，不是完整文件的 multipart overwrite。tree object 明文最多 32 MiB，单次遍历总量最多 512 MiB；目录树采用 `TreeDto` v1 的 `directory`/`recovery` typed wire 结构，旧 flat v1 不迁移。当前没有 remote DELETE 用于 association delete，也没有自动 GC；这是本计划明确的非目标，不作为未完成 gap。logical tombstone/history 不释放对象空间。

## 2. HTTP、HTTPS、proxy 与 fail-closed

`ProxyMode` 实际有三种：`direct`、`environment`、`custom`。custom URL 接受 HTTP、HTTPS、SOCKS5；输入 `socks5` 会规范化为 `socks5h`。SOCKS5h 通过代理发送域名，remote DNS 行为由代理完成；源码测试使用 `unresolvable.invalid` 验证域名传给代理。HTTP/HTTPS proxy 可带认证，日志/debug 不应泄露 credentials。

transport 默认 `.no_proxy()`、禁止 redirect、connect timeout 15 秒、request timeout 120 秒；不自动继承系统/PAC/fallback route。Environment 模式仅明确读取 `ALL_PROXY/all_proxy`、`HTTP_PROXY/http_proxy`、`HTTPS_PROXY/https_proxy` 和 `NO_PROXY/no_proxy`，无有效值时不凭空增加 route。Custom 模式只使用明确 URL。无代理 direct 就是 direct，连接失败应 fail closed，不静默改用另一条网络路径。

endpoint 的 HTTP 明文仅在 `allow_insecure_http` 明确开启时才可能被配置层接受；文档不应把它描述为安全传输。HTTPS/TLS 与 proxy 证书/网络错误都应阻止操作而非绕过验证。`path_style` 是显式 S3 config 字段，不推断成 provider 成功证明。

## 3. 能力探测与安全写入

`StorageCapabilities` 只有四个布尔能力：`create_only`、`conditional_replace`、`range_read`、`conditional_writes_verified`。repository 的门槛是四者全 true。能力不足是 pending/unsupported，不 fallback rename、非条件写或“尽力而为”同步。Capability probe 的结果必须与实际 object store 行为一致；当前代码定义了门槛，但没有完成所有真实 provider integration 验证。

Linux apply 的安全边界另行受 Landlock 约束：只接受 ABI v3、`RulesetStatus::FullyEnforced` 和 `no_new_privs`；不足时返回 unsupported，不能 fallback 普通 rename。Windows/macOS 当前明确 unsupported，不可伪称 safe apply。root loss/identity change 应暂停或报错；重选 root 后只能 manual preview/Unknown safe union，不自动覆盖、删除或迁移新目录。

## 4. 连接秘密与 vault

vault key 仅在进程 session 中持有并用 Zeroizing 清理（best effort）；SQLite 保存 AEAD ciphertext，不保存明文秘密。repository master 是独立于 device/vault key 的随机 32-byte key，包装于 repository password 派生的 Argon2id key；password 丢失无法恢复。index key 必须由 vault secret 提供，不能由设备指纹或 repository password 替代。密钥、session token、proxy auth 不应进入 debug output；redacted configuration 只暴露 `has_credentials`/`has_proxy_auth` 等状态。

## 5. 用户必须看到的隐私与数据损失边界

Encrypted repository 隐藏 block/tree/commit 的明文内容（加密 envelope），但不能隐藏所有侧信道：object size、timing、访问次数/count、block equality、prefix、网络模式等仍可能敏感。Plain mode 的内容和 metadata 风险更高；mode 不能原地翻转，必须另建/迁移设计（当前未实现迁移）。

Delete association 不是 remote DELETE。tombstone/history 是逻辑删除，不释放远端空间；恢复可能需要扫描、下载、验证、重建，且不能承诺 snapshot-consistent old handles。lost password 无法 recover。不能自动 GC，也不能向用户承诺永久保留、零泄露或即时恢复。

## 6. Source-backed tests（local fixture）

当前 source 中存在并可作为行为证据的连接/仓库测试名称（均为 local fixture/unit/in-process，不是 AWS/R2 通过证明）：

- `src-tauri/src/cloud/proxy.rs`：`http_proxy_authentication_is_sent_over_real_request`、`socks5h_authentication_sends_unresolvable_domain_to_proxy`（以及该模块后续的 proxy validation tests）。
- `src-tauri/tests/cloud_repository_crypto.rs`：`wrap_round_trip_and_aad_rejection`、`metadata_blocks_and_head_reject_tampering_and_cross_purpose`、`kdf_and_descriptor_limits_reject_before_expensive_derivation`、`canonical_wire_rejects_unknown_and_duplicate_fields`。
- `src-tauri/tests/cloud_repository_flow.rs`：`encrypted_repeat_uses_one_put_and_hides_plaintext_metadata`、`plain_round_trip_and_unknown_objects_are_rejected`。
- `src-tauri/tests/cloud_publish_flow.rs`：`empty_valid_snapshot_publishes_generation_one_and_journals`、`cancellation_does_not_advance_head`、`two_publishers_merge_disjoint_entities`（文件后部还有 transport-loss/CAS 场景，需以 source 当前名称为准）。
- `src-tauri/tests/cloud_sandbox_flow.rs`：`held_parent_moved_outside_cannot_receive_staged_file`、`held_parent_moved_outside_cannot_remove_original_into_recovery`、`normal_cross_directory_rename_and_parent_thread_remain_usable`。

这些是 local fixture/unit/in-process tests，不是 AWS、R2 或任意真实远端 provider 的通过证明；integration progress 必须明确标为 undelivered/pending。父任务已运行整体验证，具体范围与尚未验证项目见下节。

## 7. 当前实现、操作路径与验证边界

runtime commands 已接入并由 UI 使用；这不等于所有端到端验收条件已完成。用户操作顺序为：**连接 → 测试连接 → 创建或加入仓库 → 映射目录 → 首次预览 → 同步 → 调度**。若同步期间根目录消失或身份改变，运行会暂停；必须人工重新选择根目录并再次预览后才能继续，不能自动迁移、覆盖或删除新目录。

恢复区内容不会由运行时自动删除；当前目录 recovery restore 仍为 `restoreUnsupported`，UI 必须说明需要人工取回，不能声称“恢复全部”。历史对象没有自动 GC，恢复会继续占用远端空间；丢失 repository password 无法恢复。Plain/encrypted mode 是 immutable，不能原地切换。

最终本地回归共 344 项 Rust 测试通过，包含新增 12 项分层 tree 测试和 13 项 publisher 测试；前端 19 个文件、292 项测试及 `pnpm build` 通过。格式、全部目标离线编译和差异检查通过，无新增 Rust 警告。单次全量 Rust 命令达到工具 600 秒时限后，将 64 MiB 慢用例拆出单独执行：其余回归全部通过，慢用例单独通过（约 364 秒），并非忽略该测试。完整命令与分层清单验证证据见 `cloud-repository-v1.md` 第 8 节。前端构建仍提示主 bundle 超过 500 kB。

已覆盖 `active_intent`/`source_scan` 恢复协调：CAS 后取消不推进基线；同一操作重启不增加 generation；恢复期间新写入不会被删除/覆盖；安装已完成但日志未落盘时核验目标与原件。根目录移出后恢复同 inode 仍暂停，重新选择并确认人工预览后恢复资格。64 MiB 文件中间插入 4 KiB 的应用阶段下载量低于原文件的 25%，复用的本地块重新验证内容地址；该调试构建集成用例约需 6 分钟。发布前依赖完整性验证仍可能读取远端块，这个下载量断言不包括发布验证流量，不能据此承诺整个同步网络量低于 25%。

仍未执行独立 S3 服务（如 MinIO）签名验证、真实 AWS/R2、400px 原生视觉检查及所有阶段磁盘满/进程硬终止矩阵。Firefox 可执行文件存在，但浏览器自动化依赖未安装；运行环境也未安装本地 S3 服务或容器运行器。局部故障测试不等于完整故障矩阵验收。

## 8. Gaps / pending

- AWS/R2/其他真实 S3 provider endpoint 的 end-to-end capability、TLS、proxy 和 CAS 结果：pending，不能声称通过。
- no-provider 实测、400px UI 视觉检查、磁盘满下所有阶段故障注入：尚未验证。
- `active_intent`/`source_scan` 重启协调已有故障回归；完整的硬终止/重启矩阵仍未覆盖，不得用局部测试替代完整声明。
- FastCDC v2020 流式 chunker、固定 profile、buffer-independent boundary/reuse tests：已由 source 覆盖；descriptor 校验不应被误写成“没有 chunker”。
- 子目录树、nested empty directories 的父 materialization：已有 local sync 测试（包括 nested empty directory），但不扩大为真实 provider 或完整扫描保证；root migration 不做。
- missing root/identity change 后 pause/reselect manual state 已有测试；恢复和新目录迁移不承诺。
- Windows/macOS safe apply：明确 unsupported；Linux 仅接受 Landlock ABI v3、`FullyEnforced` 与 `no_new_privs`，不 fallback。
- remote DELETE/自动 GC：明确非目标；tombstone/history 不释放对象空间。
