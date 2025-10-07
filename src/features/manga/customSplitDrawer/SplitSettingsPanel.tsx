import type { ChangeEvent, FC } from 'react';
import { memo, useCallback, useMemo } from 'react';

import type {
  ManualApplyState,
  ManualSplitLines,
  SplitApplyTarget,
} from './store.js';

interface SplitSettingsPanelProps {
  lines: ManualSplitLines | null;
  accelerator: 'cpu' | 'gpu' | 'auto';
  gutterWidthRatio: number;
  pendingApply: SplitApplyTarget;
  applyState: ManualApplyState;
  locked: boolean;
  lockedCount: number;
  actionableCount: number;
  totalCount: number;
  applyCurrentLabel: string | null;
  onLinesChange: (lines: ManualSplitLines) => void;
  onAcceleratorChange: (value: 'cpu' | 'gpu' | 'auto') => void;
  onApplyCurrent: () => void;
  onApplyAll: () => void;
  onToggleLock: () => void;
}

const LABELS = [
  '左侧留白 / Left Trim',
  '左页右边界 / Left Page End',
  '右页左边界 / Right Page Start',
  '右侧留白 / Right Trim',
];

const SplitSettingsPanel: FC<SplitSettingsPanelProps> = memo(
  ({
    lines,
    accelerator,
    gutterWidthRatio,
    pendingApply,
    applyState,
    locked,
    lockedCount,
    actionableCount,
    totalCount,
    applyCurrentLabel,
    onLinesChange,
    onAcceleratorChange,
    onApplyCurrent,
    onApplyAll,
    onToggleLock,
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
        return `正在应用手动拆分（${applyState.completed}/${progressTotal}） / Applying manual splits (${applyState.completed}/${progressTotal})`;
      }
      return '正在应用手动拆分… / Applying manual splits…';
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
        next[index] = Math.max(0, Math.min(100, numeric)) / 100;
        onLinesChange(next);
      },
      [lines, onLinesChange]
    );

    const handleAcceleratorChange = useCallback(
      (event: ChangeEvent<HTMLSelectElement>) => {
        onAcceleratorChange(event.currentTarget.value as 'cpu' | 'gpu' | 'auto');
      },
      [onAcceleratorChange]
    );

    return (
      <section className="split-settings">
        <header className="split-settings-header">
          <h4>参数设置 / Parameters</h4>
          <span className="split-settings-gutter">
            中缝最小值：{(gutterWidthRatio * 100).toFixed(1)}% / Min gutter:{' '}
            {(gutterWidthRatio * 100).toFixed(1)}%
          </span>
        </header>

        <div className="split-settings-grid">
          {values.map((value: number, index: number) => (
            <label key={LABELS[index]} className="split-settings-field">
              <span className="field-label">{LABELS[index]}</span>
              <input
                type="number"
                min={0}
                max={100}
                step={0.1}
                value={value}
                onChange={handleChange(index)}
                disabled={!lines}
              />
              <span className="field-suffix">%</span>
            </label>
          ))}
        </div>

        <div className="split-settings-row">
          <label className="split-settings-field">
            <span className="field-label">计算加速器 / Accelerator</span>
            <select
              value={accelerator}
              onChange={handleAcceleratorChange}
              disabled={!lines}
            >
              <option value="auto">自动 / Auto</option>
              <option value="cpu">CPU</option>
              <option value="gpu">GPU</option>
            </select>
          </label>
        </div>

        {totalCount > 0 && (
          <div className="split-settings-summary" aria-live="polite">
            <span>总计 {totalCount} 张 / Total {totalCount}</span>
            <span>可应用 {actionableCount} 张 / Actionable {actionableCount}</span>
          </div>
        )}

        {lockedCount > 0 && (
          <p className="split-settings-locked-hint">
            已锁定 {lockedCount} 张，批量应用时将跳过，仅对 {actionableCount} 张生效。/ {lockedCount}{' '}
            locked pages; batch apply targets {actionableCount}.
          </p>
        )}

        <div className="split-settings-actions">
          <button
            type="button"
            onClick={onToggleLock}
            disabled={!lines || isApplying}
          >
            {locked ? '解除锁定 / Unlock' : '锁定当前 / Lock current'}
          </button>
          <button
            type="button"
            className="primary"
            onClick={onApplyCurrent}
            disabled={!lines || isApplying || pendingApply === 'single'}
          >
            应用到当前 / Apply current
          </button>
          <button
            type="button"
            onClick={onApplyAll}
            disabled={
              !lines ||
              isApplying ||
              actionableCount === 0 ||
              pendingApply === 'all'
            }
          >
            应用全部未锁定 / Apply all unlocked
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
                  {applyState.running && applyCurrentLabel && (
                    <span className="split-settings-progress-current">
                      正在处理：{applyCurrentLabel}
                    </span>
                  )}
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
