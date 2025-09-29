import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import MappingEditor from "./MappingEditor";
import type { DatabaseBrief as DbBrief } from "./types";

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
  const [csvFields, setCsvFields] = useState<string[] | null>(null);

  const stepOrder = [1, 2, 3, 4] as const;
  const stepIndexMap = useMemo(() => new Map([[1, 1], [2, 2], [3, 3], [4, 4]]), []);

  const goNext = useCallback(() => setStep(2), []);
  const goPrev = useCallback(() => setStep(1), []);

  return (
    <div className="notion-import-agent">
      <div className="stepper-nav" role="presentation">
        {stepOrder.map((s) => {
          const status = s === step ? "active" : s < step ? "completed" : "";
          const label = s === 1 ? "选择 Token" : s === 2 ? "搜索并选择数据库" : s === 3 ? "上传 CSV" : "映射与模板";
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
          <DatabaseSearchStep tokenId={selectedTokenId} onPrev={goPrev} onSelect={(db) => {
            setSelectedDb(db);
            setStep(3);
          }} />
          <div className="wizard-controls">
            <button type="button" onClick={goPrev}>返回上一步</button>
          </div>
        </section>
      )}

      {step === 3 && selectedTokenId && selectedDb && (
        <section className="step-card" aria-label="上传 CSV">
          <header className="step-card-header">
            <span className="step-index">步骤 {stepIndexMap.get(3)}</span>
            <h3>上传 CSV</h3>
            <p>选择 CSV 文件以提取表头，后续映射的源字段默认来自该表头。</p>
          </header>
          <CsvUploadStep onPrev={() => setStep(2)} onNext={(fields) => { setCsvFields(fields); setStep(4); }} />
        </section>
      )}

      {step === 4 && selectedTokenId && selectedDb && (
        <section className="step-card" aria-label="映射与模板">
          <header className="step-card-header">
            <span className="step-index">步骤 {stepIndexMap.get(4)}</span>
            <h3>映射与模板</h3>
            <p>编辑字段映射，保存模板，并进行 Dry-run 校验。</p>
          </header>
          <MappingEditor tokenId={selectedTokenId} databaseId={selectedDb.id} csvFields={csvFields ?? undefined} onPrev={() => setStep(3)} />
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
// Step 3: 上传 CSV（前端读取表头 + 预览）
// -----------------------------

function CsvUploadStep(props: { onPrev: () => void; onNext: (csvFields: string[]) => void }) {
  const [fileName, setFileName] = useState<string>("");
  const [fields, setFields] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<string[][]>([]);
  const [loading, setLoading] = useState(false);

  const onFile = useCallback((file: File) => {
    setLoading(true);
    setError(null);
    setFileName(file.name);
    const reader = new FileReader();
    reader.onload = () => {
      try {
        const text = String(reader.result ?? "");
        // 只读取前 256KB 以保障性能
        const slice = text.slice(0, 256 * 1024);
        const { headers, rows } = parseCsvPreview(slice, 20);
        setFields(headers);
        setPreview(rows);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setLoading(false);
      }
    };
    reader.onerror = () => {
      setLoading(false);
      setError("读取文件失败");
    };
    reader.readAsText(file);
  }, []);

  return (
    <div className="csv-upload-step">
      <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 8 }}>
        <button className="ghost" onClick={props.onPrev}>返回选择数据库</button>
      </div>
      <div className="form-field">
        <label>选择 CSV 文件</label>
        <input
          type="file"
          accept=".csv,text/csv"
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) onFile(f);
          }}
        />
      </div>
      {fileName && (
        <div className="muted" style={{ marginTop: 6 }}>已选择：{fileName}</div>
      )}
      {error && <p className="error">{error}</p>}
      {fields.length > 0 && (
        <>
          <div className="muted" style={{ margin: '8px 0' }}>检测到 {fields.length} 个表头字段。</div>
          <div className="csv-preview-wrap">
            <table className="analysis-table">
              <thead>
                <tr>
                  {fields.map((h, i) => <th key={i}>{h || <span className='muted'>(空)</span>}</th>)}
                </tr>
              </thead>
              <tbody>
                {preview.map((row, rIdx) => (
                  <tr key={rIdx}>
                    {fields.map((_, cIdx) => (
                      <td key={cIdx}>{row[cIdx] ?? ''}</td>
                    ))}
                  </tr>
                ))}
                {preview.length === 0 && (
                  <tr><td colSpan={fields.length} className="muted">无预览数据（仅提取了表头）。</td></tr>
                )}
              </tbody>
            </table>
          </div>
        </>
      )}

      <div className="wizard-controls" style={{ marginTop: 8 }}>
        <button className="primary" disabled={loading || fields.length === 0} onClick={() => props.onNext(fields)}>下一步</button>
      </div>
    </div>
  );
}

// 轻量 CSV 预览解析（支持双引号与转义），仅用于表头与前若干行
function parseCsvPreview(text: string, maxRows: number): { headers: string[]; rows: string[][] } {
  // 去除 UTF-8 BOM
  if (text.charCodeAt(0) === 0xfeff) text = text.slice(1);

  const headers: string[] = [];
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = '';
  let quoted = false;

  const endRow = () => {
    row.push(cell);
    if (headers.length === 0) {
      headers.push(...row.map((h) => h.trim()))
    } else if (row.some((v) => v.length > 0)) {
      rows.push(row);
    }
    row = [];
    cell = '';
  };

  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quoted) {
      if (ch === '"') {
        if (text[i + 1] === '"') { cell += '"'; i++; }
        else { quoted = false; }
      } else {
        cell += ch;
      }
      continue;
    }

    if (ch === '"') { quoted = true; continue; }
    if (ch === ',') { row.push(cell); cell = ''; continue; }
    if (ch === '\n' || ch === '\r') {
      // 处理 CRLF：若当前为 CR，且下一个为 LF，则跳过 LF
      if (ch === '\r' && text[i + 1] === '\n') i++;
      endRow();
      if (rows.length >= maxRows) break;
      continue;
    }
    cell += ch;
  }
  // 文件末尾最后一行（可能没有换行）
  if (cell.length > 0 || row.length > 0) {
    endRow();
  }

  return { headers, rows };
}
