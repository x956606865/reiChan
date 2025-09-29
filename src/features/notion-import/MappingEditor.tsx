import { useCallback, useEffect, useMemo, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import type { DatabaseSchema, FieldMapping, ImportTemplate, DryRunInput, DryRunReport } from './types'

type Props = {
  tokenId: string
  databaseId: string
  csvFields?: string[]
  onPrev: () => void
}

export default function MappingEditor(props: Props) {
  const { tokenId, databaseId, csvFields } = props
  const [schema, setSchema] = useState<DatabaseSchema | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [mappings, setMappings] = useState<FieldMapping[]>([])
  const [tplName, setTplName] = useState('模板 1')
  const [saving, setSaving] = useState(false)
  const [templates, setTemplates] = useState<ImportTemplate[]>([])
  const [tplSourceFields, setTplSourceFields] = useState<string[]>([])
  const [dryRun, setDryRun] = useState<DryRunReport | null>(null)
  const [dryRunLoading, setDryRunLoading] = useState(false)

  const loadSchema = useCallback(async () => {
    if (!tokenId || !databaseId) return
    try {
      setLoading(true)
      setError(null)
      const s = await invoke<DatabaseSchema>('notion_get_database', { tokenId, databaseId })
      setSchema(s)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [tokenId, databaseId])

  const loadTemplates = useCallback(async () => {
    if (!tokenId) return
    const list = await invoke<ImportTemplate[]>('notion_template_list', { tokenId })
    setTemplates(list.filter((t) => t.databaseId === databaseId))
  }, [tokenId, databaseId])

  useEffect(() => {
    loadSchema()
    loadTemplates().catch(() => void 0)
  }, [loadSchema, loadTemplates])

  const addRow = useCallback(() => {
    setMappings((arr) => [
      ...arr,
      { include: true, sourceField: '', targetProperty: '', targetType: schema?.properties[0]?.type || 'rich_text' },
    ])
  }, [schema])

  const setRow = useCallback((idx: number, patch: Partial<FieldMapping>) => {
    setMappings((arr) => arr.map((m, i) => (i === idx ? { ...m, ...patch } : m)))
  }, [])

  const removeRow = useCallback((idx: number) => {
    setMappings((arr) => arr.filter((_, i) => i !== idx))
  }, [])

  const saveTemplate = useCallback(async () => {
    if (!schema) return
    try {
      setSaving(true)
      const payload: ImportTemplate = {
        name: tplName.trim() || '模板',
        tokenId,
        databaseId,
        mappings,
      }
      await invoke<ImportTemplate>('notion_template_save', { tpl: payload })
      await loadTemplates()
    } finally {
      setSaving(false)
    }
  }, [schema, tplName, tokenId, databaseId, mappings, loadTemplates])

  const runDry = useCallback(async () => {
    if (!schema) return
    setDryRunLoading(true)
    setDryRun(null)
    try {
      const input: DryRunInput = { schema, mappings, records: [] }
      const report = await invoke<DryRunReport>('notion_import_dry_run', { input })
      setDryRun(report)
    } finally {
      setDryRunLoading(false)
    }
  }, [schema, mappings])

  const propertyNames = useMemo(() => schema?.properties.map((p) => p.name) ?? [], [schema])
  const sourceFieldOptions = useMemo(() => {
    const base = Array.from(new Set([...(csvFields ?? [])].map((s) => s || '').filter(Boolean)))
    const fromTpl = Array.from(new Set(tplSourceFields.map((s) => s || '').filter(Boolean)))
    const merged = Array.from(new Set([...base, ...fromTpl]))
    return merged
  }, [csvFields, tplSourceFields])

  const applyTemplate = useCallback((tpl: ImportTemplate) => {
    setMappings(tpl.mappings || [])
    const fields = (tpl.mappings || []).map((m) => m.sourceField).filter((s) => s && s.trim().length > 0)
    setTplSourceFields(Array.from(new Set(fields)))
  }, [])

  // ----------
  // 初始化映射：基于 CSV 表头与数据库属性名的“宽松同名”匹配
  // - 忽略大小写、空格、下划线与连字符
  // - 对 title 类型属性增加常见同义词：Name/Title/标题
  useEffect(() => {
    if (!schema) return
    if (mappings.length > 0) return // 已有映射（例如来自模板或用户操作）则不覆盖

    const normalize = (s: string) => {
      const lowered = (s || '').toLowerCase()
      let out = ''
      for (const ch of lowered) {
        const code = ch.charCodeAt(0)
        const isDigit = code >= 48 && code <= 57
        const isAsciiLetter = code >= 97 && code <= 122
        const isCjk = code >= 0x4e00 && code <= 0x9fff
        if (isDigit || isAsciiLetter || isCjk) {
          out += ch
        }
      }
      return out
    }

    const headers = Array.from(new Set([...(csvFields ?? [])].map((h) => h || '').filter(Boolean)))

    const matchProperty = (header: string): DatabaseSchema['properties'][number] | null => {
      const candidatesFor = (prop: DatabaseSchema['properties'][number]) => {
        const extras: string[] = []
        if (prop.type === 'title') {
          extras.push('name', 'title', '标题')
        }
        return [prop.name, ...extras]
      }

      for (const prop of schema.properties) {
        const options = candidatesFor(prop)
        if (options.some((c) => normalize(c) === normalize(header))) {
          return prop
        }
      }
      return null
    }

    const rows: FieldMapping[] = headers.map((header) => {
      const property = matchProperty(header)
      return {
        include: true,
        sourceField: header,
        targetProperty: property?.name ?? '',
        targetType: property?.type ?? 'rich_text',
      }
    })

    if (rows.length === 0) {
      // 保底：生成一行指向 title 属性，源字段留空（用户可从候选选择）
      const titleProp = schema.properties.find((p) => p.type === 'title')
      if (titleProp) {
        const maybe = headers.find((h) => normalize(h) === normalize(titleProp.name) || ['name', 'title', '标题'].some((alias) => normalize(alias) === normalize(h)))
        rows.push({ include: true, sourceField: maybe ?? '', targetProperty: titleProp.name, targetType: 'title' })
      }
    }

    if (rows.length > 0) setMappings(rows)
  }, [schema, csvFields, mappings.length])

  return (
    <div>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
        <button className="ghost" onClick={props.onPrev}>返回选择数据库</button>
        <div style={{ flex: 1 }} />
        {loading ? <span>加载 schema…</span> : error ? <span className="error">{error}</span> : null}
      </div>

      {schema && (
        <div className="muted" style={{ margin: '8px 0' }}>
          目标数据库：<code>{schema.title || schema.id}</code>
        </div>
      )}

      <div className="job-board" style={{ overflowX: 'auto', marginBottom: 8 }}>
        <table className="mapping-table">
          <thead>
            <tr>
              <th style={{ width: 80 }}>包含</th>
              <th>源字段</th>
              <th>目标属性</th>
              <th>类型</th>
              <th style={{ width: 80 }}>操作</th>
            </tr>
          </thead>
          <tbody>
            {mappings.map((m, i) => (
              <tr key={i}>
                <td>
                  <input type="checkbox" checked={m.include} onChange={(e) => setRow(i, { include: e.target.checked })} />
                </td>
                <td>
                  <input list="source-field-options" type="text" value={m.sourceField} onChange={(e) => setRow(i, { sourceField: e.target.value })} placeholder="源字段名（支持从 CSV/模板候选选择）" />
                </td>
                <td>
                  <select
                    value={m.targetProperty}
                    onChange={(e) => {
                      const name = e.target.value
                      const t = schema?.properties.find((p) => p.name === name)?.type || m.targetType
                      setRow(i, { targetProperty: name, targetType: t })
                    }}
                  >
                    <option value="">选择属性</option>
                    {propertyNames.map((n) => (
                      <option key={n} value={n}>{n}</option>
                    ))}
                  </select>
                </td>
                <td>
                  <code>{m.targetType}</code>
                </td>
                <td>
                  <button className="ghost" onClick={() => removeRow(i)}>删除</button>
                </td>
              </tr>
            ))}
            {mappings.length === 0 && (
              <tr>
                <td colSpan={5} className="muted">暂无映射，请添加。</td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      {/* 源字段候选 datalist（CSV + 模板）*/}
      <datalist id="source-field-options">
        {sourceFieldOptions.map((opt) => (
          <option key={opt} value={opt} />
        ))}
      </datalist>

      <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 8 }}>
        <button className="ghost" onClick={addRow}>添加映射</button>
        <div style={{ flex: 1 }} />
        <input type="text" value={tplName} onChange={(e) => setTplName(e.target.value)} placeholder="模板名称" />
        <button className="primary" disabled={saving} onClick={saveTemplate}>{saving ? '保存中…' : '保存模板'}</button>
        <button className="ghost" disabled={dryRunLoading} onClick={runDry}>{dryRunLoading ? '校验中…' : 'Dry-run 校验'}</button>
      </div>

      <section style={{ marginTop: 12 }}>
        <h4>已保存模板</h4>
        <ul className="token-list">
          {templates.map((t) => (
            <li key={t.id} style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <strong>{t.name}</strong>
              <span className="muted">（{t.mappings.length} 映射）</span>
              <button className="ghost" onClick={() => applyTemplate(t)}>加载并应用</button>
            </li>
          ))}
          {templates.length === 0 && <li className="muted">暂无模板。</li>}
        </ul>
        <div className="muted" style={{ marginTop: 4 }}>提示：加载模板后，源字段候选为「模板字段 ∪ CSV 字段」。</div>
      </section>

      {dryRun && (
        <section style={{ marginTop: 12 }}>
          <h4>Dry-run 结果</h4>
          <div className="muted">总计 {dryRun.total}，通过 {dryRun.ok}，失败 {dryRun.failed}</div>
          {dryRun.errors.length > 0 && (
            <ul className="token-list">
              {dryRun.errors.map((e, idx) => (
                <li key={idx}><code>#{e.rowIndex}</code> {e.message}</li>
              ))}
            </ul>
          )}
        </section>
      )}
    </div>
  )
}
