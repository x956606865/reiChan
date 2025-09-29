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
}

export type DryRunReport = {
  total: number
  ok: number
  failed: number
  errors: { rowIndex: number; message: string }[]
}

