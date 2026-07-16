// 聊天相关基础类型，供首页消息流和会话模块复用。

export type Role = 'system' | 'user' | 'assistant';

export interface Message {
  role: Role;
  content: string;
}
