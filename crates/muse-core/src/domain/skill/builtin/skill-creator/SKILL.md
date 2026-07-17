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
- 创建前先确认没有同名或同用途的 Skill：查看系统提示中的冻结目录，必要时用 load_skill 试探。

## 写入位置

- 唯一合法位置：运行环境上下文给出的「用户 Skill 目录」下的 `<skill-name>/SKILL.md`。
- 不要猜测路径；如果运行环境上下文中没有「用户 Skill 目录」一行，告知用户无法定位并停止。
- 该目录在工作区外，file_write 会触发用户审批；写入前先用一句话说明要创建的 Skill 名称和用途。

## 命名

- 规则：1-64 位小写字母、数字、单连字符，如 `git-release`；等价正则 `^[a-z0-9]+(-[a-z0-9]+)*$`。
- 目录名必须与 frontmatter 的 `name` 完全一致。
- 用动词短语或领域名词，见名知意，如 `weekly-report`、`pr-review`。
- `list` 和 `help` 是 load_skill 的保留参数，禁止用作名称。

## 文件格式

```markdown
---
name: skill-name
description: 一句话说明用途与触发场景
---

# 工作流

正文……
```

- frontmatter 只有 `name` 和 `description` 两个必填字段；不要写 `enabled`，启停由应用配置管理。
- description 决定以后能否被命中：写清"做什么 + 什么时候用它"，单行，不超过 1024 字符。
- 正文必须非空，整个文件不超过 128KB。

## 正文写法

- 面向模型写操作指引：先做什么、再做什么、边界和禁区、输出格式。
- 具体、可执行、有顺序；避免空泛口号。
- 只创建单个 SKILL.md；不要创建 scripts/、references/、assets/ 等辅助文件，当前运行时不会把它们暴露给模型。

## 更新已有 Skill

- 先 load_skill 或 file_read 读取现有 SKILL.md。
- 保留 frontmatter 中除 name/description 外的所有扩展字段（license、metadata 等），原样写回。
- 不要自行重命名或移动目录；需要重命名时引导用户在 Skill 管理页操作，管理页会保留辅助文件并同步配置。

## 创建之后

- 新 Skill 本轮不会进入冻结目录；不要调用 load_skill 验证，返回 skill_catalog_denied 是预期行为。
- 告知用户：已创建 `<skill-name>`，下一轮对话起生效，可在 Skill 管理页查看或停用。
- 如果下一轮 load_skill 返回校验错误（如缺少 description），按错误信息修正文件。
