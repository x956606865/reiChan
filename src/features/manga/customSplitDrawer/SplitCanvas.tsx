import type { FC, PointerEvent as ReactPointerEvent } from 'react';
import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
} from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';

import type { ManualSplitDraft, ManualSplitLines } from './store.js';

interface SplitCanvasProps {
  draft: ManualSplitDraft | null;
  gutterWidthRatio: number;
  locked: boolean;
  onLinesChange: (lines: ManualSplitLines) => void;
}

const clamp = (value: number, min: number, max: number) =>
  Math.min(max, Math.max(min, value));

const SplitCanvas: FC<SplitCanvasProps> = memo(
  ({ draft, gutterWidthRatio, locked, onLinesChange }) => {
    const canvasRef = useRef<HTMLDivElement | null>(null);
    const dragStateRef = useRef<{ index: number; pointerId: number } | null>(
      null
    );
    const latestLinesRef = useRef<ManualSplitLines | null>(
      draft ? draft.lines : null
    );
    const latestDraftRef = useRef<ManualSplitDraft | null>(draft);

    useEffect(() => {
      latestDraftRef.current = draft;
      latestLinesRef.current = draft ? draft.lines : null;
      if (!draft) {
        dragStateRef.current = null;
      }
    }, [draft]);

    useEffect(() => {
      if (locked) {
        dragStateRef.current = null;
      }
    }, [locked]);

    const handlePointerDown = useCallback(
      (index: number) => (event: ReactPointerEvent<HTMLSpanElement>) => {
        if (!draft || locked) {
          return;
        }
        event.preventDefault();
        event.stopPropagation();
        dragStateRef.current = {
          index,
          pointerId: event.pointerId,
        };
      },
      [draft, locked]
    );

    useEffect(() => {
      const handlePointerMove = (event: PointerEvent) => {
        const dragState = dragStateRef.current;
        if (!dragState || event.pointerId !== dragState.pointerId) {
          return;
        }
        if (locked) {
          return;
        }
        const currentDraft = latestDraftRef.current;
        const currentLines = latestLinesRef.current;
        if (!currentDraft || !currentLines) {
          return;
        }
        const canvas = canvasRef.current;
        if (!canvas) {
          return;
        }
        const rect = canvas.getBoundingClientRect();
        if (rect.width <= 0) {
          return;
        }
        const ratio = clamp((event.clientX - rect.left) / rect.width, 0, 1);
        const gutter = Math.max(gutterWidthRatio, 0);
        const next = [...currentLines] as ManualSplitLines;
        const isDoubleMode = currentDraft.imageKind !== 'content';

        if (isDoubleMode) {
          const guard = Math.max(gutter, 0.001);
          if (dragState.index === 0 || dragState.index === 1) {
            const max = Math.max(0, next[2] - guard);
            const value = clamp(ratio, 0, max);
            next[0] = value;
            next[1] = value;
          } else if (dragState.index === 2 || dragState.index === 3) {
            const min = Math.min(1, next[0] + guard);
            const value = clamp(ratio, min, 1);
            next[2] = value;
            next[3] = value;
          } else {
            return;
          }
        } else {
          switch (dragState.index) {
            case 0: {
              const max = Math.max(0, next[1] - gutter);
              next[0] = clamp(ratio, 0, max);
              break;
            }
            case 1: {
              const min = Math.min(1, next[0] + gutter);
              const max = Math.max(min, next[2] - gutter);
              next[1] = clamp(ratio, min, max);
              break;
            }
            case 2: {
              const min = Math.min(1, next[1] + gutter);
              const max = Math.max(min, next[3] - gutter);
              next[2] = clamp(ratio, min, max);
              break;
            }
            case 3: {
              const min = Math.min(1, next[2] + gutter);
              next[3] = clamp(ratio, min, 1);
              break;
            }
            default:
              return;
          }
        }

        let changed = false;
        for (let i = 0; i < 4; i += 1) {
          if (Math.abs(next[i] - currentLines[i]) > 1e-5) {
            changed = true;
            break;
          }
        }
        if (!changed) {
          return;
        }

        onLinesChange(next);
      };

      const handlePointerUp = (event: PointerEvent) => {
        const dragState = dragStateRef.current;
        if (!dragState || event.pointerId !== dragState.pointerId) {
          return;
        }
        dragStateRef.current = null;
      };

      window.addEventListener('pointermove', handlePointerMove);
      window.addEventListener('pointerup', handlePointerUp);
      window.addEventListener('pointercancel', handlePointerUp);

      return () => {
        window.removeEventListener('pointermove', handlePointerMove);
        window.removeEventListener('pointerup', handlePointerUp);
        window.removeEventListener('pointercancel', handlePointerUp);
      };
    }, [gutterWidthRatio, locked, onLinesChange]);

    const imageUrl = useMemo(() => {
      if (!draft) {
        return null;
      }
      if (draft.thumbnailPath) {
        return convertFileSrc(draft.thumbnailPath);
      }
      return convertFileSrc(draft.sourcePath);
    }, [draft]);

    if (!draft) {
      return <div className="split-canvas empty">请选择一张图片开始自定义。</div>;
    }

    const [leftTrim, leftPageEnd, rightPageStart, rightTrim] = draft.lines;

    return (
      <div
        className="split-canvas"
        ref={canvasRef}
        data-locked={locked ? 'true' : 'false'}
      >
        {imageUrl ? (
          <img src={imageUrl} alt={draft.displayName} className="split-canvas-image" />
        ) : (
          <div className="split-canvas empty">无法加载预览。</div>
        )}
        <div className="split-canvas-overlay">
          <span
            className="split-line"
            data-disabled={locked ? 'true' : 'false'}
            style={{ left: `${leftTrim * 100}%` }}
            onPointerDown={handlePointerDown(0)}
          />
          <span
            className="split-line"
            data-disabled={locked ? 'true' : 'false'}
            style={{ left: `${leftPageEnd * 100}%` }}
            onPointerDown={handlePointerDown(1)}
          />
          <span
            className="split-line"
            data-disabled={locked ? 'true' : 'false'}
            style={{ left: `${rightPageStart * 100}%` }}
            onPointerDown={handlePointerDown(2)}
          />
          <span
            className="split-line"
            data-disabled={locked ? 'true' : 'false'}
            style={{ left: `${rightTrim * 100}%` }}
            onPointerDown={handlePointerDown(3)}
          />
        </div>
        <footer className="split-canvas-footer">
          <span>{draft.displayName}</span>
          <span>
            {draft.width}×{draft.height}
          </span>
        </footer>
      </div>
    );
  }
);

SplitCanvas.displayName = 'SplitCanvas';

export default SplitCanvas;
