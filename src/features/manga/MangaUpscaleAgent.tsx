import type { ChangeEvent } from "react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

type RenameEntry = {
  originalName: string;
  renamedName: string;
};

type RenameOutcome = {
  directory: string;
  manifestPath?: string | null;
  entries: RenameEntry[];
  dryRun: boolean;
  warnings: string[];
};

type UploadMode = "zip" | "folder";

type UploadOutcome = {
  remoteUrl: string;
  uploadedBytes: number;
  fileCount: number;
  mode: UploadMode;
};

type UploadProgressStage =
  | "preparing"
  | "uploading"
  | "finalizing"
  | "completed"
  | "failed";

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

const REMOTE_ROOT = "incoming";

const normalizeSegment = (input: string): string => {
  if (!input.trim()) {
    return "";
  }

  const normalized = input
    .normalize("NFKD")
    .replace(/[^a-zA-Z0-9\u4e00-\u9fff]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-+|-+$/g, "")
    .toLowerCase();

  return normalized.slice(0, 64);
};

const formatSeedSegment = (seed: number): string => {
  const date = new Date(seed);
  const pad = (value: number) => value.toString().padStart(2, "0");
  const year = date.getFullYear();
  const month = pad(date.getMonth() + 1);
  const day = pad(date.getDate());
  const hours = pad(date.getHours());
  const minutes = pad(date.getMinutes());
  const seconds = pad(date.getSeconds());

  return `${year}${month}${day}-${hours}${minutes}${seconds}`;
};

const buildRemotePath = (options: { title?: string; volume?: string; seed: number; mode: UploadMode }): string => {
  const { title = "", volume = "", seed, mode } = options;
  const titleSegment = normalizeSegment(title);
  const volumeSegment = normalizeSegment(volume);
  const segments = [titleSegment, volumeSegment].filter(Boolean);
  const slug = segments.length > 0 ? segments.join("-") : "manga";
  const suffix = formatSeedSegment(seed);
  const stem = `${slug}-${suffix}`.slice(0, 96);
  const extension = mode === "zip" ? ".zip" : "";

  return `${REMOTE_ROOT}/${stem}${extension}`;
};

