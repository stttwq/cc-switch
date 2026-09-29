# CC Switch 2.3.2 后续优化与安全加固实施方案

- **状态**：施工中。**S0、S1、S2 已于 2026-09-29 完成**（SEC-B、SEC-C、DEL-A、OPT-A Claude/Codex generation、SEC-A），全量检查通过；S3–S5（REL-A、SEC-D、REL-B、OPT-A 后端预算、DOC-A）尚未施工。
- **审查日期**：2026-09-29。
- **代码基线**：`1password` 分支，`2a25d07118c7b087d6bd2618d4ca2877e3350320`，应用版本 `2.3.2`。
- **适用对象**：接手施工的模型、复核模型与维护者。
- **版本安排**：不预先绑定下一版本号；按阶段验收后由维护者决定发布。
- **定位规则**：下文行号属于上述基线；施工前按路径、函数名重新定位，不能机械套用行号。
- **变更约束**：不自动提交、推送、打标签、发布；不合并 main；不动原有未跟踪 `.zcodeignore`。真实保险箱、注册表、用户配置和远端同步数据不是测试夹具。

## 1. 结论与推荐顺序

当前不值得再重做一次凭据架构。2.3.2 已覆盖条目归属核验、危险 SQL schema 对象拒绝、普通编辑零 op、按需凭据更新、部分成功反馈和一部分发布门禁。后续重点应转向：

1. **不可信配置变为本机能力之前的授权边界**：尤其是 MCP 的命令和启用状态。
2. **携带凭据的网络请求边界**：重定向、自定义头、错误中的 URL。
3. **跨 SQLite/文件系统操作的中断恢复**：不仅处理返回 Err，还处理进程退出。
4. **有限资源与交互正确性**：op 排队预算、网络响应体上限、旧请求结果失效。
5. **发布可验证性**：签名必须覆盖实际下载的文件名和文件内容，备用发布也不能跳过。

推荐按 **S0 → S1 → S2 → S3 → S4 → S5** 施工。先修明确的边界与交付缺陷，再做渐进式加固，不进行“大一统框架”重构。

### 1.1 优先级与证据等级

- **P1**：下一轮优先解决；可能泄漏凭据、隐式启用可执行配置、产生跨资源不一致或破坏发布验证。
- **P2**：随后解决；可靠性、可维护性或纵深防御，不阻塞前面的小补丁。
- **静态确认**：已核实当前代码的数据流或行为。不等于已在真实机器/网络完成利用。
- **加固建议**：存在结构性弱点，但本轮没有证明可利用的攻击链。
- **待动态验收**：需要本地假服务、故障注入或隔离 Windows VM 测试。

本轮没有确认应直接标为“远程无交互 RCE”的 P0 漏洞。不要为了显得严重夸大威胁前提。

### 1.2 工作包总表

| 编号 | 工作包 | 优先级 / 性质 | 建议规模 | 依赖 |
|---|---|---|---|---|
| SEC-A | 导入 MCP 的本机审批与内容绑定 | P1 / 静态确认缺少边界 | 中 | S0 测试夹具 |
| SEC-B | 携凭据模型请求的重定向与目标限制 | P1 / 静态确认 | 小–中 | 无 |
| SEC-C | 网络错误脱敏与结构化错误 | P1 / 静态确认 | 小 | 可与 SEC-B 同批 |
| REL-A | DB + Skills + 后处理的中断恢复 | P1 / 静态确认中断窗口 | 中–大 | SEC-A 审批数据边界 |
| DEL-A | 发布文件名、签名、标签与门禁闭环 | P1 / 静态确认 | 小–中 | 无 |
| SEC-D | 导入到程序自建 schema 的数据归一化 | P2 / 纵深加固 | 中–大 | SEC-A、REL-A 边界已定义 |
| REL-B | op 排队预算、管道资源和失败清理 | P2 / 静态确认 + 加固 | 中 | 原 op 次数回归 |
| OPT-A | 模型获取的有限响应、总预算与旧响应隔离 | P2 / 静态确认 | 小–中 | SEC-B/C |
| DOC-A | 安全说明与历史计划状态对账 | P2 / 文档正确性 | 小 | 随各阶段更新 |

“规模”是相对改动面，不是工时承诺；SEC-D 和 REL-A 应拆小提交，不要求一次完成。

## 2. 审查范围、已有保障与限制

### 2.1 本轮直接核实的位置

- 导入与同步：`database/backup.rs`、`database/snapshot_policy.rs`、`commands/import_export.rs`、`commands/sync_support.rs`、`services/sync_protocol.rs`、`services/webdav_sync.rs`、`services/webdav_sync/archive.rs`。
- MCP 投影：`services/mcp.rs`、`services/provider/live.rs`。
- 网络：`services/http_client.rs`、`services/model_fetch.rs`、`commands/model_fetch.rs`、前端三类模型获取调用及错误处理。
- 1P 进程：`secrets/onepassword.rs::exec_op_with_timeout`。
- 交付：CI、云端 Release workflow、本地发布流程、安全说明、历史方案与 CHANGELOG。
- 依赖语义：本机 Cargo registry 中 **reqwest 0.12.28** 的重定向敏感头移除和错误 Display 实现；当前直接依赖为 `0.12`，锁文件含 `0.12.28`。另外出现 `0.13.2` 不代表本服务使用了它。

### 2.2 不重复施工的已有保障

以下已有代码/配置证据，不能再次把它们写成“完全缺失”：

- CSP 已存在，脚本默认限于打包来源和固定 hash；不能宣称“未配置 CSP”。见 `src-tauri/tauri.conf.json:28–33`。
- `ci.yml:5–9` 已覆盖 `1password` push；已有 RustSec 阻塞检查和 Windows 测试。不要重复旧方案 G-1。
- SQL 文本 authorizer 与导入 schema 审查已阻断 trigger/view 等对象，见 `backup.rs:388–409,794–838`。SEC-D 是更强的下一层边界，不是声称旧 trigger 修复失效。
- Skills 解压已有 entry 数和累计解压大小限制、`enclosed_name`，见 `archive.rs:92–133`。不要重复提一个泛化的“没有 zip bomb/路径穿越保护”。
- op 已在 spawn 前设置 deadline，并发读写管道，见 `onepassword.rs:875–926`。REL-B 处理的是排队预算、资源清理和剩余等待边界，不是重做已修复的 stdin/stdout 互等。
- Codex 和 Pi 表单已有请求序号/代际隔离；OPT-A 的确定性前端缺口在 Claude，不对三者一概重写。

