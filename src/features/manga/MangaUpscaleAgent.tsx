import type { ChangeEvent, UIEvent } from 'react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import CustomSplitDrawer from './customSplitDrawer/CustomSplitDrawer.js';
import ManualSplitIntro from './customSplitDrawer/ManualSplitIntro.js';
import { useManualSplitController } from './customSplitDrawer/useManualSplitController.js';
import { buildManualCrossModeNotice } from './manualReadiness.js';

type RenameEntry = {
  originalName: string;
  renamedName: string;
};

type ManifestManualEntry = {
  source: string;
  outputs: string[];
  lines: [number, number, number, number];
  percentages: [number, number, number, number];
  accelerator?: string | null;
  appliedAt?: string | null;
};

type RenameOutcome = {
  directory: string;
  manifestPath?: string | null;
  entries: RenameEntry[];
  dryRun: boolean;
  warnings: string[];
  splitApplied?: boolean;
  splitWorkspace?: string | null;
  splitReportPath?: string | null;
  splitSummary?: RenameSplitSummary | null;
  sourceDirectory?: string | null;
  splitManualOverrides?: boolean;
  manualEntries?: ManifestManualEntry[] | null;
};

type RenameSplitSummary = {
  analyzedFiles: number;
  emittedFiles: number;
  skippedFiles: number;
  splitPages: number;
  coverTrims: number;
  fallbackSplits: number;
};

type SplitMode = 'skip' | 'cover-trim' | 'split' | 'fallback-center';

type SplitBoundingBox = {
  x: number;
  y: number;
  width: number;
  height: number;
};

type SplitMetadata = {
  foreground_ratio?: number;
  bbox?: SplitBoundingBox;
  projection_imbalance?: number;
  projection_edge_margin?: number;
  projection_total_mass?: number;
  splitMode?: SplitMode;
  split_x?: number;
  confidence?: number;
  content_width_ratio?: number;
  bbox_height_ratio?: number;
  reason?: string;
  split_clamped?: boolean;
  splitStrategy?: string;
};

type SplitItemReport = {
  source: string;
  mode: SplitMode;
  splitX?: number | null;
  confidence: number;
  contentWidthRatio: number;
  outputs: string[];
  metadata: SplitMetadata;
};

type SplitPrimaryMode = 'edgeTexture' | 'projection';

type SplitModeSelectorInput =
  | 'edgeTextureOnly'
  | 'projectionOnly'
  | {
      hybrid: {
        primary: SplitPrimaryMode;
        fallback: SplitPrimaryMode;
      };
    };

type SplitThresholdOverrides = {
  mode?: SplitModeSelectorInput;
  max_center_offset_ratio?: number;
  edgeTexture?: any;
};

type SplitAlgorithmOption = 'edgeTexture' | 'projection' | 'manual';

type SplitCommandOutcome = {
  analyzedFiles: number;
  emittedFiles: number;
  skippedFiles: number;
  splitPages: number;
  coverTrims: number;
  fallbackSplits: number;
  workspaceDirectory?: string | null;
  reportPath?: string | null;
  items: SplitItemReport[];
  warnings: string[];
};

type EdgeMarginRegion = {
  startX: number;
  endX: number;
  meanScore: number;
  confidence: number;
};

type EdgePreviewMetrics = {
  width: number;
  height: number;
  meanIntensityMin: number;
  meanIntensityMax: number;
  meanIntensityAvg: number;
};

type EdgePreviewMode = 'split' | 'coverTrim' | 'skip';

type EdgePreviewOutputRole = 'left' | 'right' | 'coverTrim';

type EdgePreviewOutputResponse = {
  path: string;
  role: EdgePreviewOutputRole;
};

type EdgePreviewOutput = EdgePreviewOutputResponse & {
  url: string;
};

type EdgePreviewAccelerator = 'cpu' | 'gpu';
type EdgePreviewAcceleratorPreference = 'auto' | EdgePreviewAccelerator;

type EdgePreviewResponsePayload = {
  originalImage: string;
  trimmedImage?: string | null;
  outputs: EdgePreviewOutputResponse[];
  mode: EdgePreviewMode;
  leftMargin?: EdgeMarginRegion | null;
  rightMargin?: EdgeMarginRegion | null;
  brightnessThresholds: [number, number];
  brightnessWeight: number;
  confidenceThreshold: number;
  metrics: EdgePreviewMetrics;
  searchRatios: [number, number];
  accelerator: EdgePreviewAccelerator;
};

type EdgePreviewPayload = {
  originalImage: string;
  originalUrl: string;
  trimmedImage?: string | null;
  trimmedUrl: string | null;
  outputs: EdgePreviewOutput[];
  mode: EdgePreviewMode;
  leftMargin?: EdgeMarginRegion | null;
  rightMargin?: EdgeMarginRegion | null;
  brightnessThresholds: [number, number];
  brightnessWeight: number;
  confidenceThreshold: number;
  metrics: EdgePreviewMetrics;
  searchRatios: [number, number];
  accelerator: EdgePreviewAccelerator;
  requestedAccelerator: EdgePreviewAcceleratorPreference;
  acceleratorMatched: boolean;
  requestedBrightnessThresholds: [number, number];
  thresholdsMatched: boolean;
  requestedSearchRatios: [number, number];
  searchRatiosMatched: boolean;
};

type ManualReadinessResult = { ok: true } | { ok: false; reason: string };

const formatThresholdValue = (value: number): string => {
  if (!Number.isFinite(value)) {
    return '—';
  }
  return String(Number(value.toFixed(2)));
};

const formatRatioValue = (value: number): string => {
  if (!Number.isFinite(value)) {
    return '—';
  }
  return value.toFixed(3);
};

const describeAccelerator = (
  value: EdgePreviewAcceleratorPreference
): string => {
  switch (value) {
    case 'gpu':
      return 'GPU';
    case 'cpu':
      return 'CPU';
    default:
      return '自动';
  }
};

const EDGE_PREVIEW_OUTPUT_LABELS: Record<EdgePreviewOutputRole, string> = {
  left: '左页',
  right: '右页',
  coverTrim: '裁剪结果',
};

type EdgePreviewState = {
  visible: boolean;
  loading: boolean;
  data: EdgePreviewPayload | null;
  error: string | null;
};

type EdgePreviewCandidateResponse = {
  path: string;
  fileName: string;
  relativePath: string;
  fileSize?: number | null;
};

type EdgePreviewImageEntry = {
  path: string;
  fileName: string;
  relativePath: string;
  fileSize: number | null;
  url: string;
};

type EdgePreviewSelectorState = {
  visible: boolean;
  loading: boolean;
  directory: string | null;
  images: EdgePreviewImageEntry[];
  error: string | null;
};

type EdgeThresholdErrors = {
  bright?: string;
  dark?: string;
};

type EdgeSearchRatioErrors = {
  left?: string;
  right?: string;
};

type RenameSplitPayload = {
  enabled: boolean;
  workspace?: string | null;
  reportPath?: string | null;
  summary?: RenameSplitSummary | null;
  warnings?: string[];
};

type UploadMode = 'zip' | 'folder';

type UploadOutcome = {
  remoteUrl: string;
  uploadedBytes: number;
  fileCount: number;
  mode: UploadMode;
};

type UploadProgressStage =
  | 'preparing'
  | 'uploading'
  | 'finalizing'
  | 'completed'
  | 'failed';

type UploadProgressPayload = {
  stage: UploadProgressStage;
  transferredBytes: number;
  totalBytes: number;
  processedFiles: number;
  totalFiles: number;
  message?: string | null;
};

type RenameFormState = {
  directory: string;
  pad: number;
  targetExtension: string;
};

type UploadFormState = {
  serviceUrl: string;
  bearerToken: string;
  title: string;
  volume: string;
  mode: UploadMode;
};

type ServiceAddressBook = {
  upload: string[];
  job: string[];
};

const REMOTE_ROOT = 'incoming';
const STALE_PROGRESS_THRESHOLD_MS = 15_000;
const STALE_PROGRESS_CHECK_INTERVAL_MS = 5_000;

const normalizeSegment = (input: string): string => {
  if (!input.trim()) {
    return '';
  }

  const normalized = input
    .normalize('NFKD')
    .replace(/[^a-zA-Z0-9\u4e00-\u9fff]+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-+|-+$/g, '')
    .toLowerCase();

  return normalized.slice(0, 64);
};

const formatSeedSegment = (seed: number): string => {
  const date = new Date(seed);
  const pad = (value: number) => value.toString().padStart(2, '0');
  const year = date.getFullYear();
  const month = pad(date.getMonth() + 1);
  const day = pad(date.getDate());
  const hours = pad(date.getHours());
  const minutes = pad(date.getMinutes());
  const seconds = pad(date.getSeconds());

  return `${year}${month}${day}-${hours}${minutes}${seconds}`;
};

const buildRemotePath = (options: {
  title?: string;
  volume?: string;
  seed: number;
  mode: UploadMode;
}): string => {
  const { title = '', volume = '', seed, mode } = options;
  const titleSegment = normalizeSegment(title);
  const volumeSegment = normalizeSegment(volume);
  const segments = [titleSegment, volumeSegment].filter(Boolean);
  const slug = segments.length > 0 ? segments.join('-') : 'manga';
  const suffix = formatSeedSegment(seed);
  const stem = `${slug}-${suffix}`.slice(0, 96);
  const extension = mode === 'zip' ? '.zip' : '';

  return `${REMOTE_ROOT}/${stem}${extension}`;
};

const mergeServiceAddresses = (
  ...sources: (string | string[] | null | undefined)[]
): string[] => {
  const seen = new Set<string>();
  const result: string[] = [];

  for (const source of sources) {
    if (!source) {
      continue;
    }

    if (Array.isArray(source)) {
      for (const entry of source) {
        const trimmed = entry.trim();
        if (!trimmed || seen.has(trimmed)) {
          continue;
        }
        seen.add(trimmed);
        result.push(trimmed);
      }
      continue;
    }

    if (typeof source === 'string') {
      const trimmed = source.trim();
      if (!trimmed || seen.has(trimmed)) {
        continue;
      }
      seen.add(trimmed);
      result.push(trimmed);
    }
  }

  return result;
};

type JobEventTransport = 'websocket' | 'polling' | 'system';

type JobParamsConfig = {
  model: string;
  scale: number;
  denoise: 'low' | 'medium' | 'high';
  outputFormat: 'jpg' | 'png' | 'webp';
  jpegQuality: number;
  tileSize: number | null;
  tilePad: number | null;
  batchSize: number | null;
  device: 'auto' | 'cuda' | 'cpu';
};

type JobMetadataInfo = {
  title?: string | null;
  volume?: string | null;
};

type JobParamFavorite = JobParamsConfig & {
  id: string;
  name: string;
  createdAt: number;
};

type ArtifactValidationItem = {
  filename: string;
  expectedHash?: string | null;
  actualHash?: string | null;
  expectedBytes?: number | null;
  actualBytes?: number | null;
  status: 'matched' | 'missing' | 'extra' | 'mismatch';
};

type ArtifactReport = {
  jobId: string;
  artifactPath: string;
  extractPath: string;
  manifestPath?: string | null;
  hash: string;
  createdAt: string;
  summary: {
    matched: number;
    missing: number;
    extra: number;
    mismatched: number;
    totalManifest: number;
    totalExtracted: number;
  };
  items: ArtifactValidationItem[];
  warnings: string[];
  reportPath?: string | null;
  archivePath?: string | null;
};

type ArtifactDownloadSummary = {
  jobId: string;
  archivePath: string;
  extractPath: string;
  hash: string;
  fileCount: number;
  warnings: string[];
};

type JobEventPayload = {
  jobId: string;
  status: string;
  processed: number;
  total: number;
  artifactPath?: string | null;
  message?: string | null;
  transport?: JobEventTransport;
  error?: string | null;
  retries?: number;
  lastError?: string | null;
  artifactHash?: string | null;
  params?: JobParamsConfig | null;
  metadata?: JobMetadataInfo | null;
};

type JobStatusSnapshotPayload = {
  jobId: string;
  status: string;
  processed: number;
  total: number;
  artifactPath?: string | null;
  message?: string | null;
  retries?: number;
  lastError?: string | null;
  artifactHash?: string | null;
  params?: JobParamsConfig | null;
  metadata?: JobMetadataInfo | null;
};

type JobSubmission = {
  jobId: string;
};

type JobRecord = JobEventPayload & {
  lastUpdated: number;
  serviceUrl: string;
  bearerToken?: string | null;
  inputPath?: string | null;
  inputType?: JobFormState['inputType'];
  manifestPath?: string | null;
  transport: JobEventTransport;
};

type JobFormState = {
  serviceUrl: string;
  bearerToken: string;
  title: string;
  volume: string;
  inputType: 'zip' | 'folder';
  inputPath: string;
  pollIntervalMs: number;
};

type MangaSourceMode = 'singleVolume' | 'multiVolume';

type VolumeCandidate = {
  directory: string;
  folderName: string;
  imageCount: number;
  detectedNumber?: number | null;
};

type SplitDetectionSummary = {
  total: number;
  candidates: number;
};

type SplitProgressStage = 'initializing' | 'processing' | 'completed';

type SplitProgressPayload = {
  totalFiles: number;
  processedFiles: number;
  currentFile?: string | null;
  stage: SplitProgressStage;
};

type MangaSourceAnalysis = {
  root: string;
  mode: MangaSourceMode;
  rootImageCount: number;
  totalImages: number;
  volumeCandidates: VolumeCandidate[];
  skippedEntries: string[];
  splitDetection?: SplitDetectionSummary | null;
};

type VolumeMapping = {
  directory: string;
  folderName: string;
  imageCount: number;
  detectedNumber: number | null;
  volumeNumber: number | null;
  volumeName: string;
};

type VolumeRenameOutcome = {
  mapping: VolumeMapping;
  outcome: RenameOutcome;
};

type RenameSummary =
  | {
      mode: 'single';
      outcome: RenameOutcome;
    }
  | {
      mode: 'multi';
      volumes: VolumeRenameOutcome[];
      dryRun: boolean;
    };

type StepId = 'source' | 'volumes' | 'rename' | 'split' | 'upload' | 'jobs';

type StepDescriptor = {
  id: StepId;
  label: string;
};

const DEFAULT_PAD = 4;
const DEFAULT_POLL_INTERVAL = 1000;
const SETTINGS_KEY = 'manga-upscale-agent:v1';
const UPLOAD_DEFAULTS_KEY = `${SETTINGS_KEY}:upload-defaults`;
const SERVICE_ADDRESS_BOOK_KEY = `${SETTINGS_KEY}:service-addresses`;
const DEFAULT_EDGE_SEARCH_RATIO = 0.18;
const EDGE_SEARCH_RATIO_MIN = 0.02;
const EDGE_SEARCH_RATIO_MAX = 0.5;
const EDGE_PREVIEW_SELECTOR_ITEM_HEIGHT = 104;
const EDGE_PREVIEW_SELECTOR_OVERSCAN = 6;

const LEGACY_SETTINGS_KEY = 'manga-upscale-agent';
const LEGACY_UPLOAD_DEFAULTS_KEY = `${LEGACY_SETTINGS_KEY}:upload-defaults`;
const LEGACY_SERVICE_ADDRESS_BOOK_KEY = `${LEGACY_SETTINGS_KEY}:service-addresses`;
const PARAM_DEFAULTS_KEY = `${SETTINGS_KEY}:job-params`;
const PARAM_FAVORITES_KEY = `${SETTINGS_KEY}:job-param-favorites`;

const DEFAULT_JOB_PARAMS: JobParamsConfig = {
  model: 'RealESRGAN_x4plus_anime_6B',
  scale: 2,
  denoise: 'medium',
  outputFormat: 'jpg',
  jpegQuality: 95,
  tileSize: null,
  tilePad: null,
  batchSize: null,
  device: 'auto',
};

const createInitialRenameForm = (): RenameFormState => ({
  directory: '',
  pad: DEFAULT_PAD,
  targetExtension: 'jpg',
});

const createInitialUploadForm = (): UploadFormState => ({
  serviceUrl: '',
  bearerToken: '',
  title: '',
  volume: '',
  mode: 'zip',
});

const createInitialJobForm = (): JobFormState => ({
  serviceUrl: '',
  bearerToken: '',
  title: '',
  volume: '',
  inputType: 'zip',
  inputPath: '',
  pollIntervalMs: DEFAULT_POLL_INTERVAL,
});

