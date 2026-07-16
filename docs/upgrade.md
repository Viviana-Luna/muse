# Muse 升级与数据迁移

本页说明从旧工作区数据布局升级到当前 Muse 桌面版本时的数据目录、会话和旧模型文件行为。迁移遵循“先校验、只复制、保留原始数据”的原则，不会自动删除旧会话、旧目录或旧模型文件。

## 升级前准备

1. 退出所有 Muse 桌面进程。
2. 确认没有其他程序写入旧 `.agent-vp-data`。
3. 如需固定数据位置，升级前设置 `MUSE_DATA_DIR`。
4. 对有额外合规要求的环境，可以再做一份离线备份，但不要移动或改写原目录。

数据目录带有排他运行锁。若已有桌面实例占用该目录，新的实例会由单实例机制唤醒已有窗口，不会并发迁移或写入。

## 新数据目录规则

数据目录按以下顺序解析：

1. `MUSE_DATA_DIR`。
2. 桌面默认使用当前用户主目录下的 `~/.muse`。

程序不再采用任意当前工作目录中的 `.agent-vp-data`，避免从 crate 子目录启动时形成第二套运行数据。

显式路径无法创建、不是目录或不可访问时，程序直接报错，不会静默切换到另一套数据目录。

## 工作区 legacy 复制迁移

未设置 `MUSE_DATA_DIR` 时，桌面首次启动仅在当前目录同时包含 workspace、`src-tauri`（或旧 `muse-core`、`persona-core`）和根前端项目标识时，才把该目录识别为仓库根并检查 `.agent-vp-data`。只有 `~/.muse` 为空时才会执行迁移：

1. 拒绝包含符号链接或特殊文件的 legacy 目录，防止复制到目录边界之外。
2. 在目标目录旁创建临时 staging，并逐文件复制配置、角色、会话和普通资源；`model-files` 与 `models/assets.json` 明确排除。
3. 校验每个复制文件的字节数并同步落盘。
4. 写入迁移标记后原子提交 staging；标记会记录是否发现并排除了旧模型文件。
5. 全程不覆盖非空目标，也不删除、移动或修改原 `.agent-vp-data`。

如果目标目录已经包含数据，自动迁移会跳过，避免把两套状态合并。此时应明确选择要使用的数据目录，不要手工把运行中的两个目录混写。

### 模型文件

当前开发分支不复制 `model-files` 或 `models/assets.json`，也不读取、校验或改写其中内容。旧文件只保留在原 `.agent-vp-data`；语音能力改为 OpenAI-compatible API 后，这些文件不再属于运行时依赖。

### 模型 Provider Profile 迁移

首次启动会把旧 `models/config.json`、SQLite 模型目录和旧系统凭据 account 合并为 `config.toml` 中的完整 Provider Profile。迁移顺序固定为：读取旧源与系统凭据、原子写入 TOML、从磁盘回读校验、清理旧模型凭据、发布迁移标记，随后由统一 SQLite migration 删除旧 Provider 和模型表。中断后可幂等重试；TOML 未回读成功前不会清理旧凭据。

迁移后的模型 API Key 以明文写在当前用户专属权限的 `config.toml` 中，与 Provider、Base URL、模型列表和参数放在一起。旧 `models/config.json` 在本开发版本内原样保留但停止写入；迁移标记发布后运行时不再回退读取。不要上传、共享或提交 `config.toml`。

### MCP Server Profile 迁移

首次启动会优先读取旧 `mcp/servers.json`，不存在时再读取旧 `models/config.json.mcp_servers`，并把 Server、transport、普通参数以及旧系统凭据或环境引用解析出的 API Key 合并为 `config.toml` 中的 `mcp_servers`。迁移顺序固定为：读取旧源与旧凭据、原子写入 TOML、从磁盘回读并进行语义一致性检查、幂等清理旧 MCP 凭据、发布迁移标记。清理或标记发布中断后可重试，TOML 未回读成功前不会删除旧凭据。

旧 `mcp/servers.json` 在本开发版本内原样保留但停止写入；迁移完成后，管理 API、工具发现、资源读取和工具调用只使用共享 `config.toml` Store 冻结出的不可变快照，不再回退读取旧 JSON、系统凭据库或进程环境变量。MCP 工具目录、健康状态和连接结果仅保存在进程内存中。