### 2.3 验证边界

本轮是静态审查和方案编写：

- **未运行**项目全量测试、真实 op、真实远端同步、Windows 安装/卸载或线上依赖审计。
- 没有访问真实保险箱或读取发布私钥。
- 辅助审查服务因限流失败，没有返回结论；本方案依据随后直接读取的代码，不将失败代理当成独立复核。
- 不宣称覆盖全部仓库、不存在其他漏洞，或安装包已经通过下述新增测试。

## 3. 总体施工原则（强约束）

1. **先复现、再最小修复、最后泛化。** 每项先有失败测试；多个独立修复不捆成难以回滚的大提交。
2. **数据不是授权。** SQL/同步中的 `enabled`、命令、路径、来源说明和 item ID 都只是数据。后端必须自己验证本机允许做什么。
3. **保留凭据设计不变量。** 普通编辑全链路 0 op；1P 仍为钥匙和端点真源；不新增持久密钥缓存、密钥哈希缓存或自动 reveal。
4. **未知、失败、未审批、无数据必须区分。** 不以空值继续覆盖，不把安全拒绝包装成成功，不偷偷执行 fallback。
5. **本机决定只保存在本机。** 审批、恢复日志、队列状态不随同步传播，也不能因恢复旧备份而自动提升权限。
6. **秘密最少流转。** 错误按类型返回，日志不记录原始 URL、凭据头或完整 stderr。前端清状态不等于物理内存已擦除，不作这种承诺。
7. **不全局封杀正常功能。** localhost/局域网模型服务是合法需求；手工启用 MCP 是合法功能；不能用禁用整个模块代替精确边界。
8. **多资源写入不伪装成事务。** SQLite、Skills、live 文件、1P 不存在共同事务；明确 prepared/committed/needs-attention 等阶段及恢复规则。
9. **重试必须有范围。** 可以重试幂等的本地投影；不能自动重放不确定的远端凭据写入，也不能为了修 live 再下载整份旧快照覆盖新编辑。
10. **锁只保护必要区间。** 网络等待、1P 解锁不能持有主库连接锁；固定锁顺序，不能靠加一个全局大锁解决所有竞态。
11. **失败测试不碰用户资产。** 使用临时 HOME、内存/临时 DB、FakeOpRunner、本机回环假服务、合成 token。清理也只限本轮创建的测试资源。
12. **自动化发布停在人工授权前。** 可以构建、验证、生成草稿材料；推送、上传、覆盖既有 Release、签名密钥迁移必须另获授权。

## 4. SEC-A：MCP 导入后不得自动获得本机执行授权

### 4.1 证据与触发条件

- `database/snapshot_policy.rs:7–14` 把 `mcp_servers` 作为 A 级共享配置；`merge_for_import:214–228` 合并本机 settings、端点和 refs，没有 MCP 内容审批这一层。
- `commands/import_export.rs:321,338–340`：SQL 入库后执行统一后处理。
- `commands/sync_support.rs:57–61` → `ProviderService::sync_current_to_live`。
- `services/provider/live.rs:1072–1074` 调用 `McpService::sync_all_enabled`。
- `services/mcp.rs:179–184` 直接根据 `server.apps` 投影；`:99–107` 把 `server.server` 写入 Claude/Codex live。
- SQL 预览 `commands/import_export.rs:260–282` 返回的是文件头 meta、大小及令牌，不是命令/参数的内容审批。

**静态确认的结果**：用户导入含启用 MCP 的 SQL，或下载这类同步数据后，外部提供的命令配置会进入本机 CLI 的有效配置。既有同 ID MCP 的 command/args 被远端改变，同样会投影新内容。

**威胁前提**：攻击者能提供导入文件或改变用户接受的同步载荷；之后 CLI 按自身规则加载 MCP。CCS 此处是写配置，不是自己立即执行进程；具体 CLI 是否再弹确认应动态验证，不能称作 CCS 无交互直接执行。E2E 降低服务器篡改风险，但不替代本机内容审批。

### 4.2 为什么要做

`SECURITY.md:112–115` 已把“导入的可执行内容未经明确决定就启用”列为范围内。笼统点击“导入配置”且只看设备名，不等于批准未知 command/args/env。

### 4.3 施工设计

1. **将共享期望配置与本机批准状态分离。** 保留共享 MCP 配置，不让远端字段直接决定可投影。建议本机记录 `{serverId, app, approvedRevision}`；revision 对应实际行为内容。
2. 最小首版：外部新增 MCP 与已存在但行为内容变化的 MCP 进入“待审批”，不自动启用；完全相同且本机已批准者维持正常投影。
3. 行为内容至少覆盖 transport、command、args、cwd、env、URL、headers 及相关执行选项；不能只比较名称或 ID。对象 key 可排序，数组必须保序；字符串不要做会改变执行含义的 trim/大小写转换。
4. 若 revision 包含秘密材料，不另存可离线猜测的普通秘密摘要。优先复用秘密引用及其版本；确有内容指纹需求则用本机随机密钥的 HMAC，密钥留本机。此处只为绑定审批，不新增凭据读取缓存。
5. 后端 `sync_server_to_app` 和批量投影统一检查有效批准状态；不能只在 SQL 确认框加 checkbox。直接 upsert/toggle、启动重投影、手动 sync 也走同一规则。
6. 审批提交绑定预览 revision，后端在锁内重读当前内容；若期间改变，返回“内容已变化，请重新确认”。不要只把一次性 token 绑定文件路径。
7. 不批准的更新不能悄悄覆盖旧已批准命令。最小安全策略：停用这个 CCS 托管条目并提示差异；不依赖复杂的历史命令副本。删除托管 live 节点仍须核对归属，不能删除用户手写的同名对象。
8. 审批记录作为设备本地数据，从导出中剔除；导入和恢复不能采纳外部 approval。旧版升级时只允许对“本机 DB 与本机当前托管 live 内容一致”的既有条目做一次有限继承；有歧义则待审批，不在启动时批准整库。
9. 确认 UI 展示完整可滚动的 command、逐项 args、cwd、transport/目标地址、env/header 名称和变化。秘密值默认掩码并明确标为敏感字段变化，必要时用户主动查看；不能以脱敏为由隐藏有执行语义的普通字段。
10. Skills 及 Claude hooks 等其他可执行/指令性导入内容另列后续盘点，不宣称本工作包已经解决全部导入执行面。

