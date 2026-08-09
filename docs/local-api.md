# Muse 本地 API 协议

Muse 本地 API 的协议版本为 `muse-api/v1`。生产桌面端由 Tauri 宿主启动 API，并将运行信息只交付给受信任的主窗口；项目不提供独立服务端或外部 Bootstrap 通道。

## 运行时 Bootstrap

Bootstrap 响应结构固定为：

```json
{
  "api_origin": "http://127.0.0.1:<随机端口>",
  "access_token": "<仅存在于本次进程内存中的 Bearer>",
  "token_type": "Bearer",
  "protocol_version": "muse-api/v1",
  "instance_id": "<本次进程实例 ID>"
}
```

桌面端只能由标签为 `muse` 的主窗口调用 `runtime_bootstrap`。前端在请求前校验协议版本，并且只向 `api_origin` 精确同源的请求附加 Bearer；令牌不会写入 URL 或 `localStorage`。

Bootstrap 只存在于当前桌面进程内存中。调用方不得把 `access_token` 复制到文件、日志、URL、前端持久化存储或其他进程。

## 请求安全边界

所有 `/api` 路由先执行下列校验：

1. `Host` 必须精确等于 Bootstrap 中 `api_origin` 的 `127.0.0.1:<port>`。
2. 浏览器请求的 `Origin` 必须精确匹配当前 Tauri origin 或显式配置的本地开发 origin；`null`、未知来源和重复 Origin 均被拒绝。
3. 除 WebSocket 票据握手外，请求必须包含 `Authorization: Bearer <access_token>`。

没有 `Origin` 的内部请求仍然必须通过 Host 与 Bearer 校验。不要把 token 放入 query、请求正文、命令行参数或日志。

健康检查同样需要鉴权：

```bash
curl --fail \
  -H "Authorization: Bearer ${ACCESS_TOKEN}" \
  "${API_ORIGIN}/api/runtime/health"
```

成功响应返回 `status`、`protocol_version` 和 `instance_id`，不会回传 token。

## POST SSE 聊天

流式聊天使用 `POST /api/chat/stream`：

```http
POST /api/chat/stream HTTP/1.1
Host: 127.0.0.1:<port>
Authorization: Bearer <access_token>
Content-Type: application/json
Accept: text/event-stream

{
  "message": "你好",
  "conversation_id": "<会话 ID>",
  "client_request_id": "<客户端生成的唯一请求 ID>",
  "voice_enabled": false,
  "selected_skill": "<可选的 Skill 名称>"
}
```

响应为 `text/event-stream`。客户端应使用 `fetch` 读取 `ReadableStream`，按 SSE 帧边界解析事件，并在用户取消时中止请求。`client_request_id` 在客户端重试和请求关联范围内必须唯一。`selected_skill` 只能来自 `GET /api/runtime/skills` 返回的当前有效目录；服务端仍会在 Turn 开始时重新校验名称、Persona 策略、冻结 revision 和真实来源，失败时在调用模型供应商前终止准备。

旧的 `GET /api/chat/stream` 固定返回 `405 Method Not Allowed` 和错误码 `stream_post_required`，响应头只允许 `POST`，且不会创建回合或写入会话。

## 会话管理与上下文检查

Session v3 transcript 是不可变运行事件的事实源；标题、归档状态和分叉关系保存在独立元数据文件中，修改元数据不会重写 transcript。

| 接口 | 用途 |
|---|---|
| `GET /api/runtime/sessions` | 列出会话，并合并标题、归档状态与来源会话 |
| `PATCH /api/runtime/sessions/{id}` | 修改 `title` 或 `archived`；`title: null` 清除自定义标题 |
| `POST /api/runtime/sessions/{id}/resume` | 恢复已有会话 |
| `POST /api/runtime/sessions/{id}/fork` | 从已有会话创建分叉 |
| `GET /api/runtime/sessions/{id}/export` | 导出元数据和用户/助手公开 transcript |
| `GET /api/runtime/sessions/{id}/context` | 查询最新 Context Snapshot 与冻结运行策略 |
| `GET /api/runtime/sessions/{id}/runtime-profile` | 单独查询最新冻结运行策略 |
| `DELETE /api/runtime/sessions/{id}` | 删除 transcript，并清理对应独立元数据 |

