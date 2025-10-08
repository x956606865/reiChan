import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import MappingEditor from "./MappingEditor";
import Runboard from "./Runboard";
import { useNotionImportRunboard } from "./runboardStore";
import type { DatabaseBrief as DbBrief, PreviewResponse, ImportJobDraft, ImportJobSummary } from "./types";

type TokenRow = {
  id: string;
  name: string;
  workspaceName?: string | null;
  createdAt: number;
  lastUsedAt?: number | null;
};

type SaveTokenRequest = {
  name: string;
  token: string;
};

type WorkspaceInfo = {
  workspaceName?: string | null;
  botName?: string | null;
};

type DatabaseBrief = {
  id: string;
  title: string;
  icon?: string | null;
};

type DatabasePage = {
  results: DatabaseBrief[];
  hasMore: boolean;
  nextCursor?: string | null;
};

function useTokens() {
  const [tokens, setTokens] = useState<TokenRow[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const list = await invoke<TokenRow[]>("notion_list_tokens");
      setTokens(list);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const save = useCallback(async (req: SaveTokenRequest) => {
    await invoke<TokenRow>("notion_save_token", { req });
    await refresh();
  }, [refresh]);

  const remove = useCallback(async (id: string) => {
    await invoke<void>("notion_delete_token", { id });
    await refresh();
  }, [refresh]);

  return { tokens, loading, error, refresh, save, remove };
}

export default function NotionImportAgent() {
  const [step, setStep] = useState<1 | 2 | 3 | 4>(1);
  const [selectedTokenId, setSelectedTokenId] = useState<string | null>(null);
  const [selectedDb, setSelectedDb] = useState<DbBrief | null>(null);
  const [previewInfo, setPreviewInfo] = useState<{ path: string; fileType: string; data: PreviewResponse } | null>(null);
  const [jobDraft, setJobDraft] = useState<ImportJobDraft | null>(null);
  const [showRunboard, setShowRunboard] = useState(false);

  const startImport = useNotionImportRunboard((state) => state.actions.start);
  const hydrateRunboard = useNotionImportRunboard((state) => state.actions.hydrate);
  const jobState = useNotionImportRunboard((state) => state.job?.state ?? null);

  const stepOrder = [1, 2, 3, 4] as const;
  const stepIndexMap = useMemo(() => new Map([[1, 1], [2, 2], [3, 3], [4, 4]]), []);
  useEffect(() => {
    let mounted = true;
    (async () => {
      try {
        const jobs = await invoke<ImportJobSummary[]>("notion_import_list_jobs");
        if (!mounted) return;
        if (jobs.length > 0) {
          await hydrateRunboard(jobs[0]);
          if (mounted) setShowRunboard(true);
        } else {
          await hydrateRunboard(null);
        }
      } catch (err) {
        console.warn("failed to hydrate import jobs", err);
      }
    })();
    return () => {
      mounted = false;
    };
  }, [hydrateRunboard]);

  useEffect(() => {
    if (!jobState) return;
    if (jobState !== "Completed" && jobState !== "Failed" && jobState !== "Canceled") {
      setShowRunboard(true);
    }
  }, [jobState]);

  const handleStartImport = useCallback(async (draft: ImportJobDraft) => {
    try {
      await startImport(draft);
      setShowRunboard(true);
    } catch (err) {
      console.error(err);
      alert(`启动导入失败：${err instanceof Error ? err.message : String(err)}`);
    }
  }, [startImport]);

  const handleRunboardBack = useCallback(() => {
    setShowRunboard(false);
  }, []);
  const backToTokenStep = useCallback(() => setStep(1), []);

  return (
    <div className="notion-import-agent">
      <div className="stepper-nav" role="presentation">
        {stepOrder.map((s) => {
          const status = s === step ? "active" : s < step ? "completed" : "";
          const label = s === 1 ? "选择 Token" : s === 2 ? "搜索并选择数据库" : s === 3 ? "选择数据源" : "映射与模板";
          const index = stepIndexMap.get(s) ?? s;
          return (
            <div key={s} className={`stepper-nav-item ${status}`}>
              <span className="step-index">步骤 {index}</span>
              <span className="step-label">{label}</span>
            </div>
          );
        })}
      </div>

      {step === 1 && (
        <section className="step-card" aria-label="选择 Token">
          <header className="step-card-header">
            <span className="step-index">步骤 {stepIndexMap.get(1)}</span>
            <h3>选择已保存的 Token</h3>
            <p>从列表中选择或打开管理面板新增/删除。</p>
          </header>
          <TokenSelectStep value={selectedTokenId} onChange={setSelectedTokenId} />
          <div className="wizard-controls">
            <button type="button" className="primary" disabled={!selectedTokenId} onClick={() => {
              if (selectedTokenId) {
                setStep(2);
              }
            }}>
              下一步
            </button>
          </div>
        </section>
      )}

      {step === 2 && (
        <section className="step-card" aria-label="搜索并选择数据库">
          <header className="step-card-header">
            <span className="step-index">步骤 {stepIndexMap.get(2)}</span>
            <h3>搜索并选择数据库</h3>
            <p>进入本步骤时自动拉取第一页；可继续检索与分页。</p>
          </header>
          <DatabaseSearchStep tokenId={selectedTokenId} onPrev={backToTokenStep} onSelect={(db) => {
            setSelectedDb(db);
            setStep(3);
          }} />
          <div className="wizard-controls">
            <button type="button" onClick={backToTokenStep}>返回上一步</button>
          </div>
        </section>
      )}

      {step === 3 && selectedTokenId && selectedDb && (
        <section className="step-card" aria-label="选择数据源">
          <header className="step-card-header">
            <span className="step-index">步骤 {stepIndexMap.get(3)}</span>
            <h3>选择数据源</h3>
            <p>支持 CSV / JSON / JSONL。解析在后端完成，预览前 {"50"} 行以内。</p>
          </header>
          <DataSourceStep
            initialSelection={previewInfo}
            onPrev={() => setStep(2)}
            onNext={(info) => {
              setPreviewInfo(info);
              setJobDraft(null);
              setStep(4);
            }}
          />
        </section>
      )}

      {step === 4 && selectedTokenId && selectedDb && previewInfo && (
        <section className="step-card" aria-label="映射与模板">
          <header className="step-card-header">
            <span className="step-index">步骤 {stepIndexMap.get(4)}</span>
            <h3>映射与模板</h3>
            <p>
              编辑字段映射，保存模板，并在 Dry-run 成功后生成导入草稿。
              <br />
              当前数据源：<code>{previewInfo.path}</code>
            </p>
          </header>
          <MappingEditor
            tokenId={selectedTokenId}
            databaseId={selectedDb.id}
            sourceFilePath={previewInfo.path}
            fileType={previewInfo.fileType}
            previewFields={previewInfo.data.fields}
            previewRecords={previewInfo.data.records}
            draft={jobDraft}
            onDraftChange={setJobDraft}
            onStartImport={handleStartImport}
            onPrev={() => setStep(3)}
          />
          {showRunboard && (
            <Runboard onBack={handleRunboardBack} />
          )}
        </section>
      )}
    </div>
  );
}

function TokenManager() {
  const { tokens, loading, error, save, remove } = useTokens();
  const [name, setName] = useState("");
  const [token, setToken] = useState("");
  const [saving, setSaving] = useState(false);
  const canSave = name.trim().length > 0 && token.trim().length > 0;

  const onSubmit = useCallback(async (e: React.FormEvent) => {
    e.preventDefault();
    if (!canSave) return;
    try {
      setSaving(true);
      await save({ name: name.trim(), token: token.trim() });
      setName("");
      setToken("");
    } finally {
      setSaving(false);
    }
  }, [name, token, canSave, save]);

  return (
    <div>
      <form className="token-form form-grid" onSubmit={onSubmit}>
        <div className="form-field">
          <label>Token 别名</label>
          <input
            type="text"
            placeholder="例如：主工作区"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </div>
        <div className="form-field">
          <label>Notion Token</label>
          <input
            type="password"
            placeholder="secret_...（只保存在 SQLite，不写钥匙串）"
            value={token}
            onChange={(e) => setToken(e.target.value)}
          />
        </div>
        <div className="controls">
          <button type="submit" disabled={!canSave || saving}>保存</button>
        </div>
      </form>

      {loading && <p>加载中…</p>}
      {error && <p className="error">{error}</p>}

      <ul className="token-list">
        {tokens.map((t) => (
          <li key={t.id}>
            <strong>{t.name}</strong>
            {" "}
            <span className="muted">{t.workspaceName ?? "(未知工作区)"}</span>
            <button className="ghost" onClick={() => remove(t.id)}>删除</button>
          </li>
        ))}
        {tokens.length === 0 && <li className="muted">尚未添加 Token。</li>}
      </ul>
    </div>
  );
}

// -----------------------------
// Step 1: 选择 Token（从已保存列表中选择；支持打开管理对话框）
// -----------------------------

function TokenSelectStep(props: { value?: string | null; onChange?: (id: string | null) => void }) {
  const { tokens, loading, error, save, remove, refresh } = useTokens();
  const selected = props.value ?? null;
  const setSelected = (id: string | null) => props.onChange?.(id ?? null);
  const [showManager, setShowManager] = useState(false);

  useEffect(() => {
    // 默认选中第一项；若当前选中项被删除，则回退
    if (!selected && tokens.length > 0) {
      setSelected(tokens[0].id);
      return;
    }
    if (selected && !tokens.some((t) => t.id === selected)) {
      setSelected(tokens[0]?.id ?? null);
    }
  }, [tokens, selected]);

  useEffect(() => {
    if (!showManager) {
      // 关闭管理面板后刷新列表，确保与后台一致
      refresh().catch(() => void 0);
    }
  }, [showManager, refresh]);

  return (
    <div className="token-select">
      <div style={{ display: "flex", gap: 12, alignItems: "center", marginBottom: 8 }}>
        <button type="button" className="ghost" onClick={() => setShowManager(true)}>管理 Token</button>
      </div>
      {loading && <p>加载中…</p>}
      {error && <p className="error">{error}</p>}

      <ul className="token-list" style={{ marginTop: 8 }}>
        {tokens.map((t) => (
          <li key={t.id} style={{ display: "flex", gap: 8, alignItems: "center" }}>
            <label style={{ display: "flex", gap: 8, alignItems: "center", flex: 1 }}>
              <input
                type="radio"
                name="token"
                checked={selected === t.id}
                onChange={() => setSelected(t.id)}
              />
              <span>
                <strong>{t.name}</strong>{" "}
                <span className="muted">{t.workspaceName ?? "(未知工作区)"}</span>
              </span>
            </label>
            <button className="ghost" onClick={() => remove(t.id)}>删除</button>
          </li>
        ))}
        {tokens.length === 0 && <li className="muted">尚未添加 Token，请点击「管理 Token」新增。</li>}
      </ul>

      {showManager && (
        <div className="modal" style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,0.4)", display: "grid", placeItems: "center" }}>
          <div style={{ background: "#fff", padding: 16, borderRadius: 10, width: 640, maxWidth: "90vw" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
              <h4 style={{ margin: 0 }}>Token 管理</h4>
              <button className="ghost" onClick={() => setShowManager(false)}>关闭</button>
            </div>
            <TokenManager />
          </div>
        </div>
      )}
    </div>
  );
}

// -----------------------------
// Step 2: 数据库搜索 + 分页表格
// -----------------------------

function DatabaseSearchStep(props: { tokenId: string | null; onPrev: () => void; onSelect: (db: DbBrief) => void }) {
  const tokenId = props.tokenId;
  const [query, setQuery] = useState("");
  const [includeEmpty, setIncludeEmpty] = useState(false);
  const [page, setPage] = useState<DatabasePage>({ results: [], hasMore: false, nextCursor: null });
  const [cursorHistory, setCursorHistory] = useState<(string | null)[]>([null]);
  const [pageIndex, setPageIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const PAGE_SIZE = 20;

  const fetchPage = useCallback(async (cursor: string | null, resetHistory = false) => {
    if (!tokenId) return;
    setLoading(true);
    setError(null);
    try {
      const p = await invoke<DatabasePage>("notion_search_databases_page", {
        tokenId: tokenId,
        query: query.trim() || null,
        cursor,
        pageSize: PAGE_SIZE,
        includeEmptyTitle: includeEmpty,
      });
      setPage(p);
      if (resetHistory) {
        setCursorHistory([cursor]);
        setPageIndex(0);
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
    } finally {
      setLoading(false);
    }
  }, [tokenId, query, includeEmpty]);

  // 初始加载
  useEffect(() => {
    if (tokenId) {
      fetchPage(null, true);
    }
  }, [tokenId, fetchPage]);

  const onSearch = useCallback(() => {
    fetchPage(null, true);
  }, [fetchPage]);

  const onNext = useCallback(() => {
    if (!page.nextCursor) return;
    const nextCur = page.nextCursor;
    fetchPage(nextCur, false).then(() => {
      setCursorHistory((h) => [...h, nextCur]);
      setPageIndex((i) => i + 1);
    });
  }, [page.nextCursor, fetchPage]);

  const onPrev = useCallback(() => {
    if (pageIndex <= 0) return;
    const prevCursor = cursorHistory[pageIndex - 1] ?? null;
    fetchPage(prevCursor, false).then(() => {
      setPageIndex((i) => Math.max(0, i - 1));
      setCursorHistory((h) => h.slice(0, Math.max(1, h.length - 1)));
    });
  }, [pageIndex, cursorHistory, fetchPage]);

  // 直接点击行内按钮即确认选择，无需额外确认状态

  return (
    <div className="db-search-step">
      <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8 }}>
        <button className="ghost" onClick={props.onPrev}>返回上一步</button>
        <div style={{ flex: 1 }} />
        <input
          style={{ minWidth: 260 }}
          type="text"
          placeholder="搜索数据库关键词（Notion API）"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSearch();
          }}
        />
        <button className="ghost" onClick={onSearch} disabled={!tokenId || loading}>{loading ? "搜索中…" : "搜索"}</button>
        <label style={{ display: "inline-flex", alignItems: "center", gap: 6, marginLeft: 8 }} title="默认不包含标题为空的数据库。选中后会一并显示。">
          <input
            type="checkbox"
            checked={includeEmpty}
            onChange={(e) => {
              setIncludeEmpty(e.target.checked);
              // 切换时回到第一页，避免在深分页时看不到效果
              fetchPage(null, true);
            }}
          />
          包含空标题数据库
        </label>
      </div>

      {error && (
        <p className="error" style={{ marginTop: 0 }}>
          {error} {(/Token not found/i.test(error)) && (
            <button className="ghost" onClick={props.onPrev} style={{ marginLeft: 8 }}>返回选择 Token</button>
          )}
        </p>
      )}

      <div className="job-board" style={{ overflowX: "auto" }}>
        <table className="analysis-table">
          <thead>
            <tr>
              <th style={{ width: 60 }}>图标</th>
              <th>标题</th>
              <th style={{ width: 360 }}>Database ID</th>
              <th style={{ width: 120 }}>选择</th>
            </tr>
          </thead>
          <tbody>
            {page.results.map((db) => (
              <tr key={db.id}>
                <td>{db.icon ?? "📘"}</td>
                <td>{db.title && db.title.trim().length > 0 ? db.title : <span className="muted">(无标题)</span>}</td>
                <td><code>{db.id}</code></td>
                <td>
                  <button className="primary" onClick={() => props.onSelect(db as DbBrief)}>选择</button>
                </td>
              </tr>
            ))}
            {page.results.length === 0 && (
              <tr>
                <td colSpan={3}>
                  {loading ? "加载中…" : "无搜索结果。"}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      <div style={{ display: "flex", gap: 8, marginTop: 8, alignItems: "center" }}>
        <button className="ghost" onClick={onPrev} disabled={pageIndex <= 0 || loading}>上一页</button>
        <button className="ghost" onClick={onNext} disabled={!page.hasMore || loading}>下一页</button>
        <span className="muted">{`第 ${pageIndex + 1} 页`}</span>
      </div>
    </div>
  );
}

// -----------------------------
// Step 3: 数据源选择（调用后端预览）
// -----------------------------

type DataSourceSelection = { path: string; fileType: string; data: PreviewResponse };

function DataSourceStep(props: {
  initialSelection?: DataSourceSelection | null
  onPrev: () => void
  onNext: (info: DataSourceSelection) => void
}) {
  const [filePath, setFilePath] = useState<string>(props.initialSelection?.path ?? "");
  const [fileType, setFileType] = useState<string>(props.initialSelection?.fileType ?? "auto");
  const [preview, setPreview] = useState<PreviewResponse | null>(props.initialSelection?.data ?? null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const resolvedType = fileType === "auto" ? detectFileType(filePath) ?? "" : fileType;

  const loadPreview = useCallback(async (path: string, typeHint?: string) => {
    if (!path) return;
    setLoading(true);
    setError(null);
    try {
      const req = {
        path,
        fileType: typeHint && typeHint !== "auto" ? typeHint : undefined,
        limitRows: 50,
        limitBytes: 512 * 1024,
      };
      const data = await invoke<PreviewResponse>("notion_import_preview_file", { req });
      setPreview(data);
    } catch (err) {
      setPreview(null);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  const chooseFile = useCallback(async () => {
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: '数据文件', extensions: ['csv', 'json', 'jsonl', 'txt'] }],
      })
      const path = Array.isArray(selected) ? selected[0] : typeof selected === 'string' ? selected : null
      if (!path) return
      const detected = detectFileType(path) ?? 'auto'
      setError(null)
      setPreview(null)
      setFilePath(path)
      setFileType(detected)
      await loadPreview(path, detected === 'auto' ? undefined : detected)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    }
  }, [loadPreview])

  const fields = preview?.fields ?? [];
  const records = (preview?.records ?? []).slice(0, 20);

  return (
    <div className="data-source-step">
      <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8, flexWrap: 'wrap' }}>
        <button className="ghost" onClick={props.onPrev}>返回选择数据库</button>
        <input
          type="text"
          value={filePath}
          onChange={(e) => {
            setFilePath(e.target.value)
            setPreview(null)
            setError(null)
          }}
          onKeyDown={async (e) => {
            if (e.key === 'Enter' && filePath) {
              await loadPreview(filePath, fileType === 'auto' ? undefined : fileType)
            }
          }}
          placeholder="输入文件绝对路径，例如 /Users/you/data.csv"
          style={{ flex: 1, minWidth: 260 }}
        />
        <button
          className="ghost"
          onClick={async () => {
            if (!filePath) return
            await loadPreview(filePath, fileType === 'auto' ? undefined : fileType)
          }}
          disabled={!filePath || loading}
        >
          加载预览
        </button>
        <button className="ghost" onClick={chooseFile}>{filePath ? "重新选择文件" : "选择文件"}</button>
        <select
          value={fileType}
          onChange={(e) => setFileType(e.target.value)}
          style={{ minWidth: 160 }}
          title="若选择自动，将根据扩展名推断。"
        >
          <option value="auto">自动识别</option>
          <option value="csv">CSV</option>
          <option value="json">JSON</option>
          <option value="jsonl">JSONL / NDJSON</option>
        </select>
      </div>

      {filePath && (
        <div className="muted" style={{ marginBottom: 4 }}>
          当前文件：<code>{filePath}</code>（类型：{resolvedType || "自动"}）
        </div>
      )}

      {error && <p className="error">{error}</p>}
      {loading && <p className="muted">解析中…</p>}

      {fields.length > 0 && (
        <div className="csv-preview-wrap" style={{ marginTop: 12 }}>
          <table className="analysis-table">
            <thead>
              <tr>
                {fields.map((f) => (
                  <th key={f}>{f || <span className="muted">(空)</span>}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {records.map((row, idx) => {
                const recordObj = (row as Record<string, unknown>) ?? {};
                return (
                  <tr key={idx}>
                    {fields.map((field) => (
                      <td key={field}>{formatPreviewCell(recordObj[field])}</td>
                    ))}
                  </tr>
                );
              })}
              {records.length === 0 && (
                <tr>
                  <td colSpan={fields.length} className="muted">暂无样本记录。</td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}

      <div className="wizard-controls" style={{ marginTop: 12 }}>
        <button
          className="primary"
          disabled={!filePath || !preview || preview.fields.length === 0 || loading}
          onClick={() => {
            if (!filePath || !preview) return;
            const appliedType = fileType === "auto" ? (resolvedType || "auto") : fileType;
            props.onNext({ path: filePath, fileType: appliedType, data: preview });
          }}
        >
          下一步
        </button>
      </div>
    </div>
  );
}

function detectFileType(path: string): string | null {
  const lower = path.toLowerCase();
  if (lower.endsWith(".csv")) return "csv";
  if (lower.endsWith(".jsonl") || lower.endsWith(".ndjson")) return "jsonl";
  if (lower.endsWith(".json")) return "json";
  return null;
}

function formatPreviewCell(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}
