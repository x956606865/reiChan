import { create } from 'zustand';

export type ManualSplitLines = [number, number, number, number];

export interface ManualSplitDraft {
  sourcePath: string;
  displayName: string;
  width: number;
  height: number;
  lines: ManualSplitLines;
  baselineLines: ManualSplitLines;
  locked: boolean;
  thumbnailPath?: string | null;
  lastAppliedAt?: string;
  recommendedLines?: ManualSplitLines | null;
  history: ManualSplitLines[];
  redoStack: ManualSplitLines[];
  hasPendingChanges: boolean;
}

export interface SplitPreviewPayload {
  sourcePath: string;
  leftPreviewPath: string | null;
  rightPreviewPath: string | null;
  gutterPreviewPath?: string | null;
  generatedAt: string;
}

export interface ManualSplitReportSummary {
  total: number;
  applied: number;
  skipped: number;
  generatedAt: string;
}

export interface ManualSplitDraftPayload {
  sourcePath: string;
  width: number;
  height: number;
  displayName?: string;
  recommendedLines?: ManualSplitLines | null;
  existingLines?: ManualSplitLines | null;
  locked?: boolean;
  lastAppliedAt?: string | null;
  thumbnailPath?: string | null;
}

export type SplitApplyTarget = 'single' | 'all' | null;

export interface ManualApplyState {
  running: boolean;
  total: number;
  completed: number;
  currentSource: string | null;
  statusText: string | null;
  errorBubble: string | null;
  lastFinishedAt: string | null;
}

export interface CustomSplitState {
  drafts: Record<string, ManualSplitDraft>;
  order: string[];
  selection: string[];
  pendingApply: SplitApplyTarget;
  accelerator: 'cpu' | 'gpu' | 'auto';
  gutterWidthRatio: number;
  loading: boolean;
  error: string | null;
  previewMap: Record<string, SplitPreviewPayload>;
  workspace: string | null;
  initialized: boolean;
  drawerOpen: boolean;
  manualReportPath: string | null;
  manualReportSummary: ManualSplitReportSummary | null;
  canRevert: boolean;
  hasRevertHistory: boolean;
  applyState: ManualApplyState;
  openDrawer: () => void;
  closeDrawer: () => void;
  setLoading: (value: boolean) => void;
  setError: (message: string | null) => void;
  hydrateDrafts: (workspace: string, payloads: ManualSplitDraftPayload[]) => void;
  reset: () => void;
  updateLines: (sourcePath: string, lines: ManualSplitLines) => void;
  setSelection: (sources: string[]) => void;
  toggleSelection: (sourcePath: string) => void;
  setAccelerator: (accelerator: 'cpu' | 'gpu' | 'auto') => void;
  setPreview: (sourcePath: string, preview: SplitPreviewPayload | null) => void;
  setPendingApply: (target: SplitApplyTarget) => void;
  toggleLock: (sourcePath: string) => void;
  setLockState: (sourcePath: string, locked: boolean) => void;
  markApplied: (entries: Array<{ sourcePath: string; appliedAt: string }>) => void;
  undoLines: (sourcePath: string) => void;
  redoLines: (sourcePath: string) => void;
  resetLines: (sourcePath: string) => void;
  resetAllLines: () => void;
  setManualReport: (payload: {
    path: string | null;
    summary?: ManualSplitReportSummary | null;
  }) => void;
  setCanRevert: (value: boolean) => void;
  setHasRevertHistory: (value: boolean) => void;
  beginApply: (total: number) => void;
  updateApplyProgress: (
    completed: number,
    total: number,
    currentSource?: string | null
  ) => void;
  resolveApplySucceeded: (
    statusText: string | null,
    completed?: number,
    total?: number
  ) => void;
  resolveApplyFailed: (message: string) => void;
  clearApplyFeedback: () => void;
  setGutterWidthRatio: (ratio: number) => void;
}

const DEFAULT_LINES: ManualSplitLines = [0.02, 0.48, 0.52, 0.98];
const HISTORY_LIMIT = 20;

const INITIAL_APPLY_STATE: ManualApplyState = {
  running: false,
  total: 0,
  completed: 0,
  currentSource: null,
  statusText: null,
  errorBubble: null,
  lastFinishedAt: null,
};

