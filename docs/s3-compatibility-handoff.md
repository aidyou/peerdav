# PeerDAV S3 兼容扩展：新任务交接

> **醒目当前更新（2026-10-31）：下方 2026-10-30 交接全文是历史快照，不是当前实施状态或当前授权边界。** 后续用户已批准双协议扩展：新建默认 `cas_v1`，`append_v2` 显式选择；保留 CAS 可读写及能力门槛，不自动降级、迁移或切为只读。`encrypted` 与 `authenticated_plain` 均已批准且有生产路径，两者都需要密码；原文“未授权实施”“plain 待批准”“尚未完成实施单位”等只描述历史，不可作为今日结论。
>
> 当前 AppendV2 已有 repository/索引/publisher/engine/runtime 与前端入口；四个新命令为 `cloud_append_discover`、`cloud_append_create`、`cloud_append_join`、`cloud_append_save_mapping`。加入需显式选择 anchor、可信指纹或明确 TOFU；映射需显式选认证逻辑根，空仓库显式命名根。vault schema 仍为 `2`，旧配置缺 protocol 默认 CAS，仅保证新读旧兼容。恢复档案与校验持久证据后安全续作已接入；不能把局部恢复测试扩大为完整硬终止矩阵。普通对象浏览与同步独立：原始对象不解密，保护未知默认只读，后端拒绝普通变更触及同步区；普通对象删除不是仓库 GC。
>
> **当前证据（父任务/代理分批报告，非本文编辑重跑）：** 后端 81 项专项及 24 项 runtime/schedule/manager、双模式 HTTP runtime fixture 端到端（236.71 秒）、CAS 44 项及独立慢项 1 项（406.08 秒）通过。此前 append/object 所列 `6+32+17+17+6+4+17+70+21+10+2 = 202`，分项合计确为 202；最终专项 discovery 35、engine 19、index 17、publish 6、recovery 6、contract 6、repository 17，以及 object `70+21+10+2` 均通过。各批次时间不同，不猜统一总数。全部 35 个集成 test targets 已分批执行；剩余 13 targets 实质 124 项通过，另有外部默认空跑 1 项、七牛 ignored 1 项；bin/doc 为 0 项。lib 在最终生产修改后重跑，166 项全部通过；最终预算修改后受影响组均已重跑通过。两次单命令 cargo test 600 秒超时不是目标未尝试，分批全目标已覆盖，不表示单命令全量成功或全部测试在最终修改后统一重跑。Rust fmt/check all-targets offline 通过，前端 21 文件/346 tests 与 build 通过，仍有大 chunk 警告。
>
> **资源与预算当前证据：** 真实 `65 MiB + 1 byte` 文件，最大 part 8 MiB、peak buffer 8 MiB、inflight 1，测试 4.33 秒；`/proc` VmHWM 65364 kB（约 63.83 MiB），不是 GNU time 或 RSS 硬限证明，256 MiB 仍为计量分配预算。discovery/apply/freeze 统一先持久记账 stat 请求，再按声明长度持久预留 GET 请求与 read 字节，之后 bounded stream；增大超声明长度立即拒绝，缩短保守计费不退款。S3 stat 为对象 HEAD：无 CAS 探测、不读写 `control/HEAD`，允许且计费对象 HEAD；publisher 按冻结已知长度预留，无需 HEAD。持久 active unfinished run 使外层新 run id 自动重试不能刷新账本，成功才新轮。
>
> **当前未验证：** 桌面及 400px 未实渲染；SIGKILL/掉电/磁盘满全矩阵、独立服务签名及真实厂商仍未测，本轮不读取凭据；future drop/独立 SQLite 重开不等于实测断电。最终 `git diff --check` 成功；仍保留工具 backend/worktree 差异限制，untracked 文件不能由普通 `git diff` 展示，须另核对内容。当前合同与局限见 `cloud-append-v2.md` 页首和 `cloud-storage.md`；生产可达不等于所有合同/厂商验收通过。

---

## 历史原文（2026-10-30，保留，不作为当前状态）

> 整理日期：2026-10-30。
> 本文是任务上下文、设计方向及未完成事项的交接，不是追加式协议实现或验收报告。
> 最重要的约束：**新增兼容能力，不推翻现有条件读写/CAS 路径。**

## 1. 用户背景与目标

