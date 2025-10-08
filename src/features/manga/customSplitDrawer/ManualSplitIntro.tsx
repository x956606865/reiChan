import type { FC } from 'react';
import { memo } from 'react';

interface ManualSplitIntroProps {
  initializing: boolean;
  loadingDrafts: boolean;
  statusText: string;
  error: string | null;
  workspace: string | null;
  disableInitialize: boolean;
  disableReason: string | null;
  totalDrafts: number;
  appliedDrafts: number;
  lastAppliedAt: string | null;
  onInitialize: () => void;
  onOpenExisting: () => void;
}

const ManualSplitIntro: FC<ManualSplitIntroProps> = memo(
  ({
    initializing,
    loadingDrafts,
    statusText,
    error,
    workspace,
    disableInitialize,
    disableReason,
    totalDrafts,
    appliedDrafts,
    lastAppliedAt,
    onInitialize,
    onOpenExisting,
  }) => {
    const hasWorkspace = Boolean(workspace);
    const primaryLabel = hasWorkspace
      ? '重新扫描目录并打开'
      : '创建手动拆分工作区';

    return (
      <>
        <div className="manual-split-intro">
          <div className="manual-split-actions button-row">
            <button
              type="button"
              className="split-action-button primary"
              onClick={onInitialize}
              disabled={initializing || disableInitialize}
            >
              {initializing ? '准备中…' : primaryLabel}
            </button>
            {hasWorkspace && (
              <button
                type="button"
                className="split-action-button"
                onClick={onOpenExisting}
                disabled={initializing}
              >
                打开已有工作区
              </button>
            )}
          </div>

          {disableReason && (
            <p className="status status-warning">{disableReason}</p>
          )}

          {error && <p className="status status-error">{error}</p>}
        </div>

        <div className="manual-split-status">
          <p className="status">
            {loadingDrafts || initializing ? '正在载入手动拆分数据…' : statusText}
          </p>
          {hasWorkspace && (
            <ul className="status status-tip">
              <li>工作目录：{workspace}</li>
              <li>
                草稿进度：{appliedDrafts}/{totalDrafts}
                {lastAppliedAt ? `（最近 ${lastAppliedAt}）` : ''}
              </li>
            </ul>
          )}
        </div>
      </>
    );
  }
);

ManualSplitIntro.displayName = 'ManualSplitIntro';

export default ManualSplitIntro;
