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
      ? '重新扫描目录并打开 / Rescan & open'
      : '创建手动拆分工作区 / Create manual workspace';
    const secondaryLabel = hasWorkspace
      ? '打开已有工作区 / Open existing workspace'
      : '稍后再说 / Maybe later';

    return (
      <div className="manual-split-intro">
        <header>
          <h4>手动拆分工作区 / Manual Split Workspace</h4>
          <p>
            直接使用重命名后的原图进行分割，无需运行自动算法。初始化后即可在抽屉内拖拽裁剪线、应用并生成输出。{' '}
            <span>
              Work directly on renamed originals without running automation. Initialize once, then
              adjust splits and export results inside the drawer.
            </span>
          </p>
        </header>

        <div className="manual-split-actions">
          <button
            type="button"
            className="primary"
            onClick={onInitialize}
            disabled={initializing || disableInitialize}
          >
            {initializing ? '准备中… / Preparing…' : primaryLabel}
          </button>
          <button
            type="button"
            className="ghost"
            onClick={onOpenExisting}
            disabled={!hasWorkspace || initializing}
          >
            {secondaryLabel}
          </button>
        </div>

        {disableReason && (
          <p className="status status-warning">{disableReason}</p>
        )}

        {error && <p className="status status-error">{error}</p>}

        <div className="manual-split-status">
          <p className="status">
            {loadingDrafts || initializing
              ? '正在载入手动拆分数据… / Loading manual split data…'
              : statusText}
          </p>
          {hasWorkspace && (
            <ul className="status status-tip">
              <li>工作目录：{workspace} / Workspace: {workspace}</li>
              <li>
                草稿进度：{appliedDrafts}/{totalDrafts}
                {lastAppliedAt ? `（最近 ${lastAppliedAt}）` : ''} / Progress:{' '}
                {appliedDrafts}/{totalDrafts}
                {lastAppliedAt ? ` (last ${lastAppliedAt})` : ''}
              </li>
            </ul>
          )}
        </div>
      </div>
    );
  }
);

ManualSplitIntro.displayName = 'ManualSplitIntro';

export default ManualSplitIntro;
