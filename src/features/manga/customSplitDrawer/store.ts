import { create } from 'zustand';

export type ManualSplitLines = [number, number, number, number];

export type ManualImageKind = 'content' | 'cover' | 'spread';

type ManualLineMode = 'quad' | 'double';

export interface ManualSplitDraft {
  sourcePath: string;
  displayName: string;
  width: number;
  height: number;
  lines: ManualSplitLines;
  baselineLines: ManualSplitLines;
  baselineImageKind: ManualImageKind;
  baselineRotate90: boolean;
  stagedLines: ManualSplitLines;
  stagedImageKind: ManualImageKind;
  stagedRotate90: boolean;
  staged: boolean;
  imageKind: ManualImageKind;
  rotate90: boolean;
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
  imageKind?: ManualImageKind | null;
  rotate90?: boolean | null;
}

export interface ManualApplyResultEntry {
  sourcePath: string;
  appliedAt: string;
  lines: ManualSplitLines;
  imageKind: ManualImageKind;
  rotate90: boolean;
}

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
  setImageKind: (sourcePath: string, kind: ManualImageKind) => void;
  setSelection: (sources: string[]) => void;
  toggleSelection: (sourcePath: string) => void;
  setAccelerator: (accelerator: 'cpu' | 'gpu' | 'auto') => void;
  setPreview: (sourcePath: string, preview: SplitPreviewPayload | null) => void;
  stageDraft: (sourcePath: string) => void;
  applyCurrentToAllUnlocked: (sourcePath: string) => void;
  clearStage: (sourcePath: string) => void;
  clearAllStages: () => void;
  toggleLock: (sourcePath: string) => void;
  setLockState: (sourcePath: string, locked: boolean) => void;
  markApplied: (entries: ManualApplyResultEntry[]) => void;
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
const LINE_PRECISION = 1e-6;

const DEFAULT_IMAGE_KIND: ManualImageKind = 'content';

const asMode = (kind: ManualImageKind): ManualLineMode =>
  kind === 'content' ? 'quad' : 'double';

const normalizeForKind = (
  input: ManualSplitLines,
  gutterWidthRatio: number,
  kind: ManualImageKind
): ManualSplitLines => computeNormalizedLines(input, gutterWidthRatio, asMode(kind));

const areLinesEqual = (first: ManualSplitLines, second: ManualSplitLines): boolean => {
  return (
    Math.abs(first[0] - second[0]) < LINE_PRECISION &&
    Math.abs(first[1] - second[1]) < LINE_PRECISION &&
    Math.abs(first[2] - second[2]) < LINE_PRECISION &&
    Math.abs(first[3] - second[3]) < LINE_PRECISION
  );
};

