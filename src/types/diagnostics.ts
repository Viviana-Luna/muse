// 诊断相关类型，描述设置中心连通性检测结果。

export interface DiagnosticsConnectivityItem {
  id: string;
  label: string;
  target: string;
  status: 'reachable' | 'failed' | 'skipped' | string;
  latency_ms?: number | null;
  message: string;
}

export interface DiagnosticsConnectivityResponse {
  checks: DiagnosticsConnectivityItem[];
}