### 4.4 验收

- 新 MCP 的启用位为 true，经 SQL 导入或同步后仍不投影，显示待审批。
- 同 ID/同显示名只改变 args、env、cwd、URL 中任意一个，旧批准失效。
- 完全一致的已批准 MCP 同步后不反复骚扰用户。
- 在预览与确认间改变内容，确认被拒绝；客户端提交伪造 approval 不能生效。
- 手工本机创建并明确启用的正常路径仍可用。
- 单应用批准不自动批准另一应用。
- 往返导出、二进制恢复、重启后仍保持本机审批边界。
- 使用无执行副作用的哨兵配置测试投影；不需要运行未知命令证明问题。

## 5. SEC-B：携带凭据的模型请求禁止跨目标自动转发

### 5.1 证据

- `services/model_fetch.rs:66–84` 使用全局客户端发送模型请求。
- `services/http_client.rs:117–127` 未设置 redirect policy，使用 reqwest 默认策略。
- `model_fetch.rs:156–174,176–190` 会发 `x-api-key`、`x-goog-api-key` 和任意合规自定义头。
- `PiProviderForm.tsx:940–959` 将 apiFormat 与请求头送到该服务，是正常 UI 可达路径。
- reqwest 0.12.28 `redirect.rs:239–249` 跨 host/port 只移除 Authorization/Cookie 等固定头，**不包含上述自定义 API key 头**。
- `model_fetch.rs:211–215` 对 models URL override 直接采纳，未在服务层约束 scheme/origin。

**触发与影响**：用户点击获取模型，端点重定向到另一 host/port 时，自定义凭据头可能被发往第二个目标。普通 Authorization 跨域已有依赖库保护，不能把它和自定义头混为一谈。合法端点的错误重定向、被修改的网关或导入配置都可能提供这一条件；用户直接把 key 交给恶意首目标本就不在此修复能挽救的范围内。

### 5.2 施工

1. 对**携凭据的模型获取**使用专用 client policy，默认 `redirect::Policy::none()`；不要改变所有下载/公开资源请求的全局语义。
2. 专用客户端必须继承现有全局/系统代理选择，不能修复重定向后绕开用户代理。复用 client builder 的代理部分即可，不新建通用 HTTP 框架。
3. 最小版本把 3xx 返回为安全的“端点重定向，需要更新地址”，不把完整 Location 直接记录日志。
4. 若兼容性确需自动重定向，只允许同 origin（scheme、host、有效端口均相同）的有限次数跳转，拒绝 HTTPS→HTTP；自行逐跳检查后再发请求，禁止先跟随再检查最终 URL。
5. 用 URL parser 校验首目标和 override，仅允许 http/https；限制 userinfo。跨 origin override 默认不携当前 key 发送，提供明确的独立目标确认或要求重新配置。
6. 公网带钥匙 HTTP 默认拒绝；localhost/回环/受支持私网允许明确的本机 opt-in。私网识别复用现有同步的正确策略并补 IPv6 测试，不用字符串前缀判断，不把“拒绝所有私网”当 SSRF 修复。
7. 同源按 URL 语义比较，不用 `starts_with`；升级后的默认行为改变要在 UI 可解释。

### 5.3 验收

- 两个本地假 HTTP 服务 A/B，A 对 B 的 302/307/308 跳转：B 必须收到 **0 个**携凭据请求，覆盖 x-api-key、x-goog-api-key、自定义头和 Authorization。
- 同 host 不同端口属于不同 origin；HTTPS 降级被拒绝。
- 直接合法端点、同源规范化、显式允许的本机 HTTP、代理配置保持正常。
- override 跨域不能静默把现有 key 发出去。
- 不调用公网或真实供应商；用合成 token 作断言。

## 6. SEC-C：错误对象不得把秘密 URL 带回前端

### 6.1 证据

- `model_fetch.rs:76–79` 的正常日志有脱敏，但 `:86–88` 直接 `format!("Request failed: {e}")`。
- reqwest 0.12.28 `error.rs:267–269` 的 Display 会附上错误关联 URL。
- `ClaudeFormFields.tsx:206–208`、`PiProviderForm.tsx:971–974` 把原始错误送入 `console.warn`。
- 这条路径没有经过 `redact_model_fetch_error_body`；后者只处理 HTTP body，见 `model_fetch.rs:113–136`。

**触发与影响**：请求 URL 包含查询参数中的 token 等敏感内容，网络失败时 URL 可进入错误 IPC 及开发者控制台。是否被应用日志插件持久化本轮未验证，不宣称必然写入日志文件；仅 IPC/console 泄漏已值得修复。

### 6.2 施工

1. 在后端按 reqwest error 分类，优先 `without_url()` 或直接映射为稳定 code；禁止原始错误 Display 作为前端载荷。
2. 最小载荷建议 `{code, retryable, status?}`；必要诊断只带安全 origin、候选序号、耗时，不带 path/query/fragment/userinfo 或响应头。
3. 前端 `src/lib/api/model-fetch.ts:60–82` 从英文字符串 contains 分支改为 code 映射；迁移期可保留旧格式解析，不能以兼容为由继续回传原文。
4. HTTP 错误体也应明确限制展示：已知秘密替换是辅助，不是“任意远端正文绝对安全”的保证。生产默认不向日志写完整正文。
5. 检查同一网络服务所有错误出口，包括 send/json/URL 解析/redirect，避免只修一条 return。
6. 不实现仓库级错误体系大重构；本服务闭环稳定后再扩大。

### 6.3 验收

- 合成 URL 的 query、userinfo、fragment，加请求 key、自定义头值：timeout、连接拒绝、非法响应、3xx、401/404 的 IPC、console、日志采集器均不出现原文。
- UI 仍能区分认证失败、超时、不支持模型接口、重定向被拒绝。
- 错误清理不依赖用户打开某个脱敏开关。