PeerDAV 原设计通过 S3 读写对象，仓库初始化和发布依赖服务端正确实现条件写，包括 create-only PUT 与 HEAD CAS。测试七牛对象存储时，发现测试 endpoint 不满足条件写要求：历史实测中，重复带 `If-None-Match: *` 的创建仍返回 HTTP 200。因此该 endpoint 不能按现有协议的安全门槛直接使用；这不是对所有七牛产品、配置或所有 S3 能力的泛化结论。

**S3 外部验证尚不完整：截至本次交接，仅拿到了七牛可测试的连接信息，尚未完成 S3 协议及现有 CAS 路径的独立、完整端到端兼容性验收。** 目前的七牛实测只提供了条件写不满足要求的证据，不能理解为七牛其余读写、签名、分页、错误语义及故障恢复已全部测完；本地 fixture 测试也不能替代真实 S3 验收。没有真实 AWS/R2 或其他供应商的完整通过证据。

用户目前无法申请 Cloudflare R2，信用卡不在身边。不要把获取 R2 账号作为设计、fake-store 实现或本地测试的前置条件，也不要声称已验证 R2。

更长期的目标是扩大协议兼容范围：既兼容国内厂商各有差异的 S3 接口，也为 Google Drive、百度网盘等非 S3 适配留出空间。**本阶段不等于实现这些网盘适配器，更不能提前宣称它们已兼容。**

用户明确要求：

> 新的方案只是拓展，不是推翻支持条件读写的。

本轮用户要求整理交接文档，准备重开任务。本轮没有授权实施整个新协议；后续应先确定修订后的执行合同，再实施。

## 2. 已确定的方向与必须澄清的批准边界

专家推荐：A「追加式因果全快照」+ 明文块身份与密文物理 locator 分离。B「追加 delta 日志 + 不可变检查点」暂不实现。

上一轮已给出以下方案判断，但不能把判断当作新协议已实现，或把专家原稿整份当作用户逐项批准的实施计划：

- 保留现有 CAS 仓库的读写能力及安全门槛。
- 新增独立格式/命名空间的追加式仓库，使用普通 PUT/GET/全分页 LIST，不依赖 CAS。
- 同一仓库只能有明确一致的发布规则；不得自动迁移、降级或混用。
- CAS 不可用时暂停并展示真实错误，不能变成普通 HEAD overwrite，也不能悄悄改走追加式。
- 发布协议（CAS/追加式）与内容模式（plain/encrypted）是不同维度，不能混为一个开关。
- 先用可控 fake store 验证状态机与故障，再接真实适配器。真实远端写入/探测另需授权。

**专家原稿与用户约束冲突的三处，后续必须修正：**

| 专家原稿 | 应采用的边界 |
| --- | --- |
| “协议替换决策” | 新增发布协议的兼容扩展，不替换已有 CAS 支持。 |
| “v1 不写入/可选只读” | 现有 CAS 仓库继续可读写；不自动迁移、不强制只读。 |
| runtime 各路径一起“切换” | create/join/open/connect 等路径统一按明确协议分流；仅追加式免除 CAS 门禁，不全局删除 CAS 校验。 |

待新任务明确的产品/契约决策：新建仓库的默认协议、如何展示格式与创建选择、plain 模式在追加式中的支持范围、首次 TOFU/手工指纹体验、能力缓存有效期与探测预算。不能由“推荐 A”推导出这些决策已经批准。

## 3. 已有实现与证据等级

### 3.1 已完成的历史工作

历史任务完成了现有 CAS 路径的分层目录清单与发布边界改造，包含：

- 逐目录不可变 manifest、认证读取、未变目录对象复用。
- publisher/engine/cloud runtime 的现有接入。
- 借用式清单序列化、取消错误映射及局部故障测试。
- 现有 B/L/R 合并、revision 因果关系、tombstone、安全 apply 与物化确认机制。
- 前端云配置入口及相关错误消费、双语文案。

这些是历史完成项，不应在新任务无理由重写。它们不代表追加式协议已经交付。

### 3.2 历史验证结果（本轮未重跑）

已有文档记录：344 项 Rust 测试、前端 19 个文件/292 项测试、`pnpm build`、格式、全部目标离线编译及差异检查通过。Rust 单次全量命令曾达到 600 秒工具时限，64 MiB 慢用例拆分单跑通过，非跳过；前端构建仍有主 bundle 超过 500 kB 的提示。

详情入口：

- `docs/cloud-repository-v1.md`：现有协议、大小限制、密码学与测试证据；第 8 节记录验证。
- `docs/cloud-storage.md`：连接、能力门槛、操作路径、验证边界及待验收项。

