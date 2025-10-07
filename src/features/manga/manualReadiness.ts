import type { ManualSplitReportSummary } from './customSplitDrawer/store.js';

type AutoSplitSnapshot = {
  emittedFiles?: number;
  splitPages?: number;
};

export type ManualCrossModeNotice = {
  tone: 'info' | 'warning';
  message: string;
};

export interface ManualCrossModeParams {
  autoSummary?: AutoSplitSnapshot | null;
  manualSummary?: ManualSplitReportSummary | null;
  manualReportPath?: string | null;
}

const formatCount = (value: number | undefined): string => {
  if (!Number.isFinite(value ?? NaN)) {
    return '0';
  }
  return String(value);
};

const describeGeneratedAt = (timestamp: string | undefined): string => {
  if (!timestamp) {
    return '';
  }
  try {
    const parsed = new Date(timestamp);
    if (Number.isNaN(parsed.getTime())) {
      return '';
    }
    return parsed.toLocaleString();
  } catch {
    return '';
  }
};

export const buildManualCrossModeNotice = (
  params: ManualCrossModeParams
): ManualCrossModeNotice | null => {
  const { autoSummary, manualSummary, manualReportPath } = params;

  if (!manualSummary || manualSummary.total <= 0) {
    return null;
  }

  const autoPages = autoSummary?.splitPages ?? autoSummary?.emittedFiles;
  const appliedLabel = `${formatCount(manualSummary.applied)} / ${formatCount(manualSummary.total)}`;
  const generatedAt = describeGeneratedAt(manualSummary.generatedAt);

  let message = `自动拆分输出 ${formatCount(autoPages ?? manualSummary.total)} 页，手动拆分已覆盖 ${appliedLabel}`;
  if (generatedAt) {
    message += `（生成于 ${generatedAt}）`;
  }
  message += '。';

  let tone: ManualCrossModeNotice['tone'] = 'info';

  if (manualSummary.applied < manualSummary.total) {
    const remaining = manualSummary.total - manualSummary.applied;
    message += `尚有 ${remaining} 张手动拆分未完成，请在上传前完成应用。`;
    tone = 'warning';
  } else if (autoPages && manualSummary.applied !== autoPages) {
    message += '自动拆分与手动拆分页数不一致，请确认后再继续。';
    tone = 'warning';
  } else {
    message += '可在抽屉中对比手动与自动输出，选择最佳结果。';
  }

  if (manualReportPath && manualReportPath.trim().length > 0) {
    message += ` 报告：${manualReportPath}`;
  }

  return { tone, message };
};
