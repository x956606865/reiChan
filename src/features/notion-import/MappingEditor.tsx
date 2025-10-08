import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type {
  DatabaseSchema,
  FieldMapping,
  ImportTemplate,
  DryRunInput,
  DryRunReport,
  TransformEvalResult,
  ImportJobDraft,
  DryRunErrorKind,
  UpsertStrategy,
} from './types'

import { useNotionImportRunboard } from './runboardStore'

const DEFAULT_TRANSFORM = `function transform(value, ctx) {
  return value
}`

const DEFAULT_TEMPLATE_NAME = '模板 1'

type TransformEditorState = {
  index: number
  code: string
  sampleIndex: number
  testing: boolean
  result?: string
  error?: string
}

type Props = {
  tokenId: string
  databaseId: string
  sourceFilePath?: string
  fileType?: string
  previewFields: string[]
  previewRecords: unknown[]
  draft: ImportJobDraft | null
  onDraftChange?: (draft: ImportJobDraft | null) => void
  onStartImport?: (draft: ImportJobDraft) => void
  onPrev: () => void
}

export default function MappingEditor(props: Props) {
  const { tokenId, databaseId, previewFields, previewRecords, sourceFilePath, fileType, draft, onDraftChange, onStartImport, onPrev } = props

  const { starting } = useNotionImportRunboard((state) => ({ starting: state.starting }))
  const activeJobState = useNotionImportRunboard((state) => state.job?.state)
  const hasRunningJob = activeJobState !== undefined && !['Completed', 'Failed', 'Canceled'].includes(activeJobState)

  const [schema, setSchema] = useState<DatabaseSchema | null>(null)
  const [loadingSchema, setLoadingSchema] = useState(false)
  const [schemaError, setSchemaError] = useState<string | null>(null)

  const [mappings, setMappings] = useState<FieldMapping[]>(draft?.mappings ?? [])
  const [tplName, setTplName] = useState(DEFAULT_TEMPLATE_NAME)
  const [templates, setTemplates] = useState<ImportTemplate[]>([])
  const [savingTemplate, setSavingTemplate] = useState(false)
  const [deletingTemplateId, setDeletingTemplateId] = useState<string | null>(null)

  const [tplSourceFields, setTplSourceFields] = useState<string[]>([])

  const [defaultsJson, setDefaultsJson] = useState(() => JSON.stringify(draft?.defaults ?? {}, null, 2))
  const [defaultsError, setDefaultsError] = useState<string | null>(null)

  const [dryRunLoading, setDryRunLoading] = useState(false)
  const [dryRunReport, setDryRunReport] = useState<DryRunReport | null>(null)

  const [transformEditor, setTransformEditor] = useState<TransformEditorState | null>(null)

  const [upsertEnabled, setUpsertEnabled] = useState<boolean>(Boolean(draft?.upsert))
  const [upsertStrategy, setUpsertStrategy] = useState<UpsertStrategy>(draft?.upsert?.strategy ?? 'skip')
  const [upsertKey, setUpsertKey] = useState<string>(draft?.upsert?.dedupeKey ?? '')
  const [conflictColumns, setConflictColumns] = useState<string[]>(draft?.upsert?.conflictColumns ?? [])
  const [upsertError, setUpsertError] = useState<string | null>(null)
  const [draftFingerprint, setDraftFingerprint] = useState<string | null>(null)

  const hasSamples = previewRecords.length > 0
  const upsertInvalid = upsertEnabled && !upsertKey

useEffect(() => {
  if (upsertInvalid) {
    setUpsertError('Upsert 已开启，请选择唯一键字段。')
  } else {
    setUpsertError(null)
  }
}, [upsertInvalid])

  const propertyMap = useMemo(() => {
    const map = new Map<string, DatabaseSchema['properties'][number]>()
    schema?.properties.forEach((prop) => {
      map.set(prop.name, prop)
    })
    return map
  }, [schema])

  const loadSchema = useCallback(async () => {
    if (!tokenId || !databaseId) return
    try {
      setLoadingSchema(true)
      setSchemaError(null)
      const loaded = await invoke<DatabaseSchema>('notion_get_database', { tokenId, databaseId })
      setSchema(loaded)
    } catch (err) {
      setSchemaError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoadingSchema(false)
    }
  }, [tokenId, databaseId])

  const loadTemplates = useCallback(async () => {
    if (!tokenId) return
    try {
      const list = await invoke<ImportTemplate[]>('notion_template_list', { tokenId })
      setTemplates(list.filter((tpl) => tpl.databaseId === databaseId))
    } catch (err) {
      // 静默失败，UI 上不强提示
      console.warn('failed to load templates', err)
    }
  }, [tokenId, databaseId])

  useEffect(() => {
    loadSchema()
    loadTemplates()
  }, [loadSchema, loadTemplates])

  useEffect(() => {
    if (draft && draft.mappings.length > 0 && mappings.length === 0) {
      setMappings(draft.mappings)
      if (draft.defaults) {
        setDefaultsJson(JSON.stringify(draft.defaults, null, 2))
      }
    }
  }, [draft, mappings.length])

  useEffect(() => {
    if (draft?.upsert) {
      setUpsertEnabled(true)
      setUpsertStrategy(draft.upsert.strategy)
      setUpsertKey(draft.upsert.dedupeKey ?? '')
      setConflictColumns(draft.upsert.conflictColumns ?? [])
    } else {
      setUpsertEnabled(false)
      setUpsertStrategy('skip')
      setUpsertKey('')
      setConflictColumns([])
    }
  }, [draft])

  const propertyNames = useMemo(() => schema?.properties.map((p) => p.name) ?? [], [schema])

  const incompleteMappings = useMemo(() => {
    return mappings.filter((m) => m.include && (!m.sourceField.trim() || !m.targetProperty.trim())).length
  }, [mappings])

  const sourceFieldOptions = useMemo(() => {
    const fromPreview = Array.from(new Set(previewFields.map((f) => f || '').filter(Boolean)))
    const fromTpl = Array.from(new Set(tplSourceFields.map((s) => s || '').filter(Boolean)))
    const current = mappings.map((m) => m.sourceField).filter(Boolean)
    return Array.from(new Set([...fromPreview, ...fromTpl, ...current]))
  }, [previewFields, tplSourceFields, mappings])

  const targetPropertyOptions = useMemo(() => {
    return Array.from(
      new Set(
        mappings
          .filter((m) => m.include && m.targetProperty.trim())
          .map((m) => m.targetProperty.trim())
      )
    )
  }, [mappings])

  const normalizedDefaultsJson = useMemo(() => {
    const trimmed = defaultsJson.trim()
    return trimmed.length > 0 ? trimmed : ''
  }, [defaultsJson])

  const currentFingerprint = useMemo(() => {
    const upsertPayload = upsertEnabled && upsertKey
      ? {
          strategy: upsertStrategy,
          dedupeKey: upsertKey,
          conflictColumns: [...conflictColumns].sort(),
        }
      : null
    return JSON.stringify({
      tokenId,
      databaseId,
      sourceFilePath,
      fileType,
      mappings,
      defaults: normalizedDefaultsJson,
      upsert: upsertPayload,
    })
  }, [
    tokenId,
    databaseId,
    sourceFilePath,
    fileType,
    mappings,
    normalizedDefaultsJson,
    upsertEnabled,
    upsertStrategy,
    upsertKey,
    conflictColumns,
  ])

  useEffect(() => {
    if (!onDraftChange) return
    if (!draftFingerprint) return
    if (currentFingerprint !== draftFingerprint) {
      onDraftChange(null)
      setDraftFingerprint(null)
    }
  }, [currentFingerprint, draftFingerprint, onDraftChange])

  useEffect(() => {
    if (!upsertEnabled) return
    if (upsertKey && !targetPropertyOptions.includes(upsertKey)) {
      setUpsertKey('')
    }
    setConflictColumns((current) => current.filter((key) => targetPropertyOptions.includes(key)))
  }, [targetPropertyOptions, upsertEnabled, upsertKey])

  useEffect(() => {
    if (!schema) return
    if (mappings.length > 0) return
    if (previewFields.length === 0) return

    const normalize = (value: string) => value.replace(/[^a-z0-9\u4e00-\u9fff]+/gi, '').toLowerCase()

    const matchedRows: FieldMapping[] = previewFields.map((header) => {
      const normalizedHeader = normalize(header)
      const matched = schema.properties.find((prop) => {
        if (normalize(prop.name) === normalizedHeader) return true
        if (prop.type === 'title') {
          const aliases = ['name', 'title', '标题']
          return aliases.some((alias) => normalize(alias) === normalizedHeader)
        }
        return false
      })
      return {
        include: true,
        sourceField: header,
        targetProperty: matched?.name ?? '',
        targetType: matched?.type ?? 'rich_text',
        transformCode: undefined,
      }
    })

    if (matchedRows.length === 0) {
      const titleProp = schema.properties.find((p) => p.type === 'title')
      if (titleProp) {
        matchedRows.push({ include: true, sourceField: '', targetProperty: titleProp.name, targetType: 'title', transformCode: undefined })
      }
    }

    setMappings(matchedRows)
  }, [schema, previewFields, mappings.length])

  const addRow = useCallback(() => {
    setMappings((current) => {
      const defaultType = schema?.properties[0]?.type ?? 'rich_text'
      return [...current, { include: true, sourceField: '', targetProperty: '', targetType: defaultType, transformCode: undefined }]
    })
  }, [schema])

  const updateRow = useCallback((index: number, patch: Partial<FieldMapping>) => {
    setMappings((current) => current.map((row, idx) => (idx === index ? { ...row, ...patch } : row)))
  }, [])

  const removeRow = useCallback((index: number) => {
    setMappings((current) => current.filter((_, idx) => idx !== index))
  }, [])

  const applyTemplate = useCallback((tpl: ImportTemplate) => {
    setMappings(tpl.mappings ?? [])
    if (tpl.defaults) {
      setDefaultsJson(JSON.stringify(tpl.defaults, null, 2))
    }
    const fields = (tpl.mappings ?? []).map((m) => m.sourceField).filter(Boolean)
    setTplSourceFields(Array.from(new Set(fields)))
  }, [])

  const parseDefaults = useCallback((): Record<string, unknown> | undefined => {
    try {
      const trimmed = defaultsJson.trim()
      if (trimmed.length === 0) {
        setDefaultsError(null)
        return {}
      }
      const parsed = JSON.parse(trimmed)
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        setDefaultsError(null)
        return parsed as Record<string, unknown>
      }
      setDefaultsError('defaults 需为对象，例如 {"status": "已导入"}')
      return undefined
    } catch (err) {
      setDefaultsError(err instanceof Error ? err.message : String(err))
      return undefined
    }
  }, [defaultsJson])

  const saveTemplate = useCallback(async () => {
    if (!schema) return
    const defaults = parseDefaults()
    if (defaults === undefined) return
    try {
      setSavingTemplate(true)
      const payload: ImportTemplate = {
        name: tplName.trim() || DEFAULT_TEMPLATE_NAME,
        tokenId,
        databaseId,
        mappings,
        defaults,
      }
      await invoke<ImportTemplate>('notion_template_save', { tpl: payload })
      setTplName(DEFAULT_TEMPLATE_NAME)
      await loadTemplates()
    } finally {
      setSavingTemplate(false)
    }
  }, [schema, tplName, tokenId, databaseId, mappings, parseDefaults, loadTemplates])

  const deleteTemplate = useCallback(async (id: string) => {
    try {
      setDeletingTemplateId(id)
      await invoke<void>('notion_template_delete', { id })
      await loadTemplates()
    } finally {
      setDeletingTemplateId(null)
    }
  }, [loadTemplates])

  const runDry = useCallback(async () => {
    if (!schema) return
    if (upsertInvalid) {
      setUpsertError('Upsert 已开启，请选择唯一键字段。')
      return
    }
    if (!hasSamples) {
      setDryRunReport({ total: 0, ok: 0, failed: 0, errors: [{ rowIndex: 0, message: '需要至少一条样本记录，请返回上一步重新选择数据源。' }] })
      return
    }
    const defaults = parseDefaults()
    if (defaults === undefined) return
    setDryRunLoading(true)
    setDryRunReport(null)
    try {
      const records = previewRecords.slice(0, 20)
      const input: DryRunInput = defaults && Object.keys(defaults).length > 0 ? { schema, mappings, records, defaults } : { schema, mappings, records }
      const report = await invoke<DryRunReport>('notion_import_dry_run', { input })
      setDryRunReport(report)
      if (onDraftChange) {
        if (report.failed === 0 && sourceFilePath) {
          const upsert = upsertEnabled && upsertKey
            ? {
                dedupeKey: upsertKey,
                strategy: upsertStrategy,
                conflictColumns,
              }
            : undefined
          onDraftChange({
            tokenId,
            databaseId,
            sourceFilePath,
            fileType: fileType ?? 'csv',
            fields: previewFields,
            previewRecords: records,
            mappings,
            defaults: defaults && Object.keys(defaults).length > 0 ? defaults : undefined,
            upsert,
          })
          setDraftFingerprint(currentFingerprint)
        } else {
          onDraftChange(null)
          setDraftFingerprint(null)
        }
      }
    } finally {
      setDryRunLoading(false)
    }
  }, [
    schema,
    hasSamples,
    parseDefaults,
    previewRecords,
    mappings,
    sourceFilePath,
    onDraftChange,
    tokenId,
    databaseId,
    fileType,
    previewFields,
    upsertEnabled,
    upsertKey,
    upsertStrategy,
    conflictColumns,
    currentFingerprint,
  ])

  const openTransformEditor = useCallback((index: number) => {
    const current = mappings[index]
    setTransformEditor({
      index,
      code: current.transformCode?.trim() || DEFAULT_TRANSFORM,
      sampleIndex: 0,
      testing: false,
      result: undefined,
      error: undefined,
    })
  }, [mappings])

  const closeTransformEditor = useCallback(() => setTransformEditor(null), [])

  const applyTransformFromEditor = useCallback(() => {
    if (!transformEditor) return
    const trimmed = transformEditor.code.trim()
    updateRow(transformEditor.index, { transformCode: trimmed.length > 0 && trimmed !== DEFAULT_TRANSFORM ? trimmed : undefined })
    setTransformEditor(null)
  }, [transformEditor, updateRow])

  const testTransform = useCallback(async () => {
    if (!transformEditor) return
    const mapping = mappings[transformEditor.index]
    const sample = previewRecords[transformEditor.sampleIndex] as Record<string, unknown> | undefined
    if (!mapping.sourceField) {
      setTransformEditor((prev) => prev ? { ...prev, error: '请先填写源字段', result: undefined } : prev)
      return
    }
    if (!sample) {
      setTransformEditor((prev) => prev ? { ...prev, error: '缺少样本记录', result: undefined } : prev)
      return
    }
    const value = (sample as any)?.[mapping.sourceField]
    try {
      setTransformEditor((prev) => prev ? { ...prev, testing: true, error: undefined, result: undefined } : prev)
      const response = await invoke<TransformEvalResult>('notion_transform_eval_sample', {
        req: {
          code: transformEditor.code,
          value,
          record: sample,
          rowIndex: transformEditor.sampleIndex,
        },
      })
      setTransformEditor((prev) => prev ? { ...prev, testing: false, result: safeStringify(response.result), error: undefined } : prev)
    } catch (err) {
      setTransformEditor((prev) => prev ? { ...prev, testing: false, error: err instanceof Error ? err.message : String(err), result: undefined } : prev)
    }
  }, [transformEditor, mappings, previewRecords])

  return (
    <div>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 8 }}>
        <button className="ghost" onClick={onPrev}>返回数据源</button>
        <div style={{ flex: 1 }} />
        {loadingSchema ? <span>加载 schema…</span> : schemaError ? <span className="error">{schemaError}</span> : null}
      </div>

      {schema && (
        <div className="muted" style={{ marginBottom: 8 }}>
          目标数据库：<code>{schema.title || schema.id}</code>
        </div>
      )}

      <div className="job-board" style={{ overflowX: 'auto', marginBottom: 8 }}>
        <table className="mapping-table">
          <thead>
            <tr>
              <th style={{ width: 70 }}>包含</th>
              <th>源字段</th>
              <th>目标属性</th>
              <th style={{ width: 100 }}>类型</th>
              <th style={{ width: 120 }}>Transform</th>
              <th style={{ width: 80 }}>操作</th>
            </tr>
          </thead>
          <tbody>
            {mappings.map((mapping, index) => (
              <tr key={index}>
                <td>
                  <input type="checkbox" checked={mapping.include} onChange={(e) => updateRow(index, { include: e.target.checked })} />
                </td>
                <td>
                  <input
                    list="source-field-options"
                    type="text"
                    value={mapping.sourceField}
                    onChange={(e) => updateRow(index, { sourceField: e.target.value })}
                    placeholder="源字段名"
                  />
                </td>
                <td>
                  <select
                    value={mapping.targetProperty}
                    onChange={(e) => {
                      const name = e.target.value
                      const t = schema?.properties.find((p) => p.name === name)?.type || mapping.targetType
                      updateRow(index, { targetProperty: name, targetType: t })
                    }}
                  >
                    <option value="">选择属性</option>
                    {schema?.properties.map((prop) => (
                      <option key={prop.name} value={prop.name}>
                        {prop.name}
                        {prop.required ? ' *' : ''}
                      </option>
                    ))}
                  </select>
                </td>
                <td><code>{mapping.targetType}</code></td>
                <td>
                  <button className="ghost" onClick={() => openTransformEditor(index)}>
                    {mapping.transformCode ? '已设置' : '未设置'}
                  </button>
                </td>
                <td>
                  <button className="ghost" onClick={() => removeRow(index)}>删除</button>
                </td>
              </tr>
            ))}
            {mappings.length === 0 && (
              <tr><td colSpan={6} className="muted">暂无映射，请添加。</td></tr>
            )}
          </tbody>
        </table>
      </div>

      <datalist id="source-field-options">
        {sourceFieldOptions.map((opt) => (
          <option key={opt} value={opt} />
        ))}
      </datalist>

      <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 12 }}>
        <button className="ghost" onClick={addRow}>添加映射</button>
        <div style={{ flex: 1 }} />
        <input type="text" value={tplName} onChange={(e) => setTplName(e.target.value)} placeholder="模板名称" />
        <button className="primary" disabled={savingTemplate} onClick={saveTemplate}>{savingTemplate ? '保存中…' : '保存模板'}</button>
        <button
          className="ghost"
          disabled={dryRunLoading || !schema || !hasSamples || defaultsError !== null || incompleteMappings > 0 || upsertInvalid}
          onClick={runDry}
        >
          {dryRunLoading ? '校验中…' : 'Dry-run 校验'}
        </button>
      </div>

      {(defaultsError || incompleteMappings > 0 || upsertError) && (
        <div className="error" style={{ marginBottom: 8 }}>
          {defaultsError && <div>默认值错误：{defaultsError}</div>}
          {incompleteMappings > 0 && <div>仍有 {incompleteMappings} 条映射缺少源字段或目标属性。</div>}
          {upsertError && <div>{upsertError}</div>}
        </div>
      )}

      <section style={{ marginBottom: 12 }}>
        <h4>默认值（JSON）</h4>
        <textarea
          value={defaultsJson}
          onChange={(e) => setDefaultsJson(e.target.value)}
          rows={6}
          style={{ width: '100%', fontFamily: 'monospace' }}
        />
        {defaultsError && <p className="error">{defaultsError}</p>}
      </section>

      <section style={{ marginBottom: 12 }}>
        <h4>Upsert 配置</h4>
        <label style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
          <input
            type="checkbox"
            checked={upsertEnabled}
            onChange={(e) => {
              const next = e.target.checked
              setUpsertEnabled(next)
              if (!next) {
                setUpsertKey('')
                setConflictColumns([])
                setUpsertStrategy('skip')
              }
            }}
          />
          <span>启用 Upsert / 增量导入</span>
        </label>
        {upsertEnabled && (
          <div style={{ display: 'grid', gap: 12 }}>
            <div>
              <label style={{ display: 'block', fontWeight: 600, marginBottom: 4 }}>唯一键字段</label>
              <select
                value={upsertKey}
                onChange={(e) => setUpsertKey(e.target.value)}
                disabled={targetPropertyOptions.length === 0}
              >
                <option value="">选择唯一键字段</option>
                {targetPropertyOptions.map((opt) => (
                  <option key={opt} value={opt}>{opt}</option>
                ))}
              </select>
              {targetPropertyOptions.length === 0 && (
                <p className="muted" style={{ marginTop: 4 }}>暂无可用的目标属性，请先完成映射。</p>
              )}
              <p className="muted" style={{ marginTop: 4 }}>选择用来匹配现有页面的 Notion 属性，仅支持单项。</p>
            </div>
            <div>
              <label style={{ display: 'block', fontWeight: 600, marginBottom: 4 }}>冲突策略</label>
              <select value={upsertStrategy} onChange={(e) => setUpsertStrategy(e.target.value as UpsertStrategy)}>
                <option value="skip">Skip（跳过已有记录）</option>
                <option value="overwrite">Overwrite（覆盖现有记录）</option>
                <option value="merge">Merge（仅更新映射字段）</option>
              </select>
            </div>
            <div>
              <label style={{ display: 'block', fontWeight: 600, marginBottom: 4 }}>冲突报告字段（可选）</label>
              <MultiSelectField
                options={targetPropertyOptions}
                selected={conflictColumns}
                onChange={setConflictColumns}
                placeholder="选择需要在报告中展示的字段"
                disabled={targetPropertyOptions.length === 0}
                emptyHint="暂无可用字段，请先映射目标属性。"
              />
              <p className="muted" style={{ marginTop: 4 }}>在冲突行导出中额外展示这些字段，便于核对。</p>
            </div>
          </div>
        )}
      </section>

      <section style={{ marginBottom: 12 }}>
        <h4>已保存模板</h4>
        <ul className="token-list">
          {templates.map((tpl) => (
            <li key={tpl.id} style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <strong>{tpl.name}</strong>
              <span className="muted">（{tpl.mappings.length} 条映射）</span>
              <button className="ghost" onClick={() => applyTemplate(tpl)}>加载并应用</button>
              {tpl.id && (
                <button className="ghost" disabled={deletingTemplateId === tpl.id} onClick={() => deleteTemplate(tpl.id!)}>
                  {deletingTemplateId === tpl.id ? '删除中…' : '删除'}
                </button>
              )}
            </li>
          ))}
          {templates.length === 0 && <li className="muted">暂无模板。</li>}
        </ul>
      </section>

      {dryRunReport && (
        <section>
          <h4>Dry-run 结果</h4>
          <div className="muted">总计 {dryRunReport.total}，通过 {dryRunReport.ok}，失败 {dryRunReport.failed}</div>
          {dryRunReport.errors.length > 0 && (
            <ul className="token-list">
              {dryRunReport.errors.map((err, idx) => (
                <li key={idx}>
                  <code># {err.rowIndex}</code>
                  <span style={{ marginLeft: 6, marginRight: 6 }}>[{mapErrorKind(err.kind)}]</span>
                  {err.message}
                </li>
              ))}
            </ul>
          )}
          {dryRunReport.failed === 0 && onDraftChange && (
            <div className="success" style={{ marginTop: 8 }}>✅ Dry-run 通过，已生成导入草稿。</div>
          )}
          {dryRunReport.failed === 0 && draft && onStartImport && !hasRunningJob && (
            <div style={{ marginTop: 12 }}>
              <button
                className="primary"
                disabled={starting}
                onClick={() => onStartImport(draft)}
              >
                {starting ? '启动中…' : '开始导入'}
              </button>
            </div>
          )}
          {hasRunningJob && (
            <p className="muted" style={{ marginTop: 8 }}>
              当前已有导入作业在执行，请先完成或取消后再启动新的导入。
            </p>
          )}
        </section>
      )}

      {transformEditor && (
        <div className="modal" style={{ position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.4)', display: 'grid', placeItems: 'center', zIndex: 20 }}>
          <div style={{ background: '#fff', padding: 16, borderRadius: 10, width: 680, maxWidth: '92vw', maxHeight: '92vh', overflow: 'auto' }}>
            <header style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 12 }}>
              <h4 style={{ margin: 0 }}>编辑 Transform（第 {transformEditor.index + 1} 行）</h4>
              <button className="ghost" onClick={closeTransformEditor}>关闭</button>
            </header>
            <p className="muted">Transform 接收 value 与 ctx 返回新值；默认函数如下。</p>
            <textarea
              value={transformEditor.code}
              onChange={(e) => setTransformEditor((prev) => prev ? { ...prev, code: e.target.value } : prev)}
              rows={10}
              style={{ width: '100%', fontFamily: 'monospace' }}
            />
            <div style={{ display: 'flex', gap: 8, alignItems: 'center', margin: '8px 0' }}>
              <label>样本行：</label>
              <select
                value={transformEditor.sampleIndex}
                onChange={(e) => setTransformEditor((prev) => prev ? { ...prev, sampleIndex: Number(e.target.value) } : prev)}
              >
                {previewRecords.slice(0, 20).map((_, idx) => (
                  <option key={idx} value={idx}>#{idx + 1}</option>
                ))}
              </select>
              <button className="ghost" disabled={transformEditor.testing} onClick={testTransform}>
                {transformEditor.testing ? '测试中…' : '在样本上测试'}
              </button>
              <div style={{ flex: 1 }} />
              <button className="ghost" onClick={() => setTransformEditor((prev) => prev ? { ...prev, code: DEFAULT_TRANSFORM } : prev)}>重置为默认</button>
              <button className="primary" onClick={applyTransformFromEditor}>保存</button>
            </div>
            {transformEditor.error && <p className="error">{transformEditor.error}</p>}
            {transformEditor.result && (
              <p className="muted">输出：<code>{transformEditor.result}</code></p>
            )}
          </div>
        </div>
      )}
    </div>
  )
}