上述数量只能作为历史证据，不能声称是重开任务后的当前测试结果。独立 S3 服务签名验证、真实 AWS/R2、400px 原生视觉、完整磁盘满/进程硬终止矩阵仍未完成。七牛条件写反例也不等于追加式在七牛已通过。

### 3.3 本轮实际检查

本轮仅核对完整专家原文、工作区状态与上述两份文档的相关内容，创建本文；没有修改生产代码、运行构建/测试、读取凭据或访问远端。

当前工作区存在大量原有未提交修改和未跟踪文件：`src-tauri/src/cloud/`、`src-tauri/src/sync/`、大量 `cloud_*` 测试以及 `docs/` 等均可能未被 Git 跟踪。**未跟踪不等于可删除或可覆盖。** 新任务必须检查目标文件当前内容与差异，保留全部基线工作；不要 reset/clean/stash 或覆盖用户改动。

## 4. 推荐追加式方案的核心合同

### 4.1 初始化与格式隔离

- 使用独立命名空间，例如 `v2/<repo_id>/`；“v2”是拟议协议标识，不能仅凭路径猜测格式或自动升级。
- 随机 repo_id、独立 master、不可变 password envelope/descriptor；anchor 使用唯一键，并保存 anchor wire 指纹。
- 本地持久保存选择的 repo_id、anchor locator、指纹。已有可信指纹必须校验。
- LIST 仅用于发现，创建前 LIST 为空不是互斥锁，也不是不存在仓库的证明。
- 同前缀并发创建多个根时都保留，明确展示候选并让用户选择；禁止按时间、LIST 顺序或密码能否解开自动选赢家。
- 错密码、认证失败、anchor 暂不可见不得初始化空仓库。首次无带外指纹只能明确告知为 TOFU，不声称抵御恶意替换。

### 4.2 块身份与物理 locator

推荐形态：`BlockRefV2 { block_id, len, object_digest }`，具体 DTO 与序列化待设计冻结。

- block_id 保留基于仓库密钥的明文 HMAC 身份；物理对象按密文摘要定位。
- 每个对象随机加密一次，冻结 wire bytes；同键只能同字节重试。
- 下载先验证密文摘要，再验 AEAD、block_id、len；不通过确定性 nonce 或自制确定性加密省对象。
- 从认证 manifest 建立 `block_id → 已验证 locator` 去重索引，不设置共享可变去重表。
- 第二设备发现并验证已有引用后可复用；彼此尚不可见时，首次并发上传同块允许产生多个物理对象。不能承诺首次并发绝对零重复。

### 4.3 提交、发现与闭包验证

- 每次 commit 引用完整因果 MergeSet 的目录树根；“全快照”不是每次重传全文件/全目录，未变目录继续复用。
- commit 键含 operation UUID 与密文摘要；引用保存精确 locator+摘要。加密内容含 version/repo_id/operation_id/parents/root_tree；parents 排序去重且可精确解析。
- 键不暴露设备名、文件名、父关系。UUID/摘要不代替 AEAD；共享 master 不提供设备不可抵赖签名。
- 流式读完全部分页，将验证通过的提交与持久 known 集合取单调并集；LIST 遗漏不能删除 known，也不能视作空仓库。
- 父提交按引用精确 GET，无须先出现在 LIST 中。缺父/树/块进入 PendingDependency，不应用半成品；区分暂缺、认证失败、网络错误。
- 在预算内校验 DAG 闭包、环/深度、格式/仓库身份、树块认证、revision 因果图。
- **子快照必须包含全部父历史，且同 revision ID 同内容。** 不能仅凭 parents 声明丢弃祖先，也不能借“自包含”跳过父闭包验证。
- 验证后的远端 R 是所有已知有效快照的 join；时间戳不决定赢家。

### 4.4 本地变化、发布与恢复

- B 是已物化并持久确认的基线；L 是完整本地扫描；R 是认证远端已知并集。
- 本地修改的因果父来自 B，不把尚未物化的新 R 当作用户已观察历史。
- 删除只对 B 已存在且完整 L 确认缺失的实体产生 tombstone；Unknown、索引丢失、扫描不完整/失败均不得推断删除。
- 任意 PUT 前持久 staging：operation UUID、固定父 frontier、对象 locator/digest、冻结密文字节、扫描/变更意图。
- 按 blocks → trees → commit 发布；commit 是唯一发布标志。
- 同 operation 不换树、父、nonce 或字节。后续发现新提交应另建 operation，而不是修改冻结操作。
- commit PUT 附近响应丢失/取消进入 PublishedUnknown/PendingRecovery，通过持久的精确引用 GET 验证，必要时重传同字节。LIST 缺席不证明未发布。
- known、validated frontier、materialized baseline、staging、apply journal 分层持久化；完整 apply 与确认事务完成前不推进基线。
- generation 不再具有追加式全局序号语义；结果和基线须定义因果 frontier/本地标识，同时保留 CAS 调用方的既有合同。
- 索引按记录拆分，不把增长历史塞进一个 `materialized` 记录。稳态无变化不发布空 commit，仅拉取物化不强制发 merge commit，避免风暴。

