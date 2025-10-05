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
}