const mergeServiceAddresses = (...sources: (string | string[] | null | undefined)[]): string[] => {
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

    if (typeof source === "string") {
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

type JobEventTransport = "websocket" | "polling" | "system";

type JobParamsConfig = {
  model: string;
  scale: number;
  denoise: "low" | "medium" | "high";
  outputFormat: "jpg" | "png" | "webp";
  jpegQuality: number;
  tileSize: number | null;
  tilePad: number | null;
  batchSize: number | null;
  device: "auto" | "cuda" | "cpu";
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
  status: "matched" | "missing" | "extra" | "mismatch";
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

type JobSubmission = {
  jobId: string;
};

type JobRecord = JobEventPayload & {
  lastUpdated: number;
  serviceUrl: string;
  bearerToken?: string | null;
  inputPath?: string | null;
  inputType?: JobFormState["inputType"];
  manifestPath?: string | null;
  transport: JobEventTransport;
};

type JobFormState = {
  serviceUrl: string;
  bearerToken: string;
  title: string;
  volume: string;
  inputType: "zip" | "folder";
  inputPath: string;
  pollIntervalMs: number;
};

type MangaSourceMode = "singleVolume" | "multiVolume";

type VolumeCandidate = {
  directory: string;
  folderName: string;
  imageCount: number;
  detectedNumber?: number | null;
};

type MangaSourceAnalysis = {
  root: string;
  mode: MangaSourceMode;
  rootImageCount: number;
  totalImages: number;
  volumeCandidates: VolumeCandidate[];
  skippedEntries: string[];
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
      mode: "single";
      outcome: RenameOutcome;
    }
  | {
      mode: "multi";
      volumes: VolumeRenameOutcome[];
      dryRun: boolean;
    };

type StepId = "source" | "volumes" | "rename" | "upload" | "jobs";

type StepDescriptor = {
  id: StepId;
  label: string;
};

const DEFAULT_PAD = 4;
const DEFAULT_POLL_INTERVAL = 1000;
const SETTINGS_KEY = "manga-upscale-agent:v1";
const UPLOAD_DEFAULTS_KEY = `${SETTINGS_KEY}:upload-defaults`;
const SERVICE_ADDRESS_BOOK_KEY = `${SETTINGS_KEY}:service-addresses`;
const PARAM_DEFAULTS_KEY = `${SETTINGS_KEY}:job-params`;
const PARAM_FAVORITES_KEY = `${SETTINGS_KEY}:job-param-favorites`;

const DEFAULT_JOB_PARAMS: JobParamsConfig = {
  model: "RealESRGAN_x4plus_anime_6B",
  scale: 2,
  denoise: "medium",
  outputFormat: "jpg",
  jpegQuality: 95,
  tileSize: null,
  tilePad: null,
  batchSize: null,
  device: "auto",
};

const MangaUpscaleAgent = () => {
  const [renameForm, setRenameForm] = useState<RenameFormState>({
    directory: "",
    pad: DEFAULT_PAD,
    targetExtension: "jpg",
  });
  const [renameSummary, setRenameSummary] = useState<RenameSummary | null>(null);
  const [renameLoading, setRenameLoading] = useState(false);
  const [renameError, setRenameError] = useState<string | null>(null);

  const [analysisLoading, setAnalysisLoading] = useState(false);
  const [analysisError, setAnalysisError] = useState<string | null>(null);
  const [sourceAnalysis, setSourceAnalysis] = useState<MangaSourceAnalysis | null>(null);
  const [volumeMappings, setVolumeMappings] = useState<VolumeMapping[]>([]);
  const [volumeMappingError, setVolumeMappingError] = useState<string | null>(null);
  const [mappingConfirmed, setMappingConfirmed] = useState(false);
  const [selectedVolumeKey, setSelectedVolumeKey] = useState<string | null>(null);
  const [hasRestoredDefaults, setHasRestoredDefaults] = useState(false);

  const isMultiVolumeSource = sourceAnalysis?.mode === "multiVolume";

  const [uploadForm, setUploadForm] = useState<UploadFormState>({
    serviceUrl: "",
    bearerToken: "",
    title: "",
    volume: "",
    mode: "zip",
  });
  const [uploadLoading, setUploadLoading] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [uploadStatus, setUploadStatus] = useState<string | null>(null);
  const [uploadProgress, setUploadProgress] = useState<UploadProgressPayload | null>(null);
  const [remotePathSeed, setRemotePathSeed] = useState(() => Date.now());
  const [lastUploadRemotePath, setLastUploadRemotePath] = useState<string>("");
  const [uploadServiceOptions, setUploadServiceOptions] = useState<string[]>([]);
  const [isAddingUploadService, setIsAddingUploadService] = useState(false);
  const [uploadAddressDraft, setUploadAddressDraft] = useState("");
  const [uploadAddressError, setUploadAddressError] = useState<string | null>(null);

  const [jobServiceOptions, setJobServiceOptions] = useState<string[]>([]);
  const [isAddingJobService, setIsAddingJobService] = useState(false);
  const [jobAddressDraft, setJobAddressDraft] = useState("");
  const [jobAddressError, setJobAddressError] = useState<string | null>(null);

  const [jobForm, setJobForm] = useState<JobFormState>({
    serviceUrl: "",
    bearerToken: "",
    title: "",
    volume: "",
    inputType: "zip",
    inputPath: "",
    pollIntervalMs: DEFAULT_POLL_INTERVAL,
  });
  const [jobParams, setJobParams] = useState<JobParamsConfig>(DEFAULT_JOB_PARAMS);
  const [jobParamFavorites, setJobParamFavorites] = useState<JobParamFavorite[]>([]);
  const [jobParamsRestored, setJobParamsRestored] = useState(false);
  const [jobs, setJobs] = useState<JobRecord[]>([]);
  const [jobLoading, setJobLoading] = useState(false);
  const [jobError, setJobError] = useState<string | null>(null);
  const [jobStatus, setJobStatus] = useState<string | null>(null);
  const [jobStatusFilter, setJobStatusFilter] = useState<"all" | "active" | "completed" | "failed">("all");
  const [jobSearch, setJobSearch] = useState("");
  const [selectedJobIds, setSelectedJobIds] = useState<string[]>([]);
  const [artifactReports, setArtifactReports] = useState<ArtifactReport[]>([]);
  const [artifactDownloads, setArtifactDownloads] = useState<ArtifactDownloadSummary[]>([]);
  const [artifactError, setArtifactError] = useState<string | null>(null);
  const [artifactDownloadBusyJob, setArtifactDownloadBusyJob] = useState<string | null>(null);
  const [artifactValidateBusyJob, setArtifactValidateBusyJob] = useState<string | null>(null);
  const [artifactTargetRoot, setArtifactTargetRoot] = useState<string>("");
  const [currentStep, setCurrentStep] = useState<StepId>("source");

  const sanitizeVolumeName = useCallback((folderName: string) => {
    const replaced = folderName.replace(/[_-]+/g, " ").replace(/\s+/g, " ").trim();
    return replaced.length > 0 ? replaced : folderName;
  }, []);

  const buildInitialMappings = useCallback(
    (analysis: MangaSourceAnalysis) => {
      if (analysis.mode !== "multiVolume") {
        return [] as VolumeMapping[];
      }

      const usedNumbers = new Set<number>();

      return analysis.volumeCandidates.map((candidate, index) => {
        let detectedNumber =
          typeof candidate.detectedNumber === "number"
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
    [sanitizeVolumeName],
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
        const analysis = await invoke<MangaSourceAnalysis>("analyze_manga_directory", {
          directory: path,
        });

        setSourceAnalysis(analysis);

        if (analysis.mode === "multiVolume") {
          const mappings = buildInitialMappings(analysis);
          setVolumeMappings(mappings);
          setMappingConfirmed(false);
          setSelectedVolumeKey(mappings.length > 0 ? mappings[0].directory : null);
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
        setAnalysisError(error instanceof Error ? error.message : String(error));
      } finally {
        setAnalysisLoading(false);
      }
    },
    [buildInitialMappings],
  );

  const handleRefreshAnalysis = useCallback(() => {
    if (!renameForm.directory) {
      setAnalysisError("请先选择漫画文件夹路径。");
      return;
    }
    analyzeDirectory(renameForm.directory);
  }, [analyzeDirectory, renameForm.directory]);

  useEffect(() => {
    if (typeof window === "undefined") {
      return;
    }

    try {
      const stored = window.localStorage.getItem(SETTINGS_KEY);
      const storedDefaults = window.localStorage.getItem(UPLOAD_DEFAULTS_KEY);
      const storedAddressBookRaw = window.localStorage.getItem(SERVICE_ADDRESS_BOOK_KEY);

      let uploadPatch: Partial<UploadFormState> | null = null;
      let jobPatch: Partial<JobFormState> | null = null;
      let defaultsServiceUrl: string | null = null;
      let storedAddressBook: ServiceAddressBook | null = null;

      if (stored) {
        const parsed = JSON.parse(stored) as {
          uploadForm?: Partial<UploadFormState>;
          jobForm?: Partial<JobFormState>;
        };

        if (parsed.uploadForm) {
          uploadPatch = { ...parsed.uploadForm };
          if (uploadPatch && "remotePath" in uploadPatch) {
            delete (uploadPatch as Record<string, unknown>).remotePath;
          }
        }
        if (parsed.jobForm) {
          jobPatch = { ...parsed.jobForm };
        }
      }

      if (storedDefaults) {
        const defaults = JSON.parse(storedDefaults) as Partial<Pick<UploadFormState, "serviceUrl" >>;
        if (defaults?.serviceUrl) {
          defaultsServiceUrl = defaults.serviceUrl;
          if (!(uploadPatch?.serviceUrl)) {
            uploadPatch = {
              ...(uploadPatch ?? {}),
              serviceUrl: defaults.serviceUrl,
            };
          }
        }
      }

      if (storedAddressBookRaw) {
        try {
          const parsedAddressBook = JSON.parse(storedAddressBookRaw) as Partial<ServiceAddressBook> | null;
          if (parsedAddressBook && typeof parsedAddressBook === "object") {
            const upload = Array.isArray(parsedAddressBook.upload)
              ? parsedAddressBook.upload.filter((item): item is string => typeof item === "string")
              : [];
            const job = Array.isArray(parsedAddressBook.job)
              ? parsedAddressBook.job.filter((item): item is string => typeof item === "string")
              : [];
            storedAddressBook = { upload, job };
          }
        } catch (addressError) {
          console.warn("Failed to restore service address book", addressError);
        }
      }

      const uploadCandidates = mergeServiceAddresses(
        storedAddressBook?.upload ?? [],
        uploadPatch?.serviceUrl,
        defaultsServiceUrl,
      );
      const jobCandidates = mergeServiceAddresses(storedAddressBook?.job ?? [], jobPatch?.serviceUrl);

      setUploadServiceOptions(uploadCandidates);
      setJobServiceOptions(jobCandidates);

      if (uploadPatch && Object.keys(uploadPatch).length > 0) {
        setUploadForm((prev) => ({ ...prev, ...uploadPatch }));
      }
      if (jobPatch && Object.keys(jobPatch).length > 0) {
        setJobForm((prev) => ({ ...prev, ...jobPatch }));
      }
      setHasRestoredDefaults(true);
    } catch (storageError) {
      console.warn("Failed to restore manga agent settings", storageError);
      setHasRestoredDefaults(true);
    }
  }, []);

  useEffect(() => {
    if (typeof window === "undefined" || jobParamsRestored) {
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
              .filter((item) => typeof item?.id === "string" && typeof item?.name === "string")
              .map((item) => ({
                ...DEFAULT_JOB_PARAMS,
                ...item,
                id: item.id,
                name: item.name,
                createdAt: item.createdAt ?? Date.now(),
              })),
          );
        }
      }
    } catch (storageError) {
      console.warn("Failed to restore manga agent job params", storageError);
    } finally {
      setJobParamsRestored(true);
    }
  }, [jobParamsRestored]);

  useEffect(() => {
    if (typeof window === "undefined" || !jobParamsRestored) {
      return;
    }

    try {
      window.localStorage.setItem(PARAM_DEFAULTS_KEY, JSON.stringify(jobParams));
    } catch (storageError) {
      console.warn("Failed to persist job params", storageError);
    }
  }, [jobParams, jobParamsRestored]);

  useEffect(() => {
    if (typeof window === "undefined" || !jobParamsRestored) {
      return;
    }

    try {
      window.localStorage.setItem(PARAM_FAVORITES_KEY, JSON.stringify(jobParamFavorites));
    } catch (storageError) {
      console.warn("Failed to persist job param favorites", storageError);
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
    if (typeof window === "undefined") {
      return;
    }

    if (!hasRestoredDefaults) {
      return;
    }

    try {
      const defaults = {
        serviceUrl: uploadForm.serviceUrl,
      };
      window.localStorage.setItem(UPLOAD_DEFAULTS_KEY, JSON.stringify(defaults));
    } catch (storageError) {
      console.warn("Failed to persist upload defaults", storageError);
    }
  }, [hasRestoredDefaults, uploadForm.serviceUrl]);

  useEffect(() => {
    if (typeof window === "undefined") {
      return;
    }

    const addressBook: ServiceAddressBook = {
      upload: uploadServiceOptions,
      job: jobServiceOptions,
    };

    try {
      window.localStorage.setItem(SERVICE_ADDRESS_BOOK_KEY, JSON.stringify(addressBook));
    } catch (storageError) {
      console.warn("Failed to persist service address book", storageError);
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
    let disposed = false;
    const disposers: UnlistenFn[] = [];

    const bind = async () => {
      try {
        const uploadUnlisten = await listen<UploadProgressPayload>(
          "manga-upload-progress",
          (event) => {
            const payload = event.payload;
            if (!payload) {
              return;
            }

            setUploadProgress(payload);

            switch (payload.stage) {
              case "preparing":
                setUploadError(null);
                setUploadStatus(null);
                break;
              case "failed":
                setUploadError(payload.message ?? "上传失败");
                setUploadStatus(null);
                break;
              case "completed":
                setUploadError(null);
                setUploadStatus((prev) => payload.message ?? prev ?? "上传完成");
                break;
              default:
                break;
            }
          },
        );

        if (disposed) {
          uploadUnlisten();
          return;
        }
        disposers.push(uploadUnlisten);

        const jobUnlisten = await listen<JobEventPayload>("manga-job-event", (event) => {
          const payload = event.payload;
          if (!payload) {
            return;
          }

          setJobs((prev) => {
            const existing = prev.find((item) => item.jobId === payload.jobId);
            const record = mapPayloadToRecord(payload, undefined, existing ?? undefined);
            const next = prev.filter((item) => item.jobId !== record.jobId);
            next.push(record);
            next.sort((a, b) => b.lastUpdated - a.lastUpdated);
            return next;
          });
        });

        if (disposed) {
          jobUnlisten();
          return;
        }
        disposers.push(jobUnlisten);
      } catch (bindingError) {
        console.warn("Failed to bind manga upscale events", bindingError);
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
  }, []);

  useEffect(() => {
    if (typeof window === "undefined") {
      return;
    }

  const payload = {
      uploadForm,
      jobForm,
      jobParams,
    };

    try {
      window.localStorage.setItem(SETTINGS_KEY, JSON.stringify(payload));
    } catch (storageError) {
      console.warn("Failed to persist manga agent settings", storageError);
    }
  }, [uploadForm, jobForm, jobParams]);

  useEffect(() => {
    if (!isMultiVolumeSource) {
      return;
    }

    if (volumeMappings.length === 0) {
      setSelectedVolumeKey(null);
      return;
    }

    const exists = volumeMappings.some((item) => item.directory === selectedVolumeKey);
    if (!exists) {
      setSelectedVolumeKey(volumeMappings[0].directory);
    }
  }, [isMultiVolumeSource, selectedVolumeKey, volumeMappings]);

  useEffect(() => {
    const directoryCandidate =
      (sourceAnalysis?.root && sourceAnalysis.root.length > 0
        ? sourceAnalysis.root
        : undefined) ??
      (renameForm.directory && renameForm.directory.length > 0
        ? renameForm.directory
        : undefined) ??
      (renameSummary?.mode === "single" ? renameSummary.outcome.directory : "");

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
    if (!renameSummary || renameSummary.mode !== "single") {
      return [] as RenameEntry[];
    }
    return renameSummary.outcome.entries.slice(0, 8);
  }, [renameSummary]);

  const selectedVolume = useMemo(() => {
    if (!selectedVolumeKey) {
      return null;
    }
    return volumeMappings.find((item) => item.directory === selectedVolumeKey) ?? null;
  }, [selectedVolumeKey, volumeMappings]);

  const remotePathPreview = useMemo(() => {
    const resolvedTitle = uploadForm.title.trim() || jobForm.title.trim();
    const selectedVolumeName = selectedVolume ? selectedVolume.volumeName.trim() : "";
    const selectedVolumeFolder = selectedVolume ? selectedVolume.folderName.trim() : "";
    const resolvedVolume =
      uploadForm.volume.trim() ||
      jobForm.volume.trim() ||
      selectedVolumeName ||
      selectedVolumeFolder ||
      "";

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
          case "active":
            return statusUpper === "PENDING" || statusUpper === "RUNNING";
          case "completed":
            return statusUpper === "SUCCESS";
          case "failed":
            return statusUpper === "FAILED" || statusUpper === "ERROR";
          case "all":
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
        job.message ?? "",
        job.metadata?.title ?? "",
        job.metadata?.volume ?? "",
      ]
        .join(" ")
        .toLowerCase();

      return haystack.includes(keyword);
    });
  }, [jobSearch, jobStatusFilter, jobs]);

  useEffect(() => {
    setSelectedJobIds((prev) => prev.filter((id) => jobs.some((job) => job.jobId === id)));
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
    const selected = await invoke<string | string[] | null>("plugin:dialog|open", {
      options: {
        directory: true,
        multiple: false,
        title: "选择漫画图片文件夹",
      },
    });

    if (!selected) {
      return;
    }

    const first = Array.isArray(selected) ? selected[0] : selected;
    if (typeof first === "string" && first.length > 0) {
      setRenameForm((prev) => ({ ...prev, directory: first }));
      setCurrentStep("source");
      analyzeDirectory(first);
    }
  }, [analyzeDirectory]);

  const runRename = useCallback(
    async (dryRun: boolean) => {
      if (!renameForm.directory) {
        setRenameError("请先选择漫画图片所在文件夹。");
        return;
      }

      if (isMultiVolumeSource && !mappingConfirmed) {
        setRenameError("请先在卷映射步骤完成确认。");
        return;
      }

      setRenameLoading(true);
      setRenameError(null);
      try {
        const padValue = Number.isFinite(renameForm.pad)
          ? Math.max(1, Math.floor(renameForm.pad))
          : DEFAULT_PAD;

        const payload = {
          directory: renameForm.directory,
          pad: padValue,
          targetExtension:
            renameForm.targetExtension.trim().toLowerCase() || "jpg",
          dryRun,
        };

        if (isMultiVolumeSource) {
          if (volumeMappings.length === 0) {
            setRenameError("未检测到任何卷。请检查目录结构。");
            return;
          }

          const outcomes: VolumeRenameOutcome[] = [];

          for (const mapping of volumeMappings) {
            const result = await invoke<RenameOutcome>("rename_manga_sequence", {
              options: {
                ...payload,
                directory: mapping.directory,
              },
            });

            outcomes.push({ mapping, outcome: result });
          }

          setRenameSummary({ mode: "multi", volumes: outcomes, dryRun });
        } else {
          const targetDirectory =
            (sourceAnalysis?.root && sourceAnalysis.root.length > 0
              ? sourceAnalysis.root
              : undefined) ?? renameForm.directory;

          const result = await invoke<RenameOutcome>("rename_manga_sequence", {
            options: {
              ...payload,
              directory: targetDirectory,
            },
          });

          setRenameSummary({ mode: "single", outcome: result });
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
      isMultiVolumeSource,
      mappingConfirmed,
      renameForm,
      sourceAnalysis?.root,
      volumeMappings,
    ],
  );

  const handleRenameInput = useCallback(
    (field: keyof RenameFormState) =>
      (event: ChangeEvent<HTMLInputElement>) => {
        const value = event.currentTarget.value;

        if (field === "directory") {
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
          if (field === "pad") {
            return { ...prev, pad: Number(value) };
          }
          if (field === "targetExtension") {
            return { ...prev, targetExtension: value.toLowerCase() };
          }
          return { ...prev, [field]: value } as RenameFormState;
        });
      },
    [],
  );

  const handleUploadInput = useCallback(
    (field: keyof UploadFormState) =>
      (event: ChangeEvent<HTMLInputElement | HTMLSelectElement>) => {
        const value = event.currentTarget.value;
        setUploadForm((prev) => {
          if (field === "mode") {
            if (value === "folder") {
              setUploadError("Folder 模式仍在规划中，当前仅支持 Zip 上传。");
              return { ...prev, mode: "zip" };
            }
            return { ...prev, mode: value as UploadMode };
          }
          return { ...prev, [field]: value } as UploadFormState;
        });
      },
    [],
  );

  const beginAddUploadService = useCallback(() => {
    setUploadAddressError(null);
    setUploadAddressDraft(uploadForm.serviceUrl.trim() || "");
    setIsAddingUploadService(true);
  }, [uploadForm.serviceUrl]);

  const cancelAddUploadService = useCallback(() => {
    setIsAddingUploadService(false);
    setUploadAddressDraft("");
    setUploadAddressError(null);
  }, []);

  const confirmAddUploadService = useCallback(() => {
    const trimmed = uploadAddressDraft.trim();
    if (!trimmed) {
      setUploadAddressError("请输入有效的上传服务器地址。");
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
    setUploadAddressDraft("");
    setIsAddingUploadService(false);
  }, [uploadAddressDraft]);

  const handleParamChange = useCallback(
    (field: keyof JobParamsConfig) =>
      (event: ChangeEvent<HTMLInputElement | HTMLSelectElement>) => {
        const rawValue = event.currentTarget.value;

        setJobParams((prev) => {
          switch (field) {
            case "scale": {
              const numeric = Number(rawValue);
              const normalized = Number.isFinite(numeric) ? Math.min(4, Math.max(1, Math.floor(numeric))) : prev.scale;
              return { ...prev, scale: normalized };
            }
            case "denoise":
              return { ...prev, denoise: rawValue as JobParamsConfig["denoise"] };
            case "model":
              return { ...prev, model: rawValue };
            case "outputFormat":
              return { ...prev, outputFormat: rawValue as JobParamsConfig["outputFormat"] };
            case "jpegQuality": {
              const numeric = Number(rawValue);
              const normalized = Number.isFinite(numeric)
                ? Math.min(100, Math.max(1, Math.round(numeric)))
                : prev.jpegQuality;
              return { ...prev, jpegQuality: normalized };
            }
            case "tileSize": {
              if (rawValue.trim() === "") {
                return { ...prev, tileSize: null };
              }
              const numeric = Number(rawValue);
              if (!Number.isFinite(numeric)) {
                return prev;
              }
              return { ...prev, tileSize: Math.min(1024, Math.max(32, Math.round(numeric))) };
            }
            case "tilePad": {
              if (rawValue.trim() === "") {
                return { ...prev, tilePad: null };
              }
              const numeric = Number(rawValue);
              if (!Number.isFinite(numeric)) {
                return prev;
              }
              return { ...prev, tilePad: Math.min(128, Math.max(0, Math.round(numeric))) };
            }
            case "batchSize": {
              if (rawValue.trim() === "") {
                return { ...prev, batchSize: null };
              }
              const numeric = Number(rawValue);
              if (!Number.isFinite(numeric)) {
                return prev;
              }
              return { ...prev, batchSize: Math.min(16, Math.max(1, Math.round(numeric))) };
            }
            case "device":
              return { ...prev, device: rawValue as JobParamsConfig["device"] };
            default:
              return prev;
          }
        });
      },
    [],
  );

  const handleResetParams = useCallback(() => {
    setJobParams(DEFAULT_JOB_PARAMS);
  }, []);

  const computeFavoriteName = useCallback((params: JobParamsConfig) => {
    const pieces = [`${params.model}`];
    pieces.push(`×${params.scale}`);
    pieces.push(`denoise:${params.denoise}`);
    if (params.outputFormat === "jpg") {
      pieces.push(`jpg@${params.jpegQuality}`);
    } else {
      pieces.push(params.outputFormat);
    }
    return pieces.join(" · ");
  }, []);

  const handleSaveFavorite = useCallback(() => {
    const id = typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : `fav-${Date.now()}`;
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
          item.device === jobParams.device,
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
    [jobParamFavorites],
  );

  const handleRemoveFavorite = useCallback(
    (favoriteId: string) => {
      setJobParamFavorites((prev) => prev.filter((item) => item.id !== favoriteId));
    },
    [],
  );

  const inferManifestForVolume = useCallback(
    (volumeName: string | null | undefined): string | null => {
      if (!renameSummary) {
        return null;
      }

      if (renameSummary.mode === "single") {
        return renameSummary.outcome.manifestPath ?? null;
      }

      if (renameSummary.mode === "multi") {
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
          if (normalized.includes(String(entry.mapping.volumeNumber ?? "")) && normalized.includes(candidate)) {
            return true;
          }
          return false;
        });

        return match?.outcome.manifestPath ?? null;
      }

      return null;
    },
    [renameSummary],
  );

  const mapPayloadToRecord = useCallback(
    (payload: JobEventPayload, overrideMessage?: string | null, existing?: JobRecord): JobRecord => {
      const displayMessage = overrideMessage ?? payload.error ?? payload.message ?? null;
      const transport = payload.transport ?? "system";
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
          existing?.manifestPath ?? inferManifestForVolume(payload.metadata?.volume ?? null),
      };
    },
    [inferManifestForVolume, jobForm.bearerToken, jobForm.inputPath, jobForm.inputType, jobForm.serviceUrl],
  );

  const handleToggleJobSelection = useCallback(
    (jobId: string) => {
      setSelectedJobIds((prev) =>
        prev.includes(jobId) ? prev.filter((id) => id !== jobId) : [...prev, jobId],
      );
    },
    [],
  );

  const handleSelectAllVisible = useCallback(() => {
    setSelectedJobIds(filteredJobs.map((job) => job.jobId));
  }, [filteredJobs]);

  const handleClearSelections = useCallback(() => {
    setSelectedJobIds([]);
  }, []);

  const handleJobSearchChange = useCallback((event: ChangeEvent<HTMLInputElement>) => {
    setJobSearch(event.currentTarget.value);
  }, []);

  const handleJobStatusFilterChange = useCallback(
    (event: ChangeEvent<HTMLSelectElement>) => {
      const value = event.currentTarget.value as "all" | "active" | "completed" | "failed";
      setJobStatusFilter(value);
    },
    [],
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
      const terminal = job.status.toUpperCase() === "SUCCESS" || job.status.toUpperCase() === "FAILED";
      if (!job.serviceUrl || terminal) {
        return;
      }

      const request: Record<string, unknown> = {
        serviceUrl: job.serviceUrl,
        jobId: job.jobId,
      };

      if (job.bearerToken && job.bearerToken.trim()) {
        request.bearerToken = job.bearerToken.trim();
      }

      if (Number.isFinite(jobForm.pollIntervalMs) && jobForm.pollIntervalMs >= 250) {
        request.pollIntervalMs = Math.floor(jobForm.pollIntervalMs);
      }

      try {
        await invoke("watch_manga_job", { request });
      } catch (watchError) {
        if (options?.silent) {
          return;
        }
        const message = watchError instanceof Error ? watchError.message : String(watchError);
        setJobs((prev) =>
          prev.map((item) =>
            item.jobId === job.jobId
              ? {
                  ...item,
                  message: `订阅进度失败：${message}`,
                  transport: "system",
                  error: message,
                  lastUpdated: Date.now(),
                }
              : item,
          ),
        );
      }
    },
    [jobForm.pollIntervalMs],
  );

  const resumeJob = useCallback(
    async (job: JobRecord, options?: { silent?: boolean }) => {
      if (!job.serviceUrl) {
        if (!options?.silent) {
          setJobError("缺少服务地址，无法恢复作业。");
        }
        return;
      }

      if (!options?.silent) {
        setJobStatus(`正在恢复作业 ${job.jobId}…`);
        setJobError(null);
      }

      try {
        const payload = await invoke<JobEventPayload>("resume_manga_job", {
          request: buildJobRequest(job),
        });

        let updatedRecord: JobRecord | null = null;
        setJobs((prev) => {
          const existing = prev.find((item) => item.jobId === payload.jobId) ?? job;
          const mapped = mapPayloadToRecord(payload, undefined, existing);
          updatedRecord = mapped;
          const next = prev.filter((item) => item.jobId !== mapped.jobId);
          next.push(mapped);
          next.sort((a, b) => b.lastUpdated - a.lastUpdated);
          return next;
        });

        if (updatedRecord) {
          void startJobWatcher(updatedRecord, { silent: options?.silent ?? false });
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
    [buildJobRequest, mapPayloadToRecord, startJobWatcher],
  );

  const cancelJob = useCallback(
    async (job: JobRecord, options?: { silent?: boolean }) => {
      if (!job.serviceUrl) {
        if (!options?.silent) {
          setJobError("缺少服务地址，无法终止作业。");
        }
        return;
      }

      if (!options?.silent) {
        setJobStatus(`正在终止作业 ${job.jobId}…`);
        setJobError(null);
      }

      try {
        const payload = await invoke<JobEventPayload>("cancel_manga_job", {
          request: buildJobRequest(job),
        });

        setJobs((prev) => {
          const existing = prev.find((item) => item.jobId === payload.jobId) ?? job;
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
    [buildJobRequest, mapPayloadToRecord],
  );

  const handleResumeJob = useCallback(
    (job: JobRecord) => {
      void resumeJob(job);
    },
    [resumeJob],
  );

  const handleCancelJob = useCallback(
    (job: JobRecord) => {
      void cancelJob(job);
    },
    [cancelJob],
  );

  const promptForDirectory = useCallback(
    async (title: string, defaultPath?: string | null) => {
      const selection = await invoke<string | string[] | null>("plugin:dialog|open", {
        options: {
          directory: true,
          multiple: false,
          defaultPath: defaultPath ?? undefined,
          title,
        },
      });

      if (!selection) {
        return null;
      }
      const first = Array.isArray(selection) ? selection[0] : selection;
      return typeof first === "string" && first.length > 0 ? first : null;
    },
    [],
  );

  const promptForManifest = useCallback(async () => {
    const selection = await invoke<string | string[] | null>("plugin:dialog|open", {
      options: {
        filters: [{ name: "Manifest", extensions: ["json"] }],
        title: "选择 manifest.json",
        multiple: false,
      },
    });

    if (!selection) {
      return null;
    }
    const first = Array.isArray(selection) ? selection[0] : selection;
    return typeof first === "string" && first.length > 0 ? first : null;
  }, []);

  const downloadArtifactZip = useCallback(
    async (job: JobRecord, options?: { silent?: boolean; targetDir?: string | null }) => {
      const silent = options?.silent ?? false;

      if (!job.serviceUrl || !job.jobId) {
        if (!silent) {
          setArtifactError("缺少必要信息，无法下载产物。");
        }
        return false;
      }

      if (!job.artifactPath) {
        if (!silent) {
          setArtifactError("远端尚未提供产物路径。");
        }
        return false;
      }

      let targetDir = options?.targetDir ?? null;
      if (!targetDir) {
        const picked = await promptForDirectory("选择产物输出目录", artifactTargetRoot);
        if (!picked) {
          if (!silent) {
            setArtifactError("已取消选择输出目录。");
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

        const summary = await invoke<ArtifactDownloadSummary>("download_manga_artifact", {
          request,
        });

        setArtifactTargetRoot(targetDir);

        setArtifactDownloads((prev) => {
          const next = [summary, ...prev.filter((item) => item.jobId !== summary.jobId)];
          return next.slice(0, 10);
        });

        setJobs((prev) =>
          prev.map((item) =>
            item.jobId === job.jobId ? { ...item, artifactHash: summary.hash } : item,
          ),
        );

        if (!silent) {
          const warningNote =
            summary.warnings.length > 0 ? `；注意：${summary.warnings[0]}` : "";
          setJobStatus(
            `ZIP 下载完成（${summary.jobId}），共 ${summary.fileCount} 张，输出：${summary.archivePath}${warningNote}`,
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
    [artifactTargetRoot, buildJobRequest, promptForDirectory],
  );

  const validateArtifact = useCallback(
    async (
      job: JobRecord,
      options?: { silent?: boolean; targetDir?: string | null; manifestPathOverride?: string | null },
    ) => {
      const silent = options?.silent ?? false;

      if (!job.serviceUrl || !job.jobId) {
        if (!silent) {
          setArtifactError("缺少必要信息，无法校验产物。");
        }
        return false;
      }

      if (!job.artifactPath) {
        if (!silent) {
          setArtifactError("远端尚未提供产物路径。");
        }
        return false;
      }

      let targetDir = options?.targetDir ?? null;
      if (!targetDir) {
        const picked = await promptForDirectory("选择校验输出目录", artifactTargetRoot);
        if (!picked) {
          if (!silent) {
            setArtifactError("已取消选择输出目录。");
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

        const report = await invoke<ArtifactReport>("validate_manga_artifact", { request });

        setArtifactReports((prev) => {
          const next = [report, ...prev.filter((item) => item.jobId !== report.jobId)];
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
          }),
        );

        if (!silent) {
          const warningNote =
            report.warnings.length > 0 ? `；注意：${report.warnings[0]}` : "";
          setJobStatus(
            `校验完成（${report.jobId}）：匹配 ${report.summary.matched} / ${report.summary.totalManifest}${warningNote}`,
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
      artifactTargetRoot,
      buildJobRequest,
      inferManifestForVolume,
      promptForDirectory,
      promptForManifest,
    ],
  );

  const handleDownloadArtifact = useCallback(
    (job: JobRecord) => {
      void downloadArtifactZip(job);
    },
    [downloadArtifactZip],
  );

  const handleValidateArtifact = useCallback(
    (job: JobRecord) => {
      void validateArtifact(job);
    },
    [validateArtifact],
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
      setJobStatus("选中的作业暂无可下载的产物。");
      return;
    }

    let targetDir = artifactTargetRoot;
    if (!targetDir) {
      targetDir = await promptForDirectory("选择批量下载目录", artifactTargetRoot);
      if (!targetDir) {
        return;
      }
    }

    let success = 0;
    for (const job of ready) {
      const ok = await downloadArtifactZip(job, { silent: true, targetDir });
      if (ok) {
        success += 1;
      }
    }

    if (success === 0) {
      setJobStatus("批量下载未成功，请检查作业状态。");
      return;
    }

    const skipped = selected.length - ready.length;
    const summaryParts = [`已触发 ${success} 项 ZIP 下载。`];
    if (skipped > 0) {
      summaryParts.push(`跳过 ${skipped} 项尚未生成产物的作业。`);
    }
    setJobStatus(summaryParts.join(' '));
  }, [artifactTargetRoot, downloadArtifactZip, jobs, promptForDirectory, selectedJobIds]);

  const handleBatchValidate = useCallback(async () => {
    const selected = jobs.filter((job) => selectedJobIds.includes(job.jobId));
    const ready = selected.filter((job) => !!job.artifactPath);

    if (ready.length === 0) {
      setJobStatus("选中的作业暂无可校验的产物。");
      return;
    }

    let targetDir = artifactTargetRoot;
    if (!targetDir) {
      targetDir = await promptForDirectory("选择批量校验目录", artifactTargetRoot);
      if (!targetDir) {
        return;
      }
    }

    let success = 0;
    for (const job of ready) {
      const ok = await validateArtifact(job, { silent: true, targetDir });
      if (ok) {
        success += 1;
      }
    }

    if (success === 0) {
      setJobStatus("批量校验未成功，请检查作业状态。");
      return;
    }

    const skipped = selected.length - ready.length;
    const summaryParts = [`已触发 ${success} 项校验。`];
    if (skipped > 0) {
      summaryParts.push(`跳过 ${skipped} 项尚未生成产物的作业。`);
    }
    setJobStatus(summaryParts.join(' '));
  }, [artifactTargetRoot, jobs, promptForDirectory, selectedJobIds, validateArtifact]);

  const handleUpload = useCallback(async () => {
    if (!renameForm.directory) {
      setUploadError("请先完成重命名预览或执行，确保本地目录已确认。");
      return;
    }
    const serviceUrl = uploadForm.serviceUrl.trim();
    if (!serviceUrl) {
      setUploadError("请填写 Copyparty 服务地址。");
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

      let localPath = renameForm.directory;

      if (isMultiVolumeSource) {
        if (!selectedVolume) {
          setUploadError("请选择要上传的卷，并在卷映射步骤中确认。");
          return;
        }

        if (
          !renameSummary ||
          renameSummary.mode !== "multi" ||
          !renameSummary.volumes.some((item) => item.mapping.directory === selectedVolume.directory)
        ) {
          setUploadError("请先对所选卷执行重命名预览或执行，生成 manifest。");
          return;
        }

        localPath = selectedVolume.directory;
      } else if (renameSummary?.mode === "single") {
        localPath = renameSummary.outcome.directory;
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

      const result = await invoke<UploadOutcome>("upload_copyparty", {
        request,
      });

      const sizeInMb = (result.uploadedBytes / (1024 * 1024)).toFixed(2);
      setLastUploadRemotePath(remotePath);
      setJobForm((prev) => ({
        ...prev,
        serviceUrl: serviceUrl || prev.serviceUrl,
        bearerToken: trimmedToken || prev.bearerToken,
        title: trimmedTitle || prev.title,
        volume: trimmedVolume || prev.volume,
        inputPath: remotePath,
      }));
      setUploadStatus(
        `上传完成：${result.fileCount} 个文件，约 ${sizeInMb} MB，remote = ${result.remoteUrl}`,
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
    uploadForm,
  ]);

  const beginAddJobService = useCallback(() => {
    setJobAddressError(null);
    setJobAddressDraft(jobForm.serviceUrl.trim() || "");
    setIsAddingJobService(true);
  }, [jobForm.serviceUrl]);

  const cancelAddJobService = useCallback(() => {
    setIsAddingJobService(false);
    setJobAddressDraft("");
    setJobAddressError(null);
  }, []);

  const confirmAddJobService = useCallback(() => {
    const trimmed = jobAddressDraft.trim();
    if (!trimmed) {
      setJobAddressError("请输入有效的推理服务器地址。");
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
    setJobAddressDraft("");
    setIsAddingJobService(false);
  }, [jobAddressDraft]);

  const handleJobInput = useCallback(
    (field: keyof JobFormState) =>
      (event: ChangeEvent<HTMLInputElement | HTMLSelectElement>) => {
        const value = event.currentTarget.value;
        setJobForm((prev) => {
          if (field === "pollIntervalMs") {
            const numeric = Number(value);
            const normalized = Number.isFinite(numeric) ? Math.max(250, Math.floor(numeric)) : DEFAULT_POLL_INTERVAL;
            return { ...prev, pollIntervalMs: normalized };
          }
          if (field === "inputType") {
            return { ...prev, inputType: value as JobFormState["inputType"] };
          }
          return { ...prev, [field]: value } as JobFormState;
        });
    },
    [],
  );

  const stepDescriptors = useMemo(() => {
    const steps: StepDescriptor[] = [{ id: "source", label: "选择源目录" }];
    if (isMultiVolumeSource) {
      steps.push({ id: "volumes", label: "卷映射确认" });
    }
    steps.push(
      { id: "rename", label: "图片重命名" },
      { id: "upload", label: "上传到 Copyparty" },
      { id: "jobs", label: "远端推理" },
    );
    return steps;
  }, [isMultiVolumeSource]);

  const stepOrder = useMemo(() => stepDescriptors.map((item) => item.id), [stepDescriptors]);

  const stepIndexMap = useMemo(() => {
    const mapping = new Map<StepId, number>();
    stepOrder.forEach((id, index) => {
      mapping.set(id, index + 1);
    });
    return mapping;
  }, [stepOrder]);

  useEffect(() => {
    if (!stepOrder.includes(currentStep)) {
      setCurrentStep(stepOrder[0] ?? "source");
    }
  }, [currentStep, stepOrder]);

  const goToStep = useCallback(
    (step: StepId) => {
      if (stepOrder.includes(step)) {
        setCurrentStep(step);
      }
    },
    [stepOrder],
  );

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
    (index: number) =>
      (event: ChangeEvent<HTMLInputElement>) => {
        const rawValue = event.currentTarget.value;
        const numeric = Number(rawValue);
        const normalized =
          rawValue.trim() === "" || !Number.isFinite(numeric)
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
    [],
  );

  const handleVolumeNameChange = useCallback(
    (index: number) =>
      (event: ChangeEvent<HTMLInputElement>) => {
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
    [],
  );

  const handleConfirmMapping = useCallback(() => {
    if (!isMultiVolumeSource) {
      setMappingConfirmed(true);
      setVolumeMappingError(null);
      goToNextStep();
      return;
    }

    if (volumeMappings.length === 0) {
      setVolumeMappingError("未检测到任何卷目录，请返回上一步检查源目录。");
      return;
    }

    const missingNumber = volumeMappings.some((item) => item.volumeNumber === null);
    if (missingNumber) {
      setVolumeMappingError("请为每一卷填写卷号。");
      return;
    }

    const seen = new Set<number>();
    for (const mapping of volumeMappings) {
      if (mapping.volumeNumber === null) {
        continue;
      }
      if (seen.has(mapping.volumeNumber)) {
        setVolumeMappingError("卷号不能重复，请调整后再确认。");
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
      serviceUrl: trimmedService || prev.serviceUrl,
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

    if (!serviceUrl) {
      setJobError("请填写推理服务地址。");
      return;
    }
    if (!title) {
      setJobError("请填写作品名，以便在远端区分作业。");
      return;
    }
    if (!volume) {
      setJobError("请填写卷名，或至少提供批次标识。");
      return;
    }
    if (!inputPath) {
      setJobError("请提供远端输入路径（例如 incoming/volume.zip）。");
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

      const submission = await invoke<JobSubmission>("create_manga_job", { options });

      const initialRecord: JobRecord = {
        jobId: submission.jobId,
        status: "PENDING",
        processed: 0,
        total: 0,
        artifactPath: null,
        message: "作业已提交，等待远端处理。",
        transport: "system",
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

      setJobs((prev) => [initialRecord, ...prev.filter((item) => item.jobId !== initialRecord.jobId)]);
      setJobStatus(`作业 ${submission.jobId} 已创建，正在等待进度更新。`);

      void startJobWatcher(initialRecord);
    } catch (error) {
      setJobError(error instanceof Error ? error.message : String(error));
    } finally {
      setJobLoading(false);
    }
  }, [inferManifestForVolume, jobForm, jobParams, startJobWatcher]);

  const describeStatus = (status: string) => {
    switch (status.toUpperCase()) {
      case "PENDING":
        return "排队中";
      case "RUNNING":
        return "运行中";
      case "SUCCESS":
        return "已完成";
      case "FAILED":
        return "失败";
      case "ERROR":
        return "错误";
      default:
        return status;
    }
  };

  const statusTone = (status: string) => {
    switch (status.toUpperCase()) {
      case "SUCCESS":
        return "success";
      case "FAILED":
      case "ERROR":
        return "error";
      case "RUNNING":
        return "info";
      default:
        return "neutral";
    }
  };

  const describeTransport = (transport: JobEventTransport) => {
    switch (transport) {
      case "websocket":
        return "WebSocket";
      case "polling":
        return "轮询";
      case "system":
      default:
        return "系统";
    }
  };

  const describeUploadStage = (progress: UploadProgressPayload) => {
    switch (progress.stage) {
      case "preparing":
        return "准备中";
      case "uploading":
        return "上传中";
      case "finalizing":
        return "确认中";
      case "completed":
        return "完成";
      case "failed":
        return "失败";
      default:
        return progress.stage;
    }
  };

  const uploadPercent = useMemo(() => {
    if (!uploadProgress) {
      return null;
    }
    if (uploadProgress.stage === "preparing" && uploadProgress.totalFiles > 0) {
      return Math.round((uploadProgress.processedFiles / uploadProgress.totalFiles) * 100);
    }
    if (uploadProgress.stage === "uploading" && uploadProgress.totalBytes > 0) {
      return Math.round((uploadProgress.transferredBytes / uploadProgress.totalBytes) * 100);
    }
    if (uploadProgress.stage === "completed") {
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
              ? "active"
              : index !== -1 && currentIndex !== -1 && index < currentIndex
              ? "completed"
              : "";
          const stepNumber = stepIndexMap.get(descriptor.id) ?? index + 1;
          return (
            <div key={descriptor.id} className={`stepper-nav-item ${status}`}>
              <span className="step-index">步骤 {stepNumber}</span>
              <span className="step-label">{descriptor.label}</span>
            </div>
          );
        })}
      </div>

      {currentStep === "source" && (
        <section className="step-card" aria-label="选择源目录">
          <header className="step-card-header">
            <span className="step-index">步骤 {stepIndexMap.get("source") ?? 1}</span>
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
                  onChange={handleRenameInput("directory")}
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
          {analysisError && <p className="status status-error">{analysisError}</p>}

          {sourceAnalysis && (
            <div className="analysis-panel">
              <p>
                检测结果：
                {sourceAnalysis.mode === "multiVolume" ? "多卷目录" : "单卷目录"}；
                根目录图片 {sourceAnalysis.rootImageCount} 张，总计 {sourceAnalysis.totalImages} 张。
              </p>

              {sourceAnalysis.mode === "multiVolume" && sourceAnalysis.volumeCandidates.length > 0 && (
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
                        <td>{candidate.detectedNumber ?? "-"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}

              {sourceAnalysis.skippedEntries.length > 0 && (
                <details>
                  <summary>忽略的条目 ({sourceAnalysis.skippedEntries.length})</summary>
                  <ul>
                    {sourceAnalysis.skippedEntries.slice(0, 10).map((entry) => (
                      <li key={entry}>{entry}</li>
                    ))}
                    {sourceAnalysis.skippedEntries.length > 10 && (
                      <li>其余 {sourceAnalysis.skippedEntries.length - 10} 条略。</li>
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

      {currentStep === "volumes" && isMultiVolumeSource && (
        <section className="step-card" aria-label="卷映射确认">
          <header className="step-card-header">
            <span className="step-index">步骤 {stepIndexMap.get("volumes") ?? 2}</span>
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
                        value={mapping.volumeNumber ?? ""}
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

          {volumeMappingError && <p className="status status-error">{volumeMappingError}</p>}

          <div className="button-row">
            <button type="button" className="primary" onClick={handleConfirmMapping}>
              确认映射并继续
            </button>
          </div>
        </section>
      )}

      {currentStep === "rename" && (
        <section className="step-card" aria-label="图片重命名">
          <header className="step-card-header">
            <span className="step-index">步骤 {stepIndexMap.get("rename") ?? 1}</span>
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
                onChange={handleRenameInput("pad")}
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">目标扩展名</span>
              <input
                type="text"
                value={renameForm.targetExtension}
                onChange={handleRenameInput("targetExtension")}
                maxLength={8}
              />
            </label>
          </div>

          <div className="button-row">
            <button
              type="button"
              className="primary"
              onClick={() => runRename(true)}
              disabled={renameLoading}
            >
              {renameLoading ? "处理中…" : "预览重命名"}
            </button>

            <button
              type="button"
              onClick={() => runRename(false)}
              disabled={renameLoading}
            >
              {renameLoading ? "处理中…" : "执行重命名"}
            </button>
          </div>

          {renameError && <p className="status status-error">{renameError}</p>}

          {renameSummary && renameSummary.mode === "single" && (
            <div className="preview-panel">
              <div className="preview-header">
                <strong>
                  {renameSummary.outcome.entries.length} 个文件
                  {renameSummary.outcome.dryRun ? "（预览）" : "（已重命名）"}
                </strong>
                {renameSummary.outcome.manifestPath && (
                  <span className="manifest-path">manifest: {renameSummary.outcome.manifestPath}</span>
                )}
              </div>

              {renameSummary.outcome.warnings.length > 0 && (
                <ul className="status status-warning">
                  {renameSummary.outcome.warnings.map((warning) => (
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
                  其余 {renameSummary.outcome.entries.length - previewEntries.length} 条略。
                </p>
              )}
            </div>
          )}

          {renameSummary && renameSummary.mode === "multi" && (
            <div className="preview-panel multi">
              {renameSummary.volumes.map(({ mapping, outcome }) => (
                <div key={mapping.directory} className="volume-preview">
                  <header>
                    <strong>
                      卷 {mapping.volumeNumber}: {mapping.volumeName}
                    </strong>
                    <span>
                      {outcome.entries.length} 个文件
                      {renameSummary.dryRun ? "（预览）" : "（已重命名）"}
                    </span>
                    {outcome.manifestPath && (
                      <span className="manifest-path">manifest: {outcome.manifestPath}</span>
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
                        <tr key={`${mapping.directory}-${entry.originalName}-${entry.renamedName}`}>
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

      {currentStep === "upload" && (
        <section className="step-card" aria-label="步骤 2 上传 Copyparty">
        <header className="step-card-header">
          <span className="step-index">步骤 {stepIndexMap.get("upload") ?? 1}</span>
          <h3>上传到 Copyparty</h3>
          <p>将整理后的目录打包为 zip 上传，并附带作品信息。</p>
        </header>

        <div className="form-grid">
          <label className="form-field">
            <span className="field-label">服务地址</span>
            <div className="select-with-button">
              <select value={uploadForm.serviceUrl} onChange={handleUploadInput("serviceUrl")}>
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
                <button type="button" className="primary" onClick={confirmAddUploadService}>
                  保存
                </button>
                <button type="button" className="ghost" onClick={cancelAddUploadService}>
                  取消
                </button>
              </div>
            )}
            {uploadAddressError && <p className="field-error">{uploadAddressError}</p>}
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
            <small>无需手动填写，上传时会使用该路径并自动在远端推理步骤中填入。</small>
          </label>

          <label className="form-field">
            <span className="field-label">Bearer Token（可选）</span>
            <input
              type="text"
              value={uploadForm.bearerToken}
              onChange={handleUploadInput("bearerToken")}
              placeholder="xxxxxx"
            />
          </label>

          <label className="form-field compact">
            <span className="field-label">作品名（可选）</span>
            <input
              type="text"
              value={uploadForm.title}
              onChange={handleUploadInput("title")}
            />
          </label>

          <label className="form-field compact">
            <span className="field-label">卷名（可选）</span>
            <input
              type="text"
              value={uploadForm.volume}
              onChange={handleUploadInput("volume")}
            />
          </label>

          {isMultiVolumeSource && volumeMappings.length > 0 && (
            <label className="form-field compact">
              <span className="field-label">上传卷</span>
              <select
                value={selectedVolumeKey ?? ""}
                onChange={(event) => handleSelectVolume(event.currentTarget.value)}
              >
                {volumeMappings.map((mapping) => (
                  <option key={mapping.directory} value={mapping.directory}>
                    卷 {mapping.volumeNumber ?? "-"} · {mapping.volumeName}
                  </option>
                ))}
              </select>
            </label>
          )}

          <label className="form-field compact">
            <span className="field-label">模式</span>
            <select
              value={uploadForm.mode}
              onChange={handleUploadInput("mode")}
            >
              <option value="zip">Zip</option>
              <option value="folder">Folder（规划中）</option>
            </select>
          </label>
        </div>

        <div className="button-row">
          <button type="button" className="primary" onClick={handleUpload} disabled={uploadLoading}>
            {uploadLoading ? "上传中…" : "上传到 Copyparty"}
          </button>
        </div>

        {uploadError && <p className="status status-error">{uploadError}</p>}
        {uploadStatus && <p className="status status-success">{uploadStatus}</p>}
        {uploadProgress && (
          <div className="upload-progress">
            <div className="upload-progress-header">
              <span
                className={`status-chip status-${
                  uploadProgress.stage === "failed"
                    ? "error"
                    : uploadProgress.stage === "completed"
                      ? "success"
                      : "info"
                }`}
              >
                {describeUploadStage(uploadProgress)}
              </span>
              {uploadPercent != null && (
                <span className="progress-label">{uploadPercent}%</span>
              )}
            </div>
            <div className="progress-bar">
              <span style={{ width: `${Math.max(0, Math.min(uploadPercent ?? 0, 100))}%` }} />
            </div>
            {(uploadProgress.message || uploadProgress.totalBytes > 0) && (
              <p className="progress-hint">
                {uploadProgress.message ??
                  `${(uploadProgress.transferredBytes / (1024 * 1024)).toFixed(2)} / ${(uploadProgress.totalBytes / (1024 * 1024)).toFixed(2)} MB`}
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

      {currentStep === "jobs" && (
        <section className="step-card" aria-label="远端推理">
        <header className="step-card-header">
          <span className="step-index">步骤 {stepIndexMap.get("jobs") ?? 1}</span>
          <h3>远端推理</h3>
          <p>提交 FastAPI 推理作业并实时查看进度。</p>
        </header>

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
                onChange={handleParamChange("model")}
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
                onChange={handleParamChange("scale")}
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">降噪等级</span>
              <select value={jobParams.denoise} onChange={handleParamChange("denoise")}>
                <option value="low">Low</option>
                <option value="medium">Medium</option>
                <option value="high">High</option>
              </select>
            </label>

            <label className="form-field compact">
              <span className="field-label">输出格式</span>
              <select value={jobParams.outputFormat} onChange={handleParamChange("outputFormat")}>
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
                onChange={handleParamChange("jpegQuality")}
                disabled={jobParams.outputFormat !== "jpg"}
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">Tile 尺寸</span>
              <input
                type="number"
                min={32}
                max={1024}
                value={jobParams.tileSize ?? ""}
                onChange={handleParamChange("tileSize")}
                placeholder="留空表示自动"
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">Tile Pad</span>
              <input
                type="number"
                min={0}
                max={128}
                value={jobParams.tilePad ?? ""}
                onChange={handleParamChange("tilePad")}
                placeholder="留空表示自动"
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">Batch Size</span>
              <input
                type="number"
                min={1}
                max={16}
                value={jobParams.batchSize ?? ""}
                onChange={handleParamChange("batchSize")}
                placeholder="留空表示自动"
              />
            </label>

            <label className="form-field compact">
              <span className="field-label">设备偏好</span>
              <select value={jobParams.device} onChange={handleParamChange("device")}>
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
                    <button type="button" onClick={() => handleApplyFavorite(favorite.id)}>
                      {favorite.name}
                    </button>
                    <span className="favorite-meta">{new Date(favorite.createdAt).toLocaleString()}</span>
                    <button type="button" className="ghost" onClick={() => handleRemoveFavorite(favorite.id)}>
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
            <select id="job-status-filter" value={jobStatusFilter} onChange={handleJobStatusFilterChange}>
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
              <select value={jobForm.serviceUrl} onChange={handleJobInput("serviceUrl")}>
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
                <button type="button" className="primary" onClick={confirmAddJobService}>
                  保存
                </button>
                <button type="button" className="ghost" onClick={cancelAddJobService}>
                  取消
                </button>
              </div>
            )}
            {jobAddressError && <p className="field-error">{jobAddressError}</p>}
          </label>

          <label className="form-field">
            <span className="field-label">Bearer Token（可选）</span>
            <input
              type="text"
              value={jobForm.bearerToken}
              onChange={handleJobInput("bearerToken")}
              placeholder="xxxxxx"
            />
          </label>

          <label className="form-field">
            <span className="field-label">作品名</span>
            <input
              type="text"
              value={jobForm.title}
              onChange={handleJobInput("title")}
              placeholder="作品名"
            />
          </label>

          <label className="form-field compact">
            <span className="field-label">卷名</span>
            <input
              type="text"
              value={jobForm.volume}
              onChange={handleJobInput("volume")}
              placeholder="第 1 卷"
            />
          </label>

          <label className="form-field compact">
            <span className="field-label">输入类型</span>
            <select value={jobForm.inputType} onChange={handleJobInput("inputType")}>
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
              onChange={handleJobInput("pollIntervalMs")}
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
            {jobLoading ? "提交中…" : "创建推理作业"}
          </button>
        </div>

        {jobError && <p className="status status-error">{jobError}</p>}
        {jobStatus && <p className="status status-success">{jobStatus}</p>}
        {artifactError && <p className="status status-error">{artifactError}</p>}

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
                  const percent = job.total > 0 ? Math.min(100, Math.round((job.processed / job.total) * 100)) : null;
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
                        <span className={`status-chip status-${statusTone(job.status)}`}>
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
                        <span className="transport-badge">{describeTransport(job.transport)}</span>
                      </td>
                      <td className="message-cell">
                        {job.message ? job.message : "-"}
                      </td>
                      <td className="retry-cell">{job.retries ?? 0}</td>
                      <td className="error-cell">{job.lastError ?? "-"}</td>
                      <td className="artifact-cell">
                        {job.artifactPath ? (
                          <div className="artifact-info">
                            <span className="artifact-path" title={job.artifactPath}>
                              {job.artifactPath}
                            </span>
                            {job.artifactHash && (
                              <span className="artifact-hash" title={job.artifactHash}>
                                hash: {job.artifactHash.slice(0, 8)}…
                              </span>
                            )}
                          </div>
                        ) : (
                          "-"
                        )}
                      </td>
                      <td className="actions-cell">
                        <button type="button" onClick={() => handleResumeJob(job)}>
                          恢复
                        </button>
                        <button type="button" onClick={() => handleCancelJob(job)}>
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
                          {artifactDownloadBusyJob === job.jobId ? "下载中…" : "下载 ZIP"}
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
                          {artifactValidateBusyJob === job.jobId ? "校验中…" : "校验"}
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
                    <span className="report-status">文件数：{summary.fileCount}</span>
                    <span className="report-path">ZIP：{summary.archivePath}</span>
                  </div>
                  <div className="report-details">
                    <span>解压目录：{summary.extractPath}</span>
                    {summary.warnings.length > 0 && (
                      <span className="report-warning">{summary.warnings[0]}</span>
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
                      匹配 {report.summary.matched} / {report.summary.totalManifest}
                    </span>
                    <span className="report-path">输出：{report.extractPath}</span>
                    {report.archivePath && (
                      <span className="report-archive">ZIP：{report.archivePath}</span>
                    )}
                  </div>
                  <div className="report-details">
                    <span>缺失 {report.summary.missing}</span>
                    <span>多余 {report.summary.extra}</span>
                    <span>哈希不一致 {report.summary.mismatched}</span>
                    {report.warnings.length > 0 && (
                      <span className="report-warning">{report.warnings[0]}</span>
                    )}
                  </div>
                  {report.reportPath && <span className="report-file">报告：{report.reportPath}</span>}
                </li>
              ))}
            </ul>
          </div>
        )}

        <div className="wizard-controls">
          <button type="button" onClick={goToPreviousStep}>
            上一步
          </button>
        </div>
      </section>
      )}
    </div>
  );
};

export default MangaUpscaleAgent;