## 7. REL-A：同步恢复需要覆盖进程中断，而不只是函数返回 Err

### 7.1 证据与现状

- `services/sync_protocol.rs:494–510`：先备份 Skills、替换 Skills，再导入 DB；DB 返回 Err 时尝试恢复 Skills。
- `archive.rs:164–192` 的 Skills 备份是临时目录对象，未形成持久恢复协议。
- `archive.rs:143–160` 将旧目录改名为 `.bak`，复制后删除 `.bak`。
- `commands/sync_support.rs:23–81` 后处理聚合错误返回，已比静默吞错好，但没有以本次导入 revision 为单位的持久待完成任务。

**静态确认的窗口**：Skills 已替换、DB 尚未提交时进程退出，普通错误回滚不会运行；或 DB 已替换、Pi/live 后处理未完成时退出，现有一次性 warning 无法表达跨重启的恢复责任。不能把已有数据库备份等同于整个恢复过程已具备 crash consistency。

### 7.2 两步落地，避免大改

**REL-A1：先缩短破坏窗口，保留可恢复材料。**

1. DB 和 Skills 先在 staging 完成解析、边界检查、解压与内容校验，再动 live 状态。
2. 在受保护的应用数据根下创建唯一 operation ID 的 staging/backup；目录替换尽量同卷，不能用跨卷 rename 假装原子。
3. 启动提交前持久写一个最小 journal，保存阶段、operation ID、快照标识、受控相对路径及必要的预期哈希；不保存密钥或原始 op JSON。
4. journal 与临时资源也是敏感配置资产，使用现有私有写入/目录 ACL 能力；不能放在全局可读临时路径再声称安全。
5. 保留旧 Skills 与 DB backup，直到 DB 提交及必须的后处理状态已经有持久记录。失败不忽略回滚错误。

**REL-A2：实现有限状态恢复，不做通用任务平台。**

建议状态：

| 阶段 | 事实 | 启动后的默认处理 |
|---|---|---|
| prepared | staging 完整，现用数据尚未改变 | 丢弃/保留待重试 staging，不触碰现用数据 |
| skills_replaced | Skills 已换，DB 是否提交须核验 | 先核对 DB commit marker；未提交恢复旧 Skills |
| db_committed | DB 与快照身份一致 | 不重新导入；只继续幂等后处理 |
| projections_pending | 一个或多个 live 投影未完成 | 按最新 DB 与审批状态重试这些投影 |
| complete | 必需后处理已完成 | 安全清理本次已知资源 |
| needs_attention | 文件被外部修改/备份不完整/状态冲突 | 停止自动破坏操作，展示恢复选择 |

6. **关键陷阱**：journal 阶段更新与 DB commit 也不是同一个事务。需要在应用自建的本机 DB 状态中写 commit marker，且随主库替换原子生效；重启时不能仅凭旧 journal 就把“新 DB + 新 Skills”回滚成“新 DB + 旧 Skills”。
7. commit marker 必须是本机生成的，不信任外部 SQL 带来的同名值；不随同步/配置导出传播。
8. 重新投影前核对本次 expected revision；用户已再次编辑时以最新 DB 为准，不重新下载或重放旧快照。外部 live 被修改且无法证明归属时进入 needs_attention。
9. 自动恢复不得访问 1P、批准 MCP、复写凭据；缺少必要凭据时只报告待人工动作。
10. journal 恢复须在自动同步和会修改 live 的启动任务之前执行；存在未解决状态时暂停冲突操作，但不要禁用全部 UI。
11. 不改变远端协议；这首先是本机恢复能力。

### 7.3 验收

使用专用测试进程，在以下每个边界强制结束再启动：journal 写入前后、Skills 替换前后、DB 提交前后、后处理期间、清理前。

- 恢复后 DB 与 Skills 必须属于同一个确定状态，或明确 needs_attention；不能静默展示成功。
- DB 已提交时不得重新导入造成用户后续编辑丢失。
- 原来不存在 Skills 目录、复制失败、磁盘满、文件占用、回滚失败均覆盖。
- 重复启动/重复恢复幂等，不清理其他 operation 或用户目录。
- 无真实 op 调用；MCP 待审批状态不被恢复绕过。

## 8. DEL-A：发布签名必须验证用户实际下载的那份文件

### 8.1 两个确定性缺口

**本地流程文件名不一致：**

- `docs/release-process-zh.md:67–74` 在 bundle 目录对 `CC Switch_*_x64_zh-CN.msi` 生成校验清单。
- `:97–104` 却把 MSI 复制为 `CC-Switch-$TAG-Windows.msi` 上传，清单原样上传。
- 用户按文档下载 Release 资产后执行 `sha256sum -c SHA256SUMS`，清单查找的是原构建名，而不是实际下载名。即便字节哈希相同，标准验证流程仍失败。

**备用云端发布未提供同等材料：**

- `.github/workflows/release.yml:78–114,155–171` 构建并上传 MSI，没有生成 SHA256SUMS/minisign。
- `SECURITY.md:154–156` 却承诺每次 Release 都提供清单和签名。
- 工作流按标签检出，但未校验标签版本与应用各版本字段/MSI ProductVersion 一致；也未绑定相同 SHA 的全部交付检查。

### 8.2 推荐默认设计

1. **先建立唯一 release staging 目录、确定最终资产名，再算 hash、签名、验证、上传。** 目录只装本次精确 MSI，不对历史 build 目录使用通配符搜第一份。
2. 一个小型发布校验脚本承担：读取版本、确认 tag 指向的 commit、检查工作树状态、选择精确产物、读取 MSI ProductVersion、生成材料清单并校验签名。
3. `package.json`、`tauri.conf.json`、`Cargo.toml`、Cargo.lock 中应用 package 版本与 MSI 一致；允许既有标签短 hash/预发布后缀，但不要把它写进 MSI ProductVersion。
4. 验证必须在**只含最终下载文件的空目录**执行，防止本机旧原名文件掩盖文件名错误。
5. 默认私钥仍留发布机，不为补位工作流擅自上传 GitHub Secret。云端先只生成待签名 artifact/草稿；取得离线签名并验证后才能正式发布。
6. 如果维护者将来明确授权托管签名，另做密钥保管设计；不是本次默认实现。
7. 正式发布前执行同一套门禁，或校验相同 commit SHA 的既有成功结果；不能借用 main/其他提交的绿灯。已有依赖门禁不需要重写，关键是发布不能绕过它。
8. workflow 默认最小权限，只有发布 job 有 `contents: write`；保护签名阶段，不在不可信 checkout 的构建步骤暴露签名材料。
9. 缺 MSI、清单或签名应硬失败；不能 warning 后继续。建议用 draft 完成附件验证后发布，避免半套文件对外可见。
10. 更新本地流程，不默认 `git push origin main`；命令例子服从当前分支策略。不要自动覆盖已发布资产，同版本换包需维护者明确授权并重新签名。

