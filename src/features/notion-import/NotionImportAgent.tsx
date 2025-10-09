import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import MappingEditor from "./MappingEditor";
import Runboard from "./Runboard";
import { useNotionImportRunboard } from "./runboardStore";
import type { DatabaseBrief as DbBrief, PreviewResponse, ImportJobDraft, ImportJobSummary } from "./types";

type TokenKind = "manual" | "oauth";

type TokenRow = {
  id: string;
  name: string;
  kind: TokenKind;
  workspaceName?: string | null;
  workspaceIcon?: string | null;
  workspaceId?: string | null;
  createdAt: number;
  lastUsedAt?: number | null;
  expiresAt?: number | null;
  lastRefreshError?: string | null;
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

type StartOAuthSession = {
  authorizationUrl: string;
  state: string;
  expiresAt: number;
};

type OauthSettings = {
  clientId: string;
  clientSecret: string;
  redirectUri: string;
  tokenUrl?: string | null;
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

  const startOauthSession = useCallback(async () => {
    return invoke<StartOAuthSession>("notion_start_oauth_session");
  }, []);

  const exchangeOauthCode = useCallback(async (req: { tokenName: string; pastedUrl: string }) => {
    const row = await invoke<TokenRow>("notion_exchange_oauth_code", { req });
    await refresh();
    return row;
  }, [refresh]);

  const refreshOauth = useCallback(async (id: string) => {
    const row = await invoke<TokenRow>("notion_refresh_oauth_token", { tokenId: id });
    await refresh();
    return row;
  }, [refresh]);

  const fetchSecret = useCallback(async (id: string) => {
    return invoke<string>("notion_get_token_secret", { id });
  }, []);

  return {
    tokens,
    loading,
    error,
    refresh,
    save,
    remove,
    startOauthSession,
    exchangeOauthCode,
    refreshOauth,
    fetchSecret,
  };
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
            <button
              type="button"
              className="btn btn--primary"
              disabled={!selectedTokenId}
              onClick={() => {
                if (selectedTokenId) {
                  setStep(2);
                }
              }}
            >
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
            <button type="button" className="btn btn--ghost" onClick={backToTokenStep}>返回上一步</button>
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
  const { tokens, loading, error, save, remove, startOauthSession, exchangeOauthCode, refreshOauth, fetchSecret } = useTokens();
  const [name, setName] = useState("");
  const [token, setToken] = useState("");
  const [saving, setSaving] = useState(false);
  const canSave = name.trim().length > 0 && token.trim().length > 0;
  const [startingOauth, setStartingOauth] = useState(false);
  const [oauthModalOpen, setOauthModalOpen] = useState(false);
  const [oauthSession, setOauthSession] = useState<StartOAuthSession | null>(null);
  const [oauthUrl, setOauthUrl] = useState("");
  const [oauthName, setOauthName] = useState("");
  const [oauthError, setOauthError] = useState<string | null>(null);
  const [oauthSubmitting, setOauthSubmitting] = useState(false);
  const [oauthNow, setOauthNow] = useState(() => Date.now());
  const [timeNow, setTimeNow] = useState(() => Date.now());
  const [refreshingId, setRefreshingId] = useState<string | null>(null);
  const [pendingCopyId, setPendingCopyId] = useState<string | null>(null);
  const [copying, setCopying] = useState(false);
  const [copyError, setCopyError] = useState<string | null>(null);
  const [lastCopiedId, setLastCopiedId] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<{ tokenId: string; message: string; kind: "success" | "error" } | null>(null);
  const [settingsModalOpen, setSettingsModalOpen] = useState(false);
  const [settingsLoading, setSettingsLoading] = useState(false);
  const [settingsSaving, setSettingsSaving] = useState(false);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [settingsSuccess, setSettingsSuccess] = useState<string | null>(null);
  const [settingsForm, setSettingsForm] = useState<OauthSettings>({
    clientId: "",
    clientSecret: "",
    redirectUri: "https://www.yuributa.com",
    tokenUrl: "https://api.notion.com/v1/oauth/token",
  });

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

  const resetOauthFlow = useCallback(() => {
    setOauthModalOpen(false);
    setOauthSession(null);
    setOauthUrl("");
    setOauthName("");
    setOauthError(null);
    setOauthSubmitting(false);
    setOauthNow(Date.now());
  }, []);

  useEffect(() => {
    if (!oauthModalOpen) {
      return;
    }
    setOauthNow(Date.now());
    const timer = window.setInterval(() => {
      setOauthNow(Date.now());
    }, 1000);
    return () => {
      window.clearInterval(timer);
    };
  }, [oauthModalOpen]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      setTimeNow(Date.now());
    }, 1000);
    return () => {
      window.clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    if (feedback && !tokens.some((t) => t.id === feedback.tokenId)) {
      setFeedback(null);
    }
    if (pendingCopyId && !tokens.some((t) => t.id === pendingCopyId)) {
      setPendingCopyId(null);
    }
    if (lastCopiedId && !tokens.some((t) => t.id === lastCopiedId)) {
      setLastCopiedId(null);
    }
  }, [tokens, feedback, pendingCopyId, lastCopiedId]);

  const parsedOauth = useMemo(() => {
    const raw = oauthUrl.trim();
    if (!raw) {
      return { valid: false, code: null as string | null, state: null as string | null };
    }
    try {
      const url = new URL(raw);
      return {
        valid: true,
        code: url.searchParams.get("code"),
        state: url.searchParams.get("state"),
      };
    } catch {
      return { valid: false, code: null, state: null };
    }
  }, [oauthUrl]);

  const hasOauthInput = oauthUrl.trim().length > 0;
  const oauthUrlInvalid = hasOauthInput && !parsedOauth.valid;
  const oauthMissingCode = hasOauthInput && parsedOauth.valid && !parsedOauth.code;
  const oauthMissingState = hasOauthInput && parsedOauth.valid && !parsedOauth.state;
  const oauthExpired = Boolean(
    oauthSession && oauthNow >= oauthSession.expiresAt,
  );
  const oauthStateMismatch =
    Boolean(
      oauthSession &&
      parsedOauth.valid &&
      parsedOauth.state &&
      parsedOauth.state !== oauthSession.state &&
      !oauthExpired,
    );
  const oauthStateReady =
    Boolean(
      oauthSession &&
      parsedOauth.valid &&
      parsedOauth.state &&
      parsedOauth.state === oauthSession.state &&
      !oauthExpired,
    );
  const oauthReady =
    Boolean(
      oauthSession &&
      parsedOauth.valid &&
      parsedOauth.code &&
      parsedOauth.state &&
      parsedOauth.state === oauthSession.state &&
      oauthName.trim().length > 0 &&
      !oauthExpired,
    ) && !oauthSubmitting;

  const oauthExpiresLabel = useMemo(() => {
    if (!oauthSession) return "";
    const msLeft = Math.max(0, oauthSession.expiresAt - oauthNow);
    const minutes = Math.floor(msLeft / 60000);
    const seconds = Math.floor((msLeft % 60000) / 1000);
    const absolute = new Date(oauthSession.expiresAt).toLocaleString();
    return `${minutes} 分 ${seconds.toString().padStart(2, "0")} 秒（${absolute} 到期）`;
  }, [oauthSession, oauthNow]);

  const getExpiryInfo = (
    row: TokenRow,
  ): { text: string; tone: "normal" | "warning" | "danger" } | null => {
    if (!row.expiresAt) return null;
    const diff = row.expiresAt - timeNow;
    const absolute = new Date(row.expiresAt).toLocaleString();
    if (diff <= 0) {
      return { text: `已过期（${absolute}）`, tone: "danger" };
    }
    const hours = Math.floor(diff / 3_600_000);
    const minutes = Math.floor((diff % 3_600_000) / 60_000);
    const seconds = Math.floor((diff % 60_000) / 1_000);
    const formatted =
      hours > 0
        ? `${hours} 小时 ${minutes} 分 ${seconds.toString().padStart(2, "0")} 秒`
        : `${minutes} 分 ${seconds.toString().padStart(2, "0")} 秒`;
    const tone: "normal" | "warning" = diff <= 10 * 60_000 ? "warning" : "normal";
    return { text: `剩余 ${formatted}（${absolute} 到期）`, tone };
  };

  const renderWorkspaceIcon = (icon?: string | null) => {
    if (!icon) return null;
    if (/^https?:/i.test(icon)) {
      return (
        <img
          src={icon}
          alt="workspace icon"
          style={{ width: 20, height: 20, borderRadius: "50%", objectFit: "cover" }}
        />
      );
    }
    return <span style={{ fontSize: 20 }}>{icon}</span>;
  };

  const handleRefreshClick = async (id: string) => {
    setFeedback(null);
    setRefreshingId(id);
    try {
      const row = await refreshOauth(id);
      setFeedback({ tokenId: row.id, message: "刷新成功，已更新访问令牌。", kind: "success" });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setFeedback({ tokenId: id, message: msg, kind: "error" });
    } finally {
      setRefreshingId(null);
    }
  };

  const handleConfirmCopy = async (id: string) => {
    setCopying(true);
    setCopyError(null);
    try {
      const secret = await fetchSecret(id);
      const clipboard = navigator.clipboard;
      if (clipboard && clipboard.writeText) {
        await clipboard.writeText(secret);
      } else if (typeof document !== "undefined") {
        const textarea = document.createElement("textarea");
        textarea.value = secret;
        textarea.style.position = "fixed";
        textarea.style.opacity = "0";
        document.body.appendChild(textarea);
        textarea.focus();
        textarea.select();
        const success = document.execCommand("copy");
        document.body.removeChild(textarea);
        if (!success) {
          throw new Error("无法访问剪贴板，请手动复制。");
        }
      } else {
        throw new Error("无法访问剪贴板，请手动复制。");
      }
      setLastCopiedId(id);
      setPendingCopyId(null);
      window.setTimeout(() => {
        setLastCopiedId((prev) => (prev === id ? null : prev));
      }, 4000);
    } catch (err) {
      setCopyError(err instanceof Error ? err.message : String(err));
    } finally {
      setCopying(false);
    }
  };

  const onStartOauth = useCallback(async () => {
    setOauthError(null);
    setStartingOauth(true);
    try {
      const session = await startOauthSession();
      setOauthSession(session);
      setOauthModalOpen(true);
      setOauthUrl("");
      setOauthName("");
      setOauthNow(Date.now());
      try {
        await openUrl(session.authorizationUrl);
      } catch (err) {
        console.warn("failed to open browser for oauth", err);
      }
    } catch (err) {
      setOauthError(err instanceof Error ? err.message : String(err));
    } finally {
      setStartingOauth(false);
    }
  }, [startOauthSession]);

  const onOauthSubmit = useCallback(async (e: React.FormEvent) => {
    e.preventDefault();
    if (!oauthSession || !parsedOauth.code || !parsedOauth.state || oauthStateMismatch) {
      return;
    }
    try {
      setOauthSubmitting(true);
      setOauthError(null);
      await exchangeOauthCode({
        tokenName: oauthName.trim(),
        pastedUrl: oauthUrl.trim(),
      });
      resetOauthFlow();
    } catch (err) {
      setOauthError(err instanceof Error ? err.message : String(err));
    } finally {
      setOauthSubmitting(false);
    }
  }, [oauthSession, parsedOauth.code, parsedOauth.state, oauthUrl, oauthName, oauthStateMismatch, exchangeOauthCode, resetOauthFlow]);

  const loadOauthSettings = useCallback(async () => {
    try {
      setSettingsLoading(true);
      setSettingsError(null);
      setSettingsSuccess(null);
      const res = await invoke<OauthSettings>("notion_get_oauth_settings");
      setSettingsForm({
        clientId: res.clientId,
        clientSecret: res.clientSecret,
        redirectUri: res.redirectUri,
        tokenUrl: res.tokenUrl ?? "",
      });
    } catch (err) {
      setSettingsError(err instanceof Error ? err.message : String(err));
    } finally {
      setSettingsLoading(false);
    }
  }, []);

  const handleOpenSettings = useCallback(() => {
    setSettingsModalOpen(true);
    setSettingsError(null);
    setSettingsSuccess(null);
  }, []);

  const handleSaveSettings = useCallback(async () => {
    try {
      setSettingsSaving(true);
      setSettingsError(null);
      setSettingsSuccess(null);
      const payload = await invoke<OauthSettings>("notion_update_oauth_settings", {
        req: {
          clientId: settingsForm.clientId,
          clientSecret: settingsForm.clientSecret,
          redirectUri: settingsForm.redirectUri,
          tokenUrl:
            settingsForm.tokenUrl && settingsForm.tokenUrl.trim().length > 0
              ? settingsForm.tokenUrl
              : null,
        },
      });
      setSettingsForm({
        clientId: payload.clientId,
        clientSecret: payload.clientSecret,
        redirectUri: payload.redirectUri,
        tokenUrl: payload.tokenUrl ?? "",
      });
      setSettingsSuccess("已保存设置。");
    } catch (err) {
      setSettingsError(err instanceof Error ? err.message : String(err));
    } finally {
      setSettingsSaving(false);
    }
  }, [settingsForm]);

  useEffect(() => {
    if (settingsModalOpen) {
      loadOauthSettings();
    }
  }, [settingsModalOpen, loadOauthSettings]);

  return (
    <div>
      <div style={{ display: "flex", flexDirection: "column", gap: 8, marginBottom: 16 }}>
        <button
          type="button"
          className="btn btn--primary"
          onClick={onStartOauth}
          disabled={startingOauth}
        >
          {startingOauth ? "正在生成授权链接…" : "通过 Notion OAuth 连接"}
        </button>
        <p className="muted" style={{ margin: 0 }}>
          浏览器会打开 Notion 授权页面。授权后复制回调 URL 粘贴回来即可在本地保存 OAuth Token。
        </p>
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          <button
            type="button"
            className="btn btn--ghost"
            onClick={handleOpenSettings}
          >
            配置 OAuth 参数
          </button>
          {settingsSuccess && settingsModalOpen === false && (
            <span style={{ color: "#2a7a3a", fontSize: 12 }}>{settingsSuccess}</span>
          )}
        </div>
        {oauthError && !oauthModalOpen && <p className="error" style={{ margin: 0 }}>{oauthError}</p>}
      </div>

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
        <div className="token-form-actions">
          <button type="submit" className="btn btn--primary" disabled={!canSave || saving}>
            保存
          </button>
        </div>
      </form>

      {loading && <p>加载中…</p>}
      {error && <p className="error">{error}</p>}

      <div className="token-list" style={{ display: "flex", flexDirection: "column", gap: 12 }}>
        {tokens.map((t) => {
          const isOauth = t.kind === "oauth";
          const expiryInfo = isOauth ? getExpiryInfo(t) : null;
          const feedbackForToken = feedback && feedback.tokenId === t.id ? feedback : null;
          const workspaceLabel = t.workspaceName && t.workspaceName.trim().length > 0 ? t.workspaceName : "(未知工作区)";
          const expiryColor = expiryInfo?.tone === "danger" ? "#d14343" : expiryInfo?.tone === "warning" ? "#c47f17" : "#4f6f52";
          return (
            <div key={t.id} style={{ border: "1px solid #ddd", borderRadius: 8, padding: 12, display: "flex", flexDirection: "column", gap: 8 }}>
              <div style={{ display: "flex", justifyContent: "space-between", gap: 12, alignItems: "center", flexWrap: "wrap" }}>
                <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
                  {renderWorkspaceIcon(t.workspaceIcon)}
                  <div>
                    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <strong>{t.name}</strong>
                      <span className="muted">{isOauth ? "OAuth Token" : "手动 Token"}</span>
                    </div>
                    <div className="muted" style={{ display: "flex", alignItems: "center", gap: 6, flexWrap: "wrap" }}>
                      <span>{workspaceLabel}</span>
                      {t.workspaceId && <span style={{ fontFamily: "monospace" }}>ID: {t.workspaceId}</span>}
                    </div>
                  </div>
                </div>
                <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                  <button className="btn btn--ghost" onClick={() => { setPendingCopyId(t.id); setCopyError(null); }}>
                    复制 Token
                  </button>
                  {isOauth && (
                    <button
                      className="btn btn--ghost"
                      onClick={() => handleRefreshClick(t.id)}
                      disabled={refreshingId === t.id}
                    >
                      {refreshingId === t.id ? "刷新中…" : "刷新 Token"}
                    </button>
                  )}
                  <button className="btn btn--danger" onClick={() => remove(t.id)}>删除</button>
                </div>
              </div>
              {isOauth && expiryInfo && (
                <p style={{ margin: 0, color: expiryColor }}>{expiryInfo.text}</p>
              )}
              {isOauth && !expiryInfo && (
                <p className="muted" style={{ margin: 0 }}>Notion 未返回到期时间，请视为短期凭证。</p>
              )}
              {t.lastRefreshError && (
                <p className="error" style={{ margin: 0 }}>上次刷新失败：{t.lastRefreshError}</p>
              )}
              {feedbackForToken && (
                <p
                  style={{
                    margin: 0,
                    color: feedbackForToken.kind === "success" ? "#2a7a3a" : "#c2271e",
                  }}
                >
                  {feedbackForToken.message}
                </p>
              )}
              {pendingCopyId === t.id && (
                <div style={{ marginTop: 4, padding: 8, border: "1px dashed #bbb", borderRadius: 6, display: "flex", flexDirection: "column", gap: 8 }}>
                  <p style={{ margin: 0 }}>即将复制 Token 到剪贴板，请确认周围环境安全。</p>
                  {copyError && <p className="error" style={{ margin: 0 }}>{copyError}</p>}
                  <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                    <button className="btn btn--primary" disabled={copying} onClick={() => handleConfirmCopy(t.id)}>
                      {copying ? "复制中…" : "确认复制"}
                    </button>
                    <button className="btn btn--ghost" onClick={() => { setPendingCopyId(null); setCopyError(null); }}>取消</button>
                  </div>
                </div>
              )}
              {lastCopiedId === t.id && pendingCopyId !== t.id && (
                <p style={{ margin: 0, color: "#2a7a3a" }}>已复制到剪贴板。</p>
              )}
            </div>
          );
        })}
        {tokens.length === 0 && <p className="muted">尚未添加 Token。</p>}
      </div>

      {settingsModalOpen && (
        <div className="modal" style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,0.4)", display: "grid", placeItems: "center", zIndex: 40 }}>
          <div style={{ background: "#fff", padding: 20, borderRadius: 10, width: 520, maxWidth: "92vw", maxHeight: "90vh", overflowY: "auto", display: "flex", flexDirection: "column", gap: 16 }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
              <h4 style={{ margin: 0 }}>Notion OAuth 设置</h4>
              <button
                className="btn btn--ghost"
                onClick={() => {
                  setSettingsModalOpen(false);
                  setSettingsError(null);
                  setSettingsSuccess(null);
                }}
              >
                关闭
              </button>
            </div>
            <p className="muted" style={{ margin: 0 }}>
              这里配置 Notion OAuth 的 client_id、client_secret 等参数。保存后会写入本地设置文件，无需额外环境变量。
            </p>
            {settingsError && (
              <p className="error" style={{ margin: 0 }}>{settingsError}</p>
            )}
            {settingsSuccess && !settingsLoading && (
              <p style={{ margin: 0, color: "#2a7a3a" }}>{settingsSuccess}</p>
            )}
            {settingsLoading ? (
              <p style={{ margin: 0 }}>加载中…</p>
            ) : (
              <form
                className="form-grid"
                onSubmit={(e) => {
                  e.preventDefault();
                  handleSaveSettings();
                }}
                style={{ display: "flex", flexDirection: "column", gap: 12 }}
              >
                <div className="form-field">
                  <label>Client ID</label>
                  <input
                    type="text"
                    value={settingsForm.clientId}
                    onChange={(e) => setSettingsForm((prev) => ({ ...prev, clientId: e.target.value }))}
                    required
                  />
                </div>
                <div className="form-field">
                  <label>Client Secret</label>
                  <input
                    type="password"
                    value={settingsForm.clientSecret}
                    onChange={(e) => setSettingsForm((prev) => ({ ...prev, clientSecret: e.target.value }))}
                    placeholder="留空表示暂不设置"
                  />
                </div>
                <div className="form-field">
                  <label>Redirect URI</label>
                  <input
                    type="text"
                    value={settingsForm.redirectUri}
                    onChange={(e) => setSettingsForm((prev) => ({ ...prev, redirectUri: e.target.value }))}
                    required
                  />
                </div>
                <div className="form-field">
                  <label>Token URL（可选）</label>
                  <input
                    type="text"
                    value={settingsForm.tokenUrl ?? ""}
                    onChange={(e) => setSettingsForm((prev) => ({ ...prev, tokenUrl: e.target.value }))}
                    placeholder="默认 https://api.notion.com/v1/oauth/token"
                  />
                </div>
                <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
                  <button
                    type="button"
                    className="btn btn--ghost"
                    onClick={() => {
                      setSettingsModalOpen(false);
                      setSettingsError(null);
                      setSettingsSuccess(null);
                    }}
                  >
                    取消
                  </button>
                  <button type="submit" className="btn btn--primary" disabled={settingsSaving}>
                    {settingsSaving ? "保存中…" : "保存"}
                  </button>
                </div>
              </form>
            )}
          </div>
        </div>
      )}

      {oauthModalOpen && oauthSession && (
        <div className="modal" style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,0.4)", display: "grid", placeItems: "center", zIndex: 30 }}>
          <div style={{ background: "#fff", padding: 20, borderRadius: 10, width: 600, maxWidth: "92vw", maxHeight: "90vh", overflowY: "auto", display: "flex", flexDirection: "column", gap: 16 }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
              <h4 style={{ margin: 0 }}>通过 Notion OAuth 连接</h4>
              <button className="btn btn--ghost" onClick={resetOauthFlow}>关闭</button>
            </div>
            <p style={{ margin: 0 }}>
              1. 浏览器会跳转到 Notion 授权页；2. 选择工作区并确认授权；3. 授权完成后复制完整回调 URL（包含 code、state 参数）并粘贴到下方输入框。
            </p>
            <div style={{ display: "flex", gap: 12, alignItems: "center", flexWrap: "wrap" }}>
              <button
                type="button"
                className="btn btn--ghost"
                onClick={async () => {
                  try {
                    await openUrl(oauthSession.authorizationUrl);
                  } catch (err) {
                    console.warn("failed to reopen oauth url", err);
                  }
                }}
              >
                重新打开授权页面
              </button>
              {oauthExpired && (
                <button
                  type="button"
                  className="btn btn--primary"
                  onClick={onStartOauth}
                  disabled={startingOauth}
                >
                  {startingOauth ? "重新生成中…" : "重新生成授权链接"}
                </button>
              )}
              <span className="muted">state：<code>{oauthSession.state}</code></span>
              <span className="muted">有效期剩余：{oauthExpiresLabel}</span>
            </div>
            <form onSubmit={onOauthSubmit} style={{ display: "flex", flexDirection: "column", gap: 12 }}>
              <div className="form-field">
                <label>回调 URL</label>
                <textarea
                  value={oauthUrl}
                  onChange={(e) => setOauthUrl(e.target.value)}
                  placeholder="https://www.yuributa.com/?code=...&state=...#/"
                  rows={3}
                  style={{ width: "100%", resize: "vertical" }}
                />
                {oauthUrlInvalid && <p className="error">无法解析该 URL，请确认以 https 开头并包含完整参数。</p>}
                {oauthMissingCode && <p className="error">未检测到 code 参数，请复制 Notion 回调后的完整地址。</p>}
                {oauthMissingState && <p className="error">未检测到 state 参数，请确保使用同一次授权的回调地址。</p>}
                {oauthExpired && <p className="error">授权 state 已过期，请重新生成授权链接后再粘贴最新的回调 URL。</p>}
                {oauthStateMismatch && <p className="error">state 不匹配，请重新点击「重新打开授权页面」并完成授权。</p>}
                {oauthStateReady && parsedOauth.code && <p className="muted">state 校验成功，检测到授权码 <code>{parsedOauth.code.slice(0, 6)}...</code></p>}
              </div>
              <div className="form-field">
                <label>Token 别名</label>
                <input
                  type="text"
                  value={oauthName}
                  onChange={(e) => setOauthName(e.target.value)}
                  placeholder="例如：Notion OAuth（M1）"
                />
              </div>
              {oauthError && <p className="error">{oauthError}</p>}
              <div className="token-form-actions" style={{ display: "flex", justifyContent: "flex-end", gap: 12 }}>
                <button type="button" className="btn btn--ghost" onClick={resetOauthFlow}>取消</button>
                <button
                  type="submit"
                  className="btn btn--primary"
                  disabled={!oauthReady}
                >
                  {oauthSubmitting ? "保存中…" : "保存 OAuth Token"}
                </button>
              </div>
            </form>
          </div>
        </div>
      )}
    </div>
  );
}

