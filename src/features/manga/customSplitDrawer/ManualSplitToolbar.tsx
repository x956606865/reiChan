import type { ChangeEvent, FC } from 'react';
import { memo, useMemo, useCallback } from 'react';

import type { ManualApplyState, ManualSplitDraft } from './store.js';

interface ManualSplitToolbarProps {
  activeDraft: ManualSplitDraft | null;
  draftsCount: number;
  dirtyCount: number;
  stagedCount: number;
  applyState: ManualApplyState;
  accelerator: 'cpu' | 'gpu' | 'auto';
  onAcceleratorChange: (value: 'cpu' | 'gpu' | 'auto') => void;
  onUndo: () => void;
  onRedo: () => void;
  onResetCurrent: () => void;
  onResetAll: () => void;
  onRevert: () => void;
  canRevert: boolean;
  reverting: boolean;
  hasRevertHistory: boolean;
  revertHint?: string | null;
  onComplete: () => void;
  disableComplete: boolean;
}

const ManualSplitToolbar: FC<ManualSplitToolbarProps> = memo(
  ({
    activeDraft,
    draftsCount,
    dirtyCount,
    stagedCount,
    applyState,
    accelerator,
    onAcceleratorChange,
    onUndo,
    onRedo,
    onResetCurrent,
    onResetAll,
    onRevert,
    canRevert,
    reverting,
    hasRevertHistory,
    revertHint,
    onComplete,
    disableComplete,
  }) => {
    const canUndo = Boolean(activeDraft && activeDraft.history.length > 0);
    const canRedo = Boolean(activeDraft && activeDraft.redoStack.length > 0);
    const canResetCurrent = Boolean(activeDraft && activeDraft.hasPendingChanges);
    const canResetAll = dirtyCount > 0;
    const canTriggerRevert =
      canRevert && hasRevertHistory && !applyState.running && !reverting;
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

    const dirtyLabel =
      dirtyCount > 0
        ? `${dirtyCount} 张待保存草稿 / ${dirtyCount} dirty`
        : '草稿已同步 / Drafts clean';
    const stagedLabel =
      stagedCount > 0
        ? `${stagedCount} 张待完成 / ${stagedCount} staged`
        : '无待完成草稿 / Nothing staged';

    const handleAcceleratorSelect = useCallback(
      (event: ChangeEvent<HTMLSelectElement>) => {
        onAcceleratorChange(event.currentTarget.value as 'cpu' | 'gpu' | 'auto');
      },
      [onAcceleratorChange]
    );

    return (
      <div className="manual-split-toolbar">
        <div className="manual-split-toolbar-meta" aria-live="polite">
          <span>总计 {draftsCount} 张 / Total {draftsCount}</span>
          <span>{dirtyLabel}</span>
          <span>{stagedLabel}</span>
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
          <label className="accelerator-select">
            <span>加速器 / Accelerator</span>
            <select
              value={accelerator}
              onChange={handleAcceleratorSelect}
              disabled={applyState.running || reverting}
            >
              <option value="auto">自动 / Auto</option>
              <option value="cpu">CPU</option>
              <option value="gpu">GPU</option>
            </select>
          </label>
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
            onClick={onRevert}
            disabled={!canTriggerRevert}
            title={revertHint ?? undefined}
          >
            {reverting ? '回滚中… / Reverting…' : '回滚上一次应用 / Revert last apply'}
          </button>
          <button
            type="button"
            className="primary"
            onClick={onComplete}
            disabled={disableComplete}
          >
            {applyState.running ? '正在完成… / Completing…' : '完成手动拆分 / Complete'}
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