### 8.3 验收

- 合成 MSI 或测试包：改内容、改下载名、缺签名、错误公钥、多个候选 MSI、tag/version 不一致，全都拒绝。
- 完整合法资产在全新下载目录验签和 hash 均通过。
- 云端补位不能直接发布“只有 MSI”的正式 Release。
- 实际安装/升级/卸载测试在隔离 Windows VM：包括自定义目录、中文/空格路径、旧版升级数据保留；不能在用户工作机以卸载作测试。
- 本轮施工只生成/验证材料，不自动创建或上传 Release。

## 9. SEC-D：从“禁止危险对象”推进到“只导入程序认可的数据”

### 9.1 证据与定位

- `backup.rs:759–783` 的基本 schema 校验主要验证必需表存在。
- `:794–838` 拒绝 trigger/view/virtual table，但不是完整表列、约束、索引白名单。
- `:433–435` 仍将暂存连接整体 Backup 到主库。
- `:855–886` 导出也遍历 sqlite_master 的对象，而不是只由程序 schema 生成。

**性质**：这是纵深加固，不是本轮已证明的 trigger 绕过，也不声称任意非标准 schema 必然能泄密。风险在于应用未来仍可能继承外部定义的普通表约束、索引或未知对象，扩大迁移与回填的信任面。

### 9.2 分阶段实施

1. 先为支持的历史 schema 建立合法 fixture 清单：版本、必需/可选表列、允许迁移的差异。不因为文档旧就误拒绝真实历史备份。
2. SQL 在隔离暂存连接执行，继续保留现有 authorizer；二进制备份也进入不可信读取区。
3. 枚举 schema 对象和 `table_xinfo`/索引信息，拒绝未知可执行对象、异常对象类型及无法识别的结构；拒绝而非静默“修好”未知 schema。
4. 新建**由本版本程序创建**的干净 staging DB，用白名单列和参数化写入复制数据；不要复制外部建表 SQL、约束和索引。
5. schema 归一化与业务校验是两层：校验 ID、枚举、JSON/TOML、引用关系、长度等，MCP 仍走 SEC-A 审批，不因已经放进干净表而获得授权。
6. 本机 B/C/D 合并和本机审批/恢复状态合并只能在干净 staging 上进行，随后替换主库。
7. 保留迁移兼容，但不能因为外部 user_version 很高/很低就无条件跑可信迁移。版本必须与实际已识别结构对应。
8. 不在首版把所有未知 settings key 静默丢弃。对共享键建立显式策略，未知内容选择拒绝或隔离并报告；新增设备本地键必须有不导出的测试。
9. SQLite `trusted_schema=OFF` 等开关只作为补充，先确认当前 bundled SQLite 和 rusqlite 支持；不能取代干净 schema。
10. 大库复制使用事务/预编译语句，不逐行建连接，避免安全加固造成秒级变分钟级。

### 9.3 验收

- 历史正常 SQL、当前空业务表导出、当前二进制备份均能恢复。
- trigger/view、非预期索引表达式、缺列/多列冲突、伪版本、未知对象、损坏数据分别有拒绝或明确兼容结果。
- 任何拒绝都不改变主库、本机 refs、端点或审批。
- 导入后 sqlite_schema 的托管结构与应用自建模板一致。
- 保持两种凭据后端的数据边界，导入归一化本身 0 op。
- 记录当前正常样本耗时与峰值内存，再定性能阈值，不凭空承诺固定倍数提升。

## 10. REL-B：op 超时之外再补排队预算与资源清理

### 10.1 当前确切问题与边界

- `onepassword.rs:852` 先同步等待 OP_LOCK，`:875` 才建立执行 deadline。多个慢请求排队时，用户总等待不受 OP_TIMEOUT 单独约束。
- `:888–900` stdout/stderr 无上限读取，且读取错误被忽略。
- `:907–916` stdin 复制为普通 Vec；`:926` 超时返回前没有统一收拢 reader/writer 的结果；`:930–935` wait 错误路径未统一 kill/reap。
- `:939–944` join 在 child 退出之后，无独立截止控制；如继承管道的后代继续持有句柄，退出不必然意味着 EOF。

后一个条件未在真实签名 op 上验证，作为防御性进程管理要求，不能宣称正常 op 必然卡死。签名校验仍是执行前提。

### 10.2 实施

1. 分开 `queue_timeout` 与 `execution_timeout`，向 UI 提供 queued/running/failed 阶段，不把等待保险箱解锁误称卡死。
2. 优先在异步命令入口用有界 permit 限制待执行数量，再进入 `spawn_blocking`；过载返回可重试 busy。不要把同步 Mutex 生硬改成 async 锁后在深层到处 `block_on`。
3. 运行层仍保留必要串行规则。队列取消要在 spawn 前再次检查；取消前端 promise 不等于取消 Rust 进程。
4. stdout 按条目读取/列表操作设置不同的合理上限；stderr 使用小上限。超限 kill/reap 并返回 output_limit，不能截断后当成合法 JSON。
5. stdin、stdout、stderr 的秘密缓冲尽早使用 Zeroizing；读失败与线程失败不能 `unwrap_or_default` 后假装成功。此措施减少残留，不承诺消灭所有分配器/OS 副本。
6. 用统一子进程 guard 覆盖 success、timeout、wait error、读写失败、panic 清理；任何清理都要有截止预算。
7. 若测试证明后代持管道确实使 join 越界，再用 Windows Job Object 等限定本轮子进程树；不要扫描/杀死用户所有 op 进程。
8. 排队与执行指标只记操作类型、耗时、结果 code，不记 argv 原文、stdin 或 item 内容。
9. 不在此阶段扩大 1P 并行度；减少重复 op 的既有承诺继续按真实 spawn 次数验收。

