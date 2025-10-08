import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import type {
  ExportFailedResult,
  ImportDoneEvent,
  ImportJobDraft,
  ImportJobHandle,
  ImportJobSummary,
  ImportQueueSnapshot,
  ImportLogEvent,
  ImportProgressEvent,
  RowErrorSummary,
} from './types';

type RunboardState = {
  job?: ImportJobSummary;
  progress?: ImportProgressEvent;
  logs: ImportLogEvent[];
  recentErrors: RowErrorSummary[];
  lastDone?: ImportDoneEvent;
  isStreaming: boolean;
  starting: boolean;
  listeners: UnlistenFn[];
  queue?: ImportQueueSnapshot | null;
  focusedJobId?: string | null;
  actions: {
    hydrate: (summary: ImportJobSummary | null) => Promise<void>;
    start: (draft: ImportJobDraft) => Promise<ImportJobHandle>;
    pause: () => Promise<ImportJobSummary>;
    resume: () => Promise<ImportJobSummary>;
    cancel: () => Promise<ImportJobSummary>;
    exportFailed: () => Promise<ExportFailedResult>;
    refreshQueue: () => Promise<ImportQueueSnapshot>;
    promote: (jobId: string) => Promise<ImportJobSummary>;
    requeue: (jobId: string) => Promise<ImportJobSummary>;
    setPriority: (jobId: string, priority: number) => Promise<ImportJobSummary>;
    reset: () => void;
  };
};

const MAX_LOGS = 50;