### Agent Skills 标准化迁移

首次启动会扫描合法的 `<数据目录>/skills/<skill-name>/SKILL.md`。旧 frontmatter 若包含布尔 `enabled`，Muse 会先把状态原子写入 `config.toml` 的 `[[skills.config]]` 并从磁盘回读验证，再从原文移除 `enabled` 并发布幂等迁移标记；任一步失败都会保留或恢复迁移前状态。

Skill 名称现在必须匹配 `^[a-z0-9]+(-[a-z0-9]+)*$`、长度为 1–64，且目录名必须与 frontmatter `name` 一致。非标准目录不会自动改名，也不会被加载；请在确认引用关系后手工整理。页面更新和重命名会保留 `license`、`compatibility`、`metadata`、未知 frontmatter 以及 `scripts/`、`references/`、`assets/` 等辅助内容。

## 会话存储 v3

启动 API Router 前，Muse 会检查旧 `sessions/runtime.jsonl` 和 `sessions/conversations/*.jsonl`，并迁移到 v3 generation：

- `sessions/store.json` 原子指向当前 generation。
- 会话文件名由完整 conversation ID 的 SHA-256 派生，避免文件名碰撞和路径注入。
- 旧源先逐字节复制到 `sessions/backups/<migration-id>/`，写入后再次读取校验。
- 历史双写按语义指纹与源顺序对齐；合法重复事件不会按内容集合粗暴删除。
- 非法 JSON 或截断尾行原样进入当前 generation 的 `quarantine.jsonl`。
- 只有备份、事件计数、顺序和 generation 清单全部完成后，才更新 `store.json`。

迁移 ID 包含迁移器版本、相对路径、字节长度和源内容 SHA-256，因此相同输入可重复启动而不会重复导入。旧版程序在迁移后又向 legacy 文件追加记录时，下次启动只把新增后缀合并到新的 generation，同时保留上一代 canonical 事件。

不要删除 `sessions/backups`、`generations`、`quarantine.jsonl` 或 `store.json`。本发布周期内它们都是恢复和审计链的一部分。

## 本地 API 不兼容变更

升级后的本地协议为 `muse-local-api/v1`：

- 桌面 UI 改为 Tauri 内嵌资源，不再访问外部 localhost 首页。
- 桌面 API 每次启动绑定新的回环随机端口。
- 所有 API 要求精确 Host、允许的 Origin 和 Bearer；没有 Origin 的内部请求也必须鉴权。
- 流式聊天改为带 JSON 请求体的 `POST /api/chat/stream`。
- 旧 GET 流式接口返回 `405 Method Not Allowed` 和 `stream_post_required`，不会隐式创建回合。
- WebSocket 必须先通过 Bearer 换取 15 秒单次 ticket，并声明 `muse.runtime.v1` 子协议。
- 上传资源只能经鉴权 API 获取，不再从公开静态目录读取。

旧客户端必须升级 Bootstrap、SSE 和 WebSocket 接入逻辑。不要用固定端口、URL query 中的长期 Bearer 或未鉴权 health 检查模拟兼容；具体契约见 [本地 API 协议](local-api.md)。

## 验证升级结果

升级后建议确认：

- 桌面应用能从任意当前工作目录打开，不依赖源码或根目录 `dist`。
- `runtime_bootstrap.protocol_version` 为 `muse-local-api/v1`，health 的 `instance_id` 与本次 Bootstrap 一致。
- 角色、会话、模型配置和语音设置可读取。
- legacy 原目录仍存在且内容未被迁移过程改写。
- `~/.muse` 中没有因本次升级新建 `model-files` 或模型资产清单；旧模型仍留在 legacy 原目录。
- `sessions/store.json` 已指向有效 generation；若有坏记录，能在 quarantine 中找到且原始备份仍在。

## 回退注意事项

不要让旧版本直接写入已经升级的 v3 数据目录。需要回退时，先退出当前版本，再让旧版本使用升级前保留的原 `.agent-vp-data` 或独立备份；不要覆盖当前 `~/.muse`，也不要删除当前 generation。这样可以保留两侧数据，待确认后再选择后续迁移策略。
