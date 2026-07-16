import type { DiagnosticsConnectivityResponse } from '@/types';

import { apiFetch, readJson } from './client';

// 设置中心诊断 API，只在用户显式刷新时发起连通性探测。
export async function fetchDiagnosticsConnectivity(): Promise<DiagnosticsConnectivityResponse> {
  return readJson<DiagnosticsConnectivityResponse>(await apiFetch('/api/diagnostics/connectivity'));
}