导出接口不会包含系统提示词、工具调用参数、工具原始结果、凭据引用或本机路径。Context Inspector 是只读诊断接口，不接受策略修改。

## 密钥更新

设置接口统一使用显式密钥动作：`{"action":"keep"}` 保留、`{"action":"replace","value":"..."}` 替换、`{"action":"delete"}` 删除。只有 `replace` 允许携带 `value`。联网搜索接口直接使用该请求体，模型配置各段通过 `api_key_update` 携带该对象，MCP 凭据字段通过 `secret` 携带该对象。响应不会返回密钥明文或掩码。

模型配置以完整 Provider Profile 写入 `config.toml`：`providers.<id>` 同时保存供应商类型、Base URL、明文 `api_key`、模型列表地址和 `models.<id>` 参数；`active_models` 只引用当前活动的 Provider 与模型。页面保存和手工编辑使用同一文件，不再把模型 Provider、模型列表或 API Key 写入 SQLite、系统凭据库或浏览器持久状态。

MCP Server Profile 同样写入 `config.toml` 的 `mcp_servers.<name>`。`stdio` 只接受 `command`、`args`、`cwd`、`env` 和 `secret_env`；`streamable_http` 只接受 `url`、`headers` 和 `secret_headers`。Bearer Token 固定写入 `secret_headers.Authorization`。管理 API 的秘密字段名只接受 `target`；废弃的 `environment_name` 会作为未知字段被拒绝，不再保存环境引用或 Secret Reference。GET 只返回普通字段、秘密字段名和 `configured`，不会返回 `secret_env` 或 `secret_headers` 的值。

`config.toml` 使用当前用户专属权限和耐久原子替换，但其中的模型、MCP 与 Exa API Key 都是明文配置，因此不得上传、共享或提交。模型 GET、MCP GET、联网搜索 GET、目录响应、诊断、日志和会话不返回 Key 或掩码。运行时不访问系统凭据库，也不回退读取旧 JSON、旧 SQLite 配置表或进程环境变量。

## Agent Skills

用户 Skill 正文的唯一布局是 `<数据目录>/skills/<skill-name>/SKILL.md`，同目录可以包含 `scripts/`、`references/` 和 `assets/`。名称必须匹配 `^[a-z0-9]+(-[a-z0-9]+)*$`、长度为 1–64，并与 frontmatter `name` 完全一致；`name` 和 `description` 必填，`license`、`compatibility`、`metadata` 以及其他扩展字段由管理页面原样保留。

Muse 不再解释或写入 frontmatter `enabled`。现有 API 的 `enabled` 字段保持兼容，但实际状态写入 `config.toml` 的 `[[skills.config]]`，路径固定为 `skills/<skill-name>/SKILL.md`；未配置的 Skill 默认启用。创建、更新、完整目录重命名和删除会同步提交正文与启停配置，失败时回滚，辅助文件不会因页面保存而丢失。

Skill `revision` 同时覆盖 `SKILL.md` 字节与当前启停状态。手工编辑正文或 `config.toml` 后，使用旧 revision 的更新与删除返回 HTTP 409，并带 `skill_revision_conflict` 前缀；无效名称返回 HTTP 400 和 `skill_invalid` 前缀。

运行时内置只读 `skill-creator`，用于指导模型生成符合规范的 Skill。用户目录存在同名项时由用户版本遮蔽内置版本；内置项不进入用户 Skill 管理 API，也不能通过管理页修改或删除。

