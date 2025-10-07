import type { FC } from 'react';
import { memo, useMemo } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';

import type { SplitPreviewPayload } from './store.js';

interface SplitPreviewPaneProps {
  preview: SplitPreviewPayload | null;
  loading: boolean;
  onRefresh?: () => void;
}

const SplitPreviewPane: FC<SplitPreviewPaneProps> = memo(
  ({ preview, loading, onRefresh }) => {
    const leftUrl = useMemo(() => {
      if (!preview?.leftPreviewPath) {
        return null;
      }
      return convertFileSrc(preview.leftPreviewPath);
    }, [preview]);

    const rightUrl = useMemo(() => {
      if (!preview?.rightPreviewPath) {
        return null;
      }
      return convertFileSrc(preview.rightPreviewPath);
    }, [preview]);

    const gutterUrl = useMemo(() => {
      if (!preview?.gutterPreviewPath) {
        return null;
      }
      return convertFileSrc(preview.gutterPreviewPath);
    }, [preview]);

    return (
      <section className="split-preview" aria-live="polite">
        <header className="split-preview-header">
          <h4>预览</h4>
          <div className="split-preview-actions">
            {loading && <span className="split-preview-status">生成预览中…</span>}
            {onRefresh && (
              <button type="button" onClick={onRefresh} disabled={loading}>
                刷新
              </button>
            )}
          </div>
        </header>

        {!preview && !loading && (
          <div className="split-preview-empty">暂无预览数据。</div>
        )}

        {preview && (
          <div className="split-preview-grid">
            <figure>
              <figcaption>左页</figcaption>
              {leftUrl ? (
                <img src={leftUrl} alt="左页预览" />
              ) : (
                <div className="split-preview-placeholder">未生成</div>
              )}
            </figure>
            <figure>
              <figcaption>右页</figcaption>
              {rightUrl ? (
                <img src={rightUrl} alt="右页预览" />
              ) : (
                <div className="split-preview-placeholder">未生成</div>
              )}
            </figure>
            <figure>
              <figcaption>留白区域</figcaption>
              {gutterUrl ? (
                <img src={gutterUrl} alt="中缝留白预览" />
              ) : (
                <div className="split-preview-placeholder">未生成</div>
              )}
            </figure>
          </div>
        )}
      </section>
    );
  }
);

SplitPreviewPane.displayName = 'SplitPreviewPane';

export default SplitPreviewPane;