const MangaUpscaleAgent = () => {
  const [renameForm, setRenameForm] = useState<RenameFormState>(() =>
    createInitialRenameForm()
  );
  const [renameSummary, setRenameSummary] = useState<RenameSummary | null>(
    null
  );
  const [renameLoading, setRenameLoading] = useState(false);
  const [renameError, setRenameError] = useState<string | null>(null);

  const [analysisLoading, setAnalysisLoading] = useState(false);
  const [analysisError, setAnalysisError] = useState<string | null>(null);
  const [sourceAnalysis, setSourceAnalysis] =
    useState<MangaSourceAnalysis | null>(null);
  const [volumeMappings, setVolumeMappings] = useState<VolumeMapping[]>([]);
  const [volumeMappingError, setVolumeMappingError] = useState<string | null>(
    null
  );
  const [mappingConfirmed, setMappingConfirmed] = useState(false);
  const [selectedVolumeKey, setSelectedVolumeKey] = useState<string | null>(
    null
  );
  const [hasRestoredDefaults, setHasRestoredDefaults] = useState(false);

  const isMultiVolumeSource = sourceAnalysis?.mode === 'multiVolume';

  const [uploadForm, setUploadForm] = useState<UploadFormState>(() =>
    createInitialUploadForm()
  );
  const [uploadLoading, setUploadLoading] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [uploadStatus, setUploadStatus] = useState<string | null>(null);
  const [uploadProgress, setUploadProgress] =
    useState<UploadProgressPayload | null>(null);
  const [remotePathSeed, setRemotePathSeed] = useState(() => Date.now());
  const [lastUploadRemotePath, setLastUploadRemotePath] = useState<string>('');
  const [uploadServiceOptions, setUploadServiceOptions] = useState<string[]>(
    []
  );
  const [isAddingUploadService, setIsAddingUploadService] = useState(false);
  const [uploadAddressDraft, setUploadAddressDraft] = useState('');
  const [uploadAddressError, setUploadAddressError] = useState<string | null>(
    null
  );

  const [jobServiceOptions, setJobServiceOptions] = useState<string[]>([]);
  const [isAddingJobService, setIsAddingJobService] = useState(false);
  const [jobAddressDraft, setJobAddressDraft] = useState('');
  const [jobAddressError, setJobAddressError] = useState<string | null>(null);

  const [jobForm, setJobForm] = useState<JobFormState>(() =>
    createInitialJobForm()
  );
  const [jobParams, setJobParams] =
    useState<JobParamsConfig>(DEFAULT_JOB_PARAMS);
  const [contentSplitEnabled, setContentSplitEnabled] = useState(false);
  const [splitAlgorithm, setSplitAlgorithm] =
    useState<SplitAlgorithmOption>('edgeTexture');
  const [edgeBrightnessThresholds, setEdgeBrightnessThresholds] =
    useState<[number, number]>([200, 75]);
  const [edgeSearchRatios, setEdgeSearchRatios] = useState<[number, number]>([
    DEFAULT_EDGE_SEARCH_RATIO,
    DEFAULT_EDGE_SEARCH_RATIO,
  ]);
  const [edgeAcceleratorPreference, setEdgeAcceleratorPreference] =
    useState<EdgePreviewAcceleratorPreference>('auto');
  const [edgeThresholdErrors, setEdgeThresholdErrors] =
    useState<EdgeThresholdErrors>({});
  const [edgeSearchRatioErrors, setEdgeSearchRatioErrors] =
    useState<EdgeSearchRatioErrors>({});
  const [edgePreview, setEdgePreview] = useState<EdgePreviewState>({
    visible: false,
    loading: false,
    data: null,
    error: null,
  });
  const [edgePreviewSelector, setEdgePreviewSelector] =
    useState<EdgePreviewSelectorState>({
      visible: false,
      loading: false,
      directory: null,
      images: [],
      error: null,
    });
  const edgePreviewCacheRef = useRef<Map<string, EdgePreviewImageEntry[]>>(
    new Map()
  );
  const selectorListRef = useRef<HTMLDivElement | null>(null);
  const [selectorScrollTop, setSelectorScrollTop] = useState(0);
  const [selectorViewportHeight, setSelectorViewportHeight] = useState(400);
  const appliedBrightnessThresholds =
    edgePreview.data?.brightnessThresholds ?? edgeBrightnessThresholds;
  const requestedBrightnessThresholds =
    edgePreview.data?.requestedBrightnessThresholds ?? edgeBrightnessThresholds;
  const previewThresholdsMatched =
    edgePreview.data?.thresholdsMatched ?? true;
  const appliedSearchRatios = edgePreview.data?.searchRatios ?? edgeSearchRatios;
  const requestedSearchRatios =
    edgePreview.data?.requestedSearchRatios ?? edgeSearchRatios;
  const previewSearchRatiosMatched =
    edgePreview.data?.searchRatiosMatched ?? true;
  const requestedAccelerator =
    edgePreview.data?.requestedAccelerator ?? edgeAcceleratorPreference;
  const previewAcceleratorMatched =
    edgePreview.data?.acceleratorMatched ?? true;
  const selectorViewportCount = Math.max(
    1,
    Math.ceil(
      selectorViewportHeight / EDGE_PREVIEW_SELECTOR_ITEM_HEIGHT
    )
  );
  const selectorStartIndex = Math.max(
    0,
    Math.floor(selectorScrollTop / EDGE_PREVIEW_SELECTOR_ITEM_HEIGHT) -
      EDGE_PREVIEW_SELECTOR_OVERSCAN
  );
  const selectorEndIndex = Math.min(
    edgePreviewSelector.images.length,
    selectorStartIndex + selectorViewportCount + EDGE_PREVIEW_SELECTOR_OVERSCAN * 2
  );
  const selectorVisibleItems = edgePreviewSelector.images.slice(
    selectorStartIndex,
    selectorEndIndex
  );
  const selectorTotalHeight =
    edgePreviewSelector.images.length * EDGE_PREVIEW_SELECTOR_ITEM_HEIGHT;
  const computeEdgeThresholdErrors = useCallback(
    (bright: number, dark: number): EdgeThresholdErrors => {
      const next: EdgeThresholdErrors = {};
      if (!Number.isFinite(bright) || bright < 0 || bright > 255) {
        next.bright = '亮白阈值需在 0-255 范围内';
      }
      if (!Number.isFinite(dark) || dark < 0 || dark > 255) {
        next.dark = '留黑阈值需在 0-255 范围内';
      }
      if (!next.bright && !next.dark && dark > bright) {
        next.dark = '留黑阈值不得高于亮白阈值';
      }
      return next;
    },
    []
  );
  const computeEdgeSearchRatioErrors = useCallback(
    (left: number, right: number): EdgeSearchRatioErrors => {
      const next: EdgeSearchRatioErrors = {};
      if (!Number.isFinite(left) || left < EDGE_SEARCH_RATIO_MIN || left > EDGE_SEARCH_RATIO_MAX) {
        next.left = `左侧搜索比例需在 ${EDGE_SEARCH_RATIO_MIN} ~ ${EDGE_SEARCH_RATIO_MAX} 之间`;
      }
      if (
        !Number.isFinite(right) ||
        right < EDGE_SEARCH_RATIO_MIN ||
        right > EDGE_SEARCH_RATIO_MAX
      ) {
        next.right = `右侧搜索比例需在 ${EDGE_SEARCH_RATIO_MIN} ~ ${EDGE_SEARCH_RATIO_MAX} 之间`;
      }
      return next;
    },
    []
  );

  const hasEdgeThresholdError = useMemo(
    () => Boolean(edgeThresholdErrors.bright || edgeThresholdErrors.dark),
    [edgeThresholdErrors]
  );
  const hasEdgeSearchRatioError = useMemo(
    () => Boolean(edgeSearchRatioErrors.left || edgeSearchRatioErrors.right),
    [edgeSearchRatioErrors]
  );
  const hasEdgeInputError = useMemo(
    () => hasEdgeThresholdError || hasEdgeSearchRatioError,
    [hasEdgeSearchRatioError, hasEdgeThresholdError]
  );

  useEffect(() => {
    setEdgeThresholdErrors((prev) => {
      const [bright, dark] = edgeBrightnessThresholds;
      const next = computeEdgeThresholdErrors(bright, dark);
      if (prev.bright === next.bright && prev.dark === next.dark) {
        return prev;
      }
      return next;
    });
  }, [computeEdgeThresholdErrors, edgeBrightnessThresholds]);

  useEffect(() => {
    setEdgeSearchRatioErrors((prev) => {
      const [left, right] = edgeSearchRatios;
      const next = computeEdgeSearchRatioErrors(left, right);
      if (prev.left === next.left && prev.right === next.right) {
        return prev;
      }
      return next;
    });
  }, [computeEdgeSearchRatioErrors, edgeSearchRatios]);

  const handleEdgeThresholdChange = useCallback(
    (index: 0 | 1) => (event: ChangeEvent<HTMLInputElement>) => {
      const raw = event.currentTarget.value;
      const nextValue = Number(raw);
      setEdgeBrightnessThresholds((prev) => {
        const updated: [number, number] = [...prev];
        if (Number.isFinite(nextValue)) {
          updated[index] = nextValue;
          return updated;
        }
        return prev;
      });
    },
    []
  );
  const handleEdgeSearchRatioChange = useCallback(
    (index: 0 | 1) => (event: ChangeEvent<HTMLInputElement>) => {
      const raw = event.currentTarget.value;
      const nextValue = Number(raw);
      setEdgeSearchRatios((prev) => {
        const updated: [number, number] = [...prev];
        if (Number.isFinite(nextValue)) {
          updated[index] = nextValue;
          return updated;
        }
        return prev;
      });
    },
    []
  );
  const handleEdgeAcceleratorChange = useCallback(
    (event: ChangeEvent<HTMLSelectElement>) => {
      const value =
        event.currentTarget.value as EdgePreviewAcceleratorPreference;
      setEdgeAcceleratorPreference(value);
    },
    []
  );
  const [splitWorkspace, setSplitWorkspace] = useState<string | null>(null);
  const [splitSummaryState, setSplitSummaryState] =
    useState<RenameSplitSummary | null>(null);
  const [splitWarningsState, setSplitWarningsState] = useState<string[]>([]);
  const [splitReportPath, setSplitReportPath] = useState<string | null>(null);
  const [splitSourceRoot, setSplitSourceRoot] = useState<string | null>(null);
  const [splitPreparing, setSplitPreparing] = useState(false);
  const [splitError, setSplitError] = useState<string | null>(null);
  const [splitEstimate, setSplitEstimate] =
    useState<SplitDetectionSummary | null>(null);
  const [splitProgress, setSplitProgress] =
    useState<SplitProgressPayload | null>(null);
  const [manualDrawerOpen, setManualDrawerOpen] = useState(false);

  const [currentStep, setCurrentStep] = useState<StepId>('source');

  const splitProgressPercent = useMemo(() => {
    if (!splitProgress) {
      return 0;
    }
    if (splitProgress.totalFiles <= 0) {
      return splitProgress.stage === 'completed' ? 100 : 0;
    }
    const percent =
      (splitProgress.processedFiles / Math.max(splitProgress.totalFiles, 1)) *
      100;
    return Math.min(100, Math.max(0, Math.round(percent)));
  }, [splitProgress]);

  const splitProgressStatus = useMemo(() => {
    if (!splitProgress) {
      return splitPreparing ? '正在初始化拆分…' : null;
    }

    if (splitProgress.stage === 'completed') {
      return '拆分完成';
    }

    if (splitProgress.stage === 'processing') {
      if (splitProgress.totalFiles > 0) {
        return `处理中 ${splitProgress.processedFiles}/${splitProgress.totalFiles}`;
      }
      return '处理中…';
    }

    return '正在初始化拆分…';
  }, [splitPreparing, splitProgress]);

  const splitProgressFileName = useMemo(() => {
    if (!splitProgress?.currentFile) {
      return null;
    }
    const fragments = splitProgress.currentFile.split(/[\\/]/);
    const last = fragments[fragments.length - 1];
    return last && last.trim().length > 0 ? last : splitProgress.currentFile;
  }, [splitProgress]);

  const resetSplitState = useCallback(() => {
    setSplitWorkspace(null);
    setSplitSummaryState(null);
    setSplitWarningsState([]);
    setSplitReportPath(null);
    setSplitSourceRoot(null);
    setSplitError(null);
    setSplitProgress(null);
    setEdgePreview({ visible: false, loading: false, data: null, error: null });
    setManualDrawerOpen(false);
  }, []);

  const handleCloseEdgePreview = useCallback(() => {
    setEdgePreview({ visible: false, loading: false, data: null, error: null });
  }, []);

  const handleSplitAlgorithmChange = useCallback(
    (event: ChangeEvent<HTMLSelectElement>) => {
      const value = event.currentTarget.value as SplitAlgorithmOption;
      if (value === splitAlgorithm) {
        return;
      }
      setSplitAlgorithm(value);
      resetSplitState();
      setSplitPreparing(false);
      setSplitEstimate(null);
      if (value !== 'edgeTexture') {
        handleCloseEdgePreview();
      }
    },
    [handleCloseEdgePreview, resetSplitState, splitAlgorithm]
  );

  const resolveRenameRoot = useCallback(() => {
    if (sourceAnalysis?.root && sourceAnalysis.root.length > 0) {
      return sourceAnalysis.root;
    }
    return renameForm.directory;
  }, [renameForm.directory, sourceAnalysis]);

  const manualSourceDirectory = useMemo(() => {
    if (renameSummary?.mode === 'single') {
      return renameSummary.outcome.directory ?? null;
    }
    const fallback = resolveRenameRoot().trim();
    return fallback.length > 0 ? fallback : null;
  }, [renameSummary, resolveRenameRoot]);

  const manualController = useManualSplitController({
    sourceDirectory: manualSourceDirectory,
    multiVolume: isMultiVolumeSource,
    onOpenDrawer: () => setManualDrawerOpen(true),
  });

  const {
    workspace: manualWorkspace,
    initializing: manualInitializing,
    loadingDrafts: manualLoadingDrafts,
    statusText: manualStatusText,
    error: manualControllerError,
    disableInitialize: manualDisableInitialize,
    disableReason: manualDisableReason,
    totalDrafts: manualDraftTotal,
    appliedDrafts: manualAppliedCount,
    lastAppliedAt: manualLastAppliedAt,
    pendingDrafts: manualPendingDrafts,
    manualReportPath,
    manualReportSummary,
    initialize: initializeManualWorkspace,
    openExisting: openManualWorkspace,
  } = manualController;

  useEffect(() => {
    if (!manualWorkspace && manualDrawerOpen) {
      setManualDrawerOpen(false);
    }
  }, [manualDrawerOpen, manualWorkspace]);

  const assessManualReadiness = useCallback((): ManualReadinessResult => {
    const requiresManual =
      renameSummary?.mode === 'single' &&
      Boolean(renameSummary.outcome.splitManualOverrides);

    if (!requiresManual) {
      return { ok: true };
    }

    if (!manualWorkspace) {
      return {
        ok: false,
        reason:
          '检测到当前重命名结果依赖手动拆分，但尚未加载手动拆分工作区。请返回“拆分与裁剪”步骤并点击“打开手动拆分”重新载入后再继续。',
      };
    }

    const expectedWorkspace =
      renameSummary?.mode === 'single'
        ? renameSummary.outcome.splitWorkspace ?? renameSummary.outcome.directory ?? null
        : null;

    if (
      expectedWorkspace &&
      expectedWorkspace.trim().length > 0 &&
      manualWorkspace.trim().length > 0 &&
      manualWorkspace !== expectedWorkspace
    ) {
      return {
        ok: false,
        reason: `手动拆分工作区已变更（当前：${manualWorkspace}，预期：${expectedWorkspace}），请在步骤 2 重新加载正确的工作区后再继续。`,
      };
    }

    if (manualDraftTotal === 0) {
      return {
        ok: false,
        reason: '手动拆分工作区尚未准备就绪，请先在步骤 2 初始化并载入图片草稿后再继续。',
      };
    }

    if (manualAppliedCount === 0) {
      return {
        ok: false,
        reason:
          '尚未对任何图片执行“应用”操作，请在手动拆分抽屉中完成应用以生成输出。',
      };
    }

    if (manualAppliedCount < manualDraftTotal || manualPendingDrafts > 0) {
      return {
        ok: false,
        reason: `仍有 ${manualPendingDrafts} 张图片未应用最新拆分，请在手动拆分抽屉中执行“应用”操作后再继续。`,
      };
    }

    if (!manualReportPath) {
      return {
        ok: false,
        reason:
          '未找到 manual_split_report.json，请在手动拆分抽屉中重新执行一次“应用”以生成报告后再继续恢复或下载。',
      };
    }

    if (
      manualReportSummary &&
      manualReportSummary.applied < manualReportSummary.total
    ) {
      const remaining = manualReportSummary.total - manualReportSummary.applied;
      return {
        ok: false,
        reason: `手动拆分报告显示仍有 ${remaining} 张图片未完成应用，请在抽屉中重新应用后再继续。`,
      };
    }

    return { ok: true };
  }, [
    manualAppliedCount,
    manualPendingDrafts,
    manualReportPath,
    manualReportSummary,
    manualWorkspace,
    manualDraftTotal,
    renameSummary,
  ]);

  const manualReadiness = useMemo(() => assessManualReadiness(), [assessManualReadiness]);
  const manualReadinessWarning = manualReadiness.ok ? null : manualReadiness.reason;

  const manualCrossModeNotice = useMemo(
    () =>
      buildManualCrossModeNotice({
        autoSummary: renameSummary?.outcome.splitSummary ?? null,
        manualSummary: manualReportSummary,
        manualReportPath,
      }),
    [manualReportPath, manualReportSummary, renameSummary]
  );

  const handleEdgePreview = useCallback(async () => {
    const [bright, dark] = edgeBrightnessThresholds;
    const nextErrors = computeEdgeThresholdErrors(bright, dark);
    setEdgeThresholdErrors(nextErrors);
    if (nextErrors.bright || nextErrors.dark) {
      return;
    }

    const [leftRatio, rightRatio] = edgeSearchRatios;
    const nextRatioErrors = computeEdgeSearchRatioErrors(leftRatio, rightRatio);
    setEdgeSearchRatioErrors(nextRatioErrors);
    if (nextRatioErrors.left || nextRatioErrors.right) {
      return;
    }

    const root = resolveRenameRoot().trim();
    if (!root) {
      setEdgePreview({
        visible: true,
        loading: false,
        data: null,
        error: '请先选择有效的漫画目录。',
      });
      return;
    }

    setSelectorScrollTop(0);
    const cached = edgePreviewCacheRef.current.get(root) ?? null;

    if (cached) {
      setEdgePreviewSelector({
        visible: true,
        loading: false,
        directory: root,
        images: cached,
        error: cached.length === 0 ? '所选目录中没有可预览的图片。' : null,
      });
      if (typeof window !== 'undefined') {
        window.requestAnimationFrame(() => {
          if (selectorListRef.current) {
            setSelectorViewportHeight(selectorListRef.current.clientHeight);
          }
        });
      }
      return;
    }

    setEdgePreviewSelector({
      visible: true,
      loading: true,
      directory: root,
      images: [],
      error: null,
    });

    if (typeof window !== 'undefined') {
      window.requestAnimationFrame(() => {
        if (selectorListRef.current) {
          setSelectorViewportHeight(selectorListRef.current.clientHeight);
        }
      });
    }

    try {
      const response = await invoke<EdgePreviewCandidateResponse[]>(
        'list_edge_preview_candidates',
        {
          directory: root,
        }
      );

      const mapped = response.map((item) => ({
        path: item.path,
        fileName: item.fileName,
        relativePath: item.relativePath,
        fileSize: item.fileSize ?? null,
        url: convertFileSrc(item.path),
      }));

      edgePreviewCacheRef.current.set(root, mapped);

      setEdgePreviewSelector({
        visible: true,
        loading: false,
        directory: root,
        images: mapped,
        error: mapped.length === 0 ? '所选目录中没有可预览的图片。' : null,
      });
    } catch (error) {
      setEdgePreviewSelector({
        visible: true,
        loading: false,
        directory: root,
        images: [],
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }, [
    computeEdgeThresholdErrors,
    edgeBrightnessThresholds,
    computeEdgeSearchRatioErrors,
    edgeSearchRatios,
    resolveRenameRoot,
  ]);

  const executeEdgePreview = useCallback(
    async (imagePath: string) => {
      const [bright, dark] = edgeBrightnessThresholds;
      const nextErrors = computeEdgeThresholdErrors(bright, dark);
      setEdgeThresholdErrors(nextErrors);
      if (nextErrors.bright || nextErrors.dark) {
        return;
      }

      const [leftRatio, rightRatio] = edgeSearchRatios;
      const nextRatioErrors = computeEdgeSearchRatioErrors(leftRatio, rightRatio);
      setEdgeSearchRatioErrors(nextRatioErrors);
      if (nextRatioErrors.left || nextRatioErrors.right) {
        return;
      }

      setEdgePreviewSelector((prev) => ({
        ...prev,
        visible: false,
      }));

      setEdgePreview((prev) => ({
        visible: true,
        loading: true,
        data: prev.data,
        error: null,
      }));

      try {
        const response = await invoke<EdgePreviewResponsePayload>(
          'preview_edge_texture_trim',
          {
            request: {
              imagePath,
              brightnessThresholds: [bright, dark],
              brightnessWeight: 0.5,
              whiteThreshold: 1.0,
              leftSearchRatio: leftRatio,
              rightSearchRatio: rightRatio,
              accelerator: edgeAcceleratorPreference,
            },
          }
        );

        const requestedThresholds: [number, number] = [bright, dark];
        const appliedThresholds = response.brightnessThresholds;
        const thresholdsMatched = appliedThresholds.every((value, index) => {
          const expected = requestedThresholds[index];
          return Math.abs(value - expected) <= 1e-3;
        });
        const requestedRatios: [number, number] = [leftRatio, rightRatio];
        const appliedRatios = response.searchRatios;
        const ratiosMatched = appliedRatios.every((value, index) => {
          const expected = requestedRatios[index];
          return Math.abs(value - expected) <= 1e-4;
        });
        const requestedAcceleratorPref = edgeAcceleratorPreference;
        const acceleratorMatched =
          requestedAcceleratorPref === 'auto' ||
          response.accelerator === requestedAcceleratorPref;

        const trimmedUrl = response.trimmedImage
          ? convertFileSrc(response.trimmedImage)
          : null;
        const outputs: EdgePreviewOutput[] = response.outputs.map((item) => ({
          ...item,
          url: convertFileSrc(item.path),
        }));

        const payload: EdgePreviewPayload = {
          originalImage: response.originalImage,
          originalUrl: convertFileSrc(response.originalImage),
          trimmedImage: response.trimmedImage ?? null,
          trimmedUrl,
          outputs,
          mode: response.mode,
          leftMargin: response.leftMargin ?? null,
          rightMargin: response.rightMargin ?? null,
          brightnessThresholds: response.brightnessThresholds,
          brightnessWeight: response.brightnessWeight,
          confidenceThreshold: response.confidenceThreshold,
          metrics: response.metrics,
          searchRatios: response.searchRatios,
          accelerator: response.accelerator,
          requestedAccelerator: requestedAcceleratorPref,
          acceleratorMatched,
          requestedBrightnessThresholds: requestedThresholds,
          thresholdsMatched,
          requestedSearchRatios: requestedRatios,
          searchRatiosMatched: ratiosMatched,
        };

        setEdgePreview({
          visible: true,
          loading: false,
          data: payload,
          error: null,
        });
      } catch (error) {
        setEdgePreview({
          visible: true,
          loading: false,
          data: null,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    }, [
      computeEdgeThresholdErrors,
      edgeBrightnessThresholds,
      edgeSearchRatios,
      computeEdgeSearchRatioErrors,
      edgeAcceleratorPreference,
    ]
  );

  const handleCloseEdgePreviewSelector = useCallback(() => {
    setEdgePreviewSelector((prev) => ({ ...prev, visible: false }));
  }, []);

  const handleSelectorScroll = useCallback(
    (event: UIEvent<HTMLDivElement>) => {
      setSelectorScrollTop(event.currentTarget.scrollTop);
      setSelectorViewportHeight(event.currentTarget.clientHeight);
    },
    []
  );

  useEffect(() => {
    if (!edgePreviewSelector.visible) {
      return;
    }
    if (typeof window === 'undefined') {
      return;
    }
    const raf = window.requestAnimationFrame(() => {
      if (selectorListRef.current) {
        setSelectorViewportHeight(selectorListRef.current.clientHeight);
      }
    });
    return () => {
      window.cancelAnimationFrame(raf);
    };
  }, [edgePreviewSelector.visible]);

  const resetWizardState = useCallback(() => {
    setCurrentStep('source');
    setRenameForm(createInitialRenameForm());
    setRenameSummary(null);
    setRenameLoading(false);
    setRenameError(null);
    setAnalysisLoading(false);
    setAnalysisError(null);
    setSourceAnalysis(null);
    setVolumeMappings([]);
    setVolumeMappingError(null);
    setMappingConfirmed(false);
    setSelectedVolumeKey(null);
    setUploadForm((prev) => ({
      ...createInitialUploadForm(),
      serviceUrl: prev.serviceUrl,
      bearerToken: prev.bearerToken,
      mode: prev.mode,
      title: prev.title,
    }));
    setUploadLoading(false);
    setUploadError(null);
    setUploadStatus(null);
    setUploadProgress(null);
    setRemotePathSeed(Date.now());
    setLastUploadRemotePath('');
    setIsAddingUploadService(false);
    setUploadAddressDraft('');
    setUploadAddressError(null);
    setContentSplitEnabled(false);
    resetSplitState();
    setSplitPreparing(false);
    setSplitEstimate(null);
    setJobForm((prev) => ({
      ...createInitialJobForm(),
      serviceUrl: prev.serviceUrl,
      bearerToken: prev.bearerToken,
      pollIntervalMs: prev.pollIntervalMs,
      inputType: prev.inputType,
      title: prev.title,
    }));
    setJobLoading(false);
    setJobError(null);
    setJobStatus(null);
    setJobStatusFilter('all');
    setJobSearch('');
    setSelectedJobIds([]);
    setJobs([]);
    setIsAddingJobService(false);
    setJobAddressDraft('');
    setJobAddressError(null);
    setArtifactReports([]);
    setArtifactDownloads([]);
    setArtifactError(null);
    setArtifactDownloadBusyJob(null);
    setArtifactValidateBusyJob(null);
    setArtifactTargetRoot(null);
  }, [resetSplitState]);
  const [jobParamFavorites, setJobParamFavorites] = useState<
    JobParamFavorite[]
  >([]);
  const [jobParamsRestored, setJobParamsRestored] = useState(false);
  const [jobs, setJobs] = useState<JobRecord[]>([]);
  const staleJobsRef = useRef<Set<string>>(new Set());
  const [jobLoading, setJobLoading] = useState(false);
  const [jobError, setJobError] = useState<string | null>(null);
  const [jobStatus, setJobStatus] = useState<string | null>(null);
  const [jobStatusFilter, setJobStatusFilter] = useState<
    'all' | 'active' | 'completed' | 'failed'
  >('all');
  const [jobSearch, setJobSearch] = useState('');
  const [selectedJobIds, setSelectedJobIds] = useState<string[]>([]);
  const [artifactReports, setArtifactReports] = useState<ArtifactReport[]>([]);
  const [artifactDownloads, setArtifactDownloads] = useState<
    ArtifactDownloadSummary[]
  >([]);
  const [artifactError, setArtifactError] = useState<string | null>(null);
  const [artifactDownloadBusyJob, setArtifactDownloadBusyJob] = useState<
    string | null
  >(null);
  const [artifactValidateBusyJob, setArtifactValidateBusyJob] = useState<
    string | null
  >(null);
  const [artifactTargetRoot, setArtifactTargetRoot] = useState<string | null>(
    null
  );
  const sanitizeVolumeName = useCallback((folderName: string) => {
    const replaced = folderName
      .replace(/[_-]+/g, ' ')
      .replace(/\s+/g, ' ')
      .trim();
    return replaced.length > 0 ? replaced : folderName;
  }, []);

  const buildInitialMappings = useCallback(
    (analysis: MangaSourceAnalysis) => {
      if (analysis.mode !== 'multiVolume') {
        return [] as VolumeMapping[];
      }

      const usedNumbers = new Set<number>();

      return analysis.volumeCandidates.map((candidate, index) => {
        let detectedNumber =
          typeof candidate.detectedNumber === 'number'
            ? candidate.detectedNumber
            : candidate.detectedNumber ?? null;

        if (detectedNumber !== null && usedNumbers.has(detectedNumber)) {
          detectedNumber = null;
        }

        let assignedNumber = detectedNumber;
        if (assignedNumber === null) {
          let fallback = index + 1;
          while (usedNumbers.has(fallback)) {
            fallback += 1;
          }
          assignedNumber = fallback;
        }

        usedNumbers.add(assignedNumber);

        return {
          directory: candidate.directory,
          folderName: candidate.folderName,
          imageCount: candidate.imageCount,
          detectedNumber: candidate.detectedNumber ?? null,
          volumeNumber: assignedNumber,
          volumeName: sanitizeVolumeName(candidate.folderName),
        } satisfies VolumeMapping;
      });
    },
    [sanitizeVolumeName]
  );

  const analyzeDirectory = useCallback(
    async (path: string) => {
      if (!path) {
        setSourceAnalysis(null);
        setVolumeMappings([]);
        setMappingConfirmed(false);
        setSelectedVolumeKey(null);
        setAnalysisError(null);
        return;
      }

      setAnalysisLoading(true);
      setAnalysisError(null);
      setRenameSummary(null);

      try {
        const analysis = await invoke<MangaSourceAnalysis>(
          'analyze_manga_directory',
          {
            directory: path,
          }
        );

        setSourceAnalysis(analysis);
        setSplitEstimate(analysis.splitDetection ?? null);

        if (analysis.mode === 'multiVolume') {
          const mappings = buildInitialMappings(analysis);
          setVolumeMappings(mappings);
          setMappingConfirmed(false);
          setSelectedVolumeKey(
            mappings.length > 0 ? mappings[0].directory : null
          );
        } else {
          setVolumeMappings([]);
          setMappingConfirmed(true);
          setSelectedVolumeKey(null);
        }
      } catch (error) {
        setSourceAnalysis(null);
        setVolumeMappings([]);
        setMappingConfirmed(false);
        setSelectedVolumeKey(null);
        setAnalysisError(
          error instanceof Error ? error.message : String(error)
        );
        setSplitEstimate(null);
      } finally {
        setAnalysisLoading(false);
      }
    },
    [buildInitialMappings]
  );

  const handleRefreshAnalysis = useCallback(() => {
    if (!renameForm.directory) {
      setAnalysisError('请先选择漫画文件夹路径。');
      return;
    }
    analyzeDirectory(renameForm.directory);
  }, [analyzeDirectory, renameForm.directory]);

  useEffect(() => {
    if (typeof window === 'undefined') {
      return;
    }

    try {
      const stored = window.localStorage.getItem(SETTINGS_KEY);
      const legacyStored = window.localStorage.getItem(LEGACY_SETTINGS_KEY);
      const storedDefaults = window.localStorage.getItem(UPLOAD_DEFAULTS_KEY);
      const legacyDefaults = window.localStorage.getItem(
        LEGACY_UPLOAD_DEFAULTS_KEY
      );
      const storedAddressBookRaw = window.localStorage.getItem(
        SERVICE_ADDRESS_BOOK_KEY
      );
      const legacyAddressBookRaw = window.localStorage.getItem(
        LEGACY_SERVICE_ADDRESS_BOOK_KEY
      );

      let uploadPatch: Partial<UploadFormState> | null = null;
      let jobPatch: Partial<JobFormState> | null = null;
      let defaultsServiceUrl: string | null = null;
      let storedAddressBook: ServiceAddressBook | null = null;
      let legacyAddressBook: ServiceAddressBook | null = null;
      let storedEdgeThresholds: [number, number] | null = null;
      let storedEdgeSearchRatios: [number, number] | null = null;

      if (stored) {
        const parsed = JSON.parse(stored) as {
          uploadForm?: Partial<UploadFormState>;
          jobForm?: Partial<JobFormState>;
          splitOverrides?: {
            edgeTexture?: {
              brightnessThresholds?: [number, number];
              leftSearchRatio?: number;
              rightSearchRatio?: number;
            };
          };
        };

        if (parsed.uploadForm) {
          uploadPatch = { ...parsed.uploadForm };
          if (uploadPatch && 'remotePath' in uploadPatch) {
            delete (uploadPatch as Record<string, unknown>).remotePath;
          }
        }
        if (parsed.jobForm) {
          jobPatch = { ...parsed.jobForm };
        }
        const thresholds = parsed.splitOverrides?.edgeTexture?.brightnessThresholds;
        if (
          Array.isArray(thresholds) &&
          thresholds.length === 2 &&
          typeof thresholds[0] === 'number' &&
          typeof thresholds[1] === 'number'
        ) {
          storedEdgeThresholds = [thresholds[0], thresholds[1]];
        }
        const leftRatio = parsed.splitOverrides?.edgeTexture?.leftSearchRatio;
        const rightRatio = parsed.splitOverrides?.edgeTexture?.rightSearchRatio;
        if (
          typeof leftRatio === 'number' &&
          typeof rightRatio === 'number' &&
          leftRatio >= EDGE_SEARCH_RATIO_MIN &&
          leftRatio <= EDGE_SEARCH_RATIO_MAX &&
          rightRatio >= EDGE_SEARCH_RATIO_MIN &&
          rightRatio <= EDGE_SEARCH_RATIO_MAX
        ) {
          storedEdgeSearchRatios = [leftRatio, rightRatio];
        }
      }

      if (!uploadPatch && legacyStored) {
        try {
          const legacyParsed = JSON.parse(legacyStored) as {
            uploadForm?: Partial<UploadFormState>;
            jobForm?: Partial<JobFormState>;
          };

          if (legacyParsed.uploadForm) {
            uploadPatch = { ...legacyParsed.uploadForm };
            if (uploadPatch && 'remotePath' in uploadPatch) {
              delete (uploadPatch as Record<string, unknown>).remotePath;
            }
          }
          if (legacyParsed.jobForm) {
            jobPatch = { ...legacyParsed.jobForm, ...(jobPatch ?? {}) };
          }
        } catch (legacyError) {
          console.warn(
            'Failed to restore legacy manga agent settings',
            legacyError
          );
        }
      }

      if (storedDefaults) {
        const defaults = JSON.parse(storedDefaults) as Partial<
          Pick<UploadFormState, 'serviceUrl'>
        >;
        if (defaults?.serviceUrl) {
          defaultsServiceUrl = defaults.serviceUrl;
          if (!uploadPatch?.serviceUrl) {
            uploadPatch = {
              ...(uploadPatch ?? {}),
              serviceUrl: defaults.serviceUrl,
            };
          }
        }
      }

      if (!defaultsServiceUrl && legacyDefaults) {
        try {
          const defaults = JSON.parse(legacyDefaults) as Partial<
            Pick<UploadFormState, 'serviceUrl'>
          >;
          if (defaults?.serviceUrl) {
            defaultsServiceUrl = defaults.serviceUrl;
            if (!uploadPatch?.serviceUrl) {
              uploadPatch = {
                ...(uploadPatch ?? {}),
                serviceUrl: defaults.serviceUrl,
              };
            }
          }
        } catch (legacyDefaultsError) {
          console.warn(
            'Failed to restore legacy upload defaults',
            legacyDefaultsError
          );
        }
      }

      if (storedAddressBookRaw) {
        try {
          const parsedAddressBook = JSON.parse(
            storedAddressBookRaw
          ) as Partial<ServiceAddressBook> | null;
          if (parsedAddressBook && typeof parsedAddressBook === 'object') {
            const upload = Array.isArray(parsedAddressBook.upload)
              ? parsedAddressBook.upload.filter(
                  (item): item is string => typeof item === 'string'
                )
              : [];
            const job = Array.isArray(parsedAddressBook.job)
              ? parsedAddressBook.job.filter(
                  (item): item is string => typeof item === 'string'
                )
              : [];
            storedAddressBook = { upload, job };
          }
        } catch (addressError) {
          console.warn('Failed to restore service address book', addressError);
        }
      }

      if (legacyAddressBookRaw) {
        try {
          const parsedAddressBook = JSON.parse(
            legacyAddressBookRaw
          ) as Partial<ServiceAddressBook> | null;
          if (parsedAddressBook && typeof parsedAddressBook === 'object') {
            const upload = Array.isArray(parsedAddressBook.upload)
              ? parsedAddressBook.upload.filter(
                  (item): item is string => typeof item === 'string'
                )
              : [];
            const job = Array.isArray(parsedAddressBook.job)
              ? parsedAddressBook.job.filter(
                  (item): item is string => typeof item === 'string'
                )
              : [];
            legacyAddressBook = { upload, job };
          }
        } catch (legacyAddressError) {
          console.warn(
            'Failed to restore legacy service address book',
            legacyAddressError
          );
        }
      }

      const uploadCandidates = mergeServiceAddresses(
        storedAddressBook?.upload ?? [],
        legacyAddressBook?.upload ?? [],
        uploadPatch?.serviceUrl,
        defaultsServiceUrl
      );
      const jobCandidates = mergeServiceAddresses(
        storedAddressBook?.job ?? [],
        legacyAddressBook?.job ?? [],
        jobPatch?.serviceUrl
      );

      setUploadServiceOptions(uploadCandidates);
      setJobServiceOptions(jobCandidates);

      if (uploadPatch && Object.keys(uploadPatch).length > 0) {
        setUploadForm((prev) => ({ ...prev, ...uploadPatch }));
      }
      if (jobPatch && Object.keys(jobPatch).length > 0) {
        setJobForm((prev) => ({ ...prev, ...jobPatch }));
      }
      if (storedEdgeThresholds) {
        setEdgeBrightnessThresholds(storedEdgeThresholds);
      }
      if (storedEdgeSearchRatios) {
        setEdgeSearchRatios(storedEdgeSearchRatios);
      }
      setHasRestoredDefaults(true);
    } catch (storageError) {
      console.warn('Failed to restore manga agent settings', storageError);
      setHasRestoredDefaults(true);
    }
  }, []);

  useEffect(() => {
    if (typeof window === 'undefined' || jobParamsRestored) {
      return;
    }

    try {
      const storedParams = window.localStorage.getItem(PARAM_DEFAULTS_KEY);
      const storedFavorites = window.localStorage.getItem(PARAM_FAVORITES_KEY);

      if (storedParams) {
        const parsed = JSON.parse(storedParams) as Partial<JobParamsConfig>;
        setJobParams((prev) => ({ ...prev, ...parsed }));
      }

      if (storedFavorites) {
        const favorites = JSON.parse(storedFavorites) as JobParamFavorite[];
        if (Array.isArray(favorites)) {
          setJobParamFavorites(
            favorites
              .filter(
                (item) =>
                  typeof item?.id === 'string' && typeof item?.name === 'string'
              )
              .map((item) => ({
                ...DEFAULT_JOB_PARAMS,
                ...item,
                id: item.id,
                name: item.name,
                createdAt: item.createdAt ?? Date.now(),
              }))
          );
        }
      }
    } catch (storageError) {
      console.warn('Failed to restore manga agent job params', storageError);
    } finally {
      setJobParamsRestored(true);
    }
  }, [jobParamsRestored]);

  useEffect(() => {
    if (typeof window === 'undefined' || !jobParamsRestored) {
      return;
    }

    try {
      window.localStorage.setItem(
        PARAM_DEFAULTS_KEY,
        JSON.stringify(jobParams)
      );
    } catch (storageError) {
      console.warn('Failed to persist job params', storageError);
    }
  }, [jobParams, jobParamsRestored]);

  useEffect(() => {
    if (typeof window === 'undefined' || !jobParamsRestored) {
      return;
    }

    try {
      window.localStorage.setItem(
        PARAM_FAVORITES_KEY,
        JSON.stringify(jobParamFavorites)
      );
    } catch (storageError) {
      console.warn('Failed to persist job param favorites', storageError);
    }
  }, [jobParamFavorites, jobParamsRestored]);

  useEffect(() => {
    const trimmedService = uploadForm.serviceUrl.trim();
    const trimmedToken = uploadForm.bearerToken.trim();
    const trimmedTitle = uploadForm.title.trim();
    const trimmedVolume = uploadForm.volume.trim();

    setJobForm((prev) => {
      let changed = false;
      const next = { ...prev };

      if (!prev.serviceUrl && trimmedService) {
        next.serviceUrl = trimmedService;
        changed = true;
      }
      if (!prev.bearerToken && trimmedToken) {
        next.bearerToken = trimmedToken;
        changed = true;
      }
      if (!prev.title && trimmedTitle) {
        next.title = trimmedTitle;
        changed = true;
      }
      if (!prev.volume && trimmedVolume) {
        next.volume = trimmedVolume;
        changed = true;
      }

      return changed ? next : prev;
    });
  }, [
    uploadForm.serviceUrl,
    uploadForm.bearerToken,
    uploadForm.title,
    uploadForm.volume,
  ]);

  useEffect(() => {
    if (typeof window === 'undefined') {
      return;
    }

    if (!hasRestoredDefaults) {
      return;
    }

    try {
      const defaults = {
        serviceUrl: uploadForm.serviceUrl,
      };
      window.localStorage.setItem(
        UPLOAD_DEFAULTS_KEY,
        JSON.stringify(defaults)
      );
    } catch (storageError) {
      console.warn('Failed to persist upload defaults', storageError);
    }
  }, [hasRestoredDefaults, uploadForm.serviceUrl]);

  useEffect(() => {
    if (typeof window === 'undefined') {
      return;
    }

    const addressBook: ServiceAddressBook = {
      upload: uploadServiceOptions,
      job: jobServiceOptions,
    };

    try {
      window.localStorage.setItem(
        SERVICE_ADDRESS_BOOK_KEY,
        JSON.stringify(addressBook)
      );
    } catch (storageError) {
      console.warn('Failed to persist service address book', storageError);
    }
  }, [jobServiceOptions, uploadServiceOptions]);

  useEffect(() => {
    const trimmed = uploadForm.serviceUrl.trim();
    if (!trimmed) {
      return;
    }

    setUploadServiceOptions((prev) => {
      if (prev.includes(trimmed)) {
        return prev;
      }
      return [...prev, trimmed];
    });
  }, [uploadForm.serviceUrl]);

  useEffect(() => {
    const trimmed = jobForm.serviceUrl.trim();
    if (!trimmed) {
      return;
    }

    setJobServiceOptions((prev) => {
      if (prev.includes(trimmed)) {
        return prev;
      }
      return [...prev, trimmed];
    });
  }, [jobForm.serviceUrl]);

  useEffect(() => {
    if (lastUploadRemotePath || !jobForm.inputPath.trim()) {
      return;
    }
    setLastUploadRemotePath(jobForm.inputPath.trim());
  }, [jobForm.inputPath, lastUploadRemotePath]);

  useEffect(() => {
    if (typeof window === 'undefined') {
      return;
    }

    const payload = {
      uploadForm,
      jobForm,
      jobParams,
      splitOverrides: {
        edgeTexture: {
          brightnessThresholds: edgeBrightnessThresholds,
          leftSearchRatio: edgeSearchRatios[0],
          rightSearchRatio: edgeSearchRatios[1],
        },
      },
    };

    try {
      window.localStorage.setItem(SETTINGS_KEY, JSON.stringify(payload));
    } catch (storageError) {
      console.warn('Failed to persist manga agent settings', storageError);
    }
  }, [
    edgeBrightnessThresholds,
    edgeSearchRatios,
    jobForm,
    jobParams,
    uploadForm,
  ]);

  useEffect(() => {
    if (!isMultiVolumeSource) {
      return;
    }

    if (volumeMappings.length === 0) {
      setSelectedVolumeKey(null);
      return;
    }

    const exists = volumeMappings.some(
      (item) => item.directory === selectedVolumeKey
    );
    if (!exists) {
      setSelectedVolumeKey(volumeMappings[0].directory);
    }
  }, [isMultiVolumeSource, selectedVolumeKey, volumeMappings]);

  useEffect(() => {
    if (isMultiVolumeSource && contentSplitEnabled) {
      setContentSplitEnabled(false);
      resetSplitState();
    }
  }, [contentSplitEnabled, isMultiVolumeSource, resetSplitState]);

  useEffect(() => {
    if (splitPreparing) {
      return;
    }

    if (!splitProgress) {
      return;
    }

    if (typeof window === 'undefined') {
      setSplitProgress(null);
      return;
    }

    const timer = window.setTimeout(() => {
      setSplitProgress(null);
    }, 600);

    return () => {
      window.clearTimeout(timer);
    };
  }, [splitPreparing, splitProgress]);

  useEffect(() => {
    const directoryCandidate =
      (sourceAnalysis?.root && sourceAnalysis.root.length > 0
        ? sourceAnalysis.root
        : undefined) ??
      (renameForm.directory && renameForm.directory.length > 0
        ? renameForm.directory
        : undefined) ??
      (renameSummary?.mode === 'single' ? renameSummary.outcome.directory : '');

    if (!directoryCandidate) {
      return;
    }

    const segments = directoryCandidate.split(/[\\/]/).filter(Boolean);
    const folderName = segments[segments.length - 1];
    if (!folderName) {
      return;
    }

    setUploadForm((prev) => {
      if (prev.title.trim()) {
        return prev;
      }
      return { ...prev, title: folderName };
    });

    setJobForm((prev) => {
      if (prev.title.trim()) {
        return prev;
      }
      return { ...prev, title: folderName };
    });
  }, [renameForm.directory, renameSummary, sourceAnalysis?.root]);

  const previewEntries = useMemo(() => {
    if (!renameSummary || renameSummary.mode !== 'single') {
      return [] as RenameEntry[];
    }
    return renameSummary.outcome.entries.slice(0, 8);
  }, [renameSummary]);

  const selectedVolume = useMemo(() => {
    if (!selectedVolumeKey) {
      return null;
    }
    return (
      volumeMappings.find((item) => item.directory === selectedVolumeKey) ??
      null
    );
  }, [selectedVolumeKey, volumeMappings]);

  const remotePathPreview = useMemo(() => {
    const resolvedTitle = uploadForm.title.trim() || jobForm.title.trim();
    const selectedVolumeName = selectedVolume
      ? selectedVolume.volumeName.trim()
      : '';
    const selectedVolumeFolder = selectedVolume
      ? selectedVolume.folderName.trim()
      : '';
    const resolvedVolume =
      uploadForm.volume.trim() ||
      jobForm.volume.trim() ||
      selectedVolumeName ||
      selectedVolumeFolder ||
      '';

    return buildRemotePath({
      title: resolvedTitle,
      volume: resolvedVolume,
      seed: remotePathSeed,
      mode: uploadForm.mode,
    });
  }, [
    jobForm.title,
    jobForm.volume,
    remotePathSeed,
    selectedVolume?.directory,
    selectedVolume?.folderName,
    selectedVolume?.volumeName,
    uploadForm.mode,
    uploadForm.title,
    uploadForm.volume,
  ]);

  const handleRegenerateRemotePath = useCallback(() => {
    setRemotePathSeed(Date.now());
  }, []);

  const filteredJobs = useMemo(() => {
    const keyword = jobSearch.trim().toLowerCase();

    return jobs.filter((job) => {
      const statusUpper = job.status.toUpperCase();
      const matchesStatus = (() => {
        switch (jobStatusFilter) {
          case 'active':
            return statusUpper === 'PENDING' || statusUpper === 'RUNNING';
          case 'completed':
            return statusUpper === 'SUCCESS';
          case 'failed':
            return statusUpper === 'FAILED' || statusUpper === 'ERROR';
          case 'all':
          default:
            return true;
        }
      })();

      if (!matchesStatus) {
        return false;
      }

      if (!keyword) {
        return true;
      }

      const haystack = [
        job.jobId,
        job.message ?? '',
        job.metadata?.title ?? '',
        job.metadata?.volume ?? '',
      ]
        .join(' ')
        .toLowerCase();

      return haystack.includes(keyword);
    });
  }, [jobSearch, jobStatusFilter, jobs]);

  useEffect(() => {
    setSelectedJobIds((prev) =>
      prev.filter((id) => jobs.some((job) => job.jobId === id))
    );
  }, [jobs]);

  const allVisibleSelected = useMemo(() => {
    if (filteredJobs.length === 0) {
      return false;
    }
    return filteredJobs.every((job) => selectedJobIds.includes(job.jobId));
  }, [filteredJobs, selectedJobIds]);

  const hasSelection = selectedJobIds.length > 0;

  useEffect(() => {
    if (!isMultiVolumeSource || !selectedVolume) {
      return;
    }

    setUploadForm((prev) => {
      if (prev.volume.trim()) {
        return prev;
      }
      return { ...prev, volume: selectedVolume.volumeName };
    });

    setJobForm((prev) => {
      if (prev.volume.trim()) {
        return prev;
      }
      return { ...prev, volume: selectedVolume.volumeName };
    });
  }, [isMultiVolumeSource, selectedVolume]);

  const handleSelectDirectory = useCallback(async () => {
    const selected = await invoke<string | string[] | null>(
      'plugin:dialog|open',
      {
        options: {
          directory: true,
          multiple: false,
          title: '选择漫画图片文件夹',
        },
      }
    );

    if (!selected) {
      return;
    }

    const first = Array.isArray(selected) ? selected[0] : selected;
    if (typeof first === 'string' && first.length > 0) {
      setRenameForm((prev) => ({ ...prev, directory: first }));
      setCurrentStep('source');
      analyzeDirectory(first);
    }
  }, [analyzeDirectory]);

  const ensureSplitWorkspace = useCallback(
    async (overwrite = false): Promise<RenameSplitPayload | null> => {
      const root = resolveRenameRoot().trim();
      if (!root) {
        setSplitError('请先选择有效的漫画目录。');
        return null;
      }

      if (splitAlgorithm === 'manual') {
        setSplitError(null);
        return null;
      }

      if (!overwrite && splitWorkspace && splitSourceRoot === root) {
        return {
          enabled: true,
          workspace: splitWorkspace,
          reportPath: splitReportPath ?? undefined,
          summary: splitSummaryState ?? null,
          warnings:
            splitWarningsState.length > 0 ? [...splitWarningsState] : undefined,
        };
      }

      setSplitProgress(null);
      setSplitPreparing(true);
      setSplitError(null);
      try {
        const [brightThreshold, darkThreshold] = edgeBrightnessThresholds;
        const thresholds: SplitThresholdOverrides =
          splitAlgorithm === 'edgeTexture'
            ? {
                mode: 'edgeTextureOnly',
                edgeTexture: {
                  whiteThreshold: 1.0,
                  brightnessThresholds: [brightThreshold, darkThreshold],
                  brightnessWeight: 0.5,
                  leftSearchRatio: edgeSearchRatios[0],
                  rightSearchRatio: edgeSearchRatios[1],
                },
              }
            : { mode: 'projectionOnly' };

        const outcome = await invoke<SplitCommandOutcome>(
          'prepare_doublepage_split',
          {
            options: {
              directory: root,
              dryRun: false,
              overwrite,
              thresholds,
            },
          }
        );

        if (!outcome.workspaceDirectory) {
          throw new Error('拆分命令未返回工作目录。');
        }

        const summary: RenameSplitSummary = {
          analyzedFiles: outcome.analyzedFiles,
          emittedFiles: outcome.emittedFiles,
          skippedFiles: outcome.skippedFiles,
          splitPages: outcome.splitPages,
          coverTrims: outcome.coverTrims,
          fallbackSplits: outcome.fallbackSplits,
        };

        setSplitWorkspace(outcome.workspaceDirectory);
        setSplitSummaryState(summary);
        setSplitWarningsState(outcome.warnings ?? []);
        setSplitReportPath(outcome.reportPath ?? null);
        setSplitSourceRoot(root);

        return {
          enabled: true,
          workspace: outcome.workspaceDirectory,
          reportPath: outcome.reportPath ?? null,
          summary,
          warnings:
            outcome.warnings && outcome.warnings.length > 0
              ? [...outcome.warnings]
              : undefined,
        };
      } catch (error) {
        setSplitError(error instanceof Error ? error.message : String(error));
        return null;
      } finally {
        setSplitPreparing(false);
      }
    },
    [
      resolveRenameRoot,
      splitWorkspace,
      splitSourceRoot,
      splitSummaryState,
      splitReportPath,
      splitWarningsState,
      splitAlgorithm,
      edgeBrightnessThresholds,
      edgeSearchRatios,
    ]
  );

  const handlePrepareSplit = useCallback(async () => {
    if (!contentSplitEnabled) {
      setSplitError('请先启用内容感知拆分开关。');
      return;
    }
    const [bright, dark] = edgeBrightnessThresholds;
    const nextErrors = computeEdgeThresholdErrors(bright, dark);
    setEdgeThresholdErrors(nextErrors);
    if (nextErrors.bright || nextErrors.dark) {
      setSplitError('请先修复阈值输入错误。');
      return;
    }
    const [leftRatio, rightRatio] = edgeSearchRatios;
    const nextRatioErrors = computeEdgeSearchRatioErrors(leftRatio, rightRatio);
    setEdgeSearchRatioErrors(nextRatioErrors);
    if (nextRatioErrors.left || nextRatioErrors.right) {
      setSplitError('请先修复搜索比例输入错误。');
      return;
    }
    await ensureSplitWorkspace(true);
  }, [
    computeEdgeThresholdErrors,
    contentSplitEnabled,
    edgeBrightnessThresholds,
    edgeSearchRatios,
    computeEdgeSearchRatioErrors,
    ensureSplitWorkspace,
  ]);

  const handleOpenManualDrawer = useCallback(() => {
    void openManualWorkspace();
  }, [openManualWorkspace]);

  const handleCloseManualDrawer = useCallback(() => {
    setManualDrawerOpen(false);
  }, []);

  const handleToggleSplit = useCallback(
    (event: ChangeEvent<HTMLInputElement>) => {
      const enabled = event.currentTarget.checked;
      setContentSplitEnabled(enabled);
      if (!enabled) {
        resetSplitState();
      } else {
        setSplitError(null);
      }
    },
    [resetSplitState]
  );

  const runRename = useCallback(
    async (dryRun: boolean) => {
      if (!renameForm.directory) {
        setRenameError('请先选择漫画图片所在文件夹。');
        return;
      }

      if (isMultiVolumeSource && !mappingConfirmed) {
        setRenameError('请先在卷映射步骤完成确认。');
        return;
      }

      setRenameLoading(true);
      setRenameError(null);
      try {
        const resolvedRoot = resolveRenameRoot();
        const padValue = Number.isFinite(renameForm.pad)
          ? Math.max(1, Math.floor(renameForm.pad))
          : DEFAULT_PAD;

        const payload = {
          directory: resolvedRoot,
          pad: padValue,
          targetExtension:
            renameForm.targetExtension.trim().toLowerCase() || 'jpg',
          dryRun,
        };

        if (isMultiVolumeSource) {
          if (volumeMappings.length === 0) {
            setRenameError('未检测到任何卷。请检查目录结构。');
            return;
          }

          const outcomes: VolumeRenameOutcome[] = [];

          for (const mapping of volumeMappings) {
            const result = await invoke<RenameOutcome>(
              'rename_manga_sequence',
              {
                options: {
                  ...payload,
                  directory: mapping.directory,
                },
              }
            );

            outcomes.push({ mapping, outcome: result });
          }

          setRenameSummary({ mode: 'multi', volumes: outcomes, dryRun });
        } else {
          const targetDirectory = resolvedRoot;
          let splitPayload: RenameSplitPayload | undefined;

          if (splitAlgorithm === 'manual') {
            if (!manualWorkspace) {
              setRenameError('请先初始化手动拆分工作区并完成至少一次应用。');
              setRenameLoading(false);
              return;
            }
            splitPayload = {
              enabled: true,
              workspace: manualWorkspace,
              reportPath: null,
              summary: null,
            };
          } else if (contentSplitEnabled) {
            const ensured = await ensureSplitWorkspace(false);
            if (!ensured) {
              return;
            }
            splitPayload = ensured;
          }

          const result = await invoke<RenameOutcome>('rename_manga_sequence', {
            options: {
              ...payload,
              directory: targetDirectory,
              split: splitPayload ?? { enabled: false },
            },
          });

          if (result.splitApplied) {
            if (typeof result.splitWorkspace !== 'undefined') {
              setSplitWorkspace(result.splitWorkspace ?? null);
            }
            if (typeof result.splitSummary !== 'undefined') {
              setSplitSummaryState(result.splitSummary ?? null);
            }
            if (typeof result.splitReportPath !== 'undefined') {
              setSplitReportPath(result.splitReportPath ?? null);
            }
            setSplitSourceRoot(result.sourceDirectory ?? targetDirectory);
          } else if (splitPayload?.workspace) {
            setSplitWorkspace(splitPayload.workspace);
            setSplitSummaryState(splitPayload.summary ?? null);
            setSplitReportPath(splitPayload.reportPath ?? null);
            setSplitSourceRoot(targetDirectory);
          }

          if (splitPayload?.warnings) {
            setSplitWarningsState(splitPayload.warnings);
          }

          setRenameSummary({ mode: 'single', outcome: result });
          setSplitError(null);
        }

        if (dryRun) {
          setUploadStatus(null);
        }
      } catch (error) {
        setRenameError(error instanceof Error ? error.message : String(error));
      } finally {
        setRenameLoading(false);
      }
    },
    [
      contentSplitEnabled,
      ensureSplitWorkspace,
      isMultiVolumeSource,
      mappingConfirmed,
      manualWorkspace,
      renameForm,
      resolveRenameRoot,
      splitAlgorithm,
      volumeMappings,
    ]
  );

  const handleRenameInput = useCallback(
    (field: keyof RenameFormState) =>
      (event: ChangeEvent<HTMLInputElement>) => {
        const value = event.currentTarget.value;

        if (field === 'directory') {
          resetSplitState();
          setContentSplitEnabled(false);
          setSplitEstimate(null);
          setSourceAnalysis(null);
          setVolumeMappings([]);
          setMappingConfirmed(false);
          setSelectedVolumeKey(null);
          setAnalysisError(null);
          setRenameSummary(null);
          setRenameForm((prev) => ({ ...prev, directory: value }));
          return;
        }

        setRenameForm((prev) => {
          if (field === 'pad') {
            return { ...prev, pad: Number(value) };
          }
          if (field === 'targetExtension') {
            return { ...prev, targetExtension: value.toLowerCase() };
          }
          return { ...prev, [field]: value } as RenameFormState;
        });
      },
    [resetSplitState]
  );

  const handleUploadInput = useCallback(
    (field: keyof UploadFormState) =>
      (event: ChangeEvent<HTMLInputElement | HTMLSelectElement>) => {
        const value = event.currentTarget.value;
        setUploadForm((prev) => {
          if (field === 'mode') {
            if (value === 'folder') {
              setUploadError('Folder 模式仍在规划中，当前仅支持 Zip 上传。');
              return { ...prev, mode: 'zip' };
            }
            return { ...prev, mode: value as UploadMode };
          }
          return { ...prev, [field]: value } as UploadFormState;
        });
      },
    []
  );

  const beginAddUploadService = useCallback(() => {
    setUploadAddressError(null);
    setUploadAddressDraft(uploadForm.serviceUrl.trim() || '');
    setIsAddingUploadService(true);
  }, [uploadForm.serviceUrl]);

  const cancelAddUploadService = useCallback(() => {
    setIsAddingUploadService(false);
    setUploadAddressDraft('');
    setUploadAddressError(null);
  }, []);

  const confirmAddUploadService = useCallback(() => {
    const trimmed = uploadAddressDraft.trim();
    if (!trimmed) {
      setUploadAddressError('请输入有效的上传服务器地址。');
      return;
    }

    setUploadAddressError(null);
    setUploadServiceOptions((prev) => {
      if (prev.includes(trimmed)) {
        return prev;
      }
      return [...prev, trimmed];
    });
    setUploadForm((prev) => ({ ...prev, serviceUrl: trimmed }));
    setUploadAddressDraft('');
    setIsAddingUploadService(false);
  }, [uploadAddressDraft]);

  const handleParamChange = useCallback(
    (field: keyof JobParamsConfig) =>
      (event: ChangeEvent<HTMLInputElement | HTMLSelectElement>) => {
        const rawValue = event.currentTarget.value;

        setJobParams((prev) => {
          switch (field) {
            case 'scale': {
              const numeric = Number(rawValue);
              const normalized = Number.isFinite(numeric)
                ? Math.min(4, Math.max(1, Math.floor(numeric)))
                : prev.scale;
              return { ...prev, scale: normalized };
            }
            case 'denoise':
              return {
                ...prev,
                denoise: rawValue as JobParamsConfig['denoise'],
              };
            case 'model':
              return { ...prev, model: rawValue };
            case 'outputFormat':
              return {
                ...prev,
                outputFormat: rawValue as JobParamsConfig['outputFormat'],
              };
            case 'jpegQuality': {
              const numeric = Number(rawValue);
              const normalized = Number.isFinite(numeric)
                ? Math.min(100, Math.max(1, Math.round(numeric)))
                : prev.jpegQuality;
              return { ...prev, jpegQuality: normalized };
            }
            case 'tileSize': {
              if (rawValue.trim() === '') {
                return { ...prev, tileSize: null };
              }
              const numeric = Number(rawValue);
              if (!Number.isFinite(numeric)) {
                return prev;
              }
              return {
                ...prev,
                tileSize: Math.min(1024, Math.max(32, Math.round(numeric))),
              };
            }
            case 'tilePad': {
              if (rawValue.trim() === '') {
                return { ...prev, tilePad: null };
              }
              const numeric = Number(rawValue);
              if (!Number.isFinite(numeric)) {
                return prev;
              }
              return {
                ...prev,
                tilePad: Math.min(128, Math.max(0, Math.round(numeric))),
              };
            }
            case 'batchSize': {
              if (rawValue.trim() === '') {
                return { ...prev, batchSize: null };
              }
              const numeric = Number(rawValue);
              if (!Number.isFinite(numeric)) {
                return prev;
              }
              return {
                ...prev,
                batchSize: Math.min(16, Math.max(1, Math.round(numeric))),
              };
            }
            case 'device':
              return { ...prev, device: rawValue as JobParamsConfig['device'] };
            default:
              return prev;
          }
        });
      },
    []
  );

  const handleResetParams = useCallback(() => {
    setJobParams(DEFAULT_JOB_PARAMS);
  }, []);

  const computeFavoriteName = useCallback((params: JobParamsConfig) => {
    const pieces = [`${params.model}`];
    pieces.push(`×${params.scale}`);
    pieces.push(`denoise:${params.denoise}`);
    if (params.outputFormat === 'jpg') {
      pieces.push(`jpg@${params.jpegQuality}`);
    } else {
      pieces.push(params.outputFormat);
    }
    return pieces.join(' · ');
  }, []);

  const handleSaveFavorite = useCallback(() => {
    const id =
      typeof crypto !== 'undefined' && 'randomUUID' in crypto
        ? crypto.randomUUID()
        : `fav-${Date.now()}`;
    const name = computeFavoriteName(jobParams);

    setJobParamFavorites((prev) => {
      const exists = prev.some(
        (item) =>
          item.model === jobParams.model &&
          item.scale === jobParams.scale &&
          item.denoise === jobParams.denoise &&
          item.outputFormat === jobParams.outputFormat &&
          item.jpegQuality === jobParams.jpegQuality &&
          item.tileSize === jobParams.tileSize &&
          item.tilePad === jobParams.tilePad &&
          item.batchSize === jobParams.batchSize &&
          item.device === jobParams.device
      );

      if (exists) {
        return prev;
      }

      const favorite: JobParamFavorite = {
        ...jobParams,
        id,
        name,
        createdAt: Date.now(),
      };

      const combined = [favorite, ...prev];
      return combined.slice(0, 8);
    });
  }, [computeFavoriteName, jobParams]);

  const handleApplyFavorite = useCallback(
    (favoriteId: string) => {
      const favorite = jobParamFavorites.find((item) => item.id === favoriteId);
      if (!favorite) {
        return;
      }
      setJobParams({
        model: favorite.model,
        scale: favorite.scale,
        denoise: favorite.denoise,
        outputFormat: favorite.outputFormat,
        jpegQuality: favorite.jpegQuality,
        tileSize: favorite.tileSize,
        tilePad: favorite.tilePad,
        batchSize: favorite.batchSize,
        device: favorite.device,
      });
    },
    [jobParamFavorites]
  );

  const handleRemoveFavorite = useCallback((favoriteId: string) => {
    setJobParamFavorites((prev) =>
      prev.filter((item) => item.id !== favoriteId)
    );
  }, []);

  const inferManifestForVolume = useCallback(
    (volumeName: string | null | undefined): string | null => {
      if (!renameSummary) {
        return null;
      }

      if (renameSummary.mode === 'single') {
        return renameSummary.outcome.manifestPath ?? null;
      }

      if (renameSummary.mode === 'multi') {
        const normalized = volumeName?.trim().toLowerCase();
        const match = renameSummary.volumes.find((entry) => {
          if (!entry.outcome.manifestPath) {
            return false;
          }
          if (!normalized) {
            return false;
          }
          const candidate = entry.mapping.volumeName.trim().toLowerCase();
          if (candidate === normalized) {
            return true;
          }
          if (
            normalized.includes(String(entry.mapping.volumeNumber ?? '')) &&
            normalized.includes(candidate)
          ) {
            return true;
          }
          return false;
        });

        return match?.outcome.manifestPath ?? null;
      }

      return null;
    },
    [renameSummary]
  );

  const mapPayloadToRecord = useCallback(
    (
      payload: JobEventPayload,
      overrideMessage?: string | null,
      existing?: JobRecord
    ): JobRecord => {
      const displayMessage =
        overrideMessage ?? payload.error ?? payload.message ?? null;
      const transport = payload.transport ?? 'system';
      return {
        jobId: payload.jobId,
        status: payload.status,
        processed: payload.processed,
        total: payload.total,
        artifactPath: payload.artifactPath ?? null,
        message: displayMessage,
        transport,
        error: payload.error ?? null,
        retries: payload.retries ?? 0,
        lastError: payload.lastError ?? null,
        artifactHash: payload.artifactHash ?? null,
        params: payload.params ?? null,
        metadata: payload.metadata ?? null,
        lastUpdated: Date.now(),
        serviceUrl: existing?.serviceUrl ?? jobForm.serviceUrl,
        bearerToken: existing?.bearerToken ?? jobForm.bearerToken,
        inputPath: existing?.inputPath ?? jobForm.inputPath,
        inputType: existing?.inputType ?? jobForm.inputType,
        manifestPath:
          existing?.manifestPath ??
          inferManifestForVolume(payload.metadata?.volume ?? null),
      };
    },
    [
      inferManifestForVolume,
      jobForm.bearerToken,
      jobForm.inputPath,
      jobForm.inputType,
      jobForm.serviceUrl,
    ]
  );

  useEffect(() => {
    let disposed = false;
    const disposers: UnlistenFn[] = [];

    const bind = async () => {
      try {
        const uploadUnlisten = await listen<UploadProgressPayload>(
          'manga-upload-progress',
          (event) => {
            const payload = event.payload;
            if (!payload) {
              return;
            }

            setUploadProgress(payload);

            switch (payload.stage) {
              case 'preparing':
                setUploadError(null);
                setUploadStatus(null);
                break;
              case 'failed':
                setUploadError(payload.message ?? '上传失败');
                setUploadStatus(null);
                break;
              case 'completed':
                setUploadError(null);
                setUploadStatus(
                  (prev) => payload.message ?? prev ?? '上传完成'
                );
                break;
              default:
                break;
            }
          }
        );

        if (disposed) {
          uploadUnlisten();
          return;
        }
        disposers.push(uploadUnlisten);

        const jobUnlisten = await listen<JobEventPayload>(
          'manga-job-event',
          (event) => {
            const payload = event.payload;
            if (!payload) {
              return;
            }

            staleJobsRef.current.delete(payload.jobId);

            setJobs((prev) => {
              const existing = prev.find(
                (item) => item.jobId === payload.jobId
              );
              const record = mapPayloadToRecord(
                payload,
                undefined,
                existing ?? undefined
              );
              const next = prev.filter((item) => item.jobId !== record.jobId);
              next.push(record);
              next.sort((a, b) => b.lastUpdated - a.lastUpdated);
              return next;
            });
          }
        );

        if (disposed) {
          jobUnlisten();
          return;
        }
        disposers.push(jobUnlisten);

        const splitUnlisten = await listen<SplitProgressPayload>(
          'doublepage-split-progress',
          (event) => {
            if (!event.payload) {
              return;
            }
            setSplitProgress(event.payload);
          }
        );

        if (disposed) {
          splitUnlisten();
          return;
        }
        disposers.push(splitUnlisten);
      } catch (bindingError) {
        console.warn('Failed to bind manga upscale events', bindingError);
      }
    };

    void bind();

    return () => {
      disposed = true;
      disposers.forEach((dispose) => {
        try {
          dispose();
        } catch {
          /* ignore */
        }
      });
    };
  }, [mapPayloadToRecord]);

  const handleToggleJobSelection = useCallback((jobId: string) => {
    setSelectedJobIds((prev) =>
      prev.includes(jobId)
        ? prev.filter((id) => id !== jobId)
        : [...prev, jobId]
    );
  }, []);

  const handleSelectAllVisible = useCallback(() => {
    setSelectedJobIds(filteredJobs.map((job) => job.jobId));
  }, [filteredJobs]);

  const handleClearSelections = useCallback(() => {
    setSelectedJobIds([]);
  }, []);

  const handleJobSearchChange = useCallback(
    (event: ChangeEvent<HTMLInputElement>) => {
      setJobSearch(event.currentTarget.value);
    },
    []
  );

  const handleJobStatusFilterChange = useCallback(
    (event: ChangeEvent<HTMLSelectElement>) => {
      const value = event.currentTarget.value as
        | 'all'
        | 'active'
        | 'completed'
        | 'failed';
      setJobStatusFilter(value);
    },
    []
  );

  const buildJobRequest = useCallback((job: JobRecord) => {
    const request: Record<string, unknown> = {
      serviceUrl: job.serviceUrl,
      jobId: job.jobId,
    };
    if (job.bearerToken && job.bearerToken.trim()) {
      request.bearerToken = job.bearerToken.trim();
    }
    if (job.inputPath) {
      request.inputPath = job.inputPath;
    }
    if (job.inputType) {
      request.inputType = job.inputType;
    }
    if (job.artifactPath) {
      request.artifactPath = job.artifactPath;
    }
    if (job.metadata) {
      request.metadata = job.metadata;
    }
    return request;
  }, []);

  const startJobWatcher = useCallback(
    async (job: JobRecord, options?: { silent?: boolean }) => {
      const terminal =
        job.status.toUpperCase() === 'SUCCESS' ||
        job.status.toUpperCase() === 'FAILED';
      if (!job.serviceUrl || terminal) {
        staleJobsRef.current.delete(job.jobId);
        return;
      }

      const request: Record<string, unknown> = {
        serviceUrl: job.serviceUrl,
        jobId: job.jobId,
      };

      if (job.bearerToken && job.bearerToken.trim()) {
        request.bearerToken = job.bearerToken.trim();
      }

      if (
        Number.isFinite(jobForm.pollIntervalMs) &&
        jobForm.pollIntervalMs >= 250
      ) {
        request.pollIntervalMs = Math.floor(jobForm.pollIntervalMs);
      }

      try {
        await invoke('watch_manga_job', { request });
        staleJobsRef.current.delete(job.jobId);
      } catch (watchError) {
        if (options?.silent) {
          return;
        }
        const message =
          watchError instanceof Error ? watchError.message : String(watchError);
        setJobs((prev) =>
          prev.map((item) =>
            item.jobId === job.jobId
              ? {
                  ...item,
                  message: `订阅进度失败：${message}`,
                  transport: 'system',
                  error: message,
                  lastUpdated: Date.now(),
                }
              : item
          )
        );
      }
    },
    [jobForm.pollIntervalMs]
  );

  const refreshJobStatus = useCallback(
    async (job: JobRecord) => {
      if (!job.serviceUrl) {
        staleJobsRef.current.delete(job.jobId);
        return;
      }

      try {
        const snapshot = await invoke<JobStatusSnapshotPayload>(
          'fetch_manga_job_status',
          {
            request: {
              serviceUrl: job.serviceUrl,
              jobId: job.jobId,
              bearerToken: job.bearerToken?.trim() || undefined,
            },
          }
        );

        const payload: JobEventPayload = {
          jobId: snapshot.jobId,
          status: snapshot.status,
          processed: snapshot.processed,
          total: snapshot.total,
          artifactPath: snapshot.artifactPath ?? null,
          message: snapshot.message ?? null,
          transport: 'polling',
          error: null,
          retries: snapshot.retries ?? 0,
          lastError: snapshot.lastError ?? null,
          artifactHash: snapshot.artifactHash ?? null,
          params: snapshot.params ?? null,
          metadata: snapshot.metadata ?? null,
        };

        setJobs((prev) => {
          const existing = prev.find((item) => item.jobId === payload.jobId);
          const mapped = mapPayloadToRecord(payload, undefined, existing);
          const next = prev.filter((item) => item.jobId !== mapped.jobId);
          next.push(mapped);
          next.sort((a, b) => b.lastUpdated - a.lastUpdated);
          return next;
        });
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        setJobs((prev) =>
          prev.map((item) =>
            item.jobId === job.jobId
              ? {
                  ...item,
                  message: `刷新状态失败：${message}`,
                  transport: 'system',
                  error: message,
                  lastUpdated: Date.now(),
                }
              : item
          )
        );
      } finally {
        staleJobsRef.current.delete(job.jobId);
      }
    },
    [mapPayloadToRecord]
  );

  useEffect(() => {
    if (jobs.length === 0) {
      staleJobsRef.current.clear();
      return () => {
        /* noop */
      };
    }

    const timer = window.setInterval(() => {
      const now = Date.now();
      for (const job of jobs) {
        const terminal =
          job.status.toUpperCase() === 'SUCCESS' ||
          job.status.toUpperCase() === 'FAILED';
        if (terminal) {
          staleJobsRef.current.delete(job.jobId);
          continue;
        }

        if (!job.serviceUrl) {
          continue;
        }

        if (now - job.lastUpdated < STALE_PROGRESS_THRESHOLD_MS) {
          continue;
        }

        if (staleJobsRef.current.has(job.jobId)) {
          continue;
        }

        staleJobsRef.current.add(job.jobId);
        setJobs((prev) =>
          prev.map((item) =>
            item.jobId === job.jobId
              ? {
                  ...item,
                  message: item.message ?? '进度久未更新，正在轮询刷新…',
                  transport: 'system',
                }
              : item
          )
        );
        void refreshJobStatus(job);
      }
    }, STALE_PROGRESS_CHECK_INTERVAL_MS);

    return () => window.clearInterval(timer);
  }, [jobs, refreshJobStatus]);

  const resumeJob = useCallback(
    async (job: JobRecord, options?: { silent?: boolean }) => {
      const readiness = assessManualReadiness();
      if (!readiness.ok) {
        if (!options?.silent) {
          setJobError(readiness.reason);
        }
        return;
      }

      if (!job.serviceUrl) {
        if (!options?.silent) {
          setJobError('缺少服务地址，无法恢复作业。');
        }
        return;
      }

      if (!options?.silent) {
        setJobStatus(`正在恢复作业 ${job.jobId}…`);
        setJobError(null);
      }

      try {
        const payload = await invoke<JobEventPayload>('resume_manga_job', {
          request: buildJobRequest(job),
        });

        let updatedRecord: JobRecord | null = null;
        setJobs((prev) => {
          const existing =
            prev.find((item) => item.jobId === payload.jobId) ?? job;
          const mapped = mapPayloadToRecord(payload, undefined, existing);
          updatedRecord = mapped;
          const next = prev.filter((item) => item.jobId !== mapped.jobId);
          next.push(mapped);
          next.sort((a, b) => b.lastUpdated - a.lastUpdated);
          return next;
        });

        if (updatedRecord) {
          void startJobWatcher(updatedRecord, {
            silent: options?.silent ?? false,
          });
        }

        if (!options?.silent) {
          setJobStatus(`已请求恢复作业 ${job.jobId}，等待更新。`);
        }
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        if (!options?.silent) {
          setJobError(message);
        }
      }
    },
    [
      assessManualReadiness,
      buildJobRequest,
      mapPayloadToRecord,
      startJobWatcher,
    ]
  );

  const cancelJob = useCallback(
    async (job: JobRecord, options?: { silent?: boolean }) => {
      if (!job.serviceUrl) {
        if (!options?.silent) {
          setJobError('缺少服务地址，无法终止作业。');
        }
        return;
      }

      if (!options?.silent) {
        setJobStatus(`正在终止作业 ${job.jobId}…`);
        setJobError(null);
      }

      try {
        const payload = await invoke<JobEventPayload>('cancel_manga_job', {
          request: buildJobRequest(job),
        });

        setJobs((prev) => {
          const existing =
            prev.find((item) => item.jobId === payload.jobId) ?? job;
          const record = mapPayloadToRecord(payload, undefined, existing);
          const next = prev.filter((item) => item.jobId !== record.jobId);
          next.push(record);
          next.sort((a, b) => b.lastUpdated - a.lastUpdated);
          return next;
        });

        if (!options?.silent) {
          setJobStatus(`已发送终止请求 ${job.jobId}。`);
        }
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        if (!options?.silent) {
          setJobError(message);
        }
      }
    },
    [buildJobRequest, mapPayloadToRecord]
  );

  const handleResumeJob = useCallback(
    (job: JobRecord) => {
      void resumeJob(job);
    },
    [resumeJob]
  );

  const handleCancelJob = useCallback(
    (job: JobRecord) => {
      void cancelJob(job);
    },
    [cancelJob]
  );

  const promptForDirectory = useCallback(
    async (title: string, defaultPath?: string | null) => {
      const selection = await invoke<string | string[] | null>(
        'plugin:dialog|open',
        {
          options: {
            directory: true,
            multiple: false,
            defaultPath: defaultPath ?? undefined,
            title,
          },
        }
      );

      if (!selection) {
        return null;
      }
      const first = Array.isArray(selection) ? selection[0] : selection;
      return typeof first === 'string' && first.length > 0 ? first : null;
    },
    []
  );

  const promptForManifest = useCallback(async () => {
    const selection = await invoke<string | string[] | null>(
      'plugin:dialog|open',
      {
        options: {
          filters: [{ name: 'Manifest', extensions: ['json'] }],
          title: '选择 manifest.json',
          multiple: false,
        },
      }
    );

    if (!selection) {
      return null;
    }
    const first = Array.isArray(selection) ? selection[0] : selection;
    return typeof first === 'string' && first.length > 0 ? first : null;
  }, []);

  const downloadArtifactZip = useCallback(
    async (
      job: JobRecord,
      options?: { silent?: boolean; targetDir?: string | null }
    ) => {
      const readiness = assessManualReadiness();
      if (!readiness.ok) {
        if (!options?.silent) {
          setArtifactError(readiness.reason);
        }
        return false;
      }

      const silent = options?.silent ?? false;

      if (!job.serviceUrl || !job.jobId) {
        if (!silent) {
          setArtifactError('缺少必要信息，无法下载产物。');
        }
        return false;
      }

      if (!job.artifactPath) {
        if (!silent) {
          setArtifactError('远端尚未提供产物路径。');
        }
        return false;
      }

      let targetDir = options?.targetDir ?? null;
      if (!targetDir) {
        const picked = await promptForDirectory(
          '选择产物输出目录',
          artifactTargetRoot
        );
        if (!picked) {
          if (!silent) {
            setArtifactError('已取消选择输出目录。');
          }
          return false;
        }
        targetDir = picked;
      }

      setArtifactDownloadBusyJob(job.jobId);
      if (!silent) {
        setArtifactError(null);
      }

      try {
        const request: Record<string, unknown> = {
          ...buildJobRequest(job),
          targetDir,
        };

        const summary = await invoke<ArtifactDownloadSummary>(
          'download_manga_artifact',
          {
            request,
          }
        );

        setArtifactTargetRoot(targetDir);

        setArtifactDownloads((prev) => {
          const next = [
            summary,
            ...prev.filter((item) => item.jobId !== summary.jobId),
          ];
          return next.slice(0, 10);
        });

        setJobs((prev) =>
          prev.map((item) =>
            item.jobId === job.jobId
              ? { ...item, artifactHash: summary.hash }
              : item
          )
        );

        if (!silent) {
          const warningNote =
            summary.warnings.length > 0 ? `；注意：${summary.warnings[0]}` : '';
          setJobStatus(
            `ZIP 下载完成（${summary.jobId}），共 ${summary.fileCount} 张，输出：${summary.archivePath}${warningNote}`
          );
          if (summary.warnings.length > 0) {
            setArtifactError(summary.warnings[0]);
          }
        }

        return true;
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        if (!silent) {
          setArtifactError(message);
        }
        return false;
      } finally {
        setArtifactDownloadBusyJob(null);
      }
    },
    [
      assessManualReadiness,
      artifactTargetRoot,
      buildJobRequest,
      promptForDirectory,
    ]
  );

  const validateArtifact = useCallback(
    async (
      job: JobRecord,
      options?: {
        silent?: boolean;
        targetDir?: string | null;
        manifestPathOverride?: string | null;
      }
    ) => {
      const readiness = assessManualReadiness();
      if (!readiness.ok) {
        if (!options?.silent) {
          setArtifactError(readiness.reason);
        }
        return false;
      }

      const silent = options?.silent ?? false;

      if (!job.serviceUrl || !job.jobId) {
        if (!silent) {
          setArtifactError('缺少必要信息，无法校验产物。');
        }
        return false;
      }

      if (!job.artifactPath) {
        if (!silent) {
          setArtifactError('远端尚未提供产物路径。');
        }
        return false;
      }

      let targetDir = options?.targetDir ?? null;
      if (!targetDir) {
        const picked = await promptForDirectory(
          '选择校验输出目录',
          artifactTargetRoot
        );
        if (!picked) {
          if (!silent) {
            setArtifactError('已取消选择输出目录。');
          }
          return false;
        }
        targetDir = picked;
      }

      let manifestPath =
        options?.manifestPathOverride ??
        job.manifestPath ??
        inferManifestForVolume(job.metadata?.volume ?? null);

      if (!manifestPath && !silent) {
        manifestPath = await promptForManifest();
      }

      setArtifactValidateBusyJob(job.jobId);
      if (!silent) {
        setArtifactError(null);
      }

      try {
        const request: Record<string, unknown> = {
          ...buildJobRequest(job),
          targetDir,
        };

        if (manifestPath) {
          request.manifestPath = manifestPath;
        }
        if (job.artifactHash) {
          request.expectedHash = job.artifactHash;
        }

        const report = await invoke<ArtifactReport>('validate_manga_artifact', {
          request,
        });

        setArtifactReports((prev) => {
          const next = [
            report,
            ...prev.filter((item) => item.jobId !== report.jobId),
          ];
          return next.slice(0, 10);
        });

        setArtifactTargetRoot(targetDir);

        setJobs((prev) =>
          prev.map((item) => {
            if (item.jobId !== job.jobId) {
              return item;
            }
            return {
              ...item,
              manifestPath: manifestPath ?? item.manifestPath,
              artifactHash: report.hash,
            };
          })
        );

        if (!silent) {
          const warningNote =
            report.warnings.length > 0 ? `；注意：${report.warnings[0]}` : '';
          setJobStatus(
            `校验完成（${report.jobId}）：匹配 ${report.summary.matched} / ${report.summary.totalManifest}${warningNote}`
          );
          if (report.warnings.length > 0) {
            setArtifactError(report.warnings[0]);
          }
        }

        return true;
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        if (!silent) {
          setArtifactError(message);
        }
        return false;
      } finally {
        setArtifactValidateBusyJob(null);
      }
    },
    [
      assessManualReadiness,
      artifactTargetRoot,
      buildJobRequest,
      inferManifestForVolume,
      promptForDirectory,
      promptForManifest,
    ]
  );

  const handleDownloadArtifact = useCallback(
    (job: JobRecord) => {
      void downloadArtifactZip(job);
    },
    [downloadArtifactZip]
  );

  const handleValidateArtifact = useCallback(
    (job: JobRecord) => {
      void validateArtifact(job);
    },
    [validateArtifact]
  );

  const handleBatchResume = useCallback(async () => {
    const targets = jobs.filter((job) => selectedJobIds.includes(job.jobId));
    for (const job of targets) {
      await resumeJob(job, { silent: true });
    }
    setJobStatus(`已尝试恢复 ${targets.length} 项作业。`);
  }, [jobs, resumeJob, selectedJobIds]);

  const handleBatchCancel = useCallback(async () => {
    const targets = jobs.filter((job) => selectedJobIds.includes(job.jobId));
    for (const job of targets) {
      await cancelJob(job, { silent: true });
    }
    setJobStatus(`已发送终止请求 ${targets.length} 项作业。`);
  }, [cancelJob, jobs, selectedJobIds]);

  const handleBatchDownload = useCallback(async () => {
    const selected = jobs.filter((job) => selectedJobIds.includes(job.jobId));
    const ready = selected.filter((job) => !!job.artifactPath);

    if (ready.length === 0) {
      setJobStatus('选中的作业暂无可下载的产物。');
      return;
    }

    let targetDir = artifactTargetRoot ?? null;
    if (!targetDir) {
      const picked = await promptForDirectory(
        '选择批量下载目录',
        artifactTargetRoot
      );
      if (!picked) {
        return;
      }
      targetDir = picked;
    }

    let success = 0;
    for (const job of ready) {
      const ok = await downloadArtifactZip(job, { silent: true, targetDir });
      if (ok) {
        success += 1;
      }
    }

    if (success === 0) {
      setJobStatus('批量下载未成功，请检查作业状态。');
      return;
    }

    const skipped = selected.length - ready.length;
    const summaryParts = [`已触发 ${success} 项 ZIP 下载。`];
    if (skipped > 0) {
      summaryParts.push(`跳过 ${skipped} 项尚未生成产物的作业。`);
    }
    setJobStatus(summaryParts.join(' '));
  }, [
    artifactTargetRoot,
    downloadArtifactZip,
    jobs,
    promptForDirectory,
    selectedJobIds,
  ]);

  const handleBatchValidate = useCallback(async () => {
    const selected = jobs.filter((job) => selectedJobIds.includes(job.jobId));
    const ready = selected.filter((job) => !!job.artifactPath);

    if (ready.length === 0) {
      setJobStatus('选中的作业暂无可校验的产物。');
      return;
    }

    let targetDir = artifactTargetRoot ?? null;
    if (!targetDir) {
      const picked = await promptForDirectory(
        '选择批量校验目录',
        artifactTargetRoot
      );
      if (!picked) {
        return;
      }
      targetDir = picked;
    }

    let success = 0;
    for (const job of ready) {
      const ok = await validateArtifact(job, { silent: true, targetDir });
      if (ok) {
        success += 1;
      }
    }

    if (success === 0) {
      setJobStatus('批量校验未成功，请检查作业状态。');
      return;
    }

    const skipped = selected.length - ready.length;
    const summaryParts = [`已触发 ${success} 项校验。`];
    if (skipped > 0) {
      summaryParts.push(`跳过 ${skipped} 项尚未生成产物的作业。`);
    }
    setJobStatus(summaryParts.join(' '));
  }, [
    artifactTargetRoot,
    jobs,
    promptForDirectory,
    selectedJobIds,
    validateArtifact,
  ]);

  const handleUpload = useCallback(async () => {
    if (!renameForm.directory) {
      setUploadError('请先完成重命名预览或执行，确保本地目录已确认。');
      return;
    }
    const readiness = assessManualReadiness();
    if (!readiness.ok) {
      setUploadError(readiness.reason);
      setUploadStatus(null);
      return;
    }
    const serviceUrl = uploadForm.serviceUrl.trim();
    if (!serviceUrl) {
      setUploadError('请填写 Copyparty 服务地址。');
      return;
    }

    setUploadLoading(true);
    setUploadError(null);
    setUploadStatus(null);

    try {
      const metadataEntries: Record<string, string> = {};

      const trimmedTitle = uploadForm.title.trim();
      const trimmedVolume = uploadForm.volume.trim();
      const remotePath = remotePathPreview;

      if (trimmedTitle) {
        metadataEntries.title = trimmedTitle;
      }
      if (trimmedVolume) {
        metadataEntries.volume = trimmedVolume;
      }

      const requiresManualOverrides =
        !isMultiVolumeSource &&
        renameSummary?.mode === 'single' &&
        Boolean(renameSummary.outcome.splitManualOverrides);

      const manualWorkspaceReady =
        !isMultiVolumeSource &&
        manualWorkspace &&
        manualWorkspace.trim().length > 0 &&
        manualDraftTotal > 0 &&
        manualAppliedCount >= manualDraftTotal &&
        Boolean(manualReportPath && manualReportPath.trim().length > 0);

      const manualWorkspaceCandidate =
        manualWorkspaceReady ||
        (requiresManualOverrides &&
          manualWorkspace &&
          manualWorkspace.trim().length > 0)
          ? manualWorkspace
          : null;

      let localPath = renameForm.directory;
      let splitSourceAnchor: string | null = null;

      if (isMultiVolumeSource) {
        if (!selectedVolume) {
          setUploadError('请选择要上传的卷，并在卷映射步骤中确认。');
          return;
        }

        if (
          !renameSummary ||
          renameSummary.mode !== 'multi' ||
          !renameSummary.volumes.some(
            (item) => item.mapping.directory === selectedVolume.directory
          )
        ) {
          setUploadError('请先对所选卷执行重命名预览或执行，生成 manifest。');
          return;
        }

        localPath = selectedVolume.directory;
        splitSourceAnchor = selectedVolume.directory;
      } else if (renameSummary?.mode === 'single') {
        const outcome = renameSummary.outcome;
        splitSourceAnchor = outcome.sourceDirectory ?? renameForm.directory;

        if (outcome.splitApplied) {
          const workspaceCandidate = outcome.splitWorkspace ?? splitWorkspace;
          if (workspaceCandidate) {
            localPath = workspaceCandidate;
          } else {
            localPath = outcome.directory;
          }
        } else {
          localPath = outcome.directory;
        }
      } else {
        splitSourceAnchor = renameForm.directory;
      }

      if (manualWorkspaceCandidate) {
        localPath = manualWorkspaceCandidate;
      } else if (
        !isMultiVolumeSource &&
        splitWorkspace &&
        splitSourceRoot &&
        splitSourceAnchor &&
        splitSourceRoot === splitSourceAnchor
      ) {
        localPath = splitWorkspace;
      }

      const request: Record<string, unknown> = {
        serviceUrl,
        remotePath,
        localPath,
        mode: uploadForm.mode,
      };

      const trimmedToken = uploadForm.bearerToken.trim();
      if (trimmedToken) {
        request.bearerToken = trimmedToken;
      }

      if (Object.keys(metadataEntries).length > 0) {
        request.metadata = metadataEntries;
      }

      const result = await invoke<UploadOutcome>('upload_copyparty', {
        request,
      });

      const sizeInMb = (result.uploadedBytes / (1024 * 1024)).toFixed(2);
      setLastUploadRemotePath(remotePath);
      setJobForm((prev) => ({
        ...prev,
        serviceUrl: prev.serviceUrl || serviceUrl,
        bearerToken: trimmedToken || prev.bearerToken,
        title: trimmedTitle || prev.title,
        volume: trimmedVolume || prev.volume,
        inputPath: remotePath,
      }));
      setUploadStatus(
        `上传完成：${result.fileCount} 个文件，约 ${sizeInMb} MB，remote = ${result.remoteUrl}`
      );
      setRemotePathSeed(Date.now());
    } catch (error) {
      setUploadError(error instanceof Error ? error.message : String(error));
    } finally {
      setUploadLoading(false);
    }
  }, [
    isMultiVolumeSource,
    remotePathPreview,
    renameForm.directory,
    renameSummary,
    selectedVolume,
    splitSourceRoot,
    splitWorkspace,
    uploadForm,
    manualWorkspace,
    manualAppliedCount,
    manualDraftTotal,
    manualReportPath,
    assessManualReadiness,
  ]);

  const beginAddJobService = useCallback(() => {
    setJobAddressError(null);
    setJobAddressDraft(jobForm.serviceUrl.trim() || '');
    setIsAddingJobService(true);
  }, [jobForm.serviceUrl]);

  const cancelAddJobService = useCallback(() => {
    setIsAddingJobService(false);
    setJobAddressDraft('');
    setJobAddressError(null);
  }, []);

  const confirmAddJobService = useCallback(() => {
    const trimmed = jobAddressDraft.trim();
    if (!trimmed) {
      setJobAddressError('请输入有效的推理服务器地址。');
      return;
    }

    setJobAddressError(null);
    setJobServiceOptions((prev) => {
      if (prev.includes(trimmed)) {
        return prev;
      }
      return [...prev, trimmed];
    });
    setJobForm((prev) => ({ ...prev, serviceUrl: trimmed }));
    setJobAddressDraft('');
    setIsAddingJobService(false);
  }, [jobAddressDraft]);

  const handleJobInput = useCallback(
    (field: keyof JobFormState) =>
      (event: ChangeEvent<HTMLInputElement | HTMLSelectElement>) => {
        const value = event.currentTarget.value;
        setJobForm((prev) => {
          if (field === 'pollIntervalMs') {
            const numeric = Number(value);
            const normalized = Number.isFinite(numeric)
              ? Math.max(250, Math.floor(numeric))
              : DEFAULT_POLL_INTERVAL;
            return { ...prev, pollIntervalMs: normalized };
          }
          if (field === 'inputType') {
            return { ...prev, inputType: value as JobFormState['inputType'] };
          }
          return { ...prev, [field]: value } as JobFormState;
        });
      },
    []
  );

  const stepDescriptors = useMemo(() => {
    const steps: StepDescriptor[] = [{ id: 'source', label: '选择源目录' }];
    if (isMultiVolumeSource) {
      steps.push({ id: 'volumes', label: '卷映射确认' });
    }
    steps.push(
      { id: 'rename', label: '图片重命名' },
      { id: 'split', label: '拆分与裁剪（可选）' },
      { id: 'upload', label: '上传到 Copyparty' },
      { id: 'jobs', label: '远端推理' }
    );
    return steps;
  }, [isMultiVolumeSource]);

  const stepOrder = useMemo(
    () => stepDescriptors.map((item) => item.id),
    [stepDescriptors]
  );

  const stepIndexMap = useMemo(() => {
    const mapping = new Map<StepId, number>();
    stepOrder.forEach((id, index) => {
      mapping.set(id, index + 1);
    });
    return mapping;
  }, [stepOrder]);

  useEffect(() => {
    if (!stepOrder.includes(currentStep)) {
      setCurrentStep(stepOrder[0] ?? 'source');
    }
  }, [currentStep, stepOrder]);

  const goToNextStep = useCallback(() => {
    const index = stepOrder.indexOf(currentStep);
    if (index === -1) {
      return;
    }
    const next = stepOrder[index + 1];
    if (next) {
      setCurrentStep(next);
    }
  }, [currentStep, stepOrder]);

  const goToPreviousStep = useCallback(() => {
    const index = stepOrder.indexOf(currentStep);
    if (index <= 0) {
      return;
    }
    const previous = stepOrder[index - 1];
    if (previous) {
      setCurrentStep(previous);
    }
  }, [currentStep, stepOrder]);

  const handleSelectVolume = useCallback((directory: string) => {
    setSelectedVolumeKey(directory || null);
  }, []);

  const handleVolumeNumberChange = useCallback(
    (index: number) => (event: ChangeEvent<HTMLInputElement>) => {
      const rawValue = event.currentTarget.value;
      const numeric = Number(rawValue);
      const normalized =
        rawValue.trim() === '' || !Number.isFinite(numeric)
          ? null
          : Math.max(1, Math.floor(numeric));

      setVolumeMappings((prev) => {
        const next = [...prev];
        next[index] = { ...next[index], volumeNumber: normalized };
        return next;
      });

      setMappingConfirmed(false);
      setRenameSummary(null);
      setVolumeMappingError(null);
    },
    []
  );

  const handleVolumeNameChange = useCallback(
    (index: number) => (event: ChangeEvent<HTMLInputElement>) => {
      const value = event.currentTarget.value;
      setVolumeMappings((prev) => {
        const next = [...prev];
        next[index] = { ...next[index], volumeName: value };
        return next;
      });

      setMappingConfirmed(false);
      setRenameSummary(null);
      setVolumeMappingError(null);
    },
    []
  );

  const handleConfirmMapping = useCallback(() => {
    if (!isMultiVolumeSource) {
      setMappingConfirmed(true);
      setVolumeMappingError(null);
      goToNextStep();
      return;
    }

    if (volumeMappings.length === 0) {
      setVolumeMappingError('未检测到任何卷目录，请返回上一步检查源目录。');
      return;
    }

    const missingNumber = volumeMappings.some(
      (item) => item.volumeNumber === null
    );
    if (missingNumber) {
      setVolumeMappingError('请为每一卷填写卷号。');
      return;
    }

    const seen = new Set<number>();
    for (const mapping of volumeMappings) {
      if (mapping.volumeNumber === null) {
        continue;
      }
      if (seen.has(mapping.volumeNumber)) {
        setVolumeMappingError('卷号不能重复，请调整后再确认。');
        return;
      }
      seen.add(mapping.volumeNumber);
    }

    setVolumeMappingError(null);
    setMappingConfirmed(true);
    goToNextStep();
  }, [goToNextStep, isMultiVolumeSource, volumeMappings]);

  const applyUploadContext = useCallback(() => {
    const trimmedService = uploadForm.serviceUrl.trim();
    const trimmedToken = uploadForm.bearerToken.trim();
    const trimmedTitle = uploadForm.title.trim();
    const trimmedVolume = uploadForm.volume.trim();
    const trimmedPath = lastUploadRemotePath.trim();

    if (trimmedService) {
      setJobServiceOptions((prev) => {
        if (prev.includes(trimmedService)) {
          return prev;
        }
        return [...prev, trimmedService];
      });
    }

    setJobForm((prev) => ({
      ...prev,
      serviceUrl: prev.serviceUrl || trimmedService,
      bearerToken: trimmedToken || prev.bearerToken,
      title: trimmedTitle || prev.title,
      volume: trimmedVolume || prev.volume,
      inputPath: trimmedPath || prev.inputPath,
    }));
  }, [lastUploadRemotePath, uploadForm]);

  const handleCreateJob = useCallback(async () => {
    const serviceUrl = jobForm.serviceUrl.trim();
    const title = jobForm.title.trim();
    const volume = jobForm.volume.trim();
    const inputPath = jobForm.inputPath.trim();
    const bearer = jobForm.bearerToken.trim();

    const readiness = assessManualReadiness();
    if (!readiness.ok) {
      setJobError(readiness.reason);
      return;
    }

    if (!serviceUrl) {
      setJobError('请填写推理服务地址。');
      return;
    }
    if (!title) {
      setJobError('请填写作品名，以便在远端区分作业。');
      return;
    }
    if (!volume) {
      setJobError('请填写卷名，或至少提供批次标识。');
      return;
    }
    if (!inputPath) {
      setJobError('请提供远端输入路径（例如 incoming/volume.zip）。');
      return;
    }

    setJobLoading(true);
    setJobError(null);
    setJobStatus(null);

    try {
      const paramsPayload: JobParamsConfig = {
        scale: jobParams.scale,
        model: jobParams.model,
        denoise: jobParams.denoise,
        outputFormat: jobParams.outputFormat,
        jpegQuality: jobParams.jpegQuality,
        tileSize: jobParams.tileSize,
        tilePad: jobParams.tilePad,
        batchSize: jobParams.batchSize,
        device: jobParams.device,
      };

      const options = {
        serviceUrl,
        bearerToken: bearer || undefined,
        payload: {
          title,
          volume,
          input: {
            type: jobForm.inputType,
            path: inputPath,
          },
          params: paramsPayload,
        },
      };

      const submission = await invoke<JobSubmission>('create_manga_job', {
        options,
      });

      const initialRecord: JobRecord = {
        jobId: submission.jobId,
        status: 'PENDING',
        processed: 0,
        total: 0,
        artifactPath: null,
        message: '作业已提交，等待远端处理。',
        transport: 'system',
        error: null,
        retries: 0,
        lastError: null,
        artifactHash: null,
        params: paramsPayload,
        metadata: {
          title,
          volume,
        },
        serviceUrl,
        bearerToken: bearer || null,
        inputPath,
        inputType: jobForm.inputType,
        manifestPath: inferManifestForVolume(volume),
        lastUpdated: Date.now(),
      };

      setJobs((prev) => [
        initialRecord,
        ...prev.filter((item) => item.jobId !== initialRecord.jobId),
      ]);
      setJobStatus(`作业 ${submission.jobId} 已创建，正在等待进度更新。`);

      void startJobWatcher(initialRecord);
    } catch (error) {
      setJobError(error instanceof Error ? error.message : String(error));
    } finally {
      setJobLoading(false);
    }
  }, [
    assessManualReadiness,
    inferManifestForVolume,
    jobForm,
    jobParams,
    startJobWatcher,
  ]);

  const describeStatus = (status: string) => {
    switch (status.toUpperCase()) {
      case 'PENDING':
        return '排队中';
      case 'RUNNING':
        return '运行中';
      case 'SUCCESS':
        return '已完成';
      case 'FAILED':
        return '失败';
      case 'ERROR':
        return '错误';
      default:
        return status;
    }
  };

  const statusTone = (status: string) => {
    switch (status.toUpperCase()) {
      case 'SUCCESS':
        return 'success';
      case 'FAILED':
      case 'ERROR':
        return 'error';
      case 'RUNNING':
        return 'info';
      default:
        return 'neutral';
    }
  };

  const describeTransport = (transport: JobEventTransport) => {
    switch (transport) {
      case 'websocket':
        return 'WebSocket';
      case 'polling':
        return '轮询';
      case 'system':
      default:
        return '系统';
    }
  };

  const describeUploadStage = (progress: UploadProgressPayload) => {
    switch (progress.stage) {
      case 'preparing':
        return '准备中';
      case 'uploading':
        return '上传中';
      case 'finalizing':
        return '确认中';
      case 'completed':
        return '完成';
      case 'failed':
        return '失败';
      default:
        return progress.stage;
    }
  };

  const uploadPercent = useMemo(() => {
    if (!uploadProgress) {
      return null;
    }
    if (uploadProgress.stage === 'preparing' && uploadProgress.totalFiles > 0) {
      return Math.round(
        (uploadProgress.processedFiles / uploadProgress.totalFiles) * 100
      );
    }
    if (uploadProgress.stage === 'uploading' && uploadProgress.totalBytes > 0) {
      return Math.round(
        (uploadProgress.transferredBytes / uploadProgress.totalBytes) * 100
      );
    }
    if (uploadProgress.stage === 'completed') {
      return 100;
    }
    return null;
  }, [uploadProgress]);

  return (
    <div className="manga-agent">
      <div className="stepper-nav" role="presentation">
        {stepDescriptors.map((descriptor) => {
          const index = stepOrder.indexOf(descriptor.id);
          const currentIndex = stepOrder.indexOf(currentStep);
          const status =
            descriptor.id === currentStep
              ? 'active'
              : index !== -1 && currentIndex !== -1 && index < currentIndex
              ? 'completed'
              : '';
          const stepNumber = stepIndexMap.get(descriptor.id) ?? index + 1;
          return (
            <div key={descriptor.id} className={`stepper-nav-item ${status}`}>
              <span className="step-index">步骤 {stepNumber}</span>
              <span className="step-label">{descriptor.label}</span>
            </div>
          );
        })}
      </div>

      {currentStep === 'source' && (
        <section className="step-card" aria-label="选择源目录">
          <header className="step-card-header">
            <span className="step-index">
              步骤 {stepIndexMap.get('source') ?? 1}
            </span>
            <h3>选择源目录</h3>
            <p>选取原始漫画文件夹并进行结构分析，识别单卷或多卷场景。</p>
          </header>

          <div className="form-grid">
            <label className="form-field">
              <span className="field-label">漫画根目录</span>
              <div className="field-row">
                <input
                  type="text"
                  value={renameForm.directory}
                  onChange={handleRenameInput('directory')}
                  placeholder="/path/to/folder"
                />
                <button type="button" onClick={handleSelectDirectory}>
                  选择
                </button>
                <button type="button" onClick={handleRefreshAnalysis}>
                  分析
                </button>
              </div>
            </label>
          </div>

          {analysisLoading && <p className="status">正在扫描目录…</p>}
          {analysisError && (
            <p className="status status-error">{analysisError}</p>
          )}

          {sourceAnalysis && (
            <div className="analysis-panel">
              <p>
                检测结果：
                {sourceAnalysis.mode === 'multiVolume'
                  ? '多卷目录'
                  : '单卷目录'}
                ； 根目录图片 {sourceAnalysis.rootImageCount} 张，总计{' '}
                {sourceAnalysis.totalImages} 张。
              </p>

              {sourceAnalysis.mode === 'multiVolume' &&
                sourceAnalysis.volumeCandidates.length > 0 && (
                  <table className="analysis-table">
                    <thead>
                      <tr>
                        <th>子目录</th>
                        <th>图片数</th>
                        <th>推测卷号</th>
                      </tr>
                    </thead>
                    <tbody>
                      {sourceAnalysis.volumeCandidates.map((candidate) => (
                        <tr key={candidate.directory}>
                          <td>{candidate.folderName}</td>
                          <td>{candidate.imageCount}</td>
                          <td>{candidate.detectedNumber ?? '-'}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                )}

              {sourceAnalysis.skippedEntries.length > 0 && (
                <details>
                  <summary>
                    忽略的条目 ({sourceAnalysis.skippedEntries.length})
                  </summary>
                  <ul>
                    {sourceAnalysis.skippedEntries.slice(0, 10).map((entry) => (
                      <li key={entry}>{entry}</li>
                    ))}
                    {sourceAnalysis.skippedEntries.length > 10 && (
                      <li>
                        其余 {sourceAnalysis.skippedEntries.length - 10} 条略。
                      </li>
                    )}
                  </ul>
                </details>
              )}
            </div>
          )}

          <div className="wizard-controls">
            <button
              type="button"
              className="primary"
              onClick={goToNextStep}
              disabled={!sourceAnalysis || analysisLoading}
            >
              下一步
            </button>
          </div>
        </section>
      )}

      {currentStep === 'volumes' && isMultiVolumeSource && (
        <section className="step-card" aria-label="卷映射确认">
          <header className="step-card-header">
            <span className="step-index">
              步骤 {stepIndexMap.get('volumes') ?? 2}
            </span>
            <h3>卷映射确认</h3>
            <p>为每个子目录指定卷号与显示名称，确认后将用于重命名和上传。</p>
          </header>

          {volumeMappings.length === 0 ? (
            <p className="status">未检测到卷目录，请返回检查源目录。</p>
          ) : (
            <table className="mapping-table">
              <thead>
                <tr>
                  <th>选择</th>
                  <th>子目录</th>
                  <th>图片数</th>
                  <th>卷号</th>
                  <th>卷名</th>
                </tr>
              </thead>
              <tbody>
                {volumeMappings.map((mapping, index) => (
                  <tr key={mapping.directory}>
                    <td>
                      <input
                        type="radio"
                        name="active-volume"
                        value={mapping.directory}
                        checked={selectedVolumeKey === mapping.directory}
                        onChange={() => handleSelectVolume(mapping.directory)}
                      />
                    </td>
                    <td>{mapping.folderName}</td>
                    <td>{mapping.imageCount}</td>
                    <td>
                      <input
                        type="number"
                        min={1}
                        value={mapping.volumeNumber ?? ''}
                        onChange={handleVolumeNumberChange(index)}
                      />
                    </td>
                    <td>
                      <input
                        type="text"
                        value={mapping.volumeName}
                        onChange={handleVolumeNameChange(index)}
                      />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          {volumeMappingError && (
            <p className="status status-error">{volumeMappingError}</p>
          )}

          <div className="button-row">
            <button
              type="button"
              className="primary"
              onClick={handleConfirmMapping}
            >
              确认映射并继续
            </button>
          </div>
        </section>
      )}

      {currentStep === 'rename' && (
        <section className="step-card" aria-label="图片重命名">
          <header className="step-card-header">
            <span className="step-index">
              步骤 {stepIndexMap.get('rename') ?? 1}
            </span>
            <h3>图片重命名与预览</h3>
            <p>根据卷映射批量重命名图片并生成 manifest 映射。</p>
          </header>

          <div className="form-grid">
            <label className="form-field compact">
              <span className="field-label">位数</span>
              <input
                type="number"
                min={1}
                value={renameForm.pad}
                onChange={handleRenameInput('pad')}
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">目标扩展名</span>
              <input
                type="text"
                value={renameForm.targetExtension}
                onChange={handleRenameInput('targetExtension')}
                maxLength={8}
              />
            </label>
          </div>

          <p className="status status-tip">
            拆分与自定义操作已迁移到下一步“拆分与裁剪（可选）”。
          </p>

          <div className="button-row">
            <button
              type="button"
              className="primary"
              onClick={() => runRename(true)}
              disabled={renameLoading}
            >
              {renameLoading ? '处理中…' : '预览重命名'}
            </button>

            <button
              type="button"
              onClick={() => runRename(false)}
              disabled={renameLoading}
            >
              {renameLoading ? '处理中…' : '执行重命名'}
            </button>
          </div>

          {renameError && <p className="status status-error">{renameError}</p>}

          {renameSummary && renameSummary.mode === 'single' && (
            <div className="preview-panel">
              <div className="preview-header">
                <strong>
                  {renameSummary.outcome.entries.length} 个文件
                  {renameSummary.outcome.dryRun ? '（预览）' : '（已重命名）'}
                </strong>
                {renameSummary.outcome.manifestPath && (
                  <span className="manifest-path">
                    manifest: {renameSummary.outcome.manifestPath}
                  </span>
                )}
              </div>

              {renameSummary.outcome.warnings.length > 0 && (
                <ul className="status status-warning">
                  {renameSummary.outcome.warnings.map((warning) => (
                    <li key={warning}>{warning}</li>
                  ))}
                </ul>
              )}

              {renameSummary.outcome.splitApplied &&
                renameSummary.outcome.splitSummary && (
                  <p className="status status-tip">
                    内容感知拆分：输出{' '}
                    {renameSummary.outcome.splitSummary.emittedFiles} 文件，
                    拆分 {renameSummary.outcome.splitSummary.splitPages} 页。
                    {renameSummary.outcome.splitWorkspace && (
                      <span>
                        {' '}
                        工作目录：{renameSummary.outcome.splitWorkspace}
                      </span>
                    )}
                  </p>
                )}

              {renameSummary.outcome.splitManualOverrides && (
                <p className="status status-tip">
                  自定义拆分：
                  {renameSummary.outcome.manualEntries?.length
                    ? `记录 ${renameSummary.outcome.manualEntries.length} 条手动拆分。`
                    : '存在手动拆分记录。'}
                </p>
              )}

              <table className="preview-table">
                <thead>
                  <tr>
                    <th>原始文件</th>
                    <th>重命名结果</th>
                  </tr>
                </thead>
                <tbody>
                  {previewEntries.map((entry) => (
                    <tr key={`${entry.originalName}-${entry.renamedName}`}>
                      <td>{entry.originalName}</td>
                      <td>{entry.renamedName}</td>
                    </tr>
                  ))}
                </tbody>
              </table>

              {renameSummary.outcome.entries.length > previewEntries.length && (
                <p className="preview-more">
                  其余{' '}
                  {renameSummary.outcome.entries.length - previewEntries.length}{' '}
                  条略。
                </p>
              )}
            </div>
          )}

          {renameSummary && renameSummary.mode === 'multi' && (
            <div className="preview-panel multi">
              {renameSummary.volumes.map(({ mapping, outcome }) => (
                <div key={mapping.directory} className="volume-preview">
                  <header>
                    <strong>
                      卷 {mapping.volumeNumber}: {mapping.volumeName}
                    </strong>
                    <span>
                      {outcome.entries.length} 个文件
                      {renameSummary.dryRun ? '（预览）' : '（已重命名）'}
                    </span>
                    {outcome.manifestPath && (
                      <span className="manifest-path">
                        manifest: {outcome.manifestPath}
                      </span>
                    )}
                    {outcome.splitManualOverrides && (
                      <span className="status status-tip">
                        自定义拆分：
                        {outcome.manualEntries?.length
                          ? `记录 ${outcome.manualEntries.length} 条`
                          : '存在手动拆分'}
                      </span>
                    )}
                  </header>

                  {outcome.warnings.length > 0 && (
                    <ul className="status status-warning">
                      {outcome.warnings.map((warning) => (
                        <li key={warning}>{warning}</li>
                      ))}
                    </ul>
                  )}

                  <table className="preview-table">
                    <thead>
                      <tr>
                        <th>原始文件</th>
                        <th>重命名结果</th>
                      </tr>
                    </thead>
                    <tbody>
                      {outcome.entries.slice(0, 5).map((entry) => (
                        <tr
                          key={`${mapping.directory}-${entry.originalName}-${entry.renamedName}`}
                        >
                          <td>{entry.originalName}</td>
                          <td>{entry.renamedName}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>

                  {outcome.entries.length > 5 && (
                    <p className="preview-more">
                      其余 {outcome.entries.length - 5} 条略。
                    </p>
                  )}
                </div>
              ))}
            </div>
          )}

          <div className="wizard-controls">
            <button type="button" onClick={goToPreviousStep}>
              上一步
            </button>
            <button type="button" className="primary" onClick={goToNextStep}>
              下一步
            </button>
          </div>

        </section>
      )}

      {currentStep === 'split' && (
        <section className="step-card" aria-label="拆分与裁剪">
          <header className="step-card-header">
            <span className="step-index">
              步骤 {stepIndexMap.get('split') ?? 1}
            </span>
            <h3>拆分与裁剪（可选）</h3>
            <p>生成拆分工作区并按需调整手动裁剪，未使用时可直接跳过。</p>
          </header>

          <div className="split-controls">
            <div className="split-toggle-row">
              <label className="form-field checkbox">
                <input
                  type="checkbox"
                  checked={contentSplitEnabled}
                  onChange={handleToggleSplit}
                  disabled={isMultiVolumeSource}
                />
                <span>内容感知双页拆分（实验性）</span>
              </label>

              {contentSplitEnabled &&
                !isMultiVolumeSource &&
                splitAlgorithm !== 'manual' && (
                  <button
                    type="button"
                    className="split-action-button"
                    onClick={handlePrepareSplit}
                    disabled={splitPreparing}
                >
                  {splitPreparing ? '准备中…' : '开始内容拆分'}
                </button>
              )}
            </div>

            {contentSplitEnabled && !isMultiVolumeSource && (
              <div className="split-settings-row">
                <label className="form-field compact">
                  <span className="field-label">拆分算法</span>
                  <select
                    value={splitAlgorithm}
                    onChange={handleSplitAlgorithmChange}
                  >
                    <option value="edgeTexture">Edge Texture（推荐）</option>
                    <option value="projection">传统投影</option>
                    <option value="manual">手动（自定义）</option>
                  </select>
                </label>

                {splitAlgorithm === 'manual' ? (
                  <ManualSplitIntro
                    initializing={manualInitializing}
                    loadingDrafts={manualLoadingDrafts}
                    statusText={manualStatusText}
                    error={manualControllerError}
                    workspace={manualWorkspace}
                    disableInitialize={manualDisableInitialize}
                    disableReason={manualDisableReason}
                    totalDrafts={manualDraftTotal}
                    appliedDrafts={manualAppliedCount}
                    lastAppliedAt={manualLastAppliedAt}
                    onInitialize={() => {
                      void initializeManualWorkspace(Boolean(manualWorkspace));
                    }}
                    onOpenExisting={() => {
                      void openManualWorkspace();
                    }}
                  />
                ) : null}

                {splitAlgorithm === 'edgeTexture' && (
                  <div className="edge-threshold-controls">
                    <label className="form-field compact">
                      <span className="field-label">亮白阈值</span>
                      <input
                        type="number"
                        min={0}
                        max={255}
                        value={edgeBrightnessThresholds[0]}
                        onChange={handleEdgeThresholdChange(0)}
                      />
                      {edgeThresholdErrors.bright && (
                        <span className="field-error">
                          {edgeThresholdErrors.bright}
                        </span>
                      )}
                    </label>

                    <label className="form-field compact">
                      <span className="field-label">留黑阈值</span>
                      <input
                        type="number"
                        min={0}
                        max={255}
                        value={edgeBrightnessThresholds[1]}
                        onChange={handleEdgeThresholdChange(1)}
                      />
                      {edgeThresholdErrors.dark && (
                        <span className="field-error">
                          {edgeThresholdErrors.dark}
                        </span>
                      )}
                    </label>

                    <label className="form-field compact">
                      <span className="field-label">左侧搜索比例</span>
                      <input
                        type="number"
                        min={EDGE_SEARCH_RATIO_MIN}
                        max={EDGE_SEARCH_RATIO_MAX}
                        step={0.01}
                        value={edgeSearchRatios[0]}
                        onChange={handleEdgeSearchRatioChange(0)}
                      />
                      {edgeSearchRatioErrors.left && (
                        <span className="field-error">
                          {edgeSearchRatioErrors.left}
                        </span>
                      )}
                    </label>

                    <label className="form-field compact">
                      <span className="field-label">右侧搜索比例</span>
                      <input
                        type="number"
                        min={EDGE_SEARCH_RATIO_MIN}
                        max={EDGE_SEARCH_RATIO_MAX}
                        step={0.01}
                        value={edgeSearchRatios[1]}
                        onChange={handleEdgeSearchRatioChange(1)}
                      />
                      {edgeSearchRatioErrors.right && (
                        <span className="field-error">
                          {edgeSearchRatioErrors.right}
                        </span>
                      )}
                    </label>

                    <label className="form-field compact">
                      <span className="field-label">加速器偏好</span>
                      <select
                        value={edgeAcceleratorPreference}
                        onChange={handleEdgeAcceleratorChange}
                      >
                        <option value="auto">自动</option>
                        <option value="gpu">优先 GPU</option>
                        <option value="cpu">仅 CPU</option>
                      </select>
                    </label>

                    <button
                      type="button"
                      className="split-action-button"
                      onClick={handleEdgePreview}
                      disabled={edgePreview.loading || hasEdgeInputError}
                    >
                      {edgePreview.loading ? '预览中…' : '预览阈值效果'}
                    </button>
                  </div>
                )}
              </div>
            )}

            {splitAlgorithm !== 'manual' && splitEstimate && (
              <p className="status status-tip">
                预估拆分候选：{splitEstimate.candidates} / {splitEstimate.total}
              </p>
            )}

            {isMultiVolumeSource && (
              <p className="status status-tip">
                多卷目录暂不支持内容感知拆分，请在单卷模式或逐卷操作中使用。
              </p>
            )}

            {contentSplitEnabled && !isMultiVolumeSource && (
              <div className="split-summary">
                {(splitPreparing || splitProgress) && (
                  <div
                    className="split-progress"
                    role="status"
                    aria-live="polite"
                  >
                    <div
                      className="split-progress-bar"
                      role="progressbar"
                      aria-valuenow={splitProgress ? splitProgressPercent : 0}
                      aria-valuemin={0}
                      aria-valuemax={100}
                    >
                      <span
                        style={{
                          width: `${splitProgress ? splitProgressPercent : 0}%`,
                        }}
                      />
                    </div>

                    <div className="split-progress-meta">
                      <span className="split-progress-status">
                        {splitProgressStatus ?? ''}
                      </span>
                      <span className="split-progress-percent">
                        {splitProgress ? `${splitProgressPercent}%` : '--%'}
                      </span>
                      {splitProgress?.totalFiles ? (
                        <span className="split-progress-count">
                          {splitProgress.processedFiles}/
                          {splitProgress.totalFiles}
                        </span>
                      ) : null}
                    </div>

                    {splitProgressFileName && (
                      <div
                        className="split-progress-file"
                        title={splitProgress?.currentFile ?? undefined}
                      >
                        {splitProgressFileName}
                      </div>
                    )}
                  </div>
                )}

                {splitWorkspace && (
                  <p className="status">
                    工作目录：{splitWorkspace}
                    {splitSummaryState && (
                      <span>
                        ，输出 {splitSummaryState.emittedFiles} 文件，拆分{' '}
                        {splitSummaryState.splitPages} 页
                      </span>
                    )}
                  </p>
                )}

                {splitReportPath && (
                  <p className="status status-tip">
                    拆分报告：{splitReportPath}
                  </p>
                )}

                {splitError && (
                  <p className="status status-error">{splitError}</p>
                )}

                {manualControllerError &&
                  splitAlgorithm !== 'manual' && (
                    <p className="status status-error">{manualControllerError}</p>
                  )}

                {splitWarningsState.length > 0 && (
                  <ul className="status status-warning">
                    {splitWarningsState.map((warning) => (
                      <li key={warning}>{warning}</li>
                    ))}
                  </ul>
                )}

                {manualCrossModeNotice && (
                  <p
                    className={`status ${
                      manualCrossModeNotice.tone === 'warning' ? 'status-warning' : 'status-tip'
                    }`}
                  >
                    {manualCrossModeNotice.message}
                  </p>
                )}

                {manualWorkspace && splitAlgorithm !== 'manual' && (
                  <div className="manual-split-controls">
                    <button
                      type="button"
                      className="primary"
                      onClick={handleOpenManualDrawer}
                      disabled={manualInitializing || manualLoadingDrafts}
                    >
                      打开手动拆分
                    </button>
                    <span className="split-preview-status">
                      {manualStatusText}
                    </span>
                  </div>
                )}
              </div>
            )}
          </div>

          <div className="wizard-controls">
            <button type="button" onClick={goToPreviousStep}>
              上一步
            </button>
            <button type="button" className="ghost" onClick={goToNextStep}>
              跳过拆分
            </button>
            <button type="button" className="primary" onClick={goToNextStep}>
              下一步
            </button>
          </div>

          <CustomSplitDrawer
            workspace={manualWorkspace}
            open={manualDrawerOpen}
            onClose={handleCloseManualDrawer}
          />
        </section>
      )}

      {currentStep === 'upload' && (
        <section className="step-card" aria-label="步骤 2 上传 Copyparty">
          <header className="step-card-header">
            <span className="step-index">
              步骤 {stepIndexMap.get('upload') ?? 1}
            </span>
            <h3>上传到 Copyparty</h3>
            <p>将整理后的目录打包为 zip 上传，并附带作品信息。</p>
          </header>

          <div className="form-grid">
            <label className="form-field">
              <span className="field-label">服务地址</span>
              <div className="select-with-button">
                <select
                  value={uploadForm.serviceUrl}
                  onChange={handleUploadInput('serviceUrl')}
                >
                  <option value="">选择或新增上传地址</option>
                  {uploadServiceOptions.map((option) => (
                    <option key={option} value={option}>
                      {option}
                    </option>
                  ))}
                </select>
                <button type="button" onClick={beginAddUploadService}>
                  添加
                </button>
              </div>
              {isAddingUploadService && (
                <div className="address-add-row">
                  <input
                    type="text"
                    value={uploadAddressDraft}
                    onChange={(event) => {
                      setUploadAddressDraft(event.currentTarget.value);
                      if (uploadAddressError) {
                        setUploadAddressError(null);
                      }
                    }}
                    placeholder="http://127.0.0.1:3923"
                    autoFocus
                  />
                  <button
                    type="button"
                    className="primary"
                    onClick={confirmAddUploadService}
                  >
                    保存
                  </button>
                  <button
                    type="button"
                    className="ghost"
                    onClick={cancelAddUploadService}
                  >
                    取消
                  </button>
                </div>
              )}
              {uploadAddressError && (
                <p className="field-error">{uploadAddressError}</p>
              )}
            </label>

            <label className="form-field">
              <span className="field-label">远端输入路径（自动生成）</span>
              <input
                type="text"
                value={remotePathPreview}
                readOnly
                aria-readonly="true"
                onFocus={(event) => event.currentTarget.select()}
              />
              <button type="button" onClick={handleRegenerateRemotePath}>
                重新生成
              </button>
              <small>
                无需手动填写，上传时会使用该路径并自动在远端推理步骤中填入。
              </small>
            </label>

            <label className="form-field">
              <span className="field-label">Bearer Token（可选）</span>
              <input
                type="text"
                value={uploadForm.bearerToken}
                onChange={handleUploadInput('bearerToken')}
                placeholder="xxxxxx"
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">作品名（可选）</span>
              <input
                type="text"
                value={uploadForm.title}
                onChange={handleUploadInput('title')}
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">卷名（可选）</span>
              <input
                type="text"
                value={uploadForm.volume}
                onChange={handleUploadInput('volume')}
              />
            </label>

            {isMultiVolumeSource && volumeMappings.length > 0 && (
              <label className="form-field compact">
                <span className="field-label">上传卷</span>
                <select
                  value={selectedVolumeKey ?? ''}
                  onChange={(event) =>
                    handleSelectVolume(event.currentTarget.value)
                  }
                >
                  {volumeMappings.map((mapping) => (
                    <option key={mapping.directory} value={mapping.directory}>
                      卷 {mapping.volumeNumber ?? '-'} · {mapping.volumeName}
                    </option>
                  ))}
                </select>
              </label>
            )}

            <label className="form-field compact">
              <span className="field-label">模式</span>
              <select
                value={uploadForm.mode}
                onChange={handleUploadInput('mode')}
              >
                <option value="zip">Zip</option>
                <option value="folder">Folder（规划中）</option>
              </select>
            </label>
          </div>

          <div className="button-row">
            <button
              type="button"
              className="primary"
              onClick={handleUpload}
              disabled={uploadLoading}
            >
              {uploadLoading ? '上传中…' : '上传到 Copyparty'}
            </button>
          </div>

          {uploadError && <p className="status status-error">{uploadError}</p>}
          {uploadStatus && (
            <p className="status status-success">{uploadStatus}</p>
          )}
          {uploadProgress && (
            <div className="upload-progress">
              <div className="upload-progress-header">
                <span
                  className={`status-chip status-${
                    uploadProgress.stage === 'failed'
                      ? 'error'
                      : uploadProgress.stage === 'completed'
                      ? 'success'
                      : 'info'
                  }`}
                >
                  {describeUploadStage(uploadProgress)}
                </span>
                {uploadPercent != null && (
                  <span className="progress-label">{uploadPercent}%</span>
                )}
              </div>
              <div className="progress-bar">
                <span
                  style={{
                    width: `${Math.max(0, Math.min(uploadPercent ?? 0, 100))}%`,
                  }}
                />
              </div>
              {(uploadProgress.message || uploadProgress.totalBytes > 0) && (
                <p className="progress-hint">
                  {uploadProgress.message ??
                    `${(
                      uploadProgress.transferredBytes /
                      (1024 * 1024)
                    ).toFixed(2)} / ${(
                      uploadProgress.totalBytes /
                      (1024 * 1024)
                    ).toFixed(2)} MB`}
                </p>
              )}
            </div>
          )}

          <div className="wizard-controls">
            <button type="button" onClick={goToPreviousStep}>
              上一步
            </button>
            <button type="button" className="primary" onClick={goToNextStep}>
              下一步
            </button>
          </div>
        </section>
      )}

      {currentStep === 'jobs' && (
        <section className="step-card" aria-label="远端推理">
          <header className="step-card-header">
            <span className="step-index">
              步骤 {stepIndexMap.get('jobs') ?? 1}
            </span>
            <h3>远端推理</h3>
            <p>提交 FastAPI 推理作业并实时查看进度。</p>
          </header>

          {manualReadinessWarning && (
            <p className="status status-warning">{manualReadinessWarning}</p>
          )}

          <div className="params-panel" aria-label="推理参数配置">
            <div className="params-panel-header">
              <h4>推理参数</h4>
              <p>调整模型、放大倍率与高级选项，配置将自动保存为默认值。</p>
            </div>

            <div className="params-grid">
              <label className="form-field">
                <span className="field-label">模型</span>
                <input
                  type="text"
                  value={jobParams.model}
                  onChange={handleParamChange('model')}
                  placeholder="RealESRGAN_x4plus_anime_6B"
                />
              </label>

              <label className="form-field compact">
                <span className="field-label">放大倍率</span>
                <input
                  type="number"
                  min={1}
                  max={4}
                  value={jobParams.scale}
                  onChange={handleParamChange('scale')}
                />
              </label>

              <label className="form-field compact">
                <span className="field-label">降噪等级</span>
                <select
                  value={jobParams.denoise}
                  onChange={handleParamChange('denoise')}
                >
                  <option value="low">Low</option>
                  <option value="medium">Medium</option>
                  <option value="high">High</option>
                </select>
              </label>

              <label className="form-field compact">
                <span className="field-label">输出格式</span>
                <select
                  value={jobParams.outputFormat}
                  onChange={handleParamChange('outputFormat')}
                >
                  <option value="jpg">JPG</option>
                  <option value="png">PNG</option>
                  <option value="webp">WEBP</option>
                </select>
              </label>

              <label className="form-field compact">
                <span className="field-label">JPEG 质量</span>
                <input
                  type="number"
                  min={1}
                  max={100}
                  value={jobParams.jpegQuality}
                  onChange={handleParamChange('jpegQuality')}
                  disabled={jobParams.outputFormat !== 'jpg'}
                />
              </label>

              <label className="form-field compact">
                <span className="field-label">Tile 尺寸</span>
                <input
                  type="number"
                  min={32}
                  max={1024}
                  value={jobParams.tileSize ?? ''}
                  onChange={handleParamChange('tileSize')}
                  placeholder="留空表示自动"
                />
              </label>

              <label className="form-field compact">
                <span className="field-label">Tile Pad</span>
                <input
                  type="number"
                  min={0}
                  max={128}
                  value={jobParams.tilePad ?? ''}
                  onChange={handleParamChange('tilePad')}
                  placeholder="留空表示自动"
                />
              </label>

              <label className="form-field compact">
                <span className="field-label">Batch Size</span>
                <input
                  type="number"
                  min={1}
                  max={16}
                  value={jobParams.batchSize ?? ''}
                  onChange={handleParamChange('batchSize')}
                  placeholder="留空表示自动"
                />
              </label>

              <label className="form-field compact">
                <span className="field-label">设备偏好</span>
                <select
                  value={jobParams.device}
                  onChange={handleParamChange('device')}
                >
                  <option value="auto">Auto</option>
                  <option value="cuda">CUDA</option>
                  <option value="cpu">CPU</option>
                </select>
              </label>
            </div>

            <div className="params-actions">
              <button type="button" onClick={handleResetParams}>
                重置为内置默认
              </button>
              <button type="button" onClick={handleSaveFavorite}>
                收藏当前参数
              </button>
            </div>

            {jobParamFavorites.length > 0 && (
              <div className="params-favorites">
                <div className="favorite-header">
                  <h5>收藏参数预设</h5>
                  <span>最多保留 8 组，可随时应用或删除。</span>
                </div>
                <ul>
                  {jobParamFavorites.map((favorite) => (
                    <li key={favorite.id}>
                      <button
                        type="button"
                        onClick={() => handleApplyFavorite(favorite.id)}
                      >
                        {favorite.name}
                      </button>
                      <span className="favorite-meta">
                        {new Date(favorite.createdAt).toLocaleString()}
                      </span>
                      <button
                        type="button"
                        className="ghost"
                        onClick={() => handleRemoveFavorite(favorite.id)}
                      >
                        删除
                      </button>
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </div>

          <div className="job-toolbar">
            <div className="job-toolbar-group">
              <label className="field-label" htmlFor="job-status-filter">
                状态筛选
              </label>
              <select
                id="job-status-filter"
                value={jobStatusFilter}
                onChange={handleJobStatusFilterChange}
              >
                <option value="all">全部</option>
                <option value="active">进行中</option>
                <option value="completed">已完成</option>
                <option value="failed">失败/错误</option>
              </select>
            </div>

            <div className="job-toolbar-group flex">
              <label className="field-label" htmlFor="job-search">
                搜索作业
              </label>
              <input
                id="job-search"
                type="text"
                value={jobSearch}
                onChange={handleJobSearchChange}
                placeholder="按 Job ID / 标题 过滤"
              />
            </div>

            <div className="job-toolbar-actions">
              <button type="button" onClick={handleSelectAllVisible}>
                全选当前列表
              </button>
              <button type="button" onClick={handleClearSelections}>
                清除选择
              </button>
            </div>
          </div>

          <div className="form-grid">
            <label className="form-field">
              <span className="field-label">服务地址</span>
              <div className="select-with-button">
                <select
                  value={jobForm.serviceUrl}
                  onChange={handleJobInput('serviceUrl')}
                >
                  <option value="">选择或新增推理地址</option>
                  {jobServiceOptions.map((option) => (
                    <option key={option} value={option}>
                      {option}
                    </option>
                  ))}
                </select>
                <button type="button" onClick={beginAddJobService}>
                  添加
                </button>
              </div>
              {isAddingJobService && (
                <div className="address-add-row">
                  <input
                    type="text"
                    value={jobAddressDraft}
                    onChange={(event) => {
                      setJobAddressDraft(event.currentTarget.value);
                      if (jobAddressError) {
                        setJobAddressError(null);
                      }
                    }}
                    placeholder="http://127.0.0.1:9000"
                    autoFocus
                  />
                  <button
                    type="button"
                    className="primary"
                    onClick={confirmAddJobService}
                  >
                    保存
                  </button>
                  <button
                    type="button"
                    className="ghost"
                    onClick={cancelAddJobService}
                  >
                    取消
                  </button>
                </div>
              )}
              {jobAddressError && (
                <p className="field-error">{jobAddressError}</p>
              )}
            </label>

            <label className="form-field">
              <span className="field-label">Bearer Token（可选）</span>
              <input
                type="text"
                value={jobForm.bearerToken}
                onChange={handleJobInput('bearerToken')}
                placeholder="xxxxxx"
              />
            </label>

            <label className="form-field">
              <span className="field-label">作品名</span>
              <input
                type="text"
                value={jobForm.title}
                onChange={handleJobInput('title')}
                placeholder="作品名"
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">卷名</span>
              <input
                type="text"
                value={jobForm.volume}
                onChange={handleJobInput('volume')}
                placeholder="第 1 卷"
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">输入类型</span>
              <select
                value={jobForm.inputType}
                onChange={handleJobInput('inputType')}
              >
                <option value="zip">Zip</option>
                <option value="folder">Folder</option>
              </select>
            </label>

            <label className="form-field">
              <span className="field-label">远端输入路径</span>
              <input
                type="text"
                value={jobForm.inputPath}
                readOnly
                aria-readonly="true"
                onFocus={(event) => event.currentTarget.select()}
                placeholder="上传完成后自动填充"
                title={jobForm.inputPath || undefined}
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">轮询间隔 (ms)</span>
              <input
                type="number"
                min={250}
                value={jobForm.pollIntervalMs}
                onChange={handleJobInput('pollIntervalMs')}
              />
            </label>
          </div>

          <div className="button-row">
            <button type="button" onClick={applyUploadContext}>
              从上传填充
            </button>
            <button
              type="button"
              className="primary"
              onClick={handleCreateJob}
              disabled={jobLoading}
            >
              {jobLoading ? '提交中…' : '创建推理作业'}
            </button>
          </div>

          {jobError && <p className="status status-error">{jobError}</p>}
          {jobStatus && <p className="status status-success">{jobStatus}</p>}
          {artifactError && (
            <p className="status status-error">{artifactError}</p>
          )}

          {filteredJobs.length > 0 ? (
            <div className="job-board">
              <table className="jobs-table">
                <thead>
                  <tr>
                    <th className="select-col">
                      <input
                        type="checkbox"
                        checked={allVisibleSelected}
                        onChange={(event) => {
                          if (event.currentTarget.checked) {
                            handleSelectAllVisible();
                          } else {
                            handleClearSelections();
                          }
                        }}
                      />
                    </th>
                    <th>作业 ID / 状态</th>
                    <th>进度</th>
                    <th>来源</th>
                    <th>消息</th>
                    <th>重试</th>
                    <th>上次错误</th>
                    <th>产物</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {filteredJobs.map((job) => {
                    const percent =
                      job.total > 0
                        ? Math.min(
                            100,
                            Math.round((job.processed / job.total) * 100)
                          )
                        : null;
                    return (
                      <tr key={job.jobId}>
                        <td className="select-col">
                          <input
                            type="checkbox"
                            checked={selectedJobIds.includes(job.jobId)}
                            onChange={() => handleToggleJobSelection(job.jobId)}
                          />
                        </td>
                        <td className="job-id-cell">
                          <span className="job-id monospace" title={job.jobId}>
                            {job.jobId}
                          </span>
                          <span
                            className={`status-chip status-${statusTone(
                              job.status
                            )}`}
                          >
                            {describeStatus(job.status)}
                          </span>
                        </td>
                        <td className="progress-col">
                          {percent !== null ? (
                            <div className="progress-cell">
                              <div className="progress-bar">
                                <span style={{ width: `${percent}%` }} />
                              </div>
                              <span className="progress-label">
                                {job.processed}/{job.total}（{percent}%）
                              </span>
                            </div>
                          ) : (
                            <span className="progress-label">-</span>
                          )}
                        </td>
                        <td className="transport-cell">
                          <span className="transport-badge">
                            {describeTransport(job.transport)}
                          </span>
                        </td>
                        <td className="message-cell">
                          {job.message ? job.message : '-'}
                        </td>
                        <td className="retry-cell">{job.retries ?? 0}</td>
                        <td className="error-cell">{job.lastError ?? '-'}</td>
                        <td className="artifact-cell">
                          {job.artifactPath ? (
                            <div className="artifact-info">
                              <span
                                className="artifact-path"
                                title={job.artifactPath}
                              >
                                {job.artifactPath}
                              </span>
                              {job.artifactHash && (
                                <span
                                  className="artifact-hash"
                                  title={job.artifactHash}
                                >
                                  hash: {job.artifactHash.slice(0, 8)}…
                                </span>
                              )}
                            </div>
                          ) : (
                            '-'
                          )}
                        </td>
                        <td className="actions-cell">
                          <button
                            type="button"
                            onClick={() => handleResumeJob(job)}
                          >
                            恢复
                          </button>
                          <button
                            type="button"
                            onClick={() => handleCancelJob(job)}
                          >
                            终止
                          </button>
                          <button
                            type="button"
                            disabled={
                              !job.artifactPath ||
                              artifactDownloadBusyJob === job.jobId ||
                              artifactValidateBusyJob === job.jobId
                            }
                            onClick={() => handleDownloadArtifact(job)}
                          >
                            {artifactDownloadBusyJob === job.jobId
                              ? '下载中…'
                              : '下载 ZIP'}
                          </button>
                          <button
                            type="button"
                            disabled={
                              !job.artifactPath ||
                              artifactValidateBusyJob === job.jobId ||
                              artifactDownloadBusyJob === job.jobId
                            }
                            onClick={() => handleValidateArtifact(job)}
                          >
                            {artifactValidateBusyJob === job.jobId
                              ? '校验中…'
                              : '校验'}
                          </button>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          ) : (
            <p className="status">暂无符合条件的作业。</p>
          )}

          {hasSelection && (
            <div className="job-batch-actions">
              <span>已选择 {selectedJobIds.length} 个作业</span>
              <div className="job-batch-buttons">
                <button type="button" onClick={handleBatchResume}>
                  批量恢复
                </button>
                <button type="button" onClick={handleBatchCancel}>
                  批量终止
                </button>
                <button type="button" onClick={handleBatchDownload}>
                  批量下载 ZIP
                </button>
                <button type="button" onClick={handleBatchValidate}>
                  批量校验
                </button>
              </div>
            </div>
          )}

          {artifactDownloads.length > 0 && (
            <div className="artifact-downloads">
              <h4>最近下载</h4>
              <ul>
                {artifactDownloads.map((summary) => (
                  <li key={`${summary.jobId}-${summary.hash}`}>
                    <div className="report-header">
                      <span className="job-id monospace">{summary.jobId}</span>
                      <span className="report-status">
                        文件数：{summary.fileCount}
                      </span>
                      <span className="report-path">
                        ZIP：{summary.archivePath}
                      </span>
                    </div>
                    <div className="report-details">
                      <span>解压目录：{summary.extractPath}</span>
                      {summary.warnings.length > 0 && (
                        <span className="report-warning">
                          {summary.warnings[0]}
                        </span>
                      )}
                    </div>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {artifactReports.length > 0 && (
            <div className="artifact-reports">
              <h4>最近校验结果</h4>
              <ul>
                {artifactReports.map((report) => (
                  <li key={`${report.jobId}-${report.createdAt}`}>
                    <div className="report-header">
                      <span className="job-id monospace">{report.jobId}</span>
                      <span className="report-status">
                        匹配 {report.summary.matched} /{' '}
                        {report.summary.totalManifest}
                      </span>
                      <span className="report-path">
                        输出：{report.extractPath}
                      </span>
                      {report.archivePath && (
                        <span className="report-archive">
                          ZIP：{report.archivePath}
                        </span>
                      )}
                    </div>
                    <div className="report-details">
                      <span>缺失 {report.summary.missing}</span>
                      <span>多余 {report.summary.extra}</span>
                      <span>哈希不一致 {report.summary.mismatched}</span>
                      {report.warnings.length > 0 && (
                        <span className="report-warning">
                          {report.warnings[0]}
                        </span>
                      )}
                    </div>
                    {report.reportPath && (
                      <span className="report-file">
                        报告：{report.reportPath}
                      </span>
                    )}
                  </li>
                ))}
              </ul>
            </div>
          )}

          <div className="wizard-controls">
            <button type="button" onClick={goToPreviousStep}>
              上一步
            </button>
            <button type="button" className="ghost" onClick={resetWizardState}>
              开始新任务
            </button>
          </div>
        </section>
      )}

      {edgePreviewSelector.visible && (
        <div className="edge-preview-backdrop" role="dialog" aria-modal="true">
          <div className="edge-preview-dialog selector">
            <header className="edge-preview-header">
              <h3>选择预览图片</h3>
              {edgePreviewSelector.directory && (
                <p title={edgePreviewSelector.directory}>
                  当前目录：{edgePreviewSelector.directory}
                </p>
              )}
            </header>

            {edgePreviewSelector.loading && (
              <p className="status status-tip">正在加载目录图片…</p>
            )}

            {!edgePreviewSelector.loading && edgePreviewSelector.error && (
              <p className="status status-error">{edgePreviewSelector.error}</p>
            )}

            {!edgePreviewSelector.loading &&
              !edgePreviewSelector.error &&
              edgePreviewSelector.images.length === 0 && (
                <p className="status status-tip">
                  所选目录没有可预览的图片，请确认目录内包含支持的格式。
                </p>
              )}

            {!edgePreviewSelector.loading &&
              !edgePreviewSelector.error &&
              edgePreviewSelector.images.length > 0 && (
                <>
                  <p className="status status-tip">点击下方图片以生成阈值预览。</p>
                  <div
                    className="edge-preview-selector-list"
                    ref={selectorListRef}
                    onScroll={handleSelectorScroll}
                    role="listbox"
                    aria-label="可预览图片列表"
                  >
                    <div
                      className="edge-preview-selector-virtual"
                      style={{ height: selectorTotalHeight }}
                    >
                      {selectorVisibleItems.map((item, index) => {
                        const actualIndex = selectorStartIndex + index;
                        return (
                          <button
                            type="button"
                            key={item.path}
                            className="edge-preview-selector-item"
                            style={{
                              top:
                                actualIndex * EDGE_PREVIEW_SELECTOR_ITEM_HEIGHT,
                            }}
                            onClick={() => executeEdgePreview(item.path)}
                          >
                            <img
                              src={item.url}
                              alt={item.fileName}
                              loading="lazy"
                            />
                            <span className="edge-preview-selector-name">
                              {item.relativePath}
                            </span>
                            {typeof item.fileSize === 'number' && (
                              <span className="edge-preview-selector-meta">
                                {(item.fileSize / 1024 / 1024).toFixed(2)} MB
                              </span>
                            )}
                          </button>
                        );
                      })}
                    </div>
                  </div>
                </>
              )}

            <div className="edge-preview-actions">
              <button type="button" onClick={handleCloseEdgePreviewSelector}>
                取消
              </button>
            </div>
          </div>
        </div>
      )}

      {edgePreview.visible && (
        <div className="edge-preview-backdrop" role="dialog" aria-modal="true">
          <div className="edge-preview-dialog">
            <header className="edge-preview-header">
              <h3>阈值预览</h3>
              {edgePreview.data ? (
                <>
                  <p>
                    实际阈值：亮白 {formatThresholdValue(appliedBrightnessThresholds[0])}
                    ，留黑 {formatThresholdValue(appliedBrightnessThresholds[1])}
                  </p>
                  <p>
                    实际搜索比例：左 {formatRatioValue(appliedSearchRatios[0])}，右{' '}
                    {formatRatioValue(appliedSearchRatios[1])}
                  </p>
                  {!edgePreview.loading && (
                    <>
                      {previewThresholdsMatched ? (
                        <p className="status status-tip">阈值已按照当前输入生效。</p>
                      ) : (
                        <p className="status status-warning">
                          请求阈值为亮白 {formatThresholdValue(requestedBrightnessThresholds[0])}，留黑{' '}
                          {formatThresholdValue(requestedBrightnessThresholds[1])}，后端已调整为上述实际数值。
                        </p>
                      )}
                      {previewSearchRatiosMatched ? (
                        <p className="status status-tip">搜索比例已按照当前输入生效。</p>
                      ) : (
                        <p className="status status-warning">
                          请求搜索比例为左 {formatRatioValue(requestedSearchRatios[0])}，右{' '}
                          {formatRatioValue(requestedSearchRatios[1])}，后端已调整为上述实际数值。
                        </p>
                      )}
                      {requestedAccelerator === 'auto' ? (
                        <p className="status status-tip">
                          设备偏好为自动模式，实际使用{' '}
                          {describeAccelerator(edgePreview.data.accelerator)}。
                        </p>
                      ) : previewAcceleratorMatched ? (
                        <p className="status status-tip">
                          请求的设备偏好 {describeAccelerator(requestedAccelerator)} 已生效（实际使用{' '}
                          {describeAccelerator(edgePreview.data.accelerator)}）。
                        </p>
                      ) : (
                        <p className="status status-warning">
                          请求的设备偏好 {describeAccelerator(requestedAccelerator)} 无法满足，已回退为{' '}
                          {describeAccelerator(edgePreview.data.accelerator)}。
                        </p>
                      )}
                    </>
                  )}
                </>
              ) : (
                <p>
                  当前输入：亮白 {formatThresholdValue(edgeBrightnessThresholds[0])}，留黑{' '}
                  {formatThresholdValue(edgeBrightnessThresholds[1])}；搜索比例 左{' '}
                  {formatRatioValue(edgeSearchRatios[0])}，右{' '}
                  {formatRatioValue(edgeSearchRatios[1])}；加速器偏好{' '}
                  {describeAccelerator(edgeAcceleratorPreference)}
                </p>
              )}
            </header>
            <div className="edge-preview-body">
              {edgePreview.loading && (
                <p className="status status-tip">正在生成预览…</p>
              )}

              {!edgePreview.loading && edgePreview.error && (
                <p className="status status-error">{edgePreview.error}</p>
              )}

              {!edgePreview.loading && !edgePreview.error && edgePreview.data && (
                <>
                  <div className="edge-preview-images">
                    <figure>
                      <img
                        src={edgePreview.data.originalUrl}
                        alt="原图预览"
                      />
                      <figcaption>原图</figcaption>
                    </figure>
                    {edgePreview.data.outputs.map((output) => {
                      const label = EDGE_PREVIEW_OUTPUT_LABELS[output.role];
                      return (
                        <figure key={output.path}>
                          <img
                            src={output.url}
                            alt={`处理结果预览 - ${label}`}
                          />
                          <figcaption>{label}</figcaption>
                        </figure>
                      );
                    })}
                  </div>

                  {edgePreview.data.mode === 'split' && (
                    <p className="status status-tip">
                      已生成左右两页预览，可直接对比最终拆分成品。
                    </p>
                  )}
                  {edgePreview.data.mode === 'coverTrim' && (
                    <p className="status status-tip">
                      当前页面判定为封面裁剪，展示单页裁剪结果。
                    </p>
                  )}
                  {edgePreview.data.mode === 'skip' && (
                    <p className="status status-warning">
                      未识别出有效的拆分区域，请尝试调整阈值或选择其他图片。
                    </p>
                  )}

                  <div className="edge-preview-metrics">
                    <dl>
                      <div>
                        <dt>请求阈值</dt>
                        <dd>
                          亮白 {formatThresholdValue(requestedBrightnessThresholds[0])}，留黑{' '}
                          {formatThresholdValue(requestedBrightnessThresholds[1])}
                        </dd>
                      </div>
                      <div>
                        <dt>实际阈值</dt>
                        <dd>
                          亮白 {formatThresholdValue(appliedBrightnessThresholds[0])}，留黑{' '}
                          {formatThresholdValue(appliedBrightnessThresholds[1])}
                        </dd>
                      </div>
                      <div>
                        <dt>请求搜索比例</dt>
                        <dd>
                          左 {formatRatioValue(requestedSearchRatios[0])}，右{' '}
                          {formatRatioValue(requestedSearchRatios[1])}
                        </dd>
                      </div>
                      <div>
                        <dt>实际搜索比例</dt>
                        <dd>
                          左 {formatRatioValue(appliedSearchRatios[0])}，右{' '}
                          {formatRatioValue(appliedSearchRatios[1])}
                        </dd>
                      </div>
                      <div>
                        <dt>图像尺寸</dt>
                        <dd>
                          {edgePreview.data.metrics.width} ×{' '}
                          {edgePreview.data.metrics.height}
                        </dd>
                      </div>
                      <div>
                        <dt>亮度均值</dt>
                        <dd>
                          {edgePreview.data.metrics.meanIntensityAvg.toFixed(2)}
                        </dd>
                      </div>
                      <div>
                        <dt>亮度范围</dt>
                        <dd>
                          {edgePreview.data.metrics.meanIntensityMin.toFixed(2)} ~{' '}
                          {edgePreview.data.metrics.meanIntensityMax.toFixed(2)}
                        </dd>
                      </div>
                      <div>
                        <dt>置信阈值</dt>
                        <dd>{edgePreview.data.confidenceThreshold.toFixed(2)}</dd>
                      </div>
                      <div>
                        <dt>亮度权重</dt>
                        <dd>{edgePreview.data.brightnessWeight.toFixed(2)}</dd>
                      </div>
                      <div>
                        <dt>请求加速器</dt>
                        <dd>
                          {describeAccelerator(edgePreview.data.requestedAccelerator)}
                        </dd>
                      </div>
                      <div>
                        <dt>实际加速器</dt>
                        <dd>{describeAccelerator(edgePreview.data.accelerator)}</dd>
                      </div>
                    </dl>

                    <div className="edge-preview-notes">
                      {edgePreview.data.leftMargin && (
                        <span>
                          左侧置信度：
                          {edgePreview.data.leftMargin.confidence.toFixed(3)}
                        </span>
                      )}
                      {edgePreview.data.rightMargin && (
                        <span>
                          右侧置信度：
                          {edgePreview.data.rightMargin.confidence.toFixed(3)}
                        </span>
                      )}
                    </div>
                  </div>
                </>
              )}
            </div>

            <div className="edge-preview-actions">
              <button
                type="button"
                onClick={handleEdgePreview}
                disabled={edgePreview.loading}
              >
                重新选择图片
              </button>
              <button
                type="button"
                className="primary"
                onClick={handleCloseEdgePreview}
              >
                关闭
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};

export default MangaUpscaleAgent;
