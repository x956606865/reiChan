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
  onOpenDetail?: (item: SplitThumbnailGridItem) => void;
}

const SplitThumbnailGrid: FC<SplitThumbnailGridProps> = memo(
  ({ items, selection, onSelect, loading, onOpenDetail }) => {
    const handleClick = useCallback(
      (sourcePath: string) => (event: MouseEvent<HTMLDivElement>) => {
        const multi = event.metaKey || event.ctrlKey;
        onSelect(sourcePath, multi);
      },
      [onSelect]
    );

    const handleThumbnailClick = useCallback(
      (item: SplitThumbnailGridItem) => (event: MouseEvent<HTMLDivElement>) => {
        if (!onOpenDetail) {
          return;
        }
        event.preventDefault();
        event.stopPropagation();
        onOpenDetail(item);
      },
      [onOpenDetail]
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
            <div
              key={item.sourcePath}
              role="listitem"
              className={
                isSelected ? 'split-grid-item selected' : 'split-grid-item'
              }
              onClick={handleClick(item.sourcePath)}
              title={onOpenDetail ? '点击缩略图可进入细化视图' : undefined}
              tabIndex={0}
              data-selected={isSelected ? 'true' : 'false'}
              onKeyDown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault();
                  onSelect(item.sourcePath, event.metaKey || event.ctrlKey);
                }
                if ((event.key === 'Enter' || event.key === ' ') && onOpenDetail) {
                  event.stopPropagation();
                }
              }}
            >
              <div
                className="split-grid-thumbnail"
                onClick={handleThumbnailClick(item)}
                role={onOpenDetail ? 'button' : undefined}
                tabIndex={onOpenDetail ? 0 : undefined}
                onKeyDown={
                  onOpenDetail
                    ? (event) => {
                        if (event.key === 'Enter' || event.key === ' ') {
                          event.preventDefault();
                          onOpenDetail(item);
                        }
                      }
                    : undefined
                }
                aria-label={
                  onOpenDetail
                    ? `放大查看 ${item.displayName ?? item.sourcePath}`
                    : undefined
                }
              >
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
            </div>
          );
        })}
      </div>
    );
  }
);

SplitThumbnailGrid.displayName = 'SplitThumbnailGrid';

export default SplitThumbnailGrid;
