import type { ChangeEvent } from "react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type Nullable<T> = T | null | undefined;

type PortUsage = {
  protocol: string;
  localAddress: string;
  localPort?: Nullable<number>;
  remoteAddress?: Nullable<string>;
  remotePort?: Nullable<number>;
  pid?: Nullable<number>;
  processName?: Nullable<string>;
};

type FavoriteRecord = {
  protocol: string;
  localAddress: string;
  localPort?: Nullable<number>;
};

type ToolId = "port-monitor" | "clipboard" | "snippets";

type ToolMeta = {
  id: ToolId;
  label: string;
  description: string;
  status: "ready" | "planned";
};

const TOOL_CATALOG: ToolMeta[] = [
  {
    id: "port-monitor",
    label: "端口占用监视器",
    description: "列出当前系统占用的端口、协议、进程名称和 PID，方便排查冲突。",
    status: "ready",
  },
  {
    id: "clipboard",
    label: "剪贴板助手",
    description: "管理剪贴板历史并快速粘贴常用文本。（规划中）",
    status: "planned",
  },
  {
    id: "snippets",
    label: "快捷片段",
    description: "收集高频命令或代码片段，便于一键复制。（规划中）",
    status: "planned",
  },
];

function App() {
  const [selectedTool, setSelectedTool] = useState<ToolId>("port-monitor");

  const activeTool = useMemo(
    () => TOOL_CATALOG.find((tool) => tool.id === selectedTool) ?? TOOL_CATALOG[0],
    [selectedTool],
  );

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="sidebar-header">
          <h1>工具集</h1>
          <p>快速解决桌面端常见需求。</p>
        </div>
        <nav>
          <ul className="tool-list">
            {TOOL_CATALOG.map((tool) => {
              const isActive = tool.id === activeTool.id;
              return (
                <li key={tool.id}>
                  <button
                    type="button"
                    className={`tool-item${isActive ? " active" : ""}`}
                    onClick={() => setSelectedTool(tool.id)}
                  >
                    <span className="tool-item-label">{tool.label}</span>
                    {tool.status === "planned" && <span className="badge">规划中</span>}
                  </button>
                </li>
              );
            })}
          </ul>
        </nav>
      </aside>

      <main className="content">
        <header className="content-header">
          <h2>{activeTool.label}</h2>
          <p className="tool-description">{activeTool.description}</p>
        </header>

        <section className="content-body">
          {activeTool.id === "port-monitor" ? (
            <PortActivityTool />
          ) : (
            <ComingSoon label={activeTool.label} />
          )}
        </section>
      </main>
    </div>
  );
}

const buildFavoriteKey = (data: { protocol: string; localAddress: string; localPort?: Nullable<number> }) => {
  const protocol = data.protocol?.toUpperCase?.() ?? "UNKNOWN";
  const localAddress = data.localAddress ?? "";
  const localPort = data.localPort != null ? data.localPort : "-";
  return `${protocol}|${localAddress}|${localPort}`;
};