### 10.3 验收

- 一个慢任务占用执行位，后续 N 个任务中超时/取消者不再 spawn，队列长度有界。
- 假子进程不读 stdin、持续输出、退出但保留管道、stderr 非 UTF-8、wait/读取异常均能在测试预算内结束。
- 无遗留测试进程/工作线程；只清理测试自己创建的资源。
- `onepassword_op_counts.rs` 的普通编辑 0 次、合法 patch 次数与条目归属回归不退化。

## 11. OPT-A：模型获取的响应上限与旧请求隔离

### 11.1 已确认位置

- `model_fetch.rs:93–97` 成功响应直接 `.json()`；`:113–125` 先 `.text()` 全读，再通过 helper 截断到 512 字符。**显示截断不是读取内存上限。**
- `:75–83` 每个候选各自 15 秒，完整动作可能跨多个候选累加。
- `ClaudeFormFields.tsx:169–211` 没有 Codex/Pi 那样的 generation 检查。请求发出后改变 endpoint 或卸载/切换表单，旧请求仍可提交结果/提示。
- 参考而非重写：`CodexFormFields.tsx:348–351,412–427`；`PiProviderForm.tsx:953–980`。

### 11.2 实施

1. 用流式累计读取限制真实字节，检查 Content-Length 仅作提前拒绝，不作为唯一上限；无长度、chunked 响应同样受限。
2. 建议初始预算（施工时以真实合法 fixture 调整）：成功模型响应 4 MiB、错误正文读取 64 KiB、模型条目 10,000、单条 ID 1 KiB。常量集中在本模块，先不暴露一堆用户设置。
3. 超限明确返回 response_too_large，不伪装空列表。限制解码/反序列化成本，不只对最终 Vec 截断。
4. 为一次“获取模型”建立总预算，例如 20 秒；每个候选使用剩余预算与原单次上限的较小值。认证失败不重试其他候选，404/405 按既有语义有限尝试。
5. Claude 增加请求 generation；endpoint/key/isFullUrl/override 变化、切换 provider、卸载时使旧 generation 失效，并清掉不再匹配的 fetchedModels。
6. `.then/.catch/.finally` 都检查 generation，避免旧请求结束把新请求 loading 清掉。
7. 最小版本做到旧响应不更新 UI；若后端取消需要额外协议，后续再做。不要声称仅前端忽略结果就已节省网络请求。
8. 暂不加模型响应持久缓存，也不自动取 1P key 来改善按钮体验。

### 11.3 验收与指标

- 延迟 A 请求 → 改为 B → B 先返回/A 后返回：只显示 B，无 A 的成功/错误 toast，loading 正确。
- 关闭对话框/切换 provider 后无旧结果回灌；引用现有 ClaudeFormFields 测试补充 deferred promise 场景。
- 大 body、chunked、无 Content-Length、巨大模型数组、长 ID、损坏 JSON均按预算结束。
- 记录请求数、总耗时和峰值内存；不把未跑的性能测试写成优化收益。

## 12. DOC-A：安全说明与历史状态对账

### 12.1 当前需要纠正的表述

- `SECURITY.md:20–26` 仍以 HKCU 投递、直接读取凭据管理器取 Base URL 作总体描述，应区分严格投递默认值、兼容投递与 1P 模式。
- `SECURITY.md:84` 说 refs 不随云同步；`snapshot_policy.rs:149–154` 实际只删空 vault 的占位引用，同 vault 的真实引用有受限传递策略。
- `SECURITY.md:85` 对 disk/DB/backup 的绝对承诺，应与 `:150` 已写的迁移前备份/提取失败例外一致。
- `SECURITY.md:144` 对 E2E 口令“只在凭据管理器”的说法漏掉 1P 后端。
- `SECURITY.md:149` “不丢数据”与尽力而为并发覆盖不能并列作绝对保证，应说明可能丢失最后写入前的更新及备份恢复边界。
- `docs/plans/security-and-selective-1password-update-plan-zh.md:3` 仍为待施工，而 CHANGELOG 2.3.2 已记录实施；`snapshot_policy.rs:7` 等注释也有“仍为骨架”这种过期阶段信息。

### 12.2 实施与验收

1. 增加一张按 Credential Manager / 1P、严格 / 兼容投递、正常托管 / 迁移待办分列的数据位置矩阵。
2. 明确 API key、普通 Base URL、含凭据 URL、引用元数据、历史备份、进程环境是不同类别。
3. 将旧方案标为“历史设计；已实施范围见 2.3.2/关联提交；未验收项单列”，保留历史证据，不覆盖成新方案。
4. 新方案施工中更新本文件状态，但只能勾选实际通过的验收。
5. DOC-A 不调整产品安全行为；行为改变分别归属前面的代码工作包。
6. 验收者按代码逐条对账，不以“文档读起来合理”代替检查。

## 13. 分阶段施工与交付门禁

### S0：基线、夹具、回归账本

- 核对 HEAD、分支、AGENTS.md；记录用户已有变更，不清理 `.zcodeignore`。
- 运行现有必要检查，区分基线已失败项与本次引入项。
- 创建合成 SQL/MCP、本地网络假服务、进程 runner 和故障注入测试计划。
- 列出既有凭据不变量测试，尤其 `src-tauri/tests/onepassword_op_counts.rs`、`import_export_sync.rs`、`mcp_commands.rs`、`log_no_secrets.rs`。
- **退出条件**：每个 P1 有明确失败测试入口与期望；没有通过阅读旧方案就开始重构。

### S1：小而独立的优先修复

- SEC-B/C：模型请求与错误边界。
- DEL-A：先修最终资产命名和验证顺序；云端正式发布缺签名时阻断。
- OPT-A 的 Claude generation 可以作为单独小提交一并完成。
- **退出条件**：跨域假服务拿不到 key，错误无合成秘密，干净下载目录验签/hash 可通过，旧请求不污染表单。