export const useNotionImportRunboard = create<RunboardState>((set, get) => {
  const cleanupListeners = () => {
    const { listeners } = get();
    listeners.forEach((unlisten) => {
      try {
        unlisten();
      } catch (err) {
        console.warn('[notion-import] failed to unlisten', err);
      }
    });
    set({ listeners: [] });
  };

  const attachListeners = async (jobId: string) => {
    cleanupListeners();
    const nextListeners: UnlistenFn[] = [];

    const progressUnlisten = await listen<ImportProgressEvent>(
      'notion-import/progress',
      (event) => {
        const payload = event.payload;
        if (!payload || payload.jobId !== jobId) return;
        set((prev) => ({
          job: {
            jobId: payload.jobId,
            state: payload.state,
            progress: payload.progress,
            priority: payload.priority ?? prev.job?.priority,
            leaseExpiresAt: payload.leaseExpiresAt ?? prev.job?.leaseExpiresAt,
          },
          progress: payload,
          recentErrors: payload.recentErrors ?? [],
          isStreaming: true,
        }));
      }
    );
    nextListeners.push(progressUnlisten);

    const logUnlisten = await listen<ImportLogEvent>('notion-import/log', (event) => {
      const payload = event.payload;
      if (!payload || payload.jobId !== jobId) return;
      set((prev) => {
        const merged = prev.logs.concat(payload).slice(-MAX_LOGS);
        return { logs: merged };
      });
    });
    nextListeners.push(logUnlisten);

    const doneUnlisten = await listen<ImportDoneEvent>('notion-import/done', async (event) => {
      const payload = event.payload;
      if (!payload || payload.jobId !== jobId) return;
      set((prev) => ({
        job: {
          jobId: payload.jobId,
          state: payload.state,
          progress: payload.progress,
          priority: payload.priority ?? prev.job?.priority,
          leaseExpiresAt: prev.job?.leaseExpiresAt,
        },
        lastDone: payload,
        isStreaming: false,
      }));
      cleanupListeners();
      await refreshQueue();
    });
    nextListeners.push(doneUnlisten);

    set({ listeners: nextListeners, isStreaming: true });
  };

  const ensureJobId = () => {
    const jobId = get().job?.jobId;
    if (!jobId) {
      throw new Error('尚未启动导入作业');
    }
    return jobId;
  };

  const refreshQueue = async (): Promise<ImportQueueSnapshot> => {
    const snapshot = await invoke<ImportQueueSnapshot>('notion_import_queue');
    set((prev) => {
      const currentId = prev.job?.jobId;
      let nextJob = prev.job;
      if (currentId) {
        const findInList = (list: ImportJobSummary[]) =>
          list.find((item) => item.jobId === currentId);
        const fromSnapshot =
          findInList(snapshot.running) ??
          findInList(snapshot.waiting) ??
          findInList(snapshot.paused);
        if (fromSnapshot) {
          nextJob = {
            ...fromSnapshot,
            progress: nextJob?.progress ?? fromSnapshot.progress,
          };
        }
      }
      return { queue: snapshot, job: nextJob };
    });
    return snapshot;
  };

  return {
    job: undefined,
    progress: undefined,
    logs: [],
    recentErrors: [],
    lastDone: undefined,
    isStreaming: false,
    starting: false,
    listeners: [],
    queue: null,
    focusedJobId: null,
    actions: {
      hydrate: async (summary) => {
        if (!summary) {
          cleanupListeners();
          set({
            job: undefined,
            progress: undefined,
            logs: [],
            recentErrors: [],
            lastDone: undefined,
            isStreaming: false,
            focusedJobId: null,
          });
          await refreshQueue();
          return;
        }
        set({
          job: summary,
          logs: [],
          recentErrors: [],
          progress: undefined,
          lastDone: undefined,
          focusedJobId: summary.jobId,
        });
        await attachListeners(summary.jobId);
        await refreshQueue();
      },
      start: async (draft) => {
        set({ starting: true, lastDone: undefined });
        try {
          const handle = await invoke<ImportJobHandle>('notion_import_start', {
            req: {
              tokenId: draft.tokenId,
              databaseId: draft.databaseId,
              sourceFilePath: draft.sourceFilePath,
              fileType: draft.fileType,
              mappings: draft.mappings,
              defaults: draft.defaults,
              priority: draft.priority,
              upsert: draft.upsert,
            },
          });
          const initialSummary: ImportJobSummary = {
            jobId: handle.jobId,
            state: handle.state,
            progress: {
              total: undefined,
              done: 0,
              failed: 0,
              skipped: 0,
            },
            priority: draft.priority,
            leaseExpiresAt: undefined,
            tokenId: draft.tokenId,
            databaseId: draft.databaseId,
            createdAt: Date.now(),
            startedAt: null,
            endedAt: null,
            lastError: null,
            rps: null,
          };
          set({
            job: initialSummary,
            logs: [],
            recentErrors: [],
            progress: undefined,
            focusedJobId: handle.jobId,
          });
          await attachListeners(handle.jobId);
          await refreshQueue();
          return handle;
        } finally {
          set({ starting: false });
        }
      },
      pause: async () => {
        const jobId = ensureJobId();
        const summary = await invoke<ImportJobSummary>('notion_import_pause', { jobId });
        set((prev) => ({
          job: {
            ...summary,
            progress: prev.job?.progress ?? summary.progress,
          },
        }));
        await refreshQueue();
        return summary;
      },
      resume: async () => {
        const jobId = ensureJobId();
        const summary = await invoke<ImportJobSummary>('notion_import_resume', { jobId });
        set((prev) => ({
          job: {
            ...summary,
            progress: prev.job?.progress ?? summary.progress,
          },
        }));
        await refreshQueue();
        return summary;
      },
      cancel: async () => {
        const jobId = ensureJobId();
        const summary = await invoke<ImportJobSummary>('notion_import_cancel', { jobId });
        set((prev) => ({
          job: {
            ...summary,
            progress: prev.job?.progress ?? summary.progress,
          },
          isStreaming: false,
        }));
        cleanupListeners();
        await refreshQueue();
        return summary;
      },
      exportFailed: async () => {
        const jobId = ensureJobId();
        return invoke<ExportFailedResult>('notion_import_export_failed', { jobId });
      },
      refreshQueue: () => refreshQueue(),
      promote: async (jobId) => {
        const summary = await invoke<ImportJobSummary>('notion_import_promote', { jobId });
        await refreshQueue();
        if (get().job?.jobId === summary.jobId) {
          set((prev) => ({
            job: {
              ...summary,
              progress: prev.job?.progress ?? summary.progress,
            },
          }));
        }
        return summary;
      },
      requeue: async (jobId) => {
        const summary = await invoke<ImportJobSummary>('notion_import_requeue', { jobId });
        await refreshQueue();
        if (get().job?.jobId === summary.jobId) {
          set((prev) => ({
            job: {
              ...summary,
              progress: prev.job?.progress ?? summary.progress,
            },
          }));
        }
        return summary;
      },
      setPriority: async (jobId, priority) => {
        const summary = await invoke<ImportJobSummary>('notion_import_set_priority', {
          jobId,
          priority,
        });
        await refreshQueue();
        if (get().job?.jobId === summary.jobId) {
          set((prev) => ({
            job: {
              ...summary,
              progress: prev.job?.progress ?? summary.progress,
            },
          }));
        }
        return summary;
      },
      reset: () => {
        cleanupListeners();
        set({
          job: undefined,
          progress: undefined,
          logs: [],
          recentErrors: [],
          lastDone: undefined,
          isStreaming: false,
          queue: null,
          focusedJobId: null,
        });
      },
    },
  };
});