模型创建入口为 `create_skill`，只接收 `name`、`description`、Markdown `content` 和可选 `enabled`，不接收文件路径。该工具属于持久写入操作，每次调用都需要用户审批，并同时受角色工具策略和 Skill 策略约束；服务端复用与管理 API 相同的 `SkillStore` 完成校验、暂存、原子发布、启停配置提交和失败回滚，不覆盖同名 Skill。创建成功不会改变当前 Turn 的冻结目录；启用的新 Skill 从下一 Turn 起进入目录并可由 `load_skill` 加载，禁用项需要先在管理页启用。

`GET /api/runtime/skills` 返回当前角色在新 Turn 中实际可选择的有效目录，包含内置项、用户同名遮蔽结果、名称、描述、revision 与来源摘要；禁用项、坏项和 Persona 策略拒绝项不会返回，正文与本机路径永不进入响应。该接口服务于聊天输入区选择器，不替代只管理用户 Skill 的 `/api/skills`。

用户在输入区选择 Skill 后，前端以结构化 `selected_skill` 发送，消息正文保持原样。后端把安全载入的完整指引加入本轮系统上下文，并在运行策略快照和用户事件中只记录名称、revision、来源与内容哈希；运行模式切换重建系统提示词时会重新附加同一冻结指引。系统、安全和运行时约束始终高于 Skill 内容。

## 用户级外观偏好

`GET /api/preferences/appearance` 读取用户数据目录中的 `config.toml`，返回 `schema_version`、外观偏好和字段诊断。`PUT /api/preferences/appearance` 只接受当前界面可编辑的 `background_blur`、`background_opacity` 与 `motion_level`；主题、语言、其他配置段、注释、顺序和未知字段由服务端原样保留。

若 `config.toml` 在页面加载后被手工修改，陈旧页面保存返回 HTTP 409，并提供 `code = "config_revision_conflict"`、`field_path = "config.toml"` 和不包含秘密的中文 `message`；服务端不会覆盖外部修改。重新读取后，页面与手工配置看到同一个 revision。语法、版本和文件读写错误也使用稳定的 `code`、`field_path`、`message` 结构。

外观偏好不再写入浏览器持久状态。升级时，前端只在 TOML 仍为内置默认值时一次性导入旧 `localStorage` 外观值；已有 TOML 配置优先，确认或导入成功后立即清理旧键。

## Persona 长期记忆管理

Persona 记忆管理接口挂在角色路径下，全部经过统一的请求安全边界；Persona scope 只能由路径 `{id}` 绑定，请求体不接受也不能切换 Persona。请求 DTO 一律 `deny_unknown_fields`，未知字段、错误 JSON 和查询提取失败都返回既有扁平错误结构与 `memory_invalid_request`，不暴露 Axum 默认拒绝响应。记忆服务未接线时，本节所有合法管理请求固定返回 HTTP 503 与对应 `memory_*` 稳定码，不存在旁路直写表的路径。

| 接口 | 用途 |
|---|---|
| `GET /api/personas/{id}/memories` | 检索当前有效记忆；`query` 必填非空，`category`、`importance` 为页内过滤，`cursor` 原样回传上一页的不透明游标 |
| `GET /api/personas/{id}/memories/{memory_id}` | 查询详情与当前 revision，附 `source_conversation_id`/`source_turn_id` 来源跳转索引（只提供索引，不复制聊天正文；管理来源为 `null`） |
| `GET /api/personas/{id}/memories/{memory_id}/history` | 查询 update/correct 版本历史；revision 来源只返回 `source_conversation_id`/`source_turn_id`，corrected 旧值只允许该管理入口读取，永不进入模型读取面 |
| `POST /api/personas/{id}/memories` | 手工新增；经 `apply_management_content_mutation` 与同一敏感双门策略，成功返回 durable 收据 |
| `POST /api/personas/{id}/memories/{memory_id}/correct` | 纠正记忆；`expected_revision_id` 并发校验失败返回 HTTP 409 与 `memory_revision_conflict` |
| `POST /api/personas/{id}/memories/{memory_id}/importance` | 调整重要程度；只更新逻辑 entry，不创建内容 revision |
| `DELETE /api/personas/{id}/memories/{memory_id}` | 删除单条记忆；JSON 请求体必须携带稳定 `operation_id` |
| `DELETE /api/personas/{id}/memories` | 清空该 Persona 的全部记忆；JSON 请求体必须携带稳定 `operation_id` |

