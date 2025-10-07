import type { FC, MouseEvent } from 'react';
import { memo, useCallback } from 'react';

import type { ManualSplitDraft } from './store.js';

export interface SplitThumbnailGridItem extends ManualSplitDraft {
  thumbnailUrl: string | null;
}

interface SplitThumbnailGridProps {
  items: SplitThumbnailGridItem[];
  selection: string[];
  onSelect: (sourcePath: string, multi: boolean) => void;
  loading: boolean;
}

const SplitThumbnailGrid: FC<SplitThumbnailGridProps> = memo(
  ({ items, selection, onSelect, loading }) => {
    const handleClick = useCallback(
      (sourcePath: string) => (event: MouseEvent<HTMLButtonElement>) => {
        const multi = event.metaKey || event.ctrlKey;
        onSelect(sourcePath, multi);
      },
      [onSelect]
    );

    if (items.length === 0) {
      return (
        <div className="split-grid-empty">
          {loading ? '正在加载待拆分图片…' : '暂无可编辑的图片条目。'}
        </div>
      );
    }

    return (
      <div className="split-grid" role="list">
        {items.map((item) => {
          const isSelected = selection.includes(item.sourcePath);
          return (
            <button
              type="button"
              key={item.sourcePath}
              role="listitem"
              className={
                isSelected ? 'split-grid-item selected' : 'split-grid-item'
              }
              onClick={handleClick(item.sourcePath)}
            >
              <div className="split-grid-thumbnail">
                {item.thumbnailUrl ? (
                  <img src={item.thumbnailUrl} alt={item.displayName} />
                ) : (
                  <span className="split-grid-placeholder">无预览</span>
                )}
              </div>
              <div className="split-grid-meta">
                <span className="split-grid-name">{item.displayName}</span>
                <span className="split-grid-size">
                  {item.width} × {item.height}
                </span>
              </div>
              {item.locked && <span className="split-grid-lock">已锁定</span>}
            </button>
          );
        })}
      </div>
    );
  }
);

SplitThumbnailGrid.displayName = 'SplitThumbnailGrid';

export default SplitThumbnailGrid;
