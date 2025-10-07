import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { shallow } from 'zustand/shallow';

import {
  useCustomSplitStore,
  type ManualSplitDraftPayload,
  type ManualSplitReportSummary,
} from './store.js';
import { trackManualSplitTelemetry } from './telemetry.js';

interface ManualSplitSetupResponse {
  workspace: string;
  entries: ManualSplitDraftPayload[];
  manualSplitReportPath?: string | null;
  manualSplitReportSummary?: ManualSplitReportSummary | null;
  hasRevertHistory?: boolean;
}

export interface ManualSplitControllerOptions {
  sourceDirectory: string | null;
  multiVolume: boolean;
  onOpenDrawer: () => void;
}

export interface ManualSplitController {
  workspace: string | null;
  initializing: boolean;
  loadingDrafts: boolean;
  statusText: string;
  error: string | null;
  disableInitialize: boolean;
  disableReason: string | null;
  totalDrafts: number;
  appliedDrafts: number;
  lastAppliedAt: string | null;
  pendingDrafts: number;
  manualReportPath: string | null;
  manualReportSummary: ManualSplitReportSummary | null;
  canRevert: boolean;
  hasRevertHistory: boolean;
  initialize: (force?: boolean) => Promise<void>;
  openExisting: () => Promise<void>;
  clearError: () => void;
}

const DEFAULT_STATUS = '尚未创建手动拆分工作区。 / Manual workspace not created.';

export const useManualSplitController = (
  options: ManualSplitControllerOptions
): ManualSplitController => {
  const { sourceDirectory, multiVolume, onOpenDrawer } = options;
  const [initializing, setInitializing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const manualState = useCustomSplitStore(
    (state) => ({
      workspace: state.workspace,
      initialized: state.initialized,
      loading: state.loading,
      order: state.order,
      drafts: state.drafts,
    }),
    shallow
  );
  const setManualReport = useCustomSplitStore((state) => state.setManualReport);
  const manualReportPath = useCustomSplitStore((state) => state.manualReportPath);
  const manualReportSummary = useCustomSplitStore(
    (state) => state.manualReportSummary,
    shallow
  );
  const setCanRevert = useCustomSplitStore((state) => state.setCanRevert);
  const canRevert = useCustomSplitStore((state) => state.canRevert);
  const setHasRevertHistory = useCustomSplitStore((state) => state.setHasRevertHistory);
  const hasRevertHistory = useCustomSplitStore((state) => state.hasRevertHistory);

  const hydrateDrafts = useCustomSplitStore((state) => state.hydrateDrafts);
  const manualReset = useCustomSplitStore((state) => state.reset);

  const lastSourceRef = useRef<string | null>(null);

  useEffect(() => {
    const current = sourceDirectory ? sourceDirectory.trim() : '';
    const previous = lastSourceRef.current;

    if (!current) {
      if (manualState.workspace) {
        manualReset();
      }
      lastSourceRef.current = null;
      return;
    }

    if (previous && previous !== current) {
      manualReset();
      setError(null);
    }

    lastSourceRef.current = current;
  }, [manualReset, manualState.workspace, sourceDirectory]);

  const { totalDrafts, appliedDrafts, lastAppliedAt } = useMemo(() => {
    let applied = 0;
    let lastApplied: string | null = null;
    for (const key of manualState.order) {
      const draft = manualState.drafts[key];
      if (draft?.lastAppliedAt) {
        applied += 1;
        if (!lastApplied || draft.lastAppliedAt > lastApplied) {
          lastApplied = draft.lastAppliedAt;
        }
      }
    }
    return {
      totalDrafts: manualState.order.length,
      appliedDrafts: applied,
      lastAppliedAt: lastApplied,
    };
  }, [manualState.drafts, manualState.order]);

  const pendingDrafts = useMemo(() => {
    return Math.max(totalDrafts - appliedDrafts, 0);
  }, [appliedDrafts, totalDrafts]);

  const disableReason = useMemo(() => {
    if (multiVolume) {
      return '多卷目录暂不支持手动拆分，请切换单卷模式。 / Manual split is unavailable for multi-volume batches; switch to single-volume mode.';
    }
    if (!sourceDirectory || sourceDirectory.trim().length === 0) {
      return '请先完成目录选择或重命名。 / Select or rename the directory first.';
    }
    return null;
  }, [multiVolume, sourceDirectory]);

  const statusText = useMemo(() => {
    if (!manualState.workspace) {
      return DEFAULT_STATUS;
    }
    if (initializing) {
      return '正在初始化手动拆分工作区… / Initializing manual workspace…';
    }
    if (manualState.loading) {
      return '正在载入手动拆分数据… / Loading manual split data…';
    }
    if (!manualState.initialized) {
      return '工作区已创建，等待载入草稿。 / Workspace created, awaiting drafts.';
    }
    if (totalDrafts === 0) {
      return '工作区已就绪，但目录中未找到可用图片。 / Workspace ready, but no images were found.';
    }
    const appliedInfo = `已应用 ${appliedDrafts}/${totalDrafts} 张 / Applied ${appliedDrafts}/${totalDrafts}`;
    if (lastAppliedAt) {
      return `${appliedInfo}（最近 ${lastAppliedAt}） / Last applied ${lastAppliedAt}`;
    }
    return appliedInfo;
  }, [
    appliedDrafts,
    initializing,
    lastAppliedAt,
    manualState.initialized,
    manualState.loading,
    manualState.workspace,
    totalDrafts,
  ]);

  const initialize = useCallback(
    async (force = false) => {
      if (initializing) {
        return;
      }
      if (disableReason) {
        return;
      }
      if (!sourceDirectory) {
        setError('缺少重命名输出目录，无法创建工作区。 / Missing renamed output directory; cannot create workspace.');
        return;
      }

      setInitializing(true);
      setError(null);
      try {
        const response = await invoke<ManualSplitSetupResponse>(
          'prepare_manual_split_workspace',
          {
            request: {
              sourceDirectory,
              overwrite: force,
            },
          }
        );

        hydrateDrafts(response.workspace, response.entries ?? []);
        setManualReport({
          path: response.manualSplitReportPath ?? null,
          summary: response.manualSplitReportSummary ?? null,
        });
        const hasHistory = Boolean(response.hasRevertHistory);
        setHasRevertHistory(hasHistory);
        setCanRevert(hasHistory);
        void trackManualSplitTelemetry(
          'manual-split/initialized',
          {
            totalEntries: response.entries?.length ?? 0,
            hadExistingReport: Boolean(response.manualSplitReportPath),
            forced: force,
          },
          response.workspace
        );
        onOpenDrawer();
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
      } finally {
        setInitializing(false);
      }
    },
    [
      disableReason,
      hydrateDrafts,
      initializing,
      onOpenDrawer,
      setCanRevert,
      setManualReport,
      sourceDirectory,
    ]
  );

  const openExisting = useCallback(async () => {
    if (!manualState.workspace) {
      await initialize(false);
      return;
    }
    setError(null);
    onOpenDrawer();
  }, [initialize, manualState.workspace, onOpenDrawer]);

  const clearError = useCallback(() => {
    setError(null);
  }, []);

  return {
    workspace: manualState.workspace,
    initializing,
    loadingDrafts: manualState.loading,
    statusText,
    error,
    disableInitialize: Boolean(disableReason),
    disableReason,
    totalDrafts,
    appliedDrafts,
    lastAppliedAt,
    pendingDrafts,
    manualReportPath,
    manualReportSummary,
    canRevert,
    hasRevertHistory,
    initialize,
    openExisting,
    clearError,
  };
};