function PortActivityTool() {
  const [ports, setPorts] = useState<PortUsage[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null);
  const [protocolFilter, setProtocolFilter] = useState<"all" | "tcp" | "udp" | "other">("all");
  const [searchKeyword, setSearchKeyword] = useState("");
  const [portRange, setPortRange] = useState<{ from: string; to: string }>({ from: "", to: "" });
  const [favorites, setFavorites] = useState<Set<string>>(() => new Set());
  const [viewMode, setViewMode] = useState<"all" | "favorites">("all");

  const sortPorts = useCallback((items: PortUsage[]) => {
    return [...items].sort((a, b) => {
      const protocolCompare = a.protocol.localeCompare(b.protocol);
      if (protocolCompare !== 0) {
        return protocolCompare;
      }

      const portA = a.localPort ?? 0;
      const portB = b.localPort ?? 0;
      if (portA !== portB) {
        return portA - portB;
      }

      const nameA = a.processName ?? "";
      const nameB = b.processName ?? "";
      return nameA.localeCompare(nameB);
    });
  }, []);

  const loadPorts = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);

      const result = await invoke<PortUsage[]>("list_ports");
      const sorted = sortPorts(result);
      setPorts(sorted);
      setLastUpdated(new Date());
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setError(message);
    } finally {
      setLoading(false);
    }
  }, [sortPorts]);

  useEffect(() => {
    loadPorts();
  }, [loadPorts]);

  const syncFavorites = useCallback(async () => {
    try {
      const result = await invoke<FavoriteRecord[]>("list_port_favorites");
      setFavorites(new Set(result.map(buildFavoriteKey)));
    } catch (err) {
      console.error("Failed to fetch favorites", err);
    }
  }, []);

  useEffect(() => {
    syncFavorites();
  }, [syncFavorites]);

  const summary = useMemo(() => {
    const total = ports.length;
    const uniqueProcesses = new Set(
      ports.map((port) => {
        if (port.pid != null) {
          return `pid:${port.pid}`;
        }

        if (port.processName != null && port.processName.length > 0) {
          return `name:${port.processName}`;
        }

        return "?";
      }),
    ).size;
    return { total, uniqueProcesses };
  }, [ports]);

  const filteredPorts = useMemo(() => {
    const normalizedKeyword = searchKeyword.trim().toLowerCase();
    const fromValue = parseInt(portRange.from, 10);
    const toValue = parseInt(portRange.to, 10);
    const hasFrom = Number.isFinite(fromValue);
    const hasTo = Number.isFinite(toValue);

    return ports.filter((port) => {
      const protocol = port.protocol?.toUpperCase() ?? "UNKNOWN";
      const matchesProtocol =
        protocolFilter === "all" ||
        (protocolFilter === "other"
          ? protocol !== "TCP" && protocol !== "UDP"
          : protocol === protocolFilter.toUpperCase());

      const matchesKeyword =
        normalizedKeyword.length === 0 ||
        [port.processName, port.localAddress, port.remoteAddress].some((value) => {
          if (!value) {
            return false;
          }
          return value.toLowerCase().includes(normalizedKeyword);
        });

      const localPort = port.localPort ?? null;
      const matchesFrom = !hasFrom || (localPort != null && localPort >= fromValue);
      const matchesTo = !hasTo || (localPort != null && localPort <= toValue);

      if (!(matchesProtocol && matchesKeyword && matchesFrom && matchesTo)) {
        return false;
      }

      if (viewMode === "favorites") {
        return favorites.has(buildFavoriteKey(port));
      }

      return true;
    });
  }, [favorites, ports, portRange.from, portRange.to, protocolFilter, searchKeyword, viewMode]);

  const handleSearchChange = useCallback((event: ChangeEvent<HTMLInputElement>) => {
    setSearchKeyword(event.target.value);
  }, []);

  const handlePortRangeChange = useCallback((event: ChangeEvent<HTMLInputElement>) => {
    const { name, value } = event.target;
    const sanitized = value.replace(/[^0-9]/g, "");
    setPortRange((prev) => ({ ...prev, [name]: sanitized }));
  }, []);

  const resetFilters = useCallback(() => {
    setProtocolFilter("all");
    setSearchKeyword("");
    setPortRange({ from: "", to: "" });
    setViewMode("all");
  }, []);

  const toggleFavorite = useCallback(
    async (port: PortUsage) => {
      const key = buildFavoriteKey(port);
      const nextStateIsFavorite = !favorites.has(key);

      setFavorites((prev) => {
        const next = new Set(prev);
        if (nextStateIsFavorite) {
          next.add(key);
        } else {
          next.delete(key);
        }
        return next;
      });

      try {
        await invoke("update_port_favorite", {
          favorite: {
            protocol: port.protocol ?? "",
            localAddress: port.localAddress,
            localPort: port.localPort ?? null,
          },
          isFavorite: nextStateIsFavorite,
        });
      } catch (err) {
        console.error("Failed to update favorite", err);
        setFavorites((prev) => {
          const next = new Set(prev);
          if (nextStateIsFavorite) {
            next.delete(key);
          } else {
            next.add(key);
          }
          return next;
        });
      }
    },
    [favorites],
  );

  const isFavorite = useCallback(
    (port: PortUsage) => favorites.has(buildFavoriteKey(port)),
    [favorites],
  );

  const formatEndpoint = useCallback((address?: Nullable<string>, port?: Nullable<number>) => {
    if ((address == null || address.length === 0) && port == null) {
      return "-";
    }

    if (address == null || address.length === 0) {
      return port == null ? "*" : `*:${port}`;
    }

    if (port == null) {
      return address;
    }

    return `${address}:${port}`;
  }, []);

  return (
    <div className="tool-panel">
      <section className="controls">
        <button onClick={loadPorts} disabled={loading}>
          {loading ? "刷新中..." : "刷新"}
        </button>
        <div className="status">
          <span>共 {summary.total} 条记录</span>
          <span className="divider">·</span>
          <span>{summary.uniqueProcesses} 个唯一进程</span>
          <span className="divider">·</span>
          <span>
            筛选后 {filteredPorts.length} 条（收藏 {favorites.size}）
          </span>
          <span className="divider">·</span>
          <span>{lastUpdated ? `上次更新：${lastUpdated.toLocaleTimeString()}` : "尚未更新"}</span>
        </div>
      </section>

      <section className="filter-bar">
        <div className="filter-group view-mode">
          <span className="filter-label">视图</span>
          <div className="view-toggle">
            <button
              type="button"
              className={`view-toggle-btn${viewMode === "all" ? " active" : ""}`}
              onClick={() => setViewMode("all")}
            >
              全部
            </button>
            <button
              type="button"
              className={`view-toggle-btn${viewMode === "favorites" ? " active" : ""}`}
              onClick={() => setViewMode("favorites")}
            >
              收藏
            </button>
          </div>
        </div>

        <div className="filter-group">
          <label className="filter-label" htmlFor="protocol-filter">
            协议
          </label>
          <select
            id="protocol-filter"
            value={protocolFilter}
            onChange={(event) =>
              setProtocolFilter(event.target.value as "all" | "tcp" | "udp" | "other")
            }
          >
            <option value="all">全部</option>
            <option value="tcp">仅 TCP</option>
            <option value="udp">仅 UDP</option>
            <option value="other">其他</option>
          </select>
        </div>

        <div className="filter-group keyword">
          <label className="filter-label" htmlFor="keyword-filter">
            关键字
          </label>
          <input
            id="keyword-filter"
            type="text"
            placeholder="进程 / 地址关键字"
            value={searchKeyword}
            onChange={handleSearchChange}
          />
        </div>

        <div className="filter-group range">
          <span className="filter-label">本地端口</span>
          <div className="range-inputs">
            <input
              type="text"
              inputMode="numeric"
              name="from"
              placeholder="起始"
              value={portRange.from}
              onChange={handlePortRangeChange}
            />
            <span className="range-separator">-</span>
            <input
              type="text"
              inputMode="numeric"
              name="to"
              placeholder="结束"
              value={portRange.to}
              onChange={handlePortRangeChange}
            />
          </div>
        </div>

        <button type="button" className="reset-button" onClick={resetFilters}>
          重置筛选
        </button>
      </section>

      {error && <div className="error">加载失败：{error}</div>}

      <section className="table-wrapper">
        <table>
          <thead>
            <tr>
              <th>收藏</th>
              <th>协议</th>
              <th>本地地址</th>
              <th>远端地址</th>
              <th>进程</th>
              <th>PID</th>
            </tr>
          </thead>
          <tbody>
            {filteredPorts.length === 0 ? (
              <tr>
                <td colSpan={6} className="empty">
                  {loading ? "正在加载端口信息..." : "没有符合筛选条件的数据"}
                </td>
              </tr>
            ) : (
              filteredPorts.map((item, index) => (
                <tr key={`${item.pid ?? "unknown"}-${item.localAddress}-${item.localPort}-${index}`}>
                  <td>
                    <button
                      type="button"
                      className={`favorite-btn${isFavorite(item) ? " active" : ""}`}
                      onClick={() => toggleFavorite(item)}
                      aria-pressed={isFavorite(item)}
                      aria-label={isFavorite(item) ? "取消收藏" : "收藏该端口"}
                    >
                      {isFavorite(item) ? "★" : "☆"}
                    </button>
                  </td>
                  <td>{item.protocol}</td>
                  <td>{formatEndpoint(item.localAddress, item.localPort)}</td>
                  <td>{formatEndpoint(item.remoteAddress, item.remotePort)}</td>
                  <td>{item.processName ?? "未知"}</td>
                  <td>{item.pid ?? "-"}</td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </section>

      <footer>
        <p className="note">
          Windows 上的数据依赖 netstat/tasklist，若权限不足可能无法完整获取。
        </p>
      </footer>
    </div>
  );
}

function ComingSoon({ label }: { label: string }) {
  return (
    <div className="coming-soon">
      <div className="coming-soon-card">
        <h3>{label}</h3>
        <p>该工具正在规划中，欢迎补充需求或优先级。</p>
      </div>
    </div>
  );
}

export default App;
