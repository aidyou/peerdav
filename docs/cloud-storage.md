# Cloud Storage / S3 连接说明（当前核对稿）

> **文档状态：当前协议与验证边界。** 本文只描述当前 source 已实现或明确拒绝的边界，不把未执行的真实 provider、硬终止/磁盘满矩阵或 UI 检查写成已通过，也不作 provider 性能、可用性或兼容性营销承诺。

## 0. 当前双协议与界面（2026-10-31）

用户已批准双协议扩展及两种 append 内容模式；生产已有对应 runtime、命令与 UI 路径，不再是 U-1 未实施方案。新建默认 `cas_v1`；只有用户显式选择 `append_v2` 才进入追加式，不因条件写失败自动降级或迁移。CAS 的 `plain/encrypted` 与 append 的 `authenticated_plain/encrypted` 是内容维度，不是协议开关。append 两种模式都需要密码包装的独立 master；authenticated_plain 内容可读但认证，不提供保密性，模式不能原地转换。

四个新命令是 `cloud_append_discover`、`cloud_append_create`、`cloud_append_join`、`cloud_append_save_mapping`，由前端统一 invoke 包装调用。append 发现、普通打开与加入不写 CAS 探测；加入必须显式选择 anchor，并使用可信指纹或明确接受 TOFU（不声称抵御首次替换）。映射还须显式选择已认证逻辑根，空仓库显式命名根；不能按 LIST 顺序、密码可解或时间戳选赢家。vault schema 保持 `2`；旧配置缺 protocol 默认 CAS，缺 append binding 不自动转 append，仅保证新读取器兼容旧配置。

普通对象浏览独立于同步设置，显示原始对象，不解密同步仓库。保护未知默认只读；明确物理身份并完成保护发现后也仅允许尝试普通上传/下载/删除，最终由后端保护判定。CAS 与 append 同步区域禁止普通上传、覆盖、删除，普通对象删除能力不等于自动 GC 或仓库 remote DELETE。

append 持久索引保留 pending/已完成 apply 恢复档案，恢复列表/恢复命令可读；安全续作须核验原持久意图、根身份和磁盘证据，不覆盖续作期间的新本地修改。恢复目标必须在根内、为安全相对路径且尚不存在；目录 restore 不据此宣称支持。完整硬终止/磁盘满矩阵仍未完成。

## 1. S3-only 与 endpoint

当前连接模型是 S3-compatible object store：配置结构 `S3Config` 的实际字段为 `endpoint,bucket,region,prefix,path_style,access_key,secret_key,session_token,allow_insecure_http,proxy`。endpoint 必须由用户显式提供；source 不提供隐式 provider endpoint，也没有证据证明 AWS 或 Cloudflare R2 已通过真实 integration。不要声称 “AWS/R2 supported/passed”。

**CAS v1** 依赖 object store 的 create-only PUT、conditional replace/CAS、range read、已验证 conditional writes 四项能力；缺一即 `Unsupported`，不降级成普通 overwrite。对象 key 必须位于配置 prefix；HEAD/CAS 是 `control/HEAD`，数据对象是 `blocks/`、`trees/`、`commits/`。CAS 创建时 prefix 必须为空（允许的 owned probe 除外）。**AppendV2** 使用普通 PUT/GET/完整分页 LIST、不可变 anchor 和独立 `v2/` 命名空间，不要求 CAS/etag/Range；仍要求可完整发布对象、耐久 GET 和稳定 locator，不能将免 CAS 门槛说成任意提供商已兼容。

S3/对象存储不是远端文件系统：上传的是独立 block 和分层、有界 tree/commit metadata，不是完整文件的 multipart overwrite。metadata/tree object 明文最多 32 MiB，单次遍历总量最多 512 MiB；CAS 目录树采用 `TreeDto` v1 的 `directory`/`recovery` typed wire 结构，旧 flat v1 不迁移；append 格式独立，见 `cloud-append-v2.md`。当前没有 remote DELETE 用于 association delete，也没有自动 GC；这是本计划明确的非目标，不作为未完成 gap。logical tombstone/history 不释放对象空间。