// -----------------------------
// Step 1: 选择 Token（从已保存列表中选择；支持打开管理对话框）
// -----------------------------

function TokenSelectStep(props: { value?: string | null; onChange?: (id: string | null) => void }) {
  const { tokens, loading, error, remove, refresh } = useTokens();
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

  const describeExpiry = (
    expiresAt?: number | null,
  ): { text: string; tone: "normal" | "warning" | "danger" } | null => {
    if (!expiresAt) return null;
    const diff = expiresAt - Date.now();
    const absolute = new Date(expiresAt).toLocaleString();
    if (diff <= 0) {
      return { text: `已过期（${absolute}）`, tone: "danger" };
    }
    const hours = Math.floor(diff / 3_600_000);
    const minutes = Math.floor((diff % 3_600_000) / 60_000);
    const seconds = Math.floor((diff % 60_000) / 1_000);
    const formatted =
      hours > 0
        ? `${hours} 小时 ${minutes} 分 ${seconds.toString().padStart(2, "0")} 秒`
        : `${minutes} 分 ${seconds.toString().padStart(2, "0")} 秒`;
    const tone: "normal" | "warning" = diff <= 10 * 60_000 ? "warning" : "normal";
    return { text: `剩余 ${formatted}（${absolute} 到期）`, tone };
  };

  return (
    <div className="token-select">
      <div style={{ display: "flex", gap: 12, alignItems: "center", marginBottom: 8 }}>
        <button type="button" className="btn btn--ghost" onClick={() => setShowManager(true)}>管理 Token</button>
      </div>
      {loading && <p>加载中…</p>}
      {error && <p className="error">{error}</p>}

      <div className="token-list" style={{ display: "flex", flexDirection: "column", gap: 8, marginTop: 8 }}>
        {tokens.map((t) => {
          const isOauth = t.kind === "oauth";
          const expiryInfo = isOauth ? describeExpiry(t.expiresAt) : null;
          const workspaceLabel = t.workspaceName && t.workspaceName.trim().length > 0 ? t.workspaceName : "(未知工作区)";
          const toneColor = expiryInfo?.tone === "danger" ? "#d14343" : expiryInfo?.tone === "warning" ? "#c47f17" : "#4f6f52";
          return (
            <label
              key={t.id}
              style={{
                border: selected === t.id ? "1px solid #3b82f6" : "1px solid #ddd",
                borderRadius: 6,
                padding: 10,
                display: "flex",
                alignItems: "center",
                gap: 12,
                cursor: "pointer",
              }}
            >
              <input
                type="radio"
                name="token"
                checked={selected === t.id}
                onChange={() => setSelected(t.id)}
              />
              <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 4 }}>
                <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                  <strong>{t.name}</strong>
                  <span className="muted">{isOauth ? "OAuth" : "手动"}</span>
                  <span className="muted">{workspaceLabel}</span>
                </div>
                {isOauth && expiryInfo && (
                  <span style={{ fontSize: 12, color: toneColor }}>{expiryInfo.text}</span>
                )}
                {isOauth && !expiryInfo && (
                  <span className="muted" style={{ fontSize: 12 }}>未提供有效期信息</span>
                )}
                {t.lastRefreshError && (
                  <span className="error" style={{ fontSize: 12 }}>上次刷新失败：{t.lastRefreshError}</span>
                )}
              </div>
              <button
                type="button"
                className="btn btn--danger"
                onClick={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  remove(t.id);
                }}
              >
                删除
              </button>
            </label>
          );
        })}
        {tokens.length === 0 && <p className="muted">尚未添加 Token，请点击「管理 Token」新增。</p>}
      </div>

      {showManager && (
        <div className="modal" style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,0.4)", display: "grid", placeItems: "center" }}>
          <div style={{ background: "#fff", padding: 16, borderRadius: 10, width: 640, maxWidth: "90vw" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
              <h4 style={{ margin: 0 }}>Token 管理</h4>
              <button className="btn btn--ghost" onClick={() => setShowManager(false)}>关闭</button>
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
        <button className="btn btn--ghost" onClick={props.onPrev}>返回上一步</button>
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
        <button className="btn btn--ghost" onClick={onSearch} disabled={!tokenId || loading}>{loading ? "搜索中…" : "搜索"}</button>
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
            <button className="btn btn--ghost" onClick={props.onPrev} style={{ marginLeft: 8 }}>返回选择 Token</button>
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
                  <button className="btn btn--primary" onClick={() => props.onSelect(db as DbBrief)}>选择</button>
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
        <button className="btn btn--ghost" onClick={onPrev} disabled={pageIndex <= 0 || loading}>上一页</button>
        <button className="btn btn--ghost" onClick={onNext} disabled={!page.hasMore || loading}>下一页</button>
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
      const selected = await openFileDialog({
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
        <button className="btn btn--ghost" onClick={props.onPrev}>返回选择数据库</button>
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
          className="btn btn--ghost"
          onClick={async () => {
            if (!filePath) return
            await loadPreview(filePath, fileType === 'auto' ? undefined : fileType)
          }}
          disabled={!filePath || loading}
        >
          加载预览
        </button>
        <button className="btn btn--ghost" onClick={chooseFile}>{filePath ? "重新选择文件" : "选择文件"}</button>
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
          className="btn btn--primary"
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