## 5. 不可违反的不变量与适配器边界

1. 旧物理引用字节身份不变：普通 PUT 只用于新唯一键或同键同字节重试；不宣称服务端 WORM。
2. known 单调增长，远端遗漏不触发历史回退或删除。全新设备/全丢索引不能仅凭恶意 LIST 检出从未见过的隐藏提交，必须说明此限制。
3. 删除只支配已观察父版本，要求完整扫描和已知已物化 B。
4. 仅应用完整验证闭包；pending、损坏、资源超限不做破坏性应用、不推进基线。
5. operation 字节与意图冻结；未知发布结果可恢复，不能伪装取消成功。
6. 无自动 DELETE/GC、迁移、降级、选根；CAS 与追加式不混用发布规则。

追加式最低需要完整对象发布、耐久 GET、完整分页 LIST、唯一持久 locator；不要求 etag/CAS/Range。供应商不具备这些最低保证时不能宣称支持。

未来云盘可能同名多对象，不能假设“同名就是覆盖”。适配器必须保存稳定 opaque object ID、列出全部同名项并按摘要解析引用，禁止任选第一项。本阶段只设计边界，不实现 Google Drive/百度网盘。

能力状态建议 Supported/Unsupported/Unknown；权限/网络错误保留真实错误，不误报 Unsupported。追加式普通打开不得隐式执行 CAS 写探测；CAS 路径仍保留必要验证。将来授权的写探测应限定随机独立前缀、持久频率/每日请求预算且不 DELETE；不要继承旧远端授权自动开始测试。

保留单 manifest 32 MiB、单树明文总量 512 MiB、单索引记录 32 MiB 的现有边界。专家指出现有 MAX_FRONTIER=64、MAX_PARENTS=64、MAX_REVISIONS=200000；实施前以当前源码复核。新增 DAG 节点/深度、known/frontier、总认证读取字节/请求、缓冲/峰值内存预算。512 MiB 序列化预算不等于 RSS 上限。超限必须结构化报错，不截断历史伪装成功。分片/检查点/GC 另案，不承诺无限增长。

## 6. 接续工作与建议实施顺序（尚未完成）

以下是待细化和批准的实施单位，不是已完成清单：

1. **修订双协议合同与创建/加入体验。** 冻结格式标识、能力缓存/探测、anchor 选择、TOFU、plain/encrypted 兼容范围；写明 AC/INV 与停止条件，保留 CAS 可读写。
2. **先实现 fake-store 状态机与故障验收。** 定义 v2 DTO、locator、结构化错误、预算、pending/unknown recovery；用可控对象可见性与请求记录建立回归，再接真实适配器。
3. **repository/tree/crypto 路径。** 不可变 anchor、物理块定位、冻结对象准备/读写、BlockRef 格式隔离与认证目录复用，不破坏现有 CAS wire。
4. **publisher/index。** 分页发现、单调 known、父闭包与历史包含验证、追加发布、精确恢复、分记录 staging/frontier。
5. **engine/runtime/命令/前端。** B/L/R、pending 与 apply 恢复、结果与基线合同、按协议分流 create/join/open/connect、统一错误和后台出口、中文/英文 UI。不能只改 publisher 而遗漏 runtime 条件写门禁。
6. **回归与独立兼容性验收。** 证明 CAS 既有读写不回退；fake store 通过不等于真实 S3/网盘通过。独立服务或真实厂商验证需具备环境与明确授权。

### 源码导航入口

路径均相对项目根目录；专家旧行号可能已漂移，不当作当前精确位置。

