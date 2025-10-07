import type { ChangeEvent, FC, MouseEvent } from 'react';
import { memo, useCallback, useMemo } from 'react';

import type {
  ManualApplyState,
  ManualImageKind,
  ManualSplitLines,
} from './store.js';

interface SplitSettingsPanelProps {
  lines: ManualSplitLines | null;
  imageKind: ManualImageKind;
  gutterWidthRatio: number;
  applyState: ManualApplyState;
  locked: boolean;
  lockedCount: number;
  actionableCount: number;
  totalCount: number;
  staged: boolean;
  stagedAny: boolean;
  previewLoading: boolean;
  onLinesChange: (lines: ManualSplitLines) => void;
  onImageKindChange: (kind: ManualImageKind) => void;
  onStageCurrent: () => void;
  onClearStageCurrent: () => void;
  onApplyAllUnlocked: () => void;
  onClearAllStages: () => void;
  onToggleLock: () => void;
  onGeneratePreview: () => void;
}

const LABELS = ['左侧留白', '左页右边界', '右页左边界', '右侧留白'];

const SplitSettingsPanel: FC<SplitSettingsPanelProps> = memo(
  ({
    lines,
    imageKind,
    gutterWidthRatio,
    applyState,
    locked,
    lockedCount,
    actionableCount,
    totalCount,
    onLinesChange,
    onImageKindChange,
    onStageCurrent,
    onClearStageCurrent,
    onApplyAllUnlocked,
    onClearAllStages,
    onToggleLock,
    staged,
    stagedAny,
    previewLoading,
    onGeneratePreview,
  }) => {
    const values = useMemo(() => {
      if (!lines) {
        return [0, 48, 52, 100];
      }
      return lines.map((value: number) => Number((value * 100).toFixed(2))) as [
        number,
        number,
        number,
        number,
      ];
    }, [lines]);

    const isApplying = applyState.running;

    const progressTotal = useMemo(() => {
      if (applyState.total > 0) {
        return applyState.total;
      }
      if (applyState.running) {
        return Math.max(applyState.completed, 1);
      }
      return applyState.completed;
    }, [applyState.completed, applyState.running, applyState.total]);

    const progressPercent = useMemo(() => {
      if (!applyState.running && progressTotal <= 0) {
        return 0;
      }
      if (progressTotal <= 0) {
        return 0;
      }
      const value = (applyState.completed / progressTotal) * 100;
      return Math.min(100, Math.max(0, Math.round(value)));
    }, [applyState.completed, applyState.running, progressTotal]);

    const runningSummary = useMemo(() => {
      if (!applyState.running) {
        return null;
      }
      if (progressTotal > 0) {
        return `正在应用手动拆分（${applyState.completed}/${progressTotal}）`;
      }
      return '正在应用手动拆分…';
    }, [applyState.running, applyState.completed, progressTotal]);

    const hasFeedback = Boolean(
      applyState.errorBubble || runningSummary || applyState.statusText
    );

    const handleChange = useCallback(
      (index: number) => (event: ChangeEvent<HTMLInputElement>) => {
        if (!lines) {
          return;
        }
        const next = [...lines] as ManualSplitLines;
        const numeric = Number.parseFloat(event.currentTarget.value);
        if (Number.isNaN(numeric)) {
          return;
        }
        const clamped = Math.max(0, Math.min(100, numeric)) / 100;
        if (imageKind === 'content') {
          next[index] = clamped;
        } else if (index === 0) {
          next[0] = clamped;
          next[1] = clamped;
        } else {
          next[3] = clamped;
          next[2] = clamped;
        }
        onLinesChange(next);
      },
      [imageKind, lines, onLinesChange]
    );

    const handleImageKindChange = useCallback(
      (event: ChangeEvent<HTMLSelectElement>) => {
        onImageKindChange(event.currentTarget.value as ManualImageKind);
      },
      [onImageKindChange]
    );

    const handleStageCurrentClick = useCallback(
      (event: MouseEvent<HTMLButtonElement>) => {
        event.preventDefault();
        if (staged) {
          onClearStageCurrent();
        } else {
          onStageCurrent();
        }
      },
      [onClearStageCurrent, onStageCurrent, staged]
    );

    return (
      <section className="split-settings">
        <header className="split-settings-header">
          <h4>参数设置</h4>
          <span className="split-settings-gutter">
            中缝最小值：{(gutterWidthRatio * 100).toFixed(1)}%
          </span>
        </header>

        <div className="split-settings-row">
          <label className="split-settings-field">
            <span className="field-label">图片类型</span>
            <select
              value={imageKind}
              onChange={handleImageKindChange}
              disabled={isApplying}
            >
              <option value="content">内容页</option>
              <option value="cover">封面</option>
              <option value="spread">跨页</option>
            </select>
          </label>
          <button
            type="button"
            onClick={onGeneratePreview}
            disabled={!lines || isApplying || previewLoading}
          >
            {previewLoading ? '生成预览中…' : '生成预览'}
          </button>
        </div>

        <div className="split-settings-grid">
          {(imageKind === 'content' ? [0, 1, 2, 3] : [0, 3]).map((index) => (
            <label key={LABELS[index]} className="split-settings-field">
              <span className="field-label">{LABELS[index]}</span>
              <input
                type="number"
                min={0}
                max={100}
                step={0.1}
                value={values[index]}
                onChange={handleChange(index)}
                disabled={!lines || isApplying}
              />
              <span className="field-suffix">%</span>
            </label>
          ))}
        </div>

        {totalCount > 0 && (
          <div className="split-settings-summary" aria-live="polite">
            <span>总计 {totalCount} 张</span>
            <span>可应用 {actionableCount} 张</span>
          </div>
        )}

        {lockedCount > 0 && (
          <p className="split-settings-locked-hint">
            已锁定 {lockedCount} 张，批量操作仅会作用于 {actionableCount} 张。
          </p>
        )}

        <div className="split-settings-actions">
          <button
            type="button"
            onClick={onToggleLock}
            disabled={!lines || isApplying}
          >
            {locked ? '解除锁定' : '锁定当前'}
          </button>
          <button
            type="button"
            onClick={handleStageCurrentClick}
            disabled={!lines || isApplying}
          >
            {staged ? '取消应用当前' : '应用当前草稿'}
          </button>
          <button
            type="button"
            onClick={onApplyAllUnlocked}
            disabled={!lines || isApplying || actionableCount === 0}
          >
            应用到全部未锁定
          </button>
          <button
            type="button"
            onClick={onClearAllStages}
            disabled={isApplying || !stagedAny}
          >
            取消全部应用
          </button>
        </div>

        {hasFeedback && (
          <div className="split-settings-feedback" aria-live="polite">
            {applyState.errorBubble && (
              <div className="split-settings-error-bubble" role="alert">
                {applyState.errorBubble}
              </div>
            )}

            {runningSummary && (
              <p className="split-settings-status">{runningSummary}</p>
            )}

            {!applyState.running && applyState.statusText && (
              <p className="split-settings-status">{applyState.statusText}</p>
            )}

            {progressTotal > 0 && (
              <div className="split-settings-progress" role="status">
                <div className="split-settings-progress-track">
                  <div
                    className="split-settings-progress-bar"
                    style={{ width: `${progressPercent}%` }}
                  />
                </div>
                <div className="split-settings-progress-meta">
                  <span>
                    {applyState.completed}/{progressTotal}
                  </span>
                </div>
              </div>
            )}
          </div>
        )}
      </section>
    );
  }
);

SplitSettingsPanel.displayName = 'SplitSettingsPanel';

export default SplitSettingsPanel;
