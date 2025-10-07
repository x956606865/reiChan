import { invoke } from '@tauri-apps/api/core';

export type ManualSplitTelemetryProperties = Record<string, unknown>;

export async function trackManualSplitTelemetry(
  event: string,
  properties: ManualSplitTelemetryProperties = {},
  workspace?: string | null
): Promise<void> {
  if (!event || event.trim().length === 0) {
    return;
  }

  try {
    await invoke('track_manual_split_event', {
      request: {
        event,
        properties,
        workspace: workspace ?? null,
      },
    });
  } catch (err) {
    // 遥测失败不阻塞主流程，打印调试信息即可。
    const meta = import.meta as unknown as { env?: { DEV?: boolean } };
    if (meta?.env?.DEV) {
      console.warn('manual-split telemetry failed', err);
    }
  }
}
