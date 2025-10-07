import type { ManualSplitLines, SplitPreviewPayload } from './store.js';

export interface PreviewCacheEntry {
  signature: string;
  payload: SplitPreviewPayload;
  storedAt: number;
}

export type PreviewCache = Map<string, PreviewCacheEntry>;

export const createPreviewCache = (): PreviewCache => new Map();

export const computePreviewSignature = (lines: ManualSplitLines): string => {
  return lines.map((value) => Number(value).toFixed(4)).join(':');
};

export const readPreviewCache = (
  cache: PreviewCache,
  sourcePath: string,
  signature: string
): { hit: true; entry: PreviewCacheEntry; ageMs: number } | { hit: false } => {
  const entry = cache.get(sourcePath);
  if (!entry || entry.signature !== signature) {
    return { hit: false };
  }
  const ageMs = Math.max(0, Date.now() - entry.storedAt);
  return { hit: true, entry, ageMs };
};

export const writePreviewCache = (
  cache: PreviewCache,
  sourcePath: string,
  signature: string,
  payload: SplitPreviewPayload
): PreviewCacheEntry => {
  const entry: PreviewCacheEntry = {
    signature,
    payload,
    storedAt: Date.now(),
  };
  cache.set(sourcePath, entry);
  return entry;
};

export const clearPreviewCache = (cache: PreviewCache): void => {
  cache.clear();
};