- `src-tauri/src/sync/repository.rs` / `repository_types.rs`：create/join/HEAD、对象读写和格式。
- `sync/crypto.rs`、`sync/types.rs`、`sync/chunker.rs`：HMAC/AEAD、BlockRef、CDC。
- `sync/tree.rs` / `tree_types.rs`：逐目录清单、认证加载与复用。
- `sync/publisher.rs` / `publisher_types.rs`：依赖、合并、HEAD CAS、operation 恢复。
- `sync/merge.rs` / `merge_types.rs`：revision、集合 join、tombstone、Unknown 删除保护。
- `sync/index.rs` / `index_types.rs`：物化事务、日志、单记录限制。
- `sync/engine.rs` / `engine_types.rs`、`apply.rs`、`sandbox.rs`：B/L/R、安全物化与恢复。
- `src-tauri/src/cloud/{traits,types,s3,runtime}.rs` 及 runtime 拆分文件：分页、能力、连接/创建/加入/打开门禁。
- `src-tauri/src/commands.rs`、`src-tauri/src/error.rs`、`src/components/CloudSettings.vue`、`src/lib/{cloud,invoke}.js`、`src/locales/{zh-CN,en}.js`：命令、错误及 UI 合同。
- `src-tauri/tests/cloud_*`、`src/__tests__/cloud-settings.test.js`：既有测试入口。

当前源码是权威。专家此前发现 CodeGraph 索引缺目标文件、误命中其他符号，因此当时停止使用；重开任务若结构化索引不可用或陈旧，应使用可用的原生只读导航，不能据图推断不存在调用。

遵守项目指导：生产 Rust 不新增 unwrap/expect/panic 路径、未使用警告或死码；类型/常量/enum 放 types，特征放 traits，mod.rs 仅模块与再导出；命令返回结构化 AppError，保留原始 source/detail/context；前端统一 invoke 包装、按 code 分支；后台错误统一日志/事件出口并监控任务生命周期。

## 7. 必须兑现的验收矩阵

- **发现/分页：** 延迟 LIST、遗漏已知项、GET 依赖滞后、分页重复/中途错误、同名 opaque ID；全部最终可见且停止新写后收敛，不承诺有限轮看全世界。
- **多设备因果：** 三设备编辑/删除/目录改名/冲突的不同交付排列；join 交换/结合/幂等；冲突路径与 revision ID 稳定，未观察编辑不被 tombstone 杀掉。
- **初始化：** 并发双根、错密码、错指纹、anchor 暂缺均不自动选根或造空库。
- **崩溃/未知发布：** staging、各对象 PUT、commit 成功但响应丢失、索引确认、apply 各阶段注入故障；同 operation 同 bytes，不重复逻辑变更、不复用 nonce 加密不同明文/AAD；已发布取消恢复为待应用。
- **恶意/损坏输入：** 缺父/块、环、同 ID 异内容、跨仓库重放、子快照删父历史、元数据篡改、分支爆量均 fail closed。
- **本地删除与基线：** Unknown B、索引丢失、读失败、部分扫描不产生删除；物化前失败不推进 baseline。
- **去重与复用：** 稳态数据 PUT=0；第二设备发现认证 locator 后重复块 PUT=0；首次并发多副本引用均可解密；CDC 小改仅上传新增块；未变目录不重传。
- **边界/调用记录：** 预算边界、峰值内存/请求计数；追加式无条件 PUT、无 DELETE、无隐式探测；CAS 继续正常条件读写且不自动降级；无新增 Rust warning/panic 路径。
- **独立提供商：** fake store 不构成外部验收。七牛追加式、独立 S3 签名/分页/耐久性及未来云盘各需单独证据；不用 R2 账户也可推进本地阶段。

停止条件：无法稳定定位对象、分页未完成、闭包未验证、anchor 歧义、预算超限或未授权迁移时，停止相应发布/应用，保留 staging 与原始错误上下文；不得“尽力同步”后推进基线。

远端安全要求：不读取或写入文档任何凭据，不回显密钥；不执行远端删除/自动回收；测试须独立前缀、请求/流量预算和明确授权。

## 8. 新任务可直接使用的开场说明

> 请先阅读 `docs/s3-compatibility-handoff.md`、项目指导及现有两份云存储协议文档，核对当前未提交代码；不要重置或覆盖现有工作。
>
> 目标是扩展 PeerDAV 的云存储兼容性：保留现有 CAS 仓库的条件读写和读写能力，新增不依赖条件写的追加式协议。倾向 A「追加式因果全快照」+ 密文摘要块 locator，B 暂不实施。原专家稿的“替换 v1/停止 v1 写入”不适用。
>
> 先收敛双协议格式、创建选择、信任锚、能力探测、内容模式及验收合同；未决产品/契约问题需明确确认。获准实施后先 fake-store 状态机与故障测试，再接适配器。不要因无法申请 R2 而阻塞本地工作，不要默认获得远端写入权限，不要把设计或局部测试说成真实厂商已兼容。