## 2. HTTP、HTTPS、proxy 与 fail-closed

`ProxyMode` 实际有三种：`direct`、`environment`、`custom`。custom URL 接受 HTTP、HTTPS、SOCKS5；输入 `socks5` 会规范化为 `socks5h`。SOCKS5h 通过代理发送域名，remote DNS 行为由代理完成；源码测试使用 `unresolvable.invalid` 验证域名传给代理。HTTP/HTTPS proxy 可带认证，日志/debug 不应泄露 credentials。

transport 默认 `.no_proxy()`、禁止 redirect、connect timeout 15 秒、request timeout 120 秒；不自动继承系统/PAC/fallback route。Environment 模式仅明确读取 `ALL_PROXY/all_proxy`、`HTTP_PROXY/http_proxy`、`HTTPS_PROXY/https_proxy` 和 `NO_PROXY/no_proxy`，无有效值时不凭空增加 route。Custom 模式只使用明确 URL。无代理 direct 就是 direct，连接失败应 fail closed，不静默改用另一条网络路径。

endpoint 的 HTTP 明文仅在 `allow_insecure_http` 明确开启时才可能被配置层接受；文档不应把它描述为安全传输。HTTPS/TLS 与 proxy 证书/网络错误都应阻止操作而非绕过验证。`path_style` 是显式 S3 config 字段，不推断成 provider 成功证明。

## 3. 能力探测与安全写入

`StorageCapabilities` 四个布尔能力为 `create_only`、`conditional_replace`、`range_read`、`conditional_writes_verified`；四者全 true 是 **CAS repository** 的门槛，不是 append 或普通只读浏览的共同门槛。CAS 能力不足是 pending/unsupported，不 fallback rename、非条件 HEAD 写或“尽力而为”同步。Append 普通打开/发现不隐式写探测，也不全局删除 CAS 校验；当前没有完成真实 provider integration 验证。

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

runtime commands 已接入并由 UI 使用；这不等于所有端到端验收条件已完成。CAS 用户操作顺序为：**连接 → 测试连接 → 创建或加入仓库 → 映射目录 → 首次预览 → 同步 → 调度**；append 则显式选择协议后发现/创建/加入，不以 CAS 写探测为前置，再显式选根映射、预览和同步。若同步期间根目录消失或身份改变，运行会暂停；必须人工重新选择根目录并再次预览后才能继续，不能自动迁移、覆盖或删除新目录。

恢复区内容不会由运行时自动删除；当前目录 recovery restore 仍为 `restoreUnsupported`，UI 必须说明需要人工取回，不能声称“恢复全部”。历史对象没有自动 GC，恢复会继续占用远端空间；丢失 repository password 无法恢复。Plain/encrypted mode 是 immutable，不能原地切换。

**历史验证（不是本轮总数）：** 先前记录 344 项 Rust、前端 19 文件/292 tests、build 及格式/离线编译/差异检查通过，慢项拆分约 364 秒；这些只保留作历史证据。

**当前验证（2026-10-31，父任务/代理分批报告，本文编辑未重跑）：** 后端 81 项专项及 24 项 runtime/schedule/manager 通过，双模式 HTTP runtime fixture 端到端 236.71 秒通过；CAS 44 项与独立慢项 1 项（406.08 秒）通过。此前 append/object 所列 `6+32+17+17+6+4+17+70+21+10+2 = 202`，分项合计确为 202；最终专项 discovery 35、engine 19、index 17、publish 6、recovery 6、contract 6、repository 17，以及 object `70+21+10+2` 均通过。全部 35 个集成 test targets 已分批执行，剩余 13 targets 实质 124 项通过，另有外部默认空跑 1 项、七牛 ignored 1 项；bin/doc 为 0 项。lib 在最终生产修改后重跑，166 项全部通过，最终预算修改后受影响组均重跑通过。各批次时间不同，不猜统一总数；两次单命令 cargo test 600 秒超时不是目标未尝试，分批已覆盖全部目标，不表示单命令全量成功或全部测试在最终修改后统一重跑。`cargo fmt --check`、`cargo check --all-targets --offline` 通过，前端全部 21 文件/346 tests 与 build 通过，主 bundle 大 chunk 警告仍在。