### S2：导入授权边界

- 实施 SEC-A 后端批准规则、本机数据合并、UI 审批。
- 覆盖 SQL、同步、备份恢复、启动/手动投影。
- **退出条件**：外部 enabled 或同 ID 内容变更不能直接生效，合法已批准项不反复失效。

### S3：中断恢复

- REL-A1 先落持久恢复材料和缩小提交窗口；REL-A2 再落重启恢复与幂等后处理。
- 与 SEC-A 使用相同的“本机状态不随导入覆盖”原则。
- **退出条件**：故障注入矩阵每一个中断点均能一致恢复或明确暂停，不能只测试返回 Err。

### S4：纵深防御与性能预算

- REL-B、OPT-A 后端有限响应。
- SEC-D 独立开发/验收，不与 runner 改造混成一个提交。
- **退出条件**：大输入/排队可控、历史导入兼容、主库结构由程序决定；无普通编辑额外 op。

### S5：最终复核、文档与交付材料

- 完成 DOC-A，更新阶段状态。
- 在隔离 Windows VM 验收升级/恢复/安装路径；未测部分如实标注。
- 产出 release staging 和验证报告，停止在正式发布授权之前。

### 建议检查命令（由施工模型在隔离测试环境执行）

以下来自当前仓库脚本/工具，不表示本轮已经运行：

```text
pnpm typecheck
pnpm format:check
pnpm test:unit
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
node scripts/check-links.mjs
git diff --check
```

- 依赖审计复用现有 CI，记录扫描日期与数据库版本；不要把过去的绿灯当当前无漏洞证明。
- 需要真实 Credential Manager/注册表的 ignored 测试在 CI/隔离账户运行，不擅自在用户主账户执行。
- 新增测试先定向跑，再全量跑。测试失败时保留摘要与原因，不靠删除/ignore 用例过门禁。

## 14. 关键回归矩阵

| 场景 | 必须保持的结果 |
|---|---|
| 1P 锁定，仅改模型/备注 | 成功保存非秘密配置，完整链路 0 op |
| 改名/明确改凭据 | 核归属、保留未改字段，失败不假装已保存 |
| SQL 带伪 refs | 不读写他组条目；审批与 refs 是独立边界 |
| SQL/同步带新 MCP | 入库可见但待本机审批，不写入可执行 live |
| 同 ID MCP 的 args 改变 | 旧批准失效，不沿用 ID 信任 |
| 网络重定向到另一端口/域 | 不发送任何自定义凭据头 |
| 请求失败且 URL 含合成秘密 | IPC/console/日志不含秘密 |
| 无限 chunked 或巨大模型列表 | 按预算拒绝，无无限增长 |
| 模型请求 A 晚于 B 返回 | UI 只保留 B，loading/toast 不串线 |
| Skills 替换后进程退出 | 重启恢复旧一致状态或继续已提交状态 |
| DB 提交后退出 | 不重导入旧快照，仅完成必要投影 |
| op 排队时取消 | 不 spawn 过期任务，不影响在途合法任务 |
| 发布最终文件改名/改内容 | 验证失败，不发布半套附件 |
| 历史正常 SQL/当前备份 | 兼容恢复，本机安全状态不被外部覆盖 |

## 15. 不建议本轮做的事

- 不整体迁移状态库、不重写 Provider 类型系统、不换 UI 框架。
- 不为列表快一点而新增持久明文缓存或缓存密钥哈希。
- 不并行启动更多 op 来掩盖队列等待。
- 不把所有请求/文件操作套进一个万能框架。
- 不改同步协议、强推零用户提示的自动迁移，除非具体兼容性测试证明必要。
- 不把本机同账号任意代码执行能力包装成此应用可完全抵御的威胁。
- 不承诺 Secret memory 绝不残留、SQLite/Skills 真正跨介质原子提交、或条件写被远端忽略也绝不丢更新。
- 不擅自清理旧迁移备份、1P 条目、发布旧资产或用户的 live 内容。

## 16. 给接手模型的施工指令

> 先读取 AGENTS.md 和本文件，核对基线及用户现有改动。仅实施用户本次指定阶段，不一次性改完所有工作包。每项先补能失败的测试，再做最小实现；已有安全保障不能倒退。所有真实秘密、保险箱、用户目录、远端同步与发布操作均不可作为自动测试目标。每阶段报告修改文件、失败测试及修复后结果、op 实际次数、兼容性、未验证项和剩余风险。涉及新增审批记录/恢复标记时，必须同时证明它们不随配置导出、不被外部导入覆盖。不得自动提交、推送、发布或清理用户资产。

### 阶段报告模板

```text
阶段 / 基线：
实际完成的工作包：
修改文件与关键函数：
复现测试（修复前为何失败）：
修复后的定向测试：
全量检查：通过 / 失败 / 未运行（附原因）
安全不变量：普通编辑 op 次数、日志脱敏、导入授权边界
兼容性与迁移：
故障恢复与回滚方式：
仍待动态验证：
下一阶段是否具备前置条件：
```

**最终完成定义**：不是“代码写完”或“单测数量增加”，而是每个已承诺工作包有可复核测试、用户能看懂失败状态、既有凭据边界保持、发布材料可独立验证，并且未测试的环境和风险被清楚标出。

## 17. 施工记录

### 2026-09-29：S0 + S1 完成（未提交，等待维护者审查）