const clamp01 = (value: number): number => Math.min(1, Math.max(0, value));

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
  gutterWidthRatio: number,
  mode: ManualLineMode = 'quad'
): ManualSplitLines => {
  if (mode === 'double') {
    const guardWidth = Math.max(gutterWidthRatio, 0.001);
    let leftTrim = clamp01(Number.isFinite(input[0]) ? input[0] : 0);
    let rightTrim = clamp01(Number.isFinite(input[3]) ? input[3] : 1);

    if (rightTrim - leftTrim < guardWidth) {
      rightTrim = Math.min(1, leftTrim + guardWidth);
      if (rightTrim >= 1 && leftTrim > 0) {
        leftTrim = Math.max(0, 1 - guardWidth);
      }
    }

    const normalize = (value: number) => Number(clamp01(value).toFixed(6));
    const normalizedLeft = normalize(leftTrim);
    const normalizedRight = normalize(rightTrim);
    return [
      normalizedLeft,
      normalizedLeft,
      normalizedRight,
      normalizedRight,
    ];
  }

  const [rawLeftTrim, rawLeftPageEnd, rawRightPageStart, rawRightTrim] = input;

  let leftTrim = clamp01(rawLeftTrim);
  let leftPageEnd = clamp01(rawLeftPageEnd);
  let rightPageStart = clamp01(rawRightPageStart);
  let rightTrim = clamp01(rawRightTrim);

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

  const normalize = (value: number) => Number(clamp01(value).toFixed(6));

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
  const baseKind = payload.imageKind ?? DEFAULT_IMAGE_KIND;
  const rotate90 = payload.rotate90 ?? (baseKind === 'spread');

  const recommendedBase = payload.recommendedLines
    ? normalizeForKind(payload.recommendedLines, gutterWidthRatio, baseKind)
    : normalizeForKind(DEFAULT_LINES, gutterWidthRatio, baseKind);
  const existingBase = payload.existingLines
    ? normalizeForKind(payload.existingLines, gutterWidthRatio, baseKind)
    : null;

  const baseline = [...(existingBase ?? recommendedBase)] as ManualSplitLines;
  const initial = [...baseline] as ManualSplitLines;

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
     baselineImageKind: baseKind,
     baselineRotate90: rotate90,
     stagedLines: [...baseline] as ManualSplitLines,
     stagedImageKind: baseKind,
     stagedRotate90: rotate90,
     staged: false,
     imageKind: baseKind,
     rotate90,
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
      error: null,
    });
  },
  updateLines: (sourcePath, lines) => {
    const state = get();
    const draft = state.drafts[sourcePath];
    if (!draft) {
      return;
    }
    const normalized = normalizeForKind(lines, state.gutterWidthRatio, draft.imageKind);
    if (areLinesEqual(normalized, draft.lines)) {
      return;
    }

    const snapshot = [...draft.lines] as ManualSplitLines;
    const nextHistory =
      draft.history.length >= HISTORY_LIMIT
        ? [...draft.history.slice(-HISTORY_LIMIT + 1), snapshot]
        : [...draft.history, snapshot];
    const stagedMatches =
      draft.staged &&
      draft.imageKind === draft.stagedImageKind &&
      draft.rotate90 === draft.stagedRotate90 &&
      areLinesEqual(normalized, draft.stagedLines);
    set({
      drafts: {
        ...state.drafts,
        [sourcePath]: {
          ...draft,
          lines: normalized,
          history: nextHistory,
          redoStack: [],
          hasPendingChanges: !stagedMatches,
          staged: stagedMatches ? draft.staged : false,
        },
      },
    });
  },
  setImageKind: (sourcePath, kind) => {
    set((state) => {
      const draft = state.drafts[sourcePath];
      if (!draft) {
        return {};
      }
      const rotate90 = kind === 'spread' ? true : draft.rotate90;
      const normalizedLines = normalizeForKind(draft.lines, state.gutterWidthRatio, kind);
      const normalizedBaseline = normalizeForKind(
        draft.baselineLines,
        state.gutterWidthRatio,
        kind
      );
      const normalizedStaged = normalizeForKind(
        draft.stagedLines,
        state.gutterWidthRatio,
        kind
      );
      const normalizedRecommended = draft.recommendedLines
        ? normalizeForKind(draft.recommendedLines, state.gutterWidthRatio, kind)
        : null;

      const stagedMatches =
        draft.staged &&
        kind === draft.stagedImageKind &&
        rotate90 === draft.stagedRotate90 &&
        areLinesEqual(normalizedLines, normalizedStaged);

      return {
        drafts: {
          ...state.drafts,
          [sourcePath]: {
            ...draft,
            imageKind: kind,
            rotate90,
            lines: normalizedLines,
            baselineLines: normalizedBaseline,
            stagedLines: normalizedStaged,
            stagedImageKind: stagedMatches ? kind : draft.stagedImageKind,
            stagedRotate90: stagedMatches ? rotate90 : draft.stagedRotate90,
            staged: stagedMatches ? draft.staged : false,
            hasPendingChanges: !stagedMatches,
            recommendedLines: normalizedRecommended,
            history: [],
            redoStack: [],
          },
        },
      };
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
  stageDraft: (sourcePath) => {
    set((state) => {
      const draft = state.drafts[sourcePath];
      if (!draft || draft.locked) {
        return {};
      }
      const rotate90 = draft.imageKind === 'spread' ? true : draft.rotate90;
      const normalized = normalizeForKind(draft.lines, state.gutterWidthRatio, draft.imageKind);
      const stagedLines = [...normalized] as ManualSplitLines;
      return {
        drafts: {
          ...state.drafts,
          [sourcePath]: {
            ...draft,
            lines: normalized,
            rotate90,
            stagedLines,
            stagedImageKind: draft.imageKind,
            stagedRotate90: rotate90,
            staged: true,
            locked: true,
            hasPendingChanges: false,
          },
        },
      };
    });
  },
  applyCurrentToAllUnlocked: (sourcePath) => {
    set((state) => {
      const baseDraft = state.drafts[sourcePath];
      if (!baseDraft || baseDraft.locked) {
        return {};
      }

      const baseNormalized = normalizeForKind(
        baseDraft.lines,
        state.gutterWidthRatio,
        baseDraft.imageKind
      );

      let mutated = false;
      const nextDrafts: Record<string, ManualSplitDraft> = { ...state.drafts };

      for (const draftPath of state.order) {
        const draft = nextDrafts[draftPath];
        if (!draft) {
          continue;
        }

        const isBase = draftPath === sourcePath;
        if (!isBase && (draft.locked || draft.staged)) {
          continue;
        }

        const targetNormalized = isBase
          ? baseNormalized
          : normalizeForKind(baseNormalized, state.gutterWidthRatio, draft.imageKind);
        const targetLines = [...targetNormalized] as ManualSplitLines;
        const targetRotate90 = draft.imageKind === 'spread' ? true : draft.rotate90;

        nextDrafts[draftPath] = {
          ...draft,
          lines: targetLines,
          rotate90: targetRotate90,
          stagedLines: [...targetLines] as ManualSplitLines,
          stagedImageKind: draft.imageKind,
          stagedRotate90: targetRotate90,
          staged: true,
          locked: true,
          hasPendingChanges: false,
        };
        mutated = true;
      }

      if (!mutated) {
        return {};
      }

      return { drafts: nextDrafts };
    });
  },
  clearStage: (sourcePath) => {
    set((state) => {
      const draft = state.drafts[sourcePath];
      if (!draft) {
        return {};
      }
      const baselineForView = normalizeForKind(
        draft.baselineLines,
        state.gutterWidthRatio,
        draft.imageKind
      );
      const stagedLines = normalizeForKind(
        draft.baselineLines,
        state.gutterWidthRatio,
        draft.baselineImageKind
      );
      const hasPendingChanges = !areLinesEqual(draft.lines, baselineForView);
      return {
        drafts: {
          ...state.drafts,
          [sourcePath]: {
            ...draft,
            stagedLines,
            stagedImageKind: draft.baselineImageKind,
            stagedRotate90: draft.baselineRotate90,
            staged: false,
            hasPendingChanges,
          },
        },
      };
    });
  },
  clearAllStages: () => {
    set((state) => {
      let mutated = false;
      const nextDrafts: Record<string, ManualSplitDraft> = { ...state.drafts };
      for (const sourcePath of state.order) {
        const draft = nextDrafts[sourcePath];
        if (!draft || !draft.staged) {
          continue;
        }
        const baselineForView = normalizeForKind(
          draft.baselineLines,
          state.gutterWidthRatio,
          draft.imageKind
        );
        const stagedLines = normalizeForKind(
          draft.baselineLines,
          state.gutterWidthRatio,
          draft.baselineImageKind
        );
        nextDrafts[sourcePath] = {
          ...draft,
          stagedLines,
          stagedImageKind: draft.baselineImageKind,
          stagedRotate90: draft.baselineRotate90,
          staged: false,
          hasPendingChanges: !areLinesEqual(draft.lines, baselineForView),
        };
        mutated = true;
      }
      if (!mutated) {
        return {};
      }
      return { drafts: nextDrafts };
    });
  },
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
    const nextHistory = draft.history.slice(0, -1);
    const redoEntry = [...draft.lines] as ManualSplitLines;
    const nextRedo =
      draft.redoStack.length >= HISTORY_LIMIT
        ? [...draft.redoStack.slice(-HISTORY_LIMIT + 1), redoEntry]
        : [...draft.redoStack, redoEntry];
    const stagedMatches =
      draft.staged &&
      draft.imageKind === draft.stagedImageKind &&
      draft.rotate90 === draft.stagedRotate90 &&
      areLinesEqual(previous, draft.stagedLines);
    set({
      drafts: {
        ...get().drafts,
        [sourcePath]: {
          ...draft,
          lines: previous,
          history: nextHistory,
          redoStack: nextRedo,
          hasPendingChanges: !stagedMatches,
          staged: stagedMatches ? draft.staged : false,
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
    const stagedMatches =
      draft.staged &&
      draft.imageKind === draft.stagedImageKind &&
      draft.rotate90 === draft.stagedRotate90 &&
      areLinesEqual(nextLines as ManualSplitLines, draft.stagedLines);
    set({
      drafts: {
        ...get().drafts,
        [sourcePath]: {
          ...draft,
          lines: [...nextLines] as ManualSplitLines,
          history: nextHistory,
          redoStack: nextRedo,
          hasPendingChanges: !stagedMatches,
          staged: stagedMatches ? draft.staged : false,
        },
      },
    });
  },
  resetLines: (sourcePath) => {
    const draft = get().drafts[sourcePath];
    if (!draft) {
      return;
    }
    const state = get();
    const normalized = normalizeForKind(
      draft.baselineLines,
      state.gutterWidthRatio,
      draft.imageKind
    );
    const isSame = areLinesEqual(normalized, draft.lines);
    if (isSame) {
      return;
    }
    const historyEntry = [...draft.lines] as ManualSplitLines;
    const nextHistory =
      draft.history.length >= HISTORY_LIMIT
        ? [...draft.history.slice(-HISTORY_LIMIT + 1), historyEntry]
        : [...draft.history, historyEntry];
    const stagedMatches =
      draft.staged &&
      draft.imageKind === draft.stagedImageKind &&
      draft.rotate90 === draft.stagedRotate90 &&
      areLinesEqual(normalized, draft.stagedLines);
    set({
      drafts: {
        ...get().drafts,
        [sourcePath]: {
          ...draft,
          lines: normalized,
          history: nextHistory,
          redoStack: [],
          hasPendingChanges: !stagedMatches,
          staged: stagedMatches ? draft.staged : false,
        },
      },
    });
  },
  resetAllLines: () => {
    const state = get();
    const nextDrafts: Record<string, ManualSplitDraft> = {};
    let mutated = false;
    for (const [sourcePath, draft] of Object.entries(state.drafts)) {
      const normalized = normalizeForKind(
        draft.baselineLines,
        state.gutterWidthRatio,
        draft.imageKind
      );
      const isSame = areLinesEqual(draft.lines, normalized);
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
      const stagedMatches =
        draft.staged &&
        draft.imageKind === draft.stagedImageKind &&
        draft.rotate90 === draft.stagedRotate90 &&
        areLinesEqual(normalized, draft.stagedLines);
      nextDrafts[sourcePath] = {
        ...draft,
        lines: normalized,
        history: nextHistory,
        redoStack: [],
        hasPendingChanges: !stagedMatches,
        staged: stagedMatches ? draft.staged : false,
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
    set((state) => {
      let mutated = false;
      const nextDrafts: Record<string, ManualSplitDraft> = { ...state.drafts };
      for (const entry of entries) {
        const draft = nextDrafts[entry.sourcePath];
        if (!draft) {
          continue;
        }
        const normalized = normalizeForKind(
          entry.lines,
          state.gutterWidthRatio,
          entry.imageKind
        );
        const imageKind = entry.imageKind;
        const rotate90 = imageKind === 'spread' ? true : entry.rotate90;
        nextDrafts[entry.sourcePath] = {
          ...draft,
          lines: normalized,
          imageKind,
          rotate90,
          baselineLines: [...normalized] as ManualSplitLines,
          baselineImageKind: imageKind,
          baselineRotate90: rotate90,
          stagedLines: [...normalized] as ManualSplitLines,
          stagedImageKind: imageKind,
          stagedRotate90: rotate90,
          staged: false,
          hasPendingChanges: false,
          lastAppliedAt: entry.appliedAt,
          history: [],
          redoStack: [],
        };
        mutated = true;
      }
      return mutated ? { drafts: nextDrafts } : {};
    });
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
        const baseline = normalizeForKind(
          draft.baselineLines,
          clamped,
          draft.baselineImageKind
        );
        const lines = normalizeForKind(draft.lines, clamped, draft.imageKind);
        const history = draft.history.map((entry) =>
          normalizeForKind(entry, clamped, draft.imageKind)
        );
        const redoStack = draft.redoStack.map((entry) =>
          normalizeForKind(entry, clamped, draft.imageKind)
        );
        const stagedLines = normalizeForKind(
          draft.stagedLines,
          clamped,
          draft.stagedImageKind
        );
        const recommendedLines = draft.recommendedLines
          ? normalizeForKind(draft.recommendedLines, clamped, draft.imageKind)
          : null;
        const stagedMatches =
          draft.staged &&
          draft.imageKind === draft.stagedImageKind &&
          draft.rotate90 === draft.stagedRotate90 &&
          areLinesEqual(lines, stagedLines);
        updatedDrafts[sourcePath] = {
          ...draft,
          baselineLines: baseline,
          lines,
          history,
          redoStack,
          stagedLines,
          recommendedLines,
          hasPendingChanges: !stagedMatches,
          staged: stagedMatches ? draft.staged : false,
        };
      }
      return {
        gutterWidthRatio: clamped,
        drafts: updatedDrafts,
      };
    });
  },
}));
