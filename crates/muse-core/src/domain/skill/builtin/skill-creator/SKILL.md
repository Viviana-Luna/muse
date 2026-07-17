---
name: skill-creator
description: 创建新 Skill 的完整工艺：何时沉淀、命名规则、description 写法与 SKILL.md 结构。当用户想创建技能、把可复用工作流或某类任务的固定做法保存为 Skill 时使用。
---

# Skill 创建工艺

把可复用的工作流沉淀为 Skill，让以后的对话能自动命中。Skill 是写给模型的任务指引，不是代码。

## 何时创建

- 用户明确要求"创建 Skill"、"记住这个流程"或"沉淀这个做法"。
- 某类任务有稳定的多步打法，且用户表达了复用意图。
- 一次性任务、纯闲聊、没有复注意图时，不要主动创建。
- 创建前先确认没有同名或同用途的 Skill：查看系统提示中的冻结目录，必要时用 `load_skill` 读取候选项。

## 创建入口

- 使用 `create_skill` 提交 `name`、`description`、`content` 和可选的 `enabled`。
- 不要猜测或请求用户数据目录，也不要用 `file_write`、`command_run` 等通用工具绕过 Skill 存储。
- `create_skill` 会触发用户审批；调用前用一句话说明要创建的 Skill 名称和用途。
- 同名 Skill 已存在时不要覆盖。先读取现有内容，再引导用户在 Skill 管理页更新或重命名。

## 命名

- 规则：1-64 位小写字母、数字、单连字符，如 `git-release`；等价正则 `^[a-z0-9]+(-[a-z0-9]+)*$`。
- 目录名必须与 frontmatter 的 `name` 完全一致。
- 用动词短语或领域名词，见名知意，如 `weekly-report`、`pr-review`。
- `list` 和 `help` 是 load_skill 的保留参数，禁止用作名称。

## 参数格式

- `name`：合法 Skill 名称，不要自行拼接目录或文件名。
- description 决定以后能否被命中：写清"做什么 + 什么时候用它"，单行，不超过 1024 字符。
- `content`：只写 frontmatter 之后的 Markdown 正文，不要重复写 `---`、`name` 或 `description`；正文必须非空。
- `enabled`：默认 `true`；只有用户明确要求先保存但不启用时才传 `false`。
- Muse 会生成并校验完整 `SKILL.md`，文档总大小不得超过 128KB。

## 正文写法

- 面向模型写操作指引：先做什么、再做什么、边界和禁区、输出格式。
- 具体、可执行、有顺序；避免空泛口号。
- 当前工具只创建单个 `SKILL.md`；不要承诺同时生成 `scripts/`、`references/`、`assets/` 等辅助资源。

## 已有 Skill

- `create_skill` 不覆盖已有 Skill。出现同名冲突时，停止重复创建并说明冲突。
- 需要更新、重命名、启停或删除时，引导用户在 Skill 管理页操作；不要用通用文件工具直接改内部目录。

## 创建之后

- `create_skill` 成功即表示服务端已经完成格式校验和原子发布，不要在本轮调用 `load_skill` 重复验证。
- 新 Skill 不会改变当前 Turn 的冻结目录；告知用户它从下一轮对话起可加载，并可在 Skill 管理页查看或停用。
- 创建失败时依据工具返回的稳定原因修正输入；同名冲突和存储错误不得靠换路径或通用文件工具规避。
