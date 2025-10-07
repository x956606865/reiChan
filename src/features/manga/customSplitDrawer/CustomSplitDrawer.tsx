import type { FC } from 'react';
import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { save, open as openDialog } from '@tauri-apps/plugin-dialog';
import { listen } from '@tauri-apps/api/event';
import type { UnlistenFn } from '@tauri-apps/api/event';

import ManualSplitToolbar from './ManualSplitToolbar.js';
import SplitCanvas from './SplitCanvas.js';
import SplitPreviewPane from './SplitPreviewPane.js';
import SplitSettingsPanel from './SplitSettingsPanel.js';
import SplitThumbnailGrid, {
  type SplitThumbnailGridItem,
} from './SplitThumbnailGrid.js';
import {
  useCustomSplitStore,
  type ManualSplitDraft,
  type ManualSplitDraftPayload,
  type ManualImageKind,
  type ManualSplitLines,
  type SplitPreviewPayload,
  type ManualSplitReportSummary,
} from './store.js';
import { trackManualSplitTelemetry } from './telemetry.js';

interface CustomSplitDrawerProps {
  workspace: string | null;
  open: boolean;
  onClose: () => void;
}

interface ManualSplitContextResponse {
  workspace: string;
  entries: ManualSplitDraftPayload[];
  manualSplitReportPath?: string | null;
  manualSplitReportSummary?: ManualSplitReportSummary | null;
  hasRevertHistory?: boolean;
}

interface ManualSplitPreviewResponse {
  sourcePath: string;
  leftPreviewPath?: string | null;
  rightPreviewPath?: string | null;
  gutterPreviewPath?: string | null;
  generatedAt: string;
}

interface ManualSplitApplyProgressPayload {
  workspace: string;
  total: number;
  completed: number;
  current?: string | null;
}

interface ManualSplitApplyStartedPayload {
  workspace: string;
  total: number;
}

interface ManualSplitApplyFailedPayload {
  workspace: string;
  message: string;
}

interface ManualSplitApplyEntry {
  sourcePath: string;
  outputs: string[];
  appliedAt: string;
  lines: ManualSplitLines;
  pixels: [number, number, number, number];
  accelerator: 'cpu' | 'gpu';
  width: number;
  height: number;
  durationMs?: number | null;
  imageKind: ManualImageKind;
  rotate90: boolean;
}

interface ManualSplitApplyResponse {
  workspace: string;
  applied: ManualSplitApplyEntry[];
  skipped: string[];
  manualOverridesPath?: string | null;
  splitReportPath?: string | null;
  manualSplitReportPath?: string | null;
  manualSplitReportSummary?: ManualSplitReportSummary | null;
  canRevert?: boolean;
}

interface ManualSplitTemplateExportResponse {
  outputPath: string;
  entryCount: number;
}

interface ManualSplitTemplateEntryPayload {
  source: string;
  lines: ManualSplitLines;
  locked?: boolean;
  displayName?: string | null;
  width?: number | null;
  height?: number | null;
  imageKind?: ManualImageKind | null;
  rotate90?: boolean | null;
}

interface ManualSplitTemplateFile {
  generatedAt?: string;
  workspace?: string;
  accelerator?: string | null;
  gutterRatio?: number | null;
  entryCount?: number;
  entries?: ManualSplitTemplateEntryPayload[];
}

interface ManualSplitRevertResponse {
  workspace: string;
  restoredOutputs: number;
  manualSplitReportPath?: string | null;
  manualSplitReportSummary?: ManualSplitReportSummary | null;
}

interface AcceleratorSummary {
  gpu: number;
  cpu: number;
  preference: 'cpu' | 'gpu' | 'auto';
}

interface PreviewJob {
  workspace: string;
  sourcePath: string;
  lines: ManualSplitLines;
  requestId: number;
  signature: string;
}

const PREVIEW_MAX_CONCURRENCY = 6;