- **SEC-B**：`http_client.rs` 新增 `get_no_redirect_client`（代理选择与全局客户端一致、`redirect::Policy::none()`）；`model_fetch.rs` 改用它，新增候选 URL 校验（仅 http/https、拒绝 userinfo）与 override 跨源拦截（`cross_origin_override`）。3xx 一律不跟随，返回 `redirect_blocked`，不回传 Location。所有预设 `modelsUrl` 已核验与 Base URL 同源，合法用法不受影响。
- **SEC-C**：新增结构化错误 `ModelFetchError { code, retryable, status? }`，IPC 不再携带 reqwest 原始 Display（其会附完整 URL）；错误响应体仅进本机 debug 日志（脱敏+截断）。前端 `model-fetch.ts` 改为 code 映射，保留旧英文字符串解析作迁移期兜底；i18n 四语言新增 `fetchModelsRedirectBlocked`、`fetchModelsInvalidUrl`。
- **DEL-A**：`release.yml` 收敛权限（构建 job `contents: read`、发布 job `contents: write`）、新增标签版本与 `package.json` 一致性校验、MSI ProductVersion 校验、唯一精确产物选择（多候选/缺文件硬失败）、对最终资产名生成 `SHA256SUMS`、发布改为 **draft**（云端补位不能直接发布无签名正式 Release）。`release-process-zh.md` 修正「先复制最终名再算 hash/签名」顺序、新增干净目录回验步骤、`git push origin HEAD` 替代 `push origin main`、云端草稿 + 离线签名流程说明。
- **OPT-A（前端部分）**：`ClaudeFormFields.tsx` 补齐与 Codex/Pi 一致的请求 generation（baseUrl/isFullUrl/apiKey 变化及卸载即作废旧响应，清空 fetchedModels，`.then/.catch/.finally` 全部检查 generation）。随后顺带补齐 `CodexFormFields.tsx` 的同款缺口：effect 作废在途请求时重置 loading（避免按钮永久禁用）、`.finally` 加 generation 守卫（避免旧请求清掉新请求 loading）、新增卸载时作废在途请求的 cleanup，与 Claude/Pi 行为对齐；新增 `CodexFormFields.test.tsx` 覆盖 deferred promise 场景（旧响应不回灌、loading 重置、旧 finally 不清新 loading、卸载无 toast、正常路径可用）。
- **测试**：后端新增 4 个回环假 HTTP 服务集成测试（302/307/308 重定向零凭据外泄、跨源 override 零请求、同源 override 正常、网络错误载荷无 URL 材料）与 4 个单元测试；前端新增 `ClaudeFormFields.test.tsx`（旧请求不回灌、卸载无 toast、正常路径可用）。`cargo test` 975 通过 0 失败、`vitest` 747 通过 0 失败、`clippy -D warnings`、`tsc`、`check-links`、`git diff --check` 全部通过。
- **基线已存在、未处理的失败项**：`src/components/EndpointBackfillBanner.tsx` 与 `src/components/settings/EnvDeliverySection.tsx` 两文件在 `prettier --check` 下不合格（基线 `2a25d07` 即如此，与本次改动无关）。
- **未动态验证**：代理配置下重定向行为（仅静态保证继承代理）；真实公网端点兼容性；云端 workflow 实际运行；minisign 实际签名/回验（文档步骤，需发布机私钥）；Windows 安装/升级路径。

### 2026-09-29：S2 完成（SEC-A 导入授权边界）

- **审批模型**：新增设备本机表 `mcp_approvals(server_id, app, approved_revision, approved_at)`；`approved_revision` 是 `server_config` 的规范化 JSON 全文（键递归排序、数组保序、不做任何 trim/大小写转换），与同库明文的 `server_config` 同级暴露，不另存可离线猜测的秘密摘要（§4.3-4）。
- **门禁位置**：批准检查放在 `McpService::sync_server_to_app`——所有投影路径（表单保存、启用开关、启动重投影、手动同步、导入后处理 `run_post_import_sync → sync_all_enabled`）的唯一入口，任何调用方不能绕过（§4.3-5）。未批准时批量投影不再写 live，并把旧内容从 live 移除（§4.3-7：不沿用 ID 信任，不用旧命令继续执行）。
- **审批生命周期**：表单保存（upsert）= 用户对内容的显式决定，保存即批准该内容（Claude/Codex 各记一条，与启用位解耦——启用位只管投递范围）；从本机 live 导入（`import_from_claude/codex`，含启动表空自动导入）的新条目内容即本机 CLI 正在运行的配置，按「本机 DB 与本机托管 live 一致」继承批准；外部导入（SQL/同步/恢复）不携带批准，条目进入待审批。删除条目清理审批行。
- **审批确认**：新命令 `get_mcp_approval_states`（汇总 enabled/approved/revision）与 `approve_mcp_server`（绑定预览 expectedRevision，锁内重读比对，内容已变化即拒绝；一致才批准、启用并立即投影）。UI：列表行待审批徽标（点击打开确认框）、启用未批准条目被拦截改开确认框、批量启用跳过未批准条目并提示；确认框完整展示 type/command/逐项 args/cwd/url，env 与 headers 键名可见、值默认掩码并提供统一显示开关（§4.3-9），i18n 四语言。
- **导出/导入边界**：`mcp_approvals` 列为 B 级设备本地——`prune_for_export` 导出剔除；`merge_for_import` 丢弃外部文件的批准行、原样回拷本机行（§4.3-8：导入和恢复不能采纳外部 approval）。
- **一次性继承迁移**：启动时 `migrate_local_approval_inheritance`（守卫键 `mcp_approvals_migrated`，B 级 settings）只对「本机 DB 内容与本机托管 live 内容一致」的既有条目补记批准；Claude 逐字比对 live JSON，Codex 走「DB 规范→投影 TOML→JSON」与 live TOML→JSON 的同基规范化比对（吸收 headers/http_headers 映射）；读不到/不一致/含白名单外字段一律保持待审批。
- **测试**：后端 `services/mcp.rs` 新增 8 个验收测试（canonical JSON 语义、未批准不投影/批准后恢复、内容变化批准失效并移除 live 旧内容、一致条目不反复失效、过期修订拒绝、单应用批准不跨应用 + pending toggle 被拒、upsert 自动批准 + 删除清理、继承迁移一次性与歧义保守）；`tests/mcp_commands.rs` 两个导入式种子的用例补记批准以匹配新门禁；schema 断言纳入新表。前端新增 `McpApprovalDialog.test.tsx`（3）与 `UnifiedMcpPanel.approval.test.tsx`（4），既有 `UnifiedMcpPanel.test.tsx` 补 mock 两个新 hook。`cargo test` 14 套全过（lib 849）、`clippy -D warnings`、`fmt`、`vitest` 759 全过、`tsc`、`check-links` 全部通过。
- **未动态验证**：真实 SQL/同步载荷的端到端导入审批流程（测试用直接写 DB 模拟）；`.db` 恢复路径的审批保留（静态确认与 SQL 导入共用 `merge_for_import`）；前端审批框在真实 Tauri 环境的交互。