资源专项使用真实 `65 MiB + 1 byte` 文件，最大 part 8 MiB、peak buffer 8 MiB、inflight 1，测试 4.33 秒；`/proc` VmHWM 65364 kB（约 63.83 MiB），不是 GNU time 测量或 RSS 硬限证明。最终预算机制已修复：discovery/apply/freeze 先持久记账 stat 请求，再按声明长度持久预留 GET 请求与 read 字节，之后 bounded stream；增大超声明长度立即拒绝，缩短保守计费不退款。S3 stat 为对象 HEAD，追加式无 CAS 探测、不读写 `control/HEAD`，允许且计费对象 HEAD；publisher 按冻结已知长度预留，无需 HEAD。持久 active unfinished run 使外层新 run id 自动重试不能刷新账本，成功才新轮。当前细项与限制见 `cloud-append-v2.md` 页首。最终 `git diff --check` 成功；仍保留工具 backend/worktree 差异限制，untracked 文件不能由普通 `git diff` 展示，须另核对内容。

已覆盖 `active_intent`/`source_scan` 恢复协调：CAS 后取消不推进基线；同一操作重启不增加 generation；恢复期间新写入不会被删除/覆盖；安装已完成但日志未落盘时核验目标与原件。根目录移出后恢复同 inode 仍暂停，重新选择并确认人工预览后恢复资格。64 MiB 文件中间插入 4 KiB 的应用阶段下载量低于原文件的 25%，复用的本地块重新验证内容地址；该调试构建集成用例约需 6 分钟。发布前依赖完整性验证仍可能读取远端块，这个下载量断言不包括发布验证流量，不能据此承诺整个同步网络量低于 25%。

仍未执行独立 S3 服务（如 MinIO）签名验证、真实 AWS/R2、400px 原生视觉检查及所有阶段磁盘满/进程硬终止矩阵。Firefox 可执行文件存在，但浏览器自动化依赖未安装；运行环境也未安装本地 S3 服务或容器运行器。局部故障测试不等于完整故障矩阵验收。

## 8. Gaps / pending

- AWS/R2/其他真实 S3 provider endpoint 的 end-to-end capability、TLS、proxy 和 CAS 结果：pending，不能声称通过。
- 桌面/400px UI 视觉尚未实际渲染；资源专项已有 `/proc` VmHWM 观测，256 MiB 仅为计量分配预算，不能声称 RSS 硬限。SIGKILL/掉电/磁盘满全矩阵仍未完成，future drop/独立 SQLite 重开不等于实测断电。
- no-provider/独立服务签名实测及磁盘满下所有阶段故障注入：尚未验证。
- `active_intent`/`source_scan` 重启协调已有故障回归；完整的硬终止/重启矩阵仍未覆盖，不得用局部测试替代完整声明。
- FastCDC v2020 流式 chunker、固定 profile、buffer-independent boundary/reuse tests：已由 source 覆盖；descriptor 校验不应被误写成“没有 chunker”。
- 子目录树、nested empty directories 的父 materialization：已有 local sync 测试（包括 nested empty directory），但不扩大为真实 provider 或完整扫描保证；root migration 不做。
- missing root/identity change 后 pause/reselect manual state 已有测试；恢复和新目录迁移不承诺。
- Windows/macOS safe apply：明确 unsupported；Linux 仅接受 Landlock ABI v3、`FullyEnforced` 与 `no_new_privs`，不 fallback。
- remote DELETE/自动 GC：明确非目标；tombstone/history 不释放对象空间。
