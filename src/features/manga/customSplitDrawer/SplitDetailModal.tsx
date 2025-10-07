import type { ChangeEvent, FC } from 'react';
import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useState,
} from 'react';
import SplitCanvas from './SplitCanvas.js';
import type { ManualSplitDraft, ManualSplitLines } from './store.js';

interface SplitDetailModalProps {
  open: boolean;
  draft: ManualSplitDraft | null;
  gutterWidthRatio: number;
  onDismiss: () => void;
  onConfirm: (lines: ManualSplitLines) => void;
}

const LABELS = ['左侧留白', '左页右边界', '右页左边界', '右侧留白'];

const DEFAULT_VALUES: ManualSplitLines = [0.02, 0.48, 0.52, 0.98];

const SplitDetailModal: FC<SplitDetailModalProps> = memo(
  ({ open, draft, gutterWidthRatio, onDismiss, onConfirm }) => {
    const [localLines, setLocalLines] = useState<ManualSplitLines>(DEFAULT_VALUES);

    const locked = Boolean(draft?.locked);
    const imageKind = draft?.imageKind ?? 'content';
    const gutterPercent = useMemo(
      () => Number((gutterWidthRatio * 100).toFixed(1)),
      [gutterWidthRatio]
    );

    useEffect(() => {
      if (!open || !draft) {
        return;
      }
      setLocalLines([...draft.lines] as ManualSplitLines);
    }, [draft, open]);

    useEffect(() => {
      if (!open) {
        return;
      }
      const handleKeyDown = (event: KeyboardEvent) => {
        if (event.key === 'Escape') {
          event.preventDefault();
          onDismiss();
        }
      };
      window.addEventListener('keydown', handleKeyDown);
      return () => {
        window.removeEventListener('keydown', handleKeyDown);
      };
    }, [onDismiss, open]);

    const canvasDraft = useMemo(() => {
      if (!draft) {
        return null;
      }
      return {
        ...draft,
        lines: localLines,
      };
    }, [draft, localLines]);

    const values = useMemo(() => {
      return localLines.map((value) => Number((value * 100).toFixed(2))) as [
        number,
        number,
        number,
        number,
      ];
    }, [localLines]);

    const handleLinesChange = useCallback((lines: ManualSplitLines) => {
      setLocalLines([...lines] as ManualSplitLines);
    }, []);

    const handleFieldChange = useCallback(
      (index: number) => (event: ChangeEvent<HTMLInputElement>) => {
        const numeric = Number.parseFloat(event.currentTarget.value);
        if (Number.isNaN(numeric)) {
          return;
        }
        const clamped = Math.max(0, Math.min(100, numeric)) / 100;
        setLocalLines((prev) => {
          const next = [...prev] as ManualSplitLines;
          if (imageKind === 'content') {
            next[index] = clamped;
          } else if (index === 0) {
            next[0] = clamped;
            next[1] = clamped;
          } else if (index === 3) {
            next[2] = clamped;
            next[3] = clamped;
          }
          return next;
        });
      },
      [imageKind]
    );

    const handleConfirm = useCallback(() => {
      if (!draft) {
        return;
      }
      onConfirm([...localLines] as ManualSplitLines);
    }, [draft, localLines, onConfirm]);

    const handleDismiss = useCallback(() => {
      onDismiss();
    }, [onDismiss]);

    if (!open || !draft || !canvasDraft) {
      return null;
    }

    const gridIndices = imageKind === 'content' ? [0, 1, 2, 3] : [0, 3];

    return (
      <div
        className="split-detail-backdrop"
        role="dialog"
        aria-modal="true"
        onClick={handleDismiss}
      >
        <div
          className="split-detail-modal"
          onClick={(event) => event.stopPropagation()}
        >
          <header className="split-detail-header">
            <div>
              <h4>{draft.displayName}</h4>
              <p>
                {draft.width} × {draft.height}{' '}
                {locked ? '（已锁定，仅可查看）' : ''}
              </p>
            </div>
            <button
              type="button"
              onClick={handleDismiss}
              className="split-detail-close"
            >
              关闭
            </button>
          </header>

          <div className="split-detail-body">
            <div className="split-detail-canvas">
              <SplitCanvas
                draft={canvasDraft}
                gutterWidthRatio={gutterWidthRatio}
                locked={locked}
                onLinesChange={handleLinesChange}
              />
            </div>

            <aside className="split-detail-sidebar">
              <h5>精确调整</h5>
              <p className="split-detail-hint">
                输入百分比或拖拽线段，确认后将回写至草稿。当前中缝最小值 {gutterPercent}%。
              </p>
              <div className="split-detail-grid">
                {gridIndices.map((index) => (
                  <label key={LABELS[index]} className="split-detail-field">
                    <span className="field-label">{LABELS[index]}</span>
                    <input
                      type="number"
                      min={0}
                      max={100}
                      step={0.1}
                      value={values[index]}
                      onChange={handleFieldChange(index)}
                      disabled={locked}
                    />
                    <span className="field-suffix">%</span>
                  </label>
                ))}
              </div>
              <div className="split-detail-actions">
                <button type="button" onClick={handleDismiss}>
                  取消
                </button>
                <button
                  type="button"
                  className="primary"
                  onClick={handleConfirm}
                  disabled={locked}
                >
                  确认并回写
                </button>
              </div>
            </aside>
          </div>
        </div>
      </div>
    );
  }
);

SplitDetailModal.displayName = 'SplitDetailModal';

export default SplitDetailModal;
