import type { FC } from 'react';
import { memo, useMemo } from 'react';

import type { ManualApplyState, ManualSplitDraft } from './store.js';

interface ManualSplitToolbarProps {
  activeDraft: ManualSplitDraft | null;
  draftsCount: number;
  pendingCount: number;
  applyState: ManualApplyState;
  onUndo: () => void;
  onRedo: () => void;
  onResetCurrent: () => void;
  onResetAll: () => void;
  onExportTemplate: () => void;
  onImportTemplate: () => void;
  onRevert: () => void;
  canRevert: boolean;
  reverting: boolean;
  hasRevertHistory: boolean;
  revertHint?: string | null;
  exportingTemplate: boolean;
  importingTemplate: boolean;
}

const ManualSplitToolbar: FC<ManualSplitToolbarProps> = memo(
  ({
    activeDraft,
    draftsCount,
    pendingCount,
    applyState,
    onUndo,
    onRedo,
    onResetCurrent,
    onResetAll,
    onExportTemplate,
    onImportTemplate,
    onRevert,
    canRevert,
    reverting,
    hasRevertHistory,
    revertHint,
    exportingTemplate,
    importingTemplate,
  }) => {
    const canUndo = Boolean(activeDraft && activeDraft.history.length > 0);
    const canRedo = Boolean(activeDraft && activeDraft.redoStack.length > 0);
    const canResetCurrent = Boolean(activeDraft && activeDraft.hasPendingChanges);
    const canResetAll = pendingCount > 0;
    const canTriggerRevert =
      canRevert && hasRevertHistory && !applyState.running && !reverting;
    const canExport =
      draftsCount > 0 && !applyState.running && !reverting && !exportingTemplate && !importingTemplate;
    const canImport =
      draftsCount > 0 && !applyState.running && !reverting && !importingTemplate;
    const lastFinishedLabel = useMemo(() => {
      if (!applyState.lastFinishedAt) {
        return null;
      }
      return new Date(applyState.lastFinishedAt).toLocaleString();
    }, [applyState.lastFinishedAt]);

    const statusText = useMemo(() => {
      if (reverting) {
        return '正在回滚至上一次应用… / Reverting to last apply…';
      }
      if (applyState.running) {
        const total = applyState.total > 0 ? applyState.total : Math.max(applyState.completed, 1);
        const completed = Math.min(applyState.completed, total);
        return `正在应用 ${completed}/${total} / Applying ${completed}/${total}`;
      }
      if (applyState.errorBubble) {
        return applyState.errorBubble;
      }
      if (applyState.statusText) {
        return applyState.statusText;
      }
      return '等待操作 / Idle';
    }, [
      applyState.completed,
      applyState.errorBubble,
      applyState.running,
      applyState.statusText,
      applyState.total,
      reverting,
    ]);

    const pendingLabel =
      pendingCount > 0
        ? `${pendingCount} 张未应用 / ${pendingCount} pending`
        : '全部已同步 / All synced';

    return (
      <div className="manual-split-toolbar">
        <div className="manual-split-toolbar-meta" aria-live="polite">
          <span>总计 {draftsCount} 张 / Total {draftsCount}</span>
          <span>{pendingLabel}</span>
          <span className={applyState.errorBubble ? 'toolbar-status error' : 'toolbar-status'}>
            {statusText}
          </span>
          {lastFinishedLabel && !applyState.running && (
            <span className="toolbar-status-subtle">
              上次完成：{lastFinishedLabel} / Last finished: {lastFinishedLabel}
            </span>
          )}
        </div>
        <div className="manual-split-toolbar-actions">
          <button type="button" onClick={onUndo} disabled={!canUndo || applyState.running}>
            撤销上一步 / Undo
          </button>
          <button type="button" onClick={onRedo} disabled={!canRedo || applyState.running}>
            重做一步 / Redo
          </button>
          <button type="button" onClick={onResetCurrent} disabled={!canResetCurrent || applyState.running}>
            重置当前 / Reset Current
          </button>
          <button type="button" onClick={onResetAll} disabled={!canResetAll || applyState.running}>
            重置全部 / Reset All
          </button>
          <button
            type="button"
            onClick={onExportTemplate}
            disabled={!canExport}
            title={draftsCount === 0 ? '暂无草稿可导出 / No drafts to export' : undefined}
          >
            {exportingTemplate ? '导出中… / Exporting…' : '导出模板 / Export Template'}
          </button>
          <button
            type="button"
            onClick={onImportTemplate}
            disabled={!canImport}
            title={draftsCount === 0 ? '暂无草稿可导入 / No drafts to import' : undefined}
          >
            {importingTemplate ? '导入中… / Importing…' : '导入模板 / Import Template'}
          </button>
          <button
            type="button"
            onClick={onRevert}
            disabled={!canTriggerRevert}
            title={revertHint ?? undefined}
          >
            {reverting ? '回滚中… / Reverting…' : '回滚上一次应用 / Revert last apply'}
          </button>
        </div>
        {revertHint && (
          <div className="manual-split-toolbar-hint" aria-live="polite">
            {revertHint}
          </div>
        )}
      </div>
    );
  }
);

ManualSplitToolbar.displayName = 'ManualSplitToolbar';

export default ManualSplitToolbar;
