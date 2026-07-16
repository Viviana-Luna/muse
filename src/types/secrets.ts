// 所有设置密钥都通过显式动作更新，避免空字符串同时表达“保留”和“删除”。

export interface SecretUpdate {
  action: 'keep' | 'replace' | 'delete';
  value?: string;
}