const CustomSplitDrawer: FC<CustomSplitDrawerProps> = memo(
  ({ workspace, open, onClose }) => {
    const {
      drafts,
      order,
      selection,
      accelerator,
      gutterWidthRatio,
      loading,
      error,
      previewMap,
      workspace: storeWorkspace,
      initialized,
      drawerOpen,
      openDrawer,
      closeDrawer,
      setSelection,
      toggleSelection,
      hydrateDrafts,
      setAccelerator,
      setPreview,
      stageDraft,
      applyCurrentToAllUnlocked,
      clearStage,
      clearAllStages,
      setImageKind,
      toggleLock,
      setLockState,
      setLoading,
      setError,
      updateLines,
      markApplied,
      applyState,
      beginApply,
      resolveApplySucceeded,
      resolveApplyFailed,
      clearApplyFeedback,
      undoLines,
      redoLines,
      resetLines,
      resetAllLines,
      setManualReport,
      canRevert,
      hasRevertHistory,
      setCanRevert,
      setHasRevertHistory,
      setGutterWidthRatio,
    } = useCustomSplitStore();

    const [previewLoading, setPreviewLoading] = useState(false);
    const [reverting, setReverting] = useState(false);
    const [exportingTemplate, setExportingTemplate] = useState(false);
    const [importingTemplate, setImportingTemplate] = useState(false);
    const previewRequestRef = useRef(0);
    const previewQueueRef = useRef<PreviewJob[]>([]);
    const activePreviewCountRef = useRef(0);
    const lastPreviewSignatureRef = useRef<Map<string, string>>(new Map());
    const applyStartRef = useRef<number | null>(null);

    useEffect(() => {
      if (open) {
        openDrawer();
      } else {
        closeDrawer();
        clearApplyFeedback();
      }
    }, [open, openDrawer, closeDrawer, clearApplyFeedback]);

    useEffect(() => {
      if (!open || !workspace) {
        return;
      }
      void trackManualSplitTelemetry(
        'manual-split/opened',
        {
          draftCount: order.length,
          selectionCount: selection.length,
        },
        workspace
      );
    }, [open, workspace, order.length, selection.length]);

    const loadContext = useCallback(async () => {
      if (!workspace) {
        setError('未找到拆分工作区。 / Manual workspace not found.');
        return;
      }
      setLoading(true);
      try {
        const response = await invoke<ManualSplitContextResponse>(
          'load_manual_split_context',
          {
            request: {
              workspace,
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
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    }, [workspace, hydrateDrafts, setCanRevert, setError, setHasRevertHistory, setLoading]);

    useEffect(() => {
      if (!open) {
        return;
      }
      if (!workspace) {
        setError('请先初始化手动拆分工作区。 / Please initialize the manual workspace first.');
        return;
      }
      if (!initialized || storeWorkspace !== workspace) {
        void loadContext();
      }
    }, [open, workspace, initialized, storeWorkspace, loadContext, setError]);

    useEffect(() => {
      if (!workspace) {
        previewQueueRef.current = [];
        activePreviewCountRef.current = 0;
        setPreviewLoading(false);
        previewRequestRef.current = 0;
        lastPreviewSignatureRef.current.clear();
      }
    }, [workspace]);

    const composeApplyStatus = useCallback(
      (
        appliedCount: number,
        skippedCount: number,
        manualReportPath?: string | null,
        acceleratorSummary?: AcceleratorSummary | null
      ): string | null => {
        let message: string | null;
        if (appliedCount === 0 && skippedCount === 0) {
          message = '未应用任何图片。 / No pages were applied.';
        } else if (skippedCount > 0) {
          message = `已应用 ${appliedCount} 张图片，跳过 ${skippedCount} 张。 / Applied ${appliedCount} page(s), skipped ${skippedCount}.`;
        } else {
          message = `已应用 ${appliedCount} 张图片。 / Applied ${appliedCount} page(s).`;
        }

        if (acceleratorSummary) {
          const { gpu, cpu, preference } = acceleratorSummary;
          const segments: string[] = [];
          const preferenceLabel =
            preference === 'gpu'
              ? '偏好：GPU / Preference: GPU'
              : preference === 'cpu'
              ? '偏好：CPU / Preference: CPU'
              : '偏好：自动 / Preference: Auto';
          segments.push(preferenceLabel);
          if (gpu > 0) {
            segments.push(`GPU 命中 ${gpu} 张 / GPU hits ${gpu}`);
          }
          if (cpu > 0) {
            segments.push(`CPU 命中 ${cpu} 张 / CPU hits ${cpu}`);
          }
          if (appliedCount > 0 && gpu === 0) {
            if (preference === 'gpu') {
              segments.push('未命中 GPU，已回退至 CPU / GPU unavailable, fell back to CPU');
            } else if (preference === 'auto') {
              segments.push('自动模式未命中 GPU / Auto mode ran on CPU');
            }
          }
          if (segments.length > 0) {
            const detail = segments.join('；');
            message = message ? `${message} ${detail}` : detail;
          }
        }

        if (manualReportPath && manualReportPath.trim().length > 0) {
          message = message
            ? `${message} 报告：${manualReportPath} / Report: ${manualReportPath}`
            : `报告：${manualReportPath} / Report: ${manualReportPath}`;
        }
        return message;
      },
      []
    );

    useEffect(() => {
      let disposed = false;
      const unlistenRefs: UnlistenFn[] = [];

      const register = async () => {
        try {
          const handleStarted = await listen<ManualSplitApplyStartedPayload>(
            'manual-split/apply-started',
            (event) => {
              const store = useCustomSplitStore.getState();
              if (event.payload.workspace !== store.workspace) {
                return;
              }
              applyStartRef.current = performance.now();
              void trackManualSplitTelemetry(
                'manual-split/apply-started',
                {
                  total: event.payload.total,
                },
                store.workspace
              );
              store.setError(null);
              store.clearApplyFeedback();
              store.beginApply(event.payload.total);
            }
          );

          const handleProgress = await listen<ManualSplitApplyProgressPayload>(
            'manual-split/apply-progress',
            (event) => {
              const store = useCustomSplitStore.getState();
              if (event.payload.workspace !== store.workspace) {
                return;
              }
              store.updateApplyProgress(
                event.payload.completed,
                event.payload.total,
                event.payload.current ?? null
              );
              const shouldLog =
                event.payload.total <= 5 ||
                event.payload.completed === 0 ||
                event.payload.completed === event.payload.total ||
                event.payload.completed % 5 === 0;
              if (shouldLog) {
                void trackManualSplitTelemetry(
                  'manual-split/apply-progress',
                  {
                    completed: event.payload.completed,
                    total: event.payload.total,
                    current: event.payload.current ?? null,
                  },
                  store.workspace
                );
              }
            }
          );

          const handleSucceeded = await listen<ManualSplitApplyResponse>(
            'manual-split/apply-succeeded',
            (event) => {
              const store = useCustomSplitStore.getState();
              if (event.payload.workspace !== store.workspace) {
                return;
              }
              const existingHistory = store.hasRevertHistory;

              const appliedEntries = event.payload.applied ?? [];
              if (appliedEntries.length > 0) {
                store.markApplied(
                  appliedEntries.map((entry) => ({
                    sourcePath: entry.sourcePath,
                    appliedAt: entry.appliedAt,
                    lines: entry.lines,
                    imageKind: entry.imageKind,
                    rotate90: entry.rotate90,
                  }))
                );
              }

              store.setManualReport({
                path: event.payload.manualSplitReportPath ?? null,
                summary: event.payload.manualSplitReportSummary ?? null,
              });
              if (event.payload.canRevert) {
                store.setHasRevertHistory(true);
              }
              const nextCanRevert = Boolean(event.payload.canRevert)
                ? true
                : existingHistory;
              store.setCanRevert(nextCanRevert);
              store.setError(null);

              const skipped = event.payload.skipped ?? [];
              const appliedCount = appliedEntries.length;
              const total = appliedCount + skipped.length;
              const gpuHits = appliedEntries.filter(
                (entry) => entry.accelerator === 'gpu'
              ).length;
              const cpuHits = Math.max(appliedCount - gpuHits, 0);
              const status = composeApplyStatus(
                appliedCount,
                skipped.length,
                event.payload.manualSplitReportPath ?? null,
                {
                  gpu: gpuHits,
                  cpu: cpuHits,
                  preference: store.accelerator,
                }
              );

              store.resolveApplySucceeded(status, appliedCount, total);

              const duration =
                applyStartRef.current !== null
                  ? Math.max(0, performance.now() - applyStartRef.current)
                  : null;
              applyStartRef.current = null;

              void trackManualSplitTelemetry(
                'manual-split/apply-succeeded',
                {
                  applied: appliedCount,
                  skipped: skipped.length,
                  total,
                  durationMs: duration !== null ? Math.round(duration) : undefined,
                  gpuApplied: gpuHits,
                  cpuApplied: cpuHits,
                  reportGenerated: Boolean(event.payload.manualSplitReportPath),
                },
                store.workspace
              );
            }
          );

          const handleFailed = await listen<ManualSplitApplyFailedPayload>(
            'manual-split/apply-failed',
            (event) => {
              const store = useCustomSplitStore.getState();
              if (event.payload.workspace !== store.workspace) {
                return;
              }
              store.resolveApplyFailed(event.payload.message);
              store.setError(event.payload.message);
              const duration =
                applyStartRef.current !== null
                  ? Math.max(0, performance.now() - applyStartRef.current)
                  : null;
              applyStartRef.current = null;
              void trackManualSplitTelemetry(
                'manual-split/apply-failed',
                {
                  message: event.payload.message,
                  durationMs: duration !== null ? Math.round(duration) : undefined,
                },
                store.workspace
              );
            }
          );

          unlistenRefs.push(
            handleStarted,
            handleProgress,
            handleSucceeded,
            handleFailed
          );
        } catch (err) {
          if (!disposed) {
            setError(err instanceof Error ? err.message : String(err));
          }
        }
      };

      void register();

      return () => {
        disposed = true;
        for (const unlisten of unlistenRefs) {
          void unlisten();
        }
      };
    }, [composeApplyStatus, setError]);

    const listItems: SplitThumbnailGridItem[] = useMemo(() => {
      return order
        .map((sourcePath: string) => drafts[sourcePath])
        .filter((draft): draft is ManualSplitDraft => Boolean(draft))
        .map((draft: ManualSplitDraft) => {
          const previewPath = draft.thumbnailPath || draft.sourcePath;
          return {
            ...draft,
            thumbnailUrl: previewPath ? convertFileSrc(previewPath) : null,
          };
        });
    }, [drafts, order]);

    const activeDraft = useMemo(() => {
      if (selection.length === 0) {
        return null;
      }
      return drafts[selection[0]] ?? null;
    }, [drafts, selection]);

    const activePreview: SplitPreviewPayload | null = useMemo(() => {
      if (!activeDraft) {
        return null;
      }
      return previewMap[activeDraft.sourcePath] ?? null;
    }, [activeDraft, previewMap]);

    const totalDrafts = order.length;

    const activeImageKind = activeDraft?.imageKind ?? 'content';
    const activeStaged = activeDraft?.staged ?? false;
    const activeHasPending = activeDraft?.hasPendingChanges ?? false;

    const dirtyCount = useMemo(() => {
      let count = 0;
      for (const sourcePath of order) {
        if (drafts[sourcePath]?.hasPendingChanges) {
          count += 1;
        }
      }
      return count;
    }, [drafts, order]);

    const stagedCount = useMemo(() => {
      let count = 0;
      for (const sourcePath of order) {
        if (drafts[sourcePath]?.staged) {
          count += 1;
        }
      }
      return count;
    }, [drafts, order]);

    const stagedAny = stagedCount > 0;

    const lockedCount = useMemo(() => {
      let count = 0;
      for (const sourcePath of order) {
        if (drafts[sourcePath]?.locked) {
          count += 1;
        }
      }
      return count;
    }, [order, drafts]);

    const actionableCount = useMemo(() => {
      return Math.max(totalDrafts - lockedCount, 0);
    }, [totalDrafts, lockedCount]);

    const processPreviewQueue = useCallback(() => {
      while (
        activePreviewCountRef.current < PREVIEW_MAX_CONCURRENCY &&
        previewQueueRef.current.length > 0
      ) {
        const job = previewQueueRef.current.shift();
        if (!job) {
          break;
        }
        activePreviewCountRef.current += 1;

        void (async () => {
          const startedAt = performance.now();
          try {
            const response = await invoke<ManualSplitPreviewResponse>(
              'render_manual_split_preview',
              {
                request: {
                  workspace: job.workspace,
                  sourcePath: job.sourcePath,
                  lines: job.lines,
                  targetWidth: 1024,
                },
              }
            );
            if (previewRequestRef.current !== job.requestId) {
              return;
            }
            const payload: SplitPreviewPayload = {
              sourcePath: response.sourcePath,
              leftPreviewPath: response.leftPreviewPath ?? null,
              rightPreviewPath: response.rightPreviewPath ?? null,
              gutterPreviewPath: response.gutterPreviewPath ?? null,
              generatedAt: response.generatedAt,
            };
            setPreview(job.sourcePath, payload);
            const duration = Math.max(0, performance.now() - startedAt);
            void trackManualSplitTelemetry(
              'manual-split/preview',
              {
                sourcePath: job.sourcePath,
                durationMs: Math.round(duration),
                succeeded: true,
              },
              job.workspace
            );
          } catch (err) {
            const duration = Math.max(0, performance.now() - startedAt);
            const isActive = previewRequestRef.current === job.requestId;
            if (isActive) {
              setError(err instanceof Error ? err.message : String(err));
              if (lastPreviewSignatureRef.current.get(job.sourcePath) === job.signature) {
                lastPreviewSignatureRef.current.delete(job.sourcePath);
              }
            }
            void trackManualSplitTelemetry(
              'manual-split/preview',
              {
                sourcePath: job.sourcePath,
                durationMs: Math.round(duration),
                succeeded: false,
                error: err instanceof Error ? err.message : String(err),
                cancelled: !isActive,
              },
              job.workspace
            );
          } finally {
            activePreviewCountRef.current = Math.max(
              0,
              activePreviewCountRef.current - 1
            );
            if (
              previewQueueRef.current.length === 0 &&
              activePreviewCountRef.current === 0
            ) {
              setPreviewLoading(false);
            }
            processPreviewQueue();
          }
        })();
      }
    }, [setError, setPreview]);

    const requestPreview = useCallback(
      (draft: ManualSplitDraft) => {
        if (!workspace) {
          return;
        }
        const requestId = ++previewRequestRef.current;
        const snapshot = [...draft.lines] as ManualSplitLines;
        const signature = snapshot.map((value) => value.toFixed(4)).join(':');
        const lastSignature = lastPreviewSignatureRef.current.get(draft.sourcePath);
        if (lastSignature === signature) {
          return;
        }
        previewQueueRef.current = previewQueueRef.current.filter(
          (job) => job.sourcePath !== draft.sourcePath
        );
        lastPreviewSignatureRef.current.set(draft.sourcePath, signature);
        previewQueueRef.current.push({
          workspace,
          sourcePath: draft.sourcePath,
          lines: snapshot,
          requestId,
          signature,
        });
        setPreviewLoading(true);
        processPreviewQueue();
      },
      [workspace, processPreviewQueue]
    );

    const handleSelect = useCallback(
      (sourcePath: string, multi: boolean) => {
        if (multi) {
          toggleSelection(sourcePath);
        } else {
          setSelection([sourcePath]);
        }
      },
      [setSelection, toggleSelection]
    );

    const handleLinesChange = useCallback(
      (lines: ManualSplitLines) => {
        if (!activeDraft) {
          return;
        }
        updateLines(activeDraft.sourcePath, lines);
      },
      [activeDraft, updateLines]
    );

    const handleAcceleratorChange = useCallback(
      (value: 'cpu' | 'gpu' | 'auto') => {
        setAccelerator(value);
      },
      [setAccelerator]
    );

    const handleToggleLock = useCallback(() => {
      if (!activeDraft) {
        return;
      }
      toggleLock(activeDraft.sourcePath);
    }, [activeDraft, toggleLock]);

    const handleStageCurrent = useCallback(() => {
      if (!activeDraft) {
        return;
      }
      stageDraft(activeDraft.sourcePath);
    }, [activeDraft, stageDraft]);

    const handleClearCurrentStage = useCallback(() => {
      if (!activeDraft) {
        return;
      }
      clearStage(activeDraft.sourcePath);
    }, [activeDraft, clearStage]);

    const handleApplyAllUnlocked = useCallback(() => {
      if (!activeDraft) {
        return;
      }
      applyCurrentToAllUnlocked(activeDraft.sourcePath);
    }, [activeDraft, applyCurrentToAllUnlocked]);

    const handleClearAllStages = useCallback(() => {
      clearAllStages();
    }, [clearAllStages]);

    const handleImageKindChange = useCallback(
      (kind: ManualImageKind) => {
        if (!activeDraft) {
          return;
        }
        setImageKind(activeDraft.sourcePath, kind);
      },
      [activeDraft, setImageKind]
    );

    const handleGeneratePreview = useCallback(() => {
      if (!workspace || !activeDraft) {
        return;
      }
      requestPreview(activeDraft);
    }, [activeDraft, requestPreview, workspace]);

    const handleComplete = useCallback(() => {
      if (!workspace) {
        setError('请先初始化手动拆分工作区。 / Please initialize the manual workspace first.');
        return;
      }
      if (applyState.running) {
        return;
      }

      const stagedDrafts = order
        .map((sourcePath) => drafts[sourcePath])
        .filter((draft): draft is ManualSplitDraft => Boolean(draft && draft.staged));

      if (stagedDrafts.length === 0) {
        resolveApplySucceeded('没有需要完成的草稿。 / No staged drafts to complete.', 0, 0);
        return;
      }

      const confirmed = window.confirm(
        `将完成 ${stagedDrafts.length} 张手动裁剪草稿，继续吗？ / Complete manual splits for ${stagedDrafts.length} page(s)?`
      );
      if (!confirmed) {
        resolveApplySucceeded('已取消完成操作。 / Completion cancelled.', 0, stagedDrafts.length);
        return;
      }

      const overridesPayload = stagedDrafts.map((draft) => ({
        source: draft.sourcePath,
        leftTrim: draft.stagedLines[0],
        leftPageEnd: draft.stagedLines[1],
        rightPageStart: draft.stagedLines[2],
        rightTrim: draft.stagedLines[3],
        gutterRatio: Math.max(draft.stagedLines[2] - draft.stagedLines[1], 0),
        locked: draft.locked,
        imageKind: draft.stagedImageKind,
        rotate90: draft.stagedRotate90,
      }));

      clearApplyFeedback();
      setError(null);

      void (async () => {
        try {
          const response = await invoke<ManualSplitApplyResponse>('apply_manual_splits', {
            request: {
              workspace,
              overrides: overridesPayload,
              accelerator,
              generatePreview: false,
            },
          });

          if (response.applied.length > 0) {
            markApplied(
              response.applied.map((entry) => ({
                sourcePath: entry.sourcePath,
                appliedAt: entry.appliedAt,
                lines: entry.lines,
                imageKind: entry.imageKind,
                rotate90: entry.rotate90,
              }))
            );
          }

          setManualReport({
            path: response.manualSplitReportPath ?? null,
            summary: response.manualSplitReportSummary ?? null,
          });
          setCanRevert(Boolean(response.canRevert));

          const skipped = response.skipped ?? [];
          const appliedCount = response.applied.length;
          const total = appliedCount + skipped.length;
          const gpuHits = response.applied.filter((entry) => entry.accelerator === 'gpu').length;
          const cpuHits = Math.max(appliedCount - gpuHits, 0);
          const status = composeApplyStatus(
            appliedCount,
            skipped.length,
            response.manualSplitReportPath ?? null,
            {
              gpu: gpuHits,
              cpu: cpuHits,
              preference: accelerator,
            }
          );

          resolveApplySucceeded(status, appliedCount, total);
        } catch (invokeError) {
          const message =
            invokeError instanceof Error ? invokeError.message : String(invokeError);
          resolveApplyFailed(message);
          setError(message);
        }
      })();
    }, [
      accelerator,
      applyState.running,
      clearApplyFeedback,
      composeApplyStatus,
      drafts,
      markApplied,
      order,
      resolveApplyFailed,
      resolveApplySucceeded,
      setCanRevert,
      setError,
      setManualReport,
      workspace,
    ]);

    const handleUndo = useCallback(() => {
      if (!activeDraft) {
        return;
      }
      undoLines(activeDraft.sourcePath);
    }, [activeDraft, undoLines]);

    const handleRedo = useCallback(() => {
      if (!activeDraft) {
        return;
      }
      redoLines(activeDraft.sourcePath);
    }, [activeDraft, redoLines]);

    const handleResetCurrent = useCallback(() => {
      if (!activeDraft) {
        return;
      }
      resetLines(activeDraft.sourcePath);
    }, [activeDraft, resetLines]);

    const handleResetAll = useCallback(() => {
      if (dirtyCount === 0) {
        return;
      }
      const confirmed = window.confirm(
        '将清除全部未应用的手动拆分修改，确定继续吗？ / This will discard all pending manual splits. Continue?'
      );
      if (!confirmed) {
        return;
      }
      resetAllLines();
    }, [dirtyCount, resetAllLines]);

    const handleExportTemplate = useCallback(() => {
      if (!workspace) {
        setError('尚未加载手动拆分工作区，无法导出模板。 / Manual workspace unavailable; cannot export template.');
        return;
      }
      const targets = selection.length > 0 ? selection : order;
      const draftsToExport = targets
        .map((sourcePath) => drafts[sourcePath])
        .filter((draft): draft is ManualSplitDraft => Boolean(draft));
      if (draftsToExport.length === 0) {
        setError('暂无可导出的拆分草稿。 / No drafts available for export.');
        return;
      }
      setExportingTemplate(true);
      void (async () => {
        try {
          const defaultName = `manual-template-${new Date()
            .toISOString()
            .replace(/[:.]/g, '-')}.json`;
          const outputPath = await save({
            defaultPath: defaultName,
            filters: [{ name: 'JSON', extensions: ['json'] }],
          });
          if (!outputPath) {
            return;
          }
          const entries = draftsToExport.map((draft) => ({
            source: draft.sourcePath,
            lines: draft.lines,
            locked: draft.locked,
            displayName: draft.displayName,
            width: draft.width,
            height: draft.height,
            imageKind: draft.imageKind,
            rotate90: draft.rotate90,
          }));
          const response = await invoke<ManualSplitTemplateExportResponse>(
            'export_manual_split_template',
            {
              request: {
                workspace,
                outputPath,
                gutterRatio: gutterWidthRatio,
                accelerator,
                entries,
              },
            }
          );
          void trackManualSplitTelemetry(
            'manual-split/template-exported',
            {
              entryCount: response.entryCount,
              outputPath: response.outputPath,
              selectionCount: draftsToExport.length,
            },
            workspace
          );
          window.alert(`模板已导出到：${response.outputPath}`);
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
            setError(`模板导出失败：${message} / Template export failed: ${message}`);
          void trackManualSplitTelemetry(
            'manual-split/template-exported',
            {
              success: false,
              error: message,
            },
            workspace
          );
        } finally {
          setExportingTemplate(false);
        }
      })();
    }, [
      accelerator,
      drafts,
      gutterWidthRatio,
      order,
      selection,
      setError,
      workspace,
    ]);

    const handleImportTemplate = useCallback(() => {
      if (!workspace) {
        setError('尚未加载手动拆分工作区，无法导入模板。 / Manual workspace unavailable.');
        return;
      }
      setImportingTemplate(true);
      void (async () => {
        try {
          const selected = await openDialog({
            multiple: false,
            filters: [{ name: 'JSON 模板 / Template', extensions: ['json'] }],
          });
          const selectedPath = Array.isArray(selected) ? selected[0] : selected;
          if (!selectedPath) {
            return;
          }
          const content = await invoke<string>('read_template_file', {
            path: selectedPath,
          });
          let parsed: ManualSplitTemplateFile;
          try {
            parsed = JSON.parse(content) as ManualSplitTemplateFile;
          } catch (parseError) {
            setError('模板解析失败，请检查文件内容。 / Failed to parse template file.');
            return;
          }

          const entries = Array.isArray(parsed.entries) ? parsed.entries : [];
          if (entries.length === 0) {
            setError('模板中没有可导入的条目。 / Template contains no entries.');
            return;
          }

          if (
            typeof parsed.gutterRatio === 'number' &&
            Number.isFinite(parsed.gutterRatio)
          ) {
            setGutterWidthRatio(parsed.gutterRatio);
          }

          if (parsed.accelerator) {
            const normalizedPref = parsed.accelerator.toLowerCase();
            if (
              normalizedPref === 'auto' ||
              normalizedPref === 'cpu' ||
              normalizedPref === 'gpu'
            ) {
              setAccelerator(normalizedPref);
            }
          }

          const draftsByExact = new Map<string, ManualSplitDraft>();
          const draftsByNormalized = new Map<string, ManualSplitDraft>();
          const draftsByBase = new Map<string, ManualSplitDraft[]>();

          for (const sourcePath of order) {
            const draft = drafts[sourcePath];
            if (!draft) {
              continue;
            }
            draftsByExact.set(sourcePath, draft);
            const normalized = sourcePath.replace(/\\/g, '/');
            draftsByNormalized.set(normalized, draft);
            draftsByNormalized.set(normalized.toLowerCase(), draft);
            const base = normalized.split('/').pop();
            if (base) {
              const lower = base.toLowerCase();
              const existing = draftsByBase.get(lower) ?? [];
              existing.push(draft);
              draftsByBase.set(lower, existing);
            }
          }

          const matchedSources = new Set<string>();
          let appliedLineUpdates = 0;
          let lockUpdates = 0;
          const unmatched: string[] = [];

          const resolveDraft = (entry: ManualSplitTemplateEntryPayload): ManualSplitDraft | null => {
            const trySource = (value: string | undefined | null): ManualSplitDraft | null => {
              if (!value) {
                return null;
              }
              if (draftsByExact.has(value)) {
                return draftsByExact.get(value) ?? null;
              }
              const normalized = value.replace(/\\/g, '/');
              if (draftsByExact.has(normalized)) {
                return draftsByExact.get(normalized) ?? null;
              }
              const lower = normalized.toLowerCase();
              if (draftsByNormalized.has(lower)) {
                return draftsByNormalized.get(lower) ?? null;
              }
              if (draftsByNormalized.has(normalized)) {
                return draftsByNormalized.get(normalized) ?? null;
              }
              return null;
            };

            let target = trySource(entry.source);
            if (target) {
              return target;
            }

            const candidates: string[] = [];
            if (entry.source) {
              const base = entry.source.split(/[/\\]/).pop();
              if (base) {
                candidates.push(base);
              }
            }
            if (entry.displayName) {
              candidates.push(entry.displayName);
            }

            for (const candidate of candidates) {
              const lower = candidate.toLowerCase();
              const pool = draftsByBase.get(lower);
              if (!pool || pool.length === 0) {
                continue;
              }
              if (pool.length === 1) {
                return pool[0];
              }
              if (entry.width && entry.height) {
                const sized = pool.find(
                  (draft) => draft.width === entry.width && draft.height === entry.height
                );
                if (sized) {
                  return sized;
                }
              }
            }

            return null;
          };

          for (const entry of entries) {
            const targetDraft = resolveDraft(entry);
            if (!targetDraft) {
              unmatched.push(entry.source ?? entry.displayName ?? '未知条目 / Unknown entry');
              continue;
            }
            if (matchedSources.has(targetDraft.sourcePath)) {
              continue;
            }
            matchedSources.add(targetDraft.sourcePath);

            if (
              entry.imageKind === 'content' ||
              entry.imageKind === 'cover' ||
              entry.imageKind === 'spread'
            ) {
              setImageKind(targetDraft.sourcePath, entry.imageKind);
            }

            if (Array.isArray(entry.lines) && entry.lines.length === 4) {
              const normalizedLines = [
                Number(entry.lines[0]),
                Number(entry.lines[1]),
                Number(entry.lines[2]),
                Number(entry.lines[3]),
              ] as ManualSplitLines;
              updateLines(targetDraft.sourcePath, normalizedLines);
              appliedLineUpdates += 1;
            }

            if (typeof entry.locked === 'boolean') {
              setLockState(targetDraft.sourcePath, entry.locked);
              lockUpdates += 1;
            }
          }

          if (matchedSources.size > 0) {
            setSelection([Array.from(matchedSources)[0]]);
          }

          void trackManualSplitTelemetry(
            'manual-split/template-imported',
            {
              matched: matchedSources.size,
              unmatched: unmatched.length,
              appliedLines: appliedLineUpdates,
              lockUpdates,
              filePath: selectedPath,
            },
            workspace
          );

          const summaryMessage = `模板导入完成：匹配 ${matchedSources.size} 张，未匹配 ${unmatched.length} 张。/ Template import complete: matched ${matchedSources.size}, unmatched ${unmatched.length}.`;
          window.alert(summaryMessage);
          if (unmatched.length > 0) {
            setError(
              `以下条目未能匹配：${unmatched.join(
                ', '
              )} / Unmatched entries: ${unmatched.join(', ')}`
            );
          } else {
            setError(null);
          }
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          setError(`模板导入失败：${message} / Template import failed: ${message}`);
        } finally {
          setImportingTemplate(false);
        }
      })();
    }, [
      drafts,
      order,
      setAccelerator,
      setError,
      setGutterWidthRatio,
      setImageKind,
      setLockState,
      setSelection,
      updateLines,
      workspace,
    ]);

    void handleExportTemplate;
    void handleImportTemplate;

    const handleRevert = useCallback(() => {
      if (!workspace) {
        setError('尚未加载手动拆分工作区，无法回滚。 / Manual workspace unavailable; cannot revert.');
        return;
      }
      const confirmed = window.confirm(
        '将回滚至最近一次应用的状态，此操作会移除当前手动裁剪输出，确定继续吗？ / Revert to the last applied state and remove current outputs?'
      );
      if (!confirmed) {
        return;
      }
      setReverting(true);
      clearApplyFeedback();
      setError(null);

      void (async () => {
        try {
          const response = await invoke<ManualSplitRevertResponse>(
            'revert_manual_splits',
            {
              request: {
                workspace,
              },
            }
          );

          setManualReport({
            path: response.manualSplitReportPath ?? null,
            summary: response.manualSplitReportSummary ?? null,
          });
          setCanRevert(false);
          setHasRevertHistory(false);
          resolveApplySucceeded('已回滚至最近一次应用。', 0, 0);
          void trackManualSplitTelemetry(
            'manual-split/reverted',
            {
              succeeded: true,
              restoredOutputs: response.restoredOutputs,
              hadReport: Boolean(response.manualSplitReportPath),
            },
            workspace
          );
          await loadContext();
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          setError(message);
          void trackManualSplitTelemetry(
            'manual-split/reverted',
            {
              succeeded: false,
              error: message,
            },
            workspace
          );
        } finally {
          setReverting(false);
        }
      })();
    }, [
      clearApplyFeedback,
      loadContext,
      resolveApplySucceeded,
      setCanRevert,
      setError,
      setHasRevertHistory,
      setManualReport,
      workspace,
    ]);

    const revertDisabledReason = useMemo(() => {
      if (reverting) {
        return '正在回滚，请稍候。 / Reverting in progress.';
      }
      if (applyState.running) {
        return '应用进行中，完成后再尝试回滚。 / Apply in progress; retry after completion.';
      }
      if (!hasRevertHistory) {
        return '暂无可回滚记录，请先执行一次“应用”。 / No revert history. Apply once before reverting.';
      }
      return null;
    }, [applyState.running, hasRevertHistory, reverting]);

    const handleClose = useCallback(() => {
      if (dirtyCount > 0 || stagedCount > 0) {
        const confirmed = window.confirm(
          '存在未保存或未完成的拆分草稿，关闭后请记得稍后完成。仍要关闭吗？ / Unsaved or unstaged drafts detected. Close anyway?'
        );
        if (!confirmed) {
          return;
        }
      }
      onClose();
      closeDrawer();
    }, [closeDrawer, dirtyCount, onClose, stagedCount]);

    return (
      <aside
        className={drawerOpen ? 'custom-split-drawer open' : 'custom-split-drawer'}
        role="dialog"
        aria-modal="true"
      >
        <header className="custom-split-drawer-header">
          <div>
            <h3>自定义拆分 / Manual Split</h3>
            {workspace && (
              <p className="custom-split-path" title={workspace}>
                工作目录：{workspace} / Workspace: {workspace}
              </p>
            )}
          </div>
          <button type="button" onClick={handleClose}>
            关闭 / Close
          </button>
        </header>

        {error && <div className="custom-split-error">{error}</div>}

        <div className="custom-split-content">
          <ManualSplitToolbar
            activeDraft={activeDraft}
            draftsCount={totalDrafts}
            dirtyCount={dirtyCount}
            stagedCount={stagedCount}
            applyState={applyState}
            accelerator={accelerator}
            onAcceleratorChange={handleAcceleratorChange}
            onUndo={handleUndo}
            onRedo={handleRedo}
            onResetCurrent={handleResetCurrent}
            onResetAll={handleResetAll}
            onRevert={handleRevert}
            canRevert={canRevert}
            reverting={reverting}
            hasRevertHistory={hasRevertHistory}
            revertHint={revertDisabledReason}
            onComplete={handleComplete}
            disableComplete={applyState.running || stagedCount === 0 || !workspace}
          />

          <div className="custom-split-body">
            <div className="custom-split-left">
              <SplitThumbnailGrid
                items={listItems}
                selection={selection}
                onSelect={handleSelect}
                loading={loading}
              />
            </div>

            <div className="custom-split-main">
              <SplitCanvas
                draft={activeDraft}
                gutterWidthRatio={gutterWidthRatio}
                locked={Boolean(activeDraft?.locked)}
                onLinesChange={handleLinesChange}
              />
              <SplitSettingsPanel
                lines={activeDraft?.lines ?? null}
                imageKind={activeImageKind}
                gutterWidthRatio={gutterWidthRatio}
                applyState={applyState}
                locked={Boolean(activeDraft?.locked)}
                lockedCount={lockedCount}
                actionableCount={actionableCount}
                totalCount={totalDrafts}
                staged={activeStaged}
                stagedAny={stagedAny}
                hasPendingChanges={activeHasPending}
                previewLoading={previewLoading}
                onLinesChange={handleLinesChange}
                onImageKindChange={handleImageKindChange}
                onStageCurrent={handleStageCurrent}
                onClearStageCurrent={handleClearCurrentStage}
                onApplyAllUnlocked={handleApplyAllUnlocked}
                onClearAllStages={handleClearAllStages}
                onToggleLock={handleToggleLock}
                onGeneratePreview={handleGeneratePreview}
              />
            </div>

            <div className="custom-split-preview">
              <SplitPreviewPane
                preview={activePreview}
                loading={previewLoading}
                onRefresh={activeDraft ? () => requestPreview(activeDraft) : undefined}
              />
            </div>
          </div>
        </div>
      </aside>
    );
  }
);

CustomSplitDrawer.displayName = 'CustomSplitDrawer';

export default CustomSplitDrawer;