export const computeNormalizedLines = (
  input: ManualSplitLines,
  gutterWidthRatio: number
): ManualSplitLines => {
  const [rawLeftTrim, rawLeftPageEnd, rawRightPageStart, rawRightTrim] = input;
  const clamp = (value: number) => Math.min(1, Math.max(0, value));

  let leftTrim = clamp(rawLeftTrim);
  let leftPageEnd = clamp(rawLeftPageEnd);
  let rightPageStart = clamp(rawRightPageStart);
  let rightTrim = clamp(rawRightTrim);

  if (!Number.isFinite(leftTrim)) leftTrim = 0;
  if (!Number.isFinite(leftPageEnd)) leftPageEnd = leftTrim + gutterWidthRatio;
  if (!Number.isFinite(rightPageStart)) {
    rightPageStart = leftPageEnd + gutterWidthRatio;
  }
  if (!Number.isFinite(rightTrim)) rightTrim = 1;

  if (leftTrim >= 1) {
    return [1, 1, 1, 1];
  }

  if (leftPageEnd < leftTrim + gutterWidthRatio) {
    leftPageEnd = leftTrim + gutterWidthRatio;
  }

  if (rightPageStart < leftPageEnd + gutterWidthRatio) {
    rightPageStart = leftPageEnd + gutterWidthRatio;
  }

  if (rightTrim < rightPageStart + gutterWidthRatio) {
    rightTrim = rightPageStart + gutterWidthRatio;
  }

  if (rightTrim > 1) {
    const overflow = rightTrim - 1;
    rightTrim = 1;
    rightPageStart = Math.max(leftPageEnd + gutterWidthRatio, rightPageStart - overflow);
    leftPageEnd = Math.max(leftTrim + gutterWidthRatio, leftPageEnd - overflow);
  }

  if (rightPageStart > 1) {
    const overflow = rightPageStart - 1;
    rightPageStart = 1;
    leftPageEnd = Math.max(leftTrim + gutterWidthRatio, leftPageEnd - overflow);
    if (leftPageEnd > 1) {
      leftPageEnd = 1;
      leftTrim = Math.max(0, leftPageEnd - gutterWidthRatio);
    }
  }

  const normalize = (value: number) => Number(Math.min(1, Math.max(0, value)).toFixed(6));

  return [
    normalize(leftTrim),
    normalize(leftPageEnd),
    normalize(rightPageStart),
    normalize(rightTrim),
  ];
};

const createDraftFromPayload = (
  payload: ManualSplitDraftPayload,
  gutterWidthRatio: number
): ManualSplitDraft => {
  const { sourcePath, width, height } = payload;
  const displayName = payload.displayName ?? buildDisplayName(sourcePath);
  const recommendedBase = payload.recommendedLines
    ? computeNormalizedLines(payload.recommendedLines, gutterWidthRatio)
    : computeNormalizedLines(DEFAULT_LINES, gutterWidthRatio);
  const existingBase = payload.existingLines
    ? computeNormalizedLines(payload.existingLines, gutterWidthRatio)
    : null;
  const initial = [...(existingBase ?? recommendedBase)] as ManualSplitLines;
  const baseline = [...(existingBase ?? recommendedBase)] as ManualSplitLines;

  const normalizedRecommended = payload.recommendedLines
    ? ([...recommendedBase] as ManualSplitLines)
    : null;

  return {
    sourcePath,
    displayName,
    width,
    height,
    lines: initial,
    baselineLines: baseline,
    locked: Boolean(payload.locked),
    lastAppliedAt: payload.lastAppliedAt ?? undefined,
    thumbnailPath: payload.thumbnailPath ?? null,
    recommendedLines: normalizedRecommended,
    history: [],
    redoStack: [],
    hasPendingChanges: false,
  };
};

const buildDisplayName = (sourcePath: string): string => {
  const fragment = sourcePath.split(/[/\\]/).pop();
  if (!fragment || fragment.trim().length === 0) {
    return sourcePath;
  }
  return fragment;
};

