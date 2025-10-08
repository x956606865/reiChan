export type DatabaseBrief = {
  id: string
  title: string
  icon?: string | null
}

export type DatabaseProperty = {
  name: string
  type: string
  required?: boolean | null
  options?: string[] | null
}

export type DatabaseSchema = {
  id: string
  title: string
  properties: DatabaseProperty[]
}

export type FieldMapping = {
  include: boolean
  sourceField: string
  targetProperty: string
  targetType: string
  transformCode?: string
}

export type UpsertStrategy = 'skip' | 'overwrite' | 'merge'

export type ImportUpsertConfig = {
  dedupeKeys: string[]
  strategy: UpsertStrategy
  conflictColumns?: string[]
}

export type ConflictType = 'skip' | 'overwrite' | 'merge' | 'unknown'

export type ImportTemplate = {
  id?: string | null
  name: string
  tokenId: string
  databaseId: string
  mappings: FieldMapping[]
  defaults?: Record<string, unknown>
}

export type DryRunInput = {
  schema: DatabaseSchema
  mappings: FieldMapping[]
  records: unknown[]
  defaults?: Record<string, unknown>
}

export type DryRunErrorKind = 'transform' | 'mapping' | 'validation'

export type DryRunReport = {
  total: number
  ok: number
  failed: number
  errors: { rowIndex: number; message: string; kind: DryRunErrorKind }[]
}

export type PreviewRequest = {
  path: string
  fileType?: string
  limitRows?: number
  limitBytes?: number
}

export type PreviewResponse = {
  fields: string[]
  records: unknown[]
}

export type TransformEvalRequest = {
  code: string
  value: unknown
  record: unknown
  rowIndex: number
}

export type TransformEvalResult = {
  result: unknown
}

export type ImportJobDraft = {
  tokenId: string
  databaseId: string
  sourceFilePath: string
  fileType: string
  fields: string[]
  previewRecords: unknown[]
  mappings: FieldMapping[]
  defaults?: Record<string, unknown>
  priority?: number
  upsert?: ImportUpsertConfig
}

export type JobState =
  | 'Pending'
  | 'Queued'
  | 'Running'
  | 'Paused'
  | 'Completed'
  | 'Failed'
  | 'Canceled'

export type JobProgress = {
  total?: number | null
  done: number
  failed: number
  skipped: number
  conflictTotal?: number
}

export type RowErrorSummary = {
  rowIndex: number
  errorCode?: string | null
  errorMessage: string
  conflictType?: ConflictType | null
}

export type ImportProgressEvent = {
  jobId: string
  state: JobState
  progress: JobProgress
  rps?: number | null
  recentErrors: RowErrorSummary[]
  priority?: number
  leaseExpiresAt?: number | null
  timestamp: number
}

export type ImportLogLevel = 'info' | 'warn' | 'error'

export type ImportLogEvent = {
  jobId: string
  level: ImportLogLevel
  message: string
  timestamp: number
}

export type ImportDoneEvent = {
  jobId: string
  state: JobState
  progress: JobProgress
  rps?: number | null
  finishedAt: number
  lastError?: string | null
  priority?: number
  conflictTotal?: number
}

export type ImportJobSummary = {
  jobId: string
  state: JobState
  progress: JobProgress
  priority?: number
  leaseExpiresAt?: number | null
  tokenId?: string | null
  databaseId?: string | null
  createdAt?: number | null
  startedAt?: number | null
  endedAt?: number | null
  lastError?: string | null
  rps?: number | null
}

export type ImportJobHandle = {
  jobId: string
  state: JobState
}

export type ExportFailedResult = {
  jobId: string
  path: string
  total: number
}

export type ImportQueueSnapshot = {
  running: ImportJobSummary[]
  waiting: ImportJobSummary[]
  paused: ImportJobSummary[]
  timestamp: number
}