手工新增、纠正和重要程度调整接受可选 `operation_id`；需要重试时客户端必须生成并复用该 ID。删除请求必须使用 JSON 请求体 `{"operation_id":"<客户端操作 ID>"}`，同一次操作的全部重试保持不变。写命令接缝会持久化首次绑定的运行时身份、目标与收据：同一 Persona 下相同 ID、相同请求返回原收据，相同 ID、不同请求稳定拒绝；`PersonaAll` 删除重放不得重新选择 subjects，因此不会删除首次成功后新创建的记忆。删除统一先写独立删除权威再清主库，durable 后不回滚。

错误响应沿用扁平错误结构，`code` 为 16 个 `memory_*` 稳定码之一（与 `MemoryErrorCode` 一一对应）：`memory_invalid_request`（400）、`memory_cursor_invalid`（400）、`memory_query_rejected`（400）、`memory_cursor_expired`（410）、`memory_invalid_state_transition`（409）、`memory_revision_conflict`（409）、`memory_delete_confirmation_required`（409）、`memory_not_found`（404）、`memory_persona_scope_mismatch`（403）、`memory_source_ineligible`（403）、`memory_sensitive_content_rejected`（422）、`memory_query_budget_exceeded`（429）、`memory_sensitivity_unavailable`（503）、`memory_deletion_authority_unavailable`（503）、`memory_repository_unavailable`（503）、`memory_deletion_incomplete`（500）。

`GET /api/personas/{id}/deletion-impact` 的响应新增 `memory_count` 字段：已接线时返回该 Persona 当前有效记忆条数；未接线或计数不可用为 `null`，前端必须保持删除确认按钮禁用，不得按零条处理。删除角色复用 `persona-deletion-recovery.json` 与 `personas.json` 提交点：提交前任何 Persona 文件、VisualPack 或 SQLite 失败都不得触碰记忆；提交后才以恢复记录中的稳定 operation 身份清理 Persona 全部记忆。提交后清理失败时角色保持已删除、记忆保持待收敛，启动恢复幂等重试；不得回滚成“角色仍在但记忆已经永久丢失”。

## WebSocket

浏览器不能把长期 Bearer 放入 WebSocket URL。连接分为两步：

1. 携带 Bearer 调用 `POST /api/runtime/ws-ticket`。
2. 在 15 秒内连接 `ws://127.0.0.1:<port>/api/ws?ticket=<ticket>`，并声明子协议 `muse.runtime.v1`。

浏览器示意：

```ts
const socket = new WebSocket(
  `${apiOrigin.replace('http://', 'ws://')}/api/ws?ticket=${encodeURIComponent(ticket)}`,
  ['muse.runtime.v1']
);
```

ticket 只能成功消费一次；未知、过期或已消费的 ticket 均被拒绝。服务端访问日志不会记录 query、Authorization 或聊天正文。

## 受保护的二进制资源

上传目录不作为公开静态目录暴露。图片、音频和其他上传资源必须通过鉴权 API 获取；桌面前端把响应转换为 Blob URL，并在不再使用时释放。

这意味着外部网页不能依靠猜测文件名读取本地资源，客户端也不应把带鉴权状态的资源 URL 持久化为公开链接。

## 兼容性规则

- 客户端必须拒绝未知的 `protocol_version`，不能猜测兼容。
- `instance_id` 变化表示后端已经重启；旧 token、ticket 和进行中的连接全部失效。
- 客户端不得假设端口稳定，也不得扫描本机端口寻找 Muse。
- API 只允许回环地址，不支持通过反向代理暴露到局域网或公网。