export const useCustomSplitStore = create<CustomSplitState>((set, get) => ({
  drafts: {},
  order: [],
  selection: [],
  pendingApply: null,
  accelerator: 'auto',
  gutterWidthRatio: 0.01,
  loading: false,
  error: null,
  previewMap: {},
  workspace: null,
  initialized: false,
  drawerOpen: false,
  manualReportPath: null,
  manualReportSummary: null,
  canRevert: false,
  hasRevertHistory: false,
  applyState: { ...INITIAL_APPLY_STATE },
  openDrawer: () => set({ drawerOpen: true }),
  closeDrawer: () => set({ drawerOpen: false }),
  setLoading: (value) => set({ loading: value }),
  setError: (message) => set({ error: message }),
  reset: () =>
    set({
      drafts: {},
      order: [],
      selection: [],
      pendingApply: null,
      previewMap: {},
      error: null,
      loading: false,
      initialized: false,
      workspace: null,
      manualReportPath: null,
      manualReportSummary: null,
      canRevert: false,
      hasRevertHistory: false,
      applyState: { ...INITIAL_APPLY_STATE },
    }),
  hydrateDrafts: (workspace, payloads) => {
    const gutterWidthRatio = get().gutterWidthRatio;
    const drafts: Record<string, ManualSplitDraft> = {};
    const order: string[] = [];

    for (const payload of payloads) {
      const draft = createDraftFromPayload(payload, gutterWidthRatio);
      drafts[draft.sourcePath] = draft;
      order.push(draft.sourcePath);
    }

    set({
      drafts,
      order,
      selection: order.length > 0 ? [order[0]] : [],
      workspace,
      initialized: true,
      pendingApply: null,
      error: null,
    });
  },
  updateLines: (sourcePath, lines) => {
    const state = get();
    const draft = state.drafts[sourcePath];
    if (!draft) {
      return;
    }
    const normalized = computeNormalizedLines(lines, state.gutterWidthRatio);
    const current = draft.lines;
    const areSame =
      normalized[0] === current[0] &&
      normalized[1] === current[1] &&
      normalized[2] === current[2] &&
      normalized[3] === current[3];
    if (areSame) {
      return;
    }

    const pushHistory = () => {
      const snapshot = [...current] as ManualSplitLines;
      if (draft.history.length >= HISTORY_LIMIT) {
        return [...draft.history.slice(-HISTORY_LIMIT + 1), snapshot];
      }
      return [...draft.history, snapshot];
    };

    const nextHistory = pushHistory();
    const hasPendingChanges =
      normalized[0] !== draft.baselineLines[0] ||
      normalized[1] !== draft.baselineLines[1] ||
      normalized[2] !== draft.baselineLines[2] ||
      normalized[3] !== draft.baselineLines[3];
    set({
      drafts: {
        ...state.drafts,
        [sourcePath]: {
          ...draft,
          lines: normalized,
          history: nextHistory,
          redoStack: [],
          hasPendingChanges,
        },
      },
    });
  },
  setSelection: (sources) => set({ selection: [...new Set(sources)] }),
  toggleSelection: (sourcePath) => {
    const selection = get().selection;
    if (selection.includes(sourcePath)) {
      set({ selection: selection.filter((item) => item !== sourcePath) });
    } else {
      set({ selection: [...selection, sourcePath] });
    }
  },
  setAccelerator: (accelerator) => set({ accelerator }),
  setPreview: (sourcePath, preview) => {
    const nextMap = { ...get().previewMap };
    if (!preview) {
      delete nextMap[sourcePath];
    } else {
      nextMap[sourcePath] = preview;
    }
    set({ previewMap: nextMap });
  },
  setPendingApply: (target) => set({ pendingApply: target }),
  toggleLock: (sourcePath) => {
    const draft = get().drafts[sourcePath];
    if (!draft) {
      return;
    }
    set({
      drafts: {
        ...get().drafts,
        [sourcePath]: {
          ...draft,
          locked: !draft.locked,
        },
      },
    });
  },
  setLockState: (sourcePath, locked) => {
    const state = get();
    const draft = state.drafts[sourcePath];
    if (!draft || draft.locked === locked) {
      return;
    }
    set({
      drafts: {
        ...state.drafts,
        [sourcePath]: {
          ...draft,
          locked,
        },
      },
    });
  },
  setManualReport: ({ path, summary }) => {
    set({
      manualReportPath: path ?? null,
      manualReportSummary: summary ?? null,
    });
  },
  setCanRevert: (value) => set({ canRevert: value }),
  setHasRevertHistory: (value) => set({ hasRevertHistory: value }),
  undoLines: (sourcePath) => {
    const draft = get().drafts[sourcePath];
    if (!draft || draft.history.length === 0) {
      return;
    }
    const previous = draft.history[draft.history.length - 1];
    const hasPendingChanges =
      previous[0] !== draft.baselineLines[0] ||
      previous[1] !== draft.baselineLines[1] ||
      previous[2] !== draft.baselineLines[2] ||
      previous[3] !== draft.baselineLines[3];
    const nextHistory = draft.history.slice(0, -1);
    const redoEntry = [...draft.lines] as ManualSplitLines;
    const nextRedo =
      draft.redoStack.length >= HISTORY_LIMIT
        ? [...draft.redoStack.slice(-HISTORY_LIMIT + 1), redoEntry]
        : [...draft.redoStack, redoEntry];
    set({
      drafts: {
        ...get().drafts,
        [sourcePath]: {
          ...draft,
          lines: previous,
          history: nextHistory,
          redoStack: nextRedo,
          hasPendingChanges,
        },
      },
    });
  },
  redoLines: (sourcePath) => {
    const draft = get().drafts[sourcePath];
    if (!draft || draft.redoStack.length === 0) {
      return;
    }
    const nextLines = draft.redoStack[draft.redoStack.length - 1];
    const nextRedo = draft.redoStack.slice(0, -1);
    const historyEntry = [...draft.lines] as ManualSplitLines;
    const nextHistory =
      draft.history.length >= HISTORY_LIMIT
        ? [...draft.history.slice(-HISTORY_LIMIT + 1), historyEntry]
        : [...draft.history, historyEntry];
    const hasPendingChanges =
      nextLines[0] !== draft.baselineLines[0] ||
      nextLines[1] !== draft.baselineLines[1] ||
      nextLines[2] !== draft.baselineLines[2] ||
      nextLines[3] !== draft.baselineLines[3];
    set({
      drafts: {
        ...get().drafts,
        [sourcePath]: {
          ...draft,
          lines: [...nextLines] as ManualSplitLines,
          history: nextHistory,
          redoStack: nextRedo,
          hasPendingChanges,
        },
      },
    });
  },
  resetLines: (sourcePath) => {
    const draft = get().drafts[sourcePath];
    if (!draft) {
      return;
    }
    const target = draft.baselineLines;
    const normalized = [...target] as ManualSplitLines;
    const isSame =
      normalized[0] === draft.lines[0] &&
      normalized[1] === draft.lines[1] &&
      normalized[2] === draft.lines[2] &&
      normalized[3] === draft.lines[3];
    if (isSame) {
      return;
    }
    const historyEntry = [...draft.lines] as ManualSplitLines;
    const nextHistory =
      draft.history.length >= HISTORY_LIMIT
        ? [...draft.history.slice(-HISTORY_LIMIT + 1), historyEntry]
        : [...draft.history, historyEntry];
    set({
      drafts: {
        ...get().drafts,
        [sourcePath]: {
          ...draft,
          lines: normalized,
          history: nextHistory,
          redoStack: [],
          hasPendingChanges: false,
        },
      },
    });
  },
  resetAllLines: () => {
    const state = get();
    const nextDrafts: Record<string, ManualSplitDraft> = {};
    let mutated = false;
    for (const [sourcePath, draft] of Object.entries(state.drafts)) {
      const target = draft.baselineLines;
      const isSame =
        draft.lines[0] === target[0] &&
        draft.lines[1] === target[1] &&
        draft.lines[2] === target[2] &&
        draft.lines[3] === target[3];
      if (isSame) {
        nextDrafts[sourcePath] = draft;
        continue;
      }
      mutated = true;
      const historyEntry = [...draft.lines] as ManualSplitLines;
      const nextHistory =
        draft.history.length >= HISTORY_LIMIT
          ? [...draft.history.slice(-HISTORY_LIMIT + 1), historyEntry]
          : [...draft.history, historyEntry];
      nextDrafts[sourcePath] = {
        ...draft,
        lines: [...target] as ManualSplitLines,
        history: nextHistory,
        redoStack: [],
        hasPendingChanges: false,
      };
    }
    if (mutated) {
      set({ drafts: nextDrafts });
    }
  },
  markApplied: (entries) => {
    if (!entries || entries.length === 0) {
      return;
    }
    const appliedMap = new Map(entries.map((entry) => [entry.sourcePath, entry.appliedAt]));
    const currentDrafts = get().drafts;
    let mutated = false;
    const nextDrafts: Record<string, ManualSplitDraft> = {};
    for (const [sourcePath, draft] of Object.entries(currentDrafts)) {
      const appliedAt = appliedMap.get(sourcePath);
      if (appliedAt) {
        mutated = true;
        nextDrafts[sourcePath] = {
          ...draft,
          lastAppliedAt: appliedAt,
          baselineLines: [...draft.lines] as ManualSplitLines,
          history: [],
          redoStack: [],
          hasPendingChanges: false,
        };
      } else {
        nextDrafts[sourcePath] = draft;
      }
    }
    if (mutated) {
      set({ drafts: nextDrafts });
    }
  },
  beginApply: (total) => {
    const safeTotal = Number.isFinite(total) && total > 0 ? Math.max(1, Math.floor(total)) : 1;
    set((state) => ({
      applyState: {
        ...state.applyState,
        running: true,
        total: safeTotal,
        completed: 0,
        currentSource: null,
        statusText: null,
        errorBubble: null,
      },
    }));
  },
  updateApplyProgress: (completed, total, currentSource) => {
    set((state) => {
      const nextTotal = Number.isFinite(total) && total >= 0 ? total : state.applyState.total;
      const boundedTotal = nextTotal > 0 ? nextTotal : state.applyState.total;
      const boundedCompleted = Math.max(0, Math.min(completed, Math.max(boundedTotal, 1)));
      return {
        applyState: {
          ...state.applyState,
          running: true,
          total: boundedTotal,
          completed: boundedCompleted,
          currentSource: currentSource ?? state.applyState.currentSource,
        },
      };
    });
  },
  resolveApplySucceeded: (statusText, completed, total) => {
    set((state) => {
      const resolvedTotal =
        total !== undefined && Number.isFinite(total) && total >= 0
          ? total
          : state.applyState.total;
      const resolvedCompleted =
        completed !== undefined && Number.isFinite(completed) && completed >= 0
          ? completed
          : Math.max(state.applyState.completed, resolvedTotal);
      return {
        applyState: {
          ...state.applyState,
          running: false,
          statusText,
          errorBubble: null,
          total: resolvedTotal,
          completed: resolvedCompleted,
          currentSource: null,
          lastFinishedAt: new Date().toISOString(),
        },
      };
    });
  },
  resolveApplyFailed: (message) => {
    set((state) => ({
      applyState: {
        ...state.applyState,
        running: false,
        errorBubble: message,
        statusText: null,
        currentSource: null,
        lastFinishedAt: new Date().toISOString(),
      },
    }));
  },
  clearApplyFeedback: () => {
    set((state) => ({
      applyState: {
        ...state.applyState,
        statusText: null,
        errorBubble: null,
      },
    }));
  },
  setGutterWidthRatio: (ratioInput) => {
    const numeric = Number(ratioInput);
    if (!Number.isFinite(numeric)) {
      return;
    }
    const clamped = Math.max(0, Math.min(numeric, 0.5));
    const currentRatio = get().gutterWidthRatio;
    if (Math.abs(clamped - currentRatio) < 1e-6) {
      return;
    }
    set((state) => {
      const updatedDrafts: Record<string, ManualSplitDraft> = {};
      for (const [sourcePath, draft] of Object.entries(state.drafts)) {
        const baseline = computeNormalizedLines(draft.baselineLines, clamped);
        const lines = computeNormalizedLines(draft.lines, clamped);
        const history = draft.history.map((entry) =>
          computeNormalizedLines(entry, clamped)
        );
        const redoStack = draft.redoStack.map((entry) =>
          computeNormalizedLines(entry, clamped)
        );
        const hasPendingChanges =
          lines[0] !== baseline[0] ||
          lines[1] !== baseline[1] ||
          lines[2] !== baseline[2] ||
          lines[3] !== baseline[3];
        updatedDrafts[sourcePath] = {
          ...draft,
          baselineLines: baseline,
          lines,
          history,
          redoStack,
          hasPendingChanges,
        };
      }
      return {
        gutterWidthRatio: clamped,
        drafts: updatedDrafts,
      };
    });
  },
}));
