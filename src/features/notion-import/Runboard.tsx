import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';

import { useNotionImportRunboard } from './runboardStore';
import type {
  ImportHistoryPage,
  ImportJobSummary,
  ImportQueueSnapshot,
  JobState,
} from './types';

type RunboardProps = {
  onBack?: () => void;
};

const formatNumber = (value: number | undefined): string => {
  if (value === undefined) return '-';
  return value.toLocaleString();
};

const formatDuration = (seconds: number | undefined): string => {
  if (seconds === undefined || !Number.isFinite(seconds)) return '-';
  if (seconds < 60) return `${seconds.toFixed(0)} 秒`;
  const minutes = Math.floor(seconds / 60);
  const remainder = Math.floor(seconds % 60);
  return `${minutes} 分 ${remainder} 秒`;
};

const terminalStates = new Set(['Completed', 'Failed', 'Canceled']);

export default function Runboard({ onBack }: RunboardProps) {
  const [exportInfo, setExportInfo] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  const {
    job,
    progress,
    logs,
    recentErrors,
    lastDone,
    isStreaming,
    starting,
    queue,
    focusedJobId,
    history,
    historyLoading,
    historyError,
    historyFilter,
    actions,
  } = useNotionImportRunboard((state) => ({
    job: state.job,
    progress: state.progress,
    logs: state.logs,
    recentErrors: state.recentErrors,
    lastDone: state.lastDone,
    isStreaming: state.isStreaming,
    starting: state.starting,
    queue: state.queue,
    focusedJobId: state.focusedJobId ?? null,
    history: state.history ?? null,
    historyLoading: state.historyLoading,
    historyError: state.historyError ?? null,
    historyFilter: state.historyFilter,
    actions: state.actions,
  }));

  useEffect(() => {
    if (!history && !historyLoading) {
      actions
        .loadHistory()
        .catch((err) => console.warn('[notion-import] 初次加载历史失败', err));
    }
  }, [history, historyLoading, actions]);

  const counts = useMemo(() => {
    const done = job?.progress.done ?? 0;
    const failed = job?.progress.failed ?? 0;
    const skipped = job?.progress.skipped ?? 0;
    const total = job?.progress.total ?? progress?.progress.total ?? undefined;
    const processed = done + failed + skipped;
    return { total, done, failed, skipped, processed };
  }, [job, progress]);

  const currentRps = progress?.rps ?? lastDone?.rps ?? null;
  const estimatedRemainingSeconds = useMemo(() => {
    if (!counts.total || !currentRps || currentRps <= 0) return undefined;
    const remaining = counts.total - counts.processed;
    if (remaining <= 0) return 0;
    return remaining / currentRps;
  }, [counts, currentRps]);

  const state = job?.state ?? 'Pending';
  const canPause = state === 'Running' && isStreaming;
  const canResume = state === 'Paused';
  const canCancel = !terminalStates.has(state) && !!job;
  const canExport = (job?.progress.failed ?? 0) > 0;

  const findJobInSnapshot = useCallback(
    (snapshot: ImportQueueSnapshot | null, jobId: string): ImportJobSummary | undefined => {
      if (!snapshot) return undefined;
      for (const list of [snapshot.running, snapshot.waiting, snapshot.paused]) {
        const found = list.find((item) => item.jobId === jobId);
        if (found) return found;
      }
      return undefined;
    },
    []
  );

  const handleSelectJob = useCallback(
    async (jobId: string) => {
      const summary = findJobInSnapshot(queue ?? null, jobId);
      if (!summary) return;
      try {
        await actions.hydrate(summary);
      } catch (err) {
        console.error(err);
        alert(`切换作业失败：${err instanceof Error ? err.message : String(err)}`);
      }
    },
    [actions, queue, findJobInSnapshot]
  );

  const handlePromote = useCallback(
    async (jobId: string) => {
      try {
        await actions.promote(jobId);
      } catch (err) {
        console.error(err);
        alert(`提升优先级失败：${err instanceof Error ? err.message : String(err)}`);
      }
    },
    [actions]
  );

  const handleRequeue = useCallback(
    async (jobId: string) => {
      try {
        await actions.requeue(jobId);
      } catch (err) {
        console.error(err);
        alert(`重新排队失败：${err instanceof Error ? err.message : String(err)}`);
      }
    },
    [actions]
  );

  const handleSetPriority = useCallback(
    async (jobSummary: ImportJobSummary) => {
      const current = jobSummary.priority ?? 0;
      const input = window.prompt('请输入新的优先级（整数，越大越靠前）', String(current));
      if (input === null) return;
      const parsed = Number.parseInt(input, 10);
      if (Number.isNaN(parsed)) {
        alert('请输入有效的整数优先级');
        return;
      }
      try {
        await actions.setPriority(jobSummary.jobId, parsed);
      } catch (err) {
        console.error(err);
        alert(`设置优先级失败：${err instanceof Error ? err.message : String(err)}`);
      }
    },
    [actions]
  );

  const handleRefreshQueue = useCallback(async () => {
    try {
      await actions.refreshQueue();
    } catch (err) {
      console.error(err);
      alert(`刷新队列失败：${err instanceof Error ? err.message : String(err)}`);
    }
  }, [actions]);

  const handleHistoryFilterChange = useCallback(
    (states?: JobState[]) => {
      actions
        .loadHistory({ states })
        .catch((err) => {
          console.error(err);
          alert(`刷新历史失败：${err instanceof Error ? err.message : String(err)}`);
        });
    },
    [actions]
  );

  const handleHistoryPageChange = useCallback(
    (page: number) => {
      if (page < 0) return;
      actions
        .loadHistory({ page })
        .catch((err) => {
          console.error(err);
          alert(`翻页失败：${err instanceof Error ? err.message : String(err)}`);
        });
    },
    [actions]
  );

  const handleHistoryRefresh = useCallback(() => {
    actions
      .loadHistory()
      .catch((err) => {
        console.error(err);
        alert(`刷新历史失败：${err instanceof Error ? err.message : String(err)}`);
      });
  }, [actions]);

  const handlePause = async () => {
    try {
      await actions.pause();
    } catch (err) {
      console.error(err);
      alert(`暂停失败：${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const handleResume = async () => {
    try {
      await actions.resume();
    } catch (err) {
      console.error(err);
      alert(`继续失败：${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const handleCancel = async () => {
    if (!window.confirm('确定要取消当前导入作业吗？')) return;
    try {
      await actions.cancel();
    } catch (err) {
      console.error(err);
      alert(`取消失败：${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const handleExportFailed = async () => {
    setExporting(true);
    try {
      const result = await actions.exportFailed();
      setExportInfo(`已导出 ${result.total} 条失败记录至：${result.path}`);
    } catch (err) {
      console.error(err);
      alert(`导出失败：${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setExporting(false);
    }
  };

  return (
    <section className="runboard" style={{ marginTop: 16 }}>
      <header style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
        {onBack && (
          <button className="btn btn--ghost" onClick={onBack}>
            返回映射
          </button>
        )}
        <h3 style={{ margin: 0 }}>导入执行看板</h3>
        <div style={{ flex: 1 }} />
        {starting && <span className="muted">启动中…</span>}
        {state && <span className="badge">状态：{state}</span>}
      </header>

      <QueuePanel
        snapshot={queue ?? null}
        focusedJobId={focusedJobId ?? job?.jobId}
        onSelect={handleSelectJob}
        onPromote={handlePromote}
        onSetPriority={handleSetPriority}
        onRequeue={handleRequeue}
        onRefresh={handleRefreshQueue}
      />

      <HistoryPanel
        history={history ?? null}
        loading={historyLoading}
        error={historyError}
        filterStates={historyFilter.states}
        currentPage={history?.page ?? historyFilter.page}
        pageSize={historyFilter.pageSize}
        onFilterChange={handleHistoryFilterChange}
        onPageChange={handleHistoryPageChange}
        onRefresh={handleHistoryRefresh}
      />

      <div
        className="summary-cards"
        style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))', gap: 12, marginTop: 12 }}
      >
        <StatCard label="已完成" value={formatNumber(counts.done)} />
        <StatCard label="失败" value={formatNumber(counts.failed)} />
        <StatCard label="跳过" value={formatNumber(counts.skipped)} />
        <StatCard
          label="总数"
          value={counts.total !== undefined ? formatNumber(counts.total) : '未知'}
        />
        <StatCard
          label="RPS"
          value={currentRps ? currentRps.toFixed(2) : '-'}
          hint={currentRps ? '平均请求速率' : undefined}
        />
        <StatCard
          label="预计剩余"
          value={formatDuration(estimatedRemainingSeconds)}
        />
      </div>

      <ProgressBar counts={counts} />

      <div className="runboard-controls" style={{ display: 'flex', gap: 8, marginTop: 16, flexWrap: 'wrap', alignItems: 'center' }}>
        <button className="btn" onClick={handlePause} disabled={!canPause}>
          暂停
        </button>
        <button className="btn btn--primary" onClick={handleResume} disabled={!canResume}>
          继续
        </button>
        <button className="btn btn--danger" onClick={handleCancel} disabled={!canCancel}>
          取消
        </button>
        <button className="btn" onClick={handleExportFailed} disabled={!canExport || exporting}>
          {exporting ? '导出中…' : '导出失败行'}
        </button>
        <div style={{ flex: 1 }} />
        <button className="btn btn--ghost" onClick={() => actions.reset()}>
          重置看板
        </button>
      </div>

      {exportInfo && (
        <p className="muted" style={{ marginTop: 8 }}>
          {exportInfo}
        </p>
      )}

      {recentErrors.length > 0 && (
        <section style={{ marginTop: 20 }}>
          <h4 style={{ marginBottom: 8 }}>最近失败记录</h4>
          <ul className="token-list">
            {recentErrors.map((err) => (
              <li key={err.rowIndex}>
                <strong># {err.rowIndex}</strong>
                <span style={{ marginLeft: 8 }}>{err.errorCode ?? '未知错误'}</span>
                <span style={{ marginLeft: 8 }}>{err.errorMessage}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section style={{ marginTop: 20 }}>
        <h4 style={{ marginBottom: 8 }}>日志</h4>
        <div
          style={{
            maxHeight: 220,
            overflowY: 'auto',
            padding: 12,
            border: '1px solid var(--border-muted)',
            borderRadius: 8,
            background: '#fafafa',
          }}
        >
          {logs.length === 0 ? (
            <p className="muted">暂无日志。</p>
          ) : (
            <ul style={{ listStyle: 'none', padding: 0, margin: 0, display: 'grid', gap: 6 }}>
              {logs
                .slice()
                .reverse()
                .map((log) => (
                  <li key={`${log.timestamp}-${log.message}`}>
                    <span className={`log-${log.level}`} style={{ fontWeight: 600 }}>
                      [{log.level.toUpperCase()}]
                    </span>{' '}
                    <span className="muted">
                      {new Date(log.timestamp).toLocaleTimeString()}
                    </span>{' '}
                    {log.message}
                  </li>
                ))}
            </ul>
          )}
        </div>
      </section>

      {lastDone && (
        <section style={{ marginTop: 20 }}>
          <h4>完成摘要</h4>
          <p className="muted">
            完成时间：{new Date(lastDone.finishedAt).toLocaleString()}，终态：{lastDone.state}
            {lastDone.lastError ? `，最后错误：${lastDone.lastError}` : ''}
          </p>
        </section>
      )}
    </section>
  );
}

type StatCardProps = {
  label: string;
  value: string;
  hint?: string;
};

function StatCard({ label, value, hint }: StatCardProps) {
  return (
    <div
      className="stat-card"
      style={{
        padding: 12,
        border: '1px solid var(--border-muted)',
        borderRadius: 8,
        background: '#fff',
        minHeight: 72,
        display: 'flex',
        flexDirection: 'column',
        justifyContent: 'center',
      }}
    >
      <span className="muted" style={{ fontSize: 12 }}>
        {label}
      </span>
      <strong style={{ fontSize: 20 }}>{value}</strong>
      {hint && (
        <span className="muted" style={{ fontSize: 12 }}>
          {hint}
        </span>
      )}
    </div>
  );
}

type ProgressProps = {
  counts: {
    total?: number;
    done: number;
    failed: number;
    skipped: number;
    processed: number;
  };
};

function ProgressBar({ counts }: ProgressProps) {
  const effectiveTotal = counts.total ?? Math.max(counts.processed, 1);
  const ratio = Math.min(counts.processed / effectiveTotal, 1);
  const totalLabel = counts.total !== undefined ? formatNumber(counts.total) : '未知';

  return (
    <div style={{ marginTop: 16 }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 12 }}>
        <span className="muted">进度</span>
        <span className="muted">
          {formatNumber(counts.processed)} / {totalLabel}
        </span>
      </div>
      <div
        style={{
          marginTop: 6,
          background: '#e5e5e5',
          borderRadius: 6,
          height: 12,
          overflow: 'hidden',
        }}
      >
        <div
          style={{
            width: `${(ratio * 100).toFixed(1)}%`,
            background: '#4f46e5',
            height: '100%',
            transition: 'width 0.3s ease',
          }}
        />
      </div>
    </div>
  );
}

type QueuePanelProps = {
  snapshot: ImportQueueSnapshot | null;
  focusedJobId?: string;
  onSelect: (jobId: string) => void;
  onPromote: (jobId: string) => void;
  onSetPriority: (job: ImportJobSummary) => void;
  onRequeue: (jobId: string) => void;
  onRefresh: () => void;
};

function QueuePanel({
  snapshot,
  focusedJobId,
  onSelect,
  onPromote,
  onSetPriority,
  onRequeue,
  onRefresh,
}: QueuePanelProps) {
  return (
    <section
      className="queue-panel"
      style={{
        marginTop: 16,
        padding: 12,
        border: '1px solid var(--border-muted)',
        borderRadius: 8,
        background: '#fff',
      }}
    >
      <header style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <strong>作业队列</strong>
        <div style={{ flex: 1 }} />
        <button className="btn" onClick={onRefresh}>
          刷新
        </button>
      </header>
      {!snapshot ? (
        <p className="muted" style={{ marginTop: 8 }}>
          队列信息尚未加载。
        </p>
      ) : (
        <>
          <div style={{ display: 'flex', gap: 16, marginTop: 8, fontSize: 12, flexWrap: 'wrap' }}>
            <span className="muted">运行中：{snapshot.running.length}</span>
            <span className="muted">等待：{snapshot.waiting.length}</span>
            <span className="muted">暂停：{snapshot.paused.length}</span>
            <span className="muted">
              更新时间：{new Date(snapshot.timestamp).toLocaleTimeString()}
            </span>
          </div>
          <div
            style={{
              display: 'grid',
              gridTemplateColumns: 'repeat(auto-fit, minmax(240px, 1fr))',
              gap: 12,
              marginTop: 12,
            }}
          >
            <QueueSection
              title="运行中"
              emptyHint="暂无运行中的作业"
              jobs={snapshot.running}
              focusedJobId={focusedJobId}
              onSelect={onSelect}
              renderActions={(job) => (
                <button className="btn btn--ghost" onClick={() => onSetPriority(job)}>
                  设置优先级
                </button>
              )}
            />
            <QueueSection
              title="等待中"
              emptyHint="当前没有等待执行的作业"
              jobs={snapshot.waiting}
              focusedJobId={focusedJobId}
              onSelect={onSelect}
              renderActions={(job) => (
                <>
                  <button className="btn" onClick={() => onPromote(job.jobId)}>
                    提升优先级
                  </button>
                  <button className="btn btn--ghost" onClick={() => onSetPriority(job)}>
                    设置优先级
                  </button>
                </>
              )}
            />
            <QueueSection
              title="暂停"
              emptyHint="没有暂停的作业"
              jobs={snapshot.paused}
              focusedJobId={focusedJobId}
              onSelect={onSelect}
              renderActions={(job) => (
                <button className="btn btn--ghost" onClick={() => onRequeue(job.jobId)}>
                  重新排队
                </button>
              )}
            />
          </div>
        </>
      )}
    </section>
  );
}

type QueueSectionProps = {
  title: string;
  jobs: ImportJobSummary[];
  emptyHint: string;
  focusedJobId?: string;
  onSelect: (jobId: string) => void;
  renderActions?: (job: ImportJobSummary) => ReactNode;
};

function QueueSection({
  title,
  jobs,
  emptyHint,
  focusedJobId,
  onSelect,
  renderActions,
}: QueueSectionProps) {
  return (
    <div>
      <h4 style={{ marginBottom: 8 }}>{title}</h4>
      {jobs.length === 0 ? (
        <p className="muted" style={{ fontSize: 12 }}>
          {emptyHint}
        </p>
      ) : (
        <ul
          className="queue-list"
          style={{ listStyle: 'none', padding: 0, margin: 0, display: 'grid', gap: 6 }}
        >
          {jobs.map((job) => (
            <li
              key={job.jobId}
              style={{
                border: '1px solid var(--border-muted)',
                borderRadius: 6,
                padding: 8,
                background: job.jobId === focusedJobId ? '#eef2ff' : '#fafafa',
              }}
            >
              <div
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 8,
                  flexWrap: 'wrap',
                }}
              >
                <button className="btn" onClick={() => onSelect(job.jobId)}>
                  查看
                </button>
                <span style={{ fontWeight: 600 }}>{job.jobId}</span>
                <span className="muted">状态：{job.state}</span>
                <span className="muted">优先级：{job.priority ?? 0}</span>
                <span className="muted">
                  进度：{job.progress.done}/{job.progress.total ?? '未知'}
                </span>
                <div style={{ marginLeft: 'auto', display: 'flex', gap: 6 }}>
                  {renderActions ? renderActions(job) : null}
                </div>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

type HistoryPanelProps = {
  history: ImportHistoryPage | null;
  loading: boolean;
  error: string | null;
  filterStates?: JobState[];
  currentPage: number;
  pageSize: number;
  onFilterChange: (states?: JobState[]) => void;
  onPageChange: (page: number) => void;
  onRefresh: () => void;
};

function HistoryPanel({
  history,
  loading,
  error,
  filterStates,
  currentPage,
  pageSize,
  onFilterChange,
  onPageChange,
  onRefresh,
}: HistoryPanelProps) {
  const filterOptions: Array<{ key: string; label: string; states?: JobState[] }> = [
    { key: 'all', label: '全部', states: undefined },
    { key: 'failed', label: '失败', states: ['Failed'] },
    { key: 'completed', label: '已完成', states: ['Completed'] },
    { key: 'canceled', label: '已取消', states: ['Canceled'] },
  ];

  const activeFilterKey = (() => {
    if (!filterStates || filterStates.length === 0) return 'all';
    if (filterStates.length === 1) {
      switch (filterStates[0]) {
        case 'Failed':
          return 'failed';
        case 'Completed':
          return 'completed';
        case 'Canceled':
          return 'canceled';
        default:
          return 'custom';
      }
    }
    return 'custom';
  })();

  const resolvedPage = history?.page ?? currentPage;
  const total = history?.total ?? 0;
  const hasMore = history?.hasMore ?? false;
  const items = history?.items ?? [];

  const stateLabels: Record<JobState, string> = {
    Pending: '待调度',
    Queued: '排队中',
    Running: '执行中',
    Paused: '已暂停',
    Completed: '已完成',
    Failed: '失败',
    Canceled: '已取消',
  };

  const formatTimestamp = (value?: number | null) => {
    if (!value) return '未知时间';
    try {
      return new Date(value).toLocaleString();
    } catch {
      return String(value);
    }
  };

  const formatCount = (value?: number | null) => {
    if (value === undefined || value === null) return '未知';
    return value.toLocaleString();
  };

  return (
    <section
      className="history-panel"
      style={{
        marginTop: 16,
        padding: 12,
        border: '1px solid var(--border-muted)',
        borderRadius: 8,
        background: '#fff',
      }}
    >
      <header style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
        <strong>历史作业</strong>
        <div style={{ display: 'flex', gap: 6 }}>
          {filterOptions.map((opt) => {
            const isActive = opt.key === activeFilterKey;
            return (
              <button
                key={opt.key}
                className="btn btn--ghost"
                disabled={loading}
                onClick={() => onFilterChange(opt.states)}
                style={{
                  fontWeight: isActive ? 600 : 400,
                  background: isActive ? '#eef2ff' : undefined,
                }}
              >
                {opt.label}
              </button>
            );
          })}
        </div>
        <div style={{ flex: 1 }} />
        <button className="btn" onClick={onRefresh} disabled={loading}>
          {loading ? '加载中…' : '刷新'}
        </button>
      </header>

      {error && (
        <p style={{ color: '#b91c1c', marginTop: 8 }}>
          历史加载失败：{error}
        </p>
      )}

      {(!history || items.length === 0) && !loading ? (
        <p className="muted" style={{ marginTop: 12 }}>
          暂无历史记录。
        </p>
      ) : (
        <ul
          className="history-list"
          style={{
            listStyle: 'none',
            padding: 0,
            margin: 12,
            marginTop: 12,
            display: 'grid',
            gap: 8,
          }}
        >
          {items.map((item) => {
            const finishedAt =
              item.endedAt ?? item.startedAt ?? item.createdAt ?? Date.now();
            return (
              <li
                key={item.jobId}
                style={{
                  border: '1px solid var(--border-muted)',
                  borderRadius: 6,
                  padding: 10,
                  background: '#fafafa',
                }}
              >
                <div
                  style={{
                    display: 'flex',
                    flexWrap: 'wrap',
                    gap: 10,
                    alignItems: 'center',
                  }}
                >
                  <span style={{ fontWeight: 600 }}>{item.jobId}</span>
                  <span className="muted">状态：{stateLabels[item.state] ?? item.state}</span>
                  <span className="muted">
                    完成：{formatCount(item.progress.done)}
                  </span>
                  <span className="muted">
                    失败：{formatCount(item.progress.failed)}
                  </span>
                  <span className="muted">
                    跳过：{formatCount(item.progress.skipped)}
                  </span>
                  <span className="muted">
                    总数：{item.progress.total !== undefined ? formatCount(item.progress.total ?? 0) : '未知'}
                  </span>
                  {item.progress.conflictTotal !== undefined && (
                    <span className="muted">
                      冲突：{formatCount(item.progress.conflictTotal)}
                    </span>
                  )}
                  <span className="muted">时间：{formatTimestamp(finishedAt)}</span>
                  {item.lastError && (
                    <span className="muted" style={{ color: '#b91c1c' }}>
                      最后错误：{item.lastError}
                    </span>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}

      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 8,
          marginTop: 8,
          flexWrap: 'wrap',
        }}
      >
        <button
          className="btn btn--ghost"
          onClick={() => onPageChange(Math.max(resolvedPage - 1, 0))}
          disabled={loading || resolvedPage <= 0}
        >
          上一页
        </button>
        <button
          className="btn btn--ghost"
          onClick={() => onPageChange(resolvedPage + 1)}
          disabled={loading || (!hasMore && (!history || history.items.length < pageSize))}
        >
          下一页
        </button>
        <span className="muted">
          第 {resolvedPage + 1} 页 · 每页 {pageSize} 条 · 共 {total.toLocaleString()} 条
        </span>
        {loading && <span className="muted">加载中…</span>}
      </div>
    </section>
  );
}