type MultiSelectFieldProps = {
  options: string[]
  selected: string[]
  onChange: (next: string[]) => void
  placeholder?: string
  disabled?: boolean
  emptyHint?: string
}

function MultiSelectField({ options, selected, onChange, placeholder = '请选择', disabled = false, emptyHint }: MultiSelectFieldProps) {
  const containerRef = useRef<HTMLDivElement | null>(null)
  const [open, setOpen] = useState(false)

  const orderedSelected = useMemo(() => {
    const inOptions = options.filter((opt) => selected.includes(opt))
    const extra = selected.filter((value) => !options.includes(value))
    return [...inOptions, ...extra]
  }, [options, selected])

  const noOptionAvailable = options.length === 0
  const isDisabled = disabled || noOptionAvailable

  useEffect(() => {
    if (!open) return
    const handleClickOutside = (event: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(event.target as Node)) {
        setOpen(false)
      }
    }
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', handleClickOutside)
    document.addEventListener('keydown', handleEscape)
    return () => {
      document.removeEventListener('mousedown', handleClickOutside)
      document.removeEventListener('keydown', handleEscape)
    }
  }, [open])

  useEffect(() => {
    if (isDisabled) {
      setOpen(false)
    }
  }, [isDisabled])

  const toggleOption = useCallback((value: string) => {
    if (isDisabled) return
    const next = new Set(selected)
    if (next.has(value)) {
      next.delete(value)
    } else {
      next.add(value)
    }
    const ordered = options.filter((opt) => next.has(opt))
    const leftovers = Array.from(next).filter((opt) => !options.includes(opt))
    onChange([...ordered, ...leftovers])
  }, [isDisabled, onChange, options, selected])

  const clearSelection = useCallback(() => {
    if (isDisabled) return
    onChange([])
  }, [isDisabled, onChange])

  const hasSelection = orderedSelected.length > 0
  const placeholderText = noOptionAvailable ? (emptyHint ?? placeholder) : placeholder

  return (
    <div
      className={`multi-select-field${open ? ' open' : ''}${isDisabled ? ' disabled' : ''}`}
      ref={containerRef}
    >
      <button
        type="button"
        className={`multi-select-trigger${!hasSelection ? ' placeholder' : ''}`}
        onClick={() => {
          if (isDisabled) return
          setOpen((prev) => !prev)
        }}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-disabled={isDisabled}
        disabled={isDisabled}
      >
        {hasSelection ? (
          <span className="multi-select-badges">
            {orderedSelected.map((value) => (
              <span key={value} className="multi-select-badge">{value}</span>
            ))}
          </span>
        ) : (
          <span className="multi-select-placeholder">{placeholderText}</span>
        )}
        <span className="multi-select-caret" aria-hidden="true">▾</span>
      </button>
      {hasSelection && !isDisabled && (
        <button
          type="button"
          className="multi-select-clear"
          onClick={(event) => {
            event.stopPropagation()
            clearSelection()
          }}
          aria-label="清除已选择的字段"
        >
          ×
        </button>
      )}
      {open && (
        <div className="multi-select-dropdown" role="listbox" aria-multiselectable="true">
          {options.map((opt) => {
            const checked = selected.includes(opt)
            return (
              <label key={opt} className={`multi-select-option${checked ? ' checked' : ''}`}>
                <input
                  type="checkbox"
                  checked={checked}
                  onChange={() => toggleOption(opt)}
                />
                <span>{opt}</span>
              </label>
            )
          })}
          {noOptionAvailable && (
            <div className="multi-select-empty">
              {emptyHint ?? '暂无可选项'}
            </div>
          )}
        </div>
      )}
    </div>
  )
}

function safeStringify(value: unknown): string {
  try {
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

function mapErrorKind(kind: DryRunErrorKind): string {
  switch (kind) {
    case 'transform':
      return 'Transform'
    case 'mapping':
      return 'Mapping'
    case 'validation':
      return 'Validation'
    default:
      return 'Unknown'
  }
}
