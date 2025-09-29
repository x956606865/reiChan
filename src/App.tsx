import type { ChangeEvent } from "react";
import type { JSX } from "react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";
import MangaUpscaleAgent from "./features/manga/MangaUpscaleAgent";
import NotionImportAgent from "./features/notion-import/NotionImportAgent";

type Nullable<T> = T | null | undefined;

type ProcessLink = {
  pid?: Nullable<number>;
  processName?: Nullable<string>;
};

type PortUsage = {
  protocol: string;
  localAddress: string;
  localPort?: Nullable<number>;
  remoteAddress?: Nullable<string>;
  remotePort?: Nullable<number>;
  pid?: Nullable<number>;
  processName?: Nullable<string>;
  parentPid?: Nullable<number>;
  parentProcessName?: Nullable<string>;
  ancestors?: ProcessLink[];
};

type ProcessTreeNode = {
  key: string;
  pid: Nullable<number>;
  processName: string;
  ports: PortUsage[];
  children: ProcessTreeNode[];
  portNumbers: number[];
};

type KillTarget =
  | {
      kind: "port";
      port: PortUsage;
    }
  | {
      kind: "process";
      pid: number;
      processName?: Nullable<string>;
      portNumbers: number[];
    };

type FavoriteRecord = {
  protocol: string;
  localAddress: string;
  localPort?: Nullable<number>;
};

type ToolId = "port-monitor" | "manga-upscale" | "notion-import" | "clipboard" | "snippets";

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
    id: "manga-upscale",
    label: "漫画高清化",
    description: "提供重命名、上传 Copyparty 的基础流程，准备后端推理。",
    status: "ready",
  },
  {
    id: "notion-import",
    label: "Notion 导入",
    description: "从 JSON/CSV 导入到 Notion 数据库（M1 框架）。",
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
          {activeTool.id === "port-monitor" && <PortActivityTool />}
          {activeTool.id === "manga-upscale" && <MangaUpscaleAgent />}
          {activeTool.id === "notion-import" && <NotionImportAgent />}
          {activeTool.id !== "port-monitor" && activeTool.id !== "manga-upscale" && (
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

const getProcessDisplayName = (name?: Nullable<string>): string => {
  const trimmed = name?.trim();
  if (!trimmed || trimmed.length === 0) {
    return "未知进程";
  }
  const segments = trimmed.split(/[\\/]+/).filter((segment) => segment.length > 0);
  if (segments.length === 0) {
    return trimmed;
  }
  return segments[segments.length - 1];
};

const getProcessTitle = (name?: Nullable<string>): string => {
  const trimmed = name?.trim();
  if (!trimmed || trimmed.length === 0) {
    return "未知进程";
  }
  return trimmed;
};

const buildPortSummary = (ports: number[]): string => {
  if (ports.length === 0) {
    return "";
  }
  const MAX_DISPLAY = 4;
  if (ports.length <= MAX_DISPLAY) {
    return ports.join(", ");
  }
  const visible = ports.slice(0, MAX_DISPLAY).join(", ");
  return `${visible}…`;
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
  const [killingPids, setKillingPids] = useState<Set<number>>(() => new Set());
  const [pendingKill, setPendingKill] = useState<KillTarget | null>(null);
  const [layoutMode, setLayoutMode] = useState<"table" | "tree">("table");

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
      const normalized = result.map((item) => ({
        ...item,
        ancestors: item.ancestors ?? [],
      }));
      const sorted = sortPorts(normalized);
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

  const processTree = useMemo(() => {
    type MutableNode = {
      key: string;
      pid: Nullable<number>;
      processName: string;
      ports: PortUsage[];
      children: Map<string, MutableNode>;
    };

    const rootMap = new Map<string, MutableNode>();

    filteredPorts.forEach((port) => {
      const chain: ProcessLink[] = [...(port.ancestors ?? [])];
      chain.push({ pid: port.pid, processName: port.processName });

      if (chain.length === 0) {
        return;
      }

      let currentMap = rootMap;
      let pathKey = "root";

      chain.forEach((link, index) => {
        const pid = link.pid ?? null;
        const processName = link.processName && link.processName.length > 0 ? link.processName : "未知进程";
        pathKey = `${pathKey}-${pid != null ? pid : processName}`;
        const key = pathKey;

        let node = currentMap.get(key);
        if (!node) {
          node = {
            key,
            pid,
            processName,
            ports: [],
            children: new Map(),
          };
          currentMap.set(key, node);
        }

        if (index === chain.length - 1) {
          node.ports.push(port);
        }

        currentMap = node.children;
      });
    });

    const toTreeNodes = (map: Map<string, MutableNode>): ProcessTreeNode[] =>
      Array.from(map.values()).map((node) => {
        const children = toTreeNodes(node.children);
        const portSet = new Set<number>();
        node.ports.forEach((port) => {
          if (port.localPort != null) {
            portSet.add(port.localPort);
          }
        });
        children.forEach((child) => {
          child.portNumbers.forEach((value) => portSet.add(value));
        });

        return {
          key: node.key,
          pid: node.pid,
          processName: node.processName,
          ports: node.ports,
          children,
          portNumbers: Array.from(portSet).sort((a, b) => a - b),
        };
      });

    const sortNodes = (nodes: ProcessTreeNode[]): ProcessTreeNode[] =>
      nodes
        .map((node) => ({
          ...node,
          children: sortNodes(node.children),
        }))
        .sort((a, b) => {
          const nameCompare = a.processName.localeCompare(b.processName, "zh-Hans-CN");
          if (nameCompare !== 0) {
            return nameCompare;
          }
          if (a.pid == null && b.pid == null) {
            return 0;
          }
          if (a.pid == null) {
            return 1;
          }
          if (b.pid == null) {
            return -1;
          }
          return a.pid - b.pid;
        });

    return sortNodes(toTreeNodes(rootMap));
  }, [filteredPorts]);

  const executeKill = useCallback(
    async (pid: number, processName?: Nullable<string>) => {
      if (!Number.isFinite(pid)) {
        setError("终止进程失败：PID 非法");
        return;
      }

      const label = processName?.length ? `${processName} (PID ${pid})` : `PID ${pid}`;

      setKillingPids((prev) => {
        const next = new Set(prev);
        next.add(pid);
        return next;
      });

      try {
        await invoke("kill_port_process", { pid });
        await loadPorts();
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setError(`终止 ${label} 失败：${message}`);
      } finally {
        setKillingPids((prev) => {
          const next = new Set(prev);
          next.delete(pid);
          return next;
        });
      }
    },
    [loadPorts],
  );

  const requestKillPort = useCallback(
    (port: PortUsage) => {
      if (port.pid == null) {
        setError("终止进程失败：缺少 PID 信息");
        return;
      }
      setPendingKill({ kind: "port", port });
    },
    [],
  );

  const requestKillProcess = useCallback(
    (pid: number, processName: Nullable<string>, portNumbers: number[]) => {
      if (!Number.isFinite(pid) || pid <= 0) {
        setError("终止进程失败：PID 非法");
        return;
      }
      setPendingKill({ kind: "process", pid, processName, portNumbers });
    },
    [],
  );

  const confirmKill = useCallback(async () => {
    if (!pendingKill) {
      return;
    }

    const target = pendingKill;
    setPendingKill(null);

    if (target.kind === "port") {
      const pid = target.port.pid;
      if (pid == null) {
        setError("终止进程失败：缺少 PID 信息");
        return;
      }
      await executeKill(pid, target.port.processName);
    } else {
      await executeKill(target.pid, target.processName);
    }
  }, [executeKill, pendingKill]);

  const cancelKill = useCallback(() => {
    setPendingKill(null);
  }, []);

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

  const computePortCount = useCallback((node: ProcessTreeNode): number => {
    const childCount = node.children.reduce((total, child) => total + computePortCount(child), 0);
    return node.ports.length + childCount;
  }, []);

  const renderProcessTree = useCallback(
    (nodes: ProcessTreeNode[], depth = 0): JSX.Element => (
      <ul className={`process-tree${depth === 0 ? " root" : ""}`}>
        {nodes.map((node) => {
          const totalPorts = computePortCount(node);
          const nodePid = node.pid != null && node.pid > 0 ? node.pid : null;
          const isProcessKilling = nodePid != null && killingPids.has(nodePid);
          const displayName = getProcessDisplayName(node.processName);
          const nodeTitle = getProcessTitle(node.processName);
          return (
            <li key={`${node.key}-${depth}`} className="process-tree-node">
              <details>
                <summary>
                  <div className="tree-node-head">
                    <div className="tree-node-title">
                      <span className="tree-node-name" title={nodeTitle}>
                        {displayName}
                      </span>
                      <span className="tree-node-meta">PID {node.pid ?? "-"}</span>
                    </div>
                    <div className="tree-node-meta-group">
                      <span className="tree-node-count">{totalPorts} 个端口</span>
                      {node.portNumbers.length > 0 && (
                        <span
                          className="tree-node-ports"
                          title={`端口：${node.portNumbers.join(", ")}`}
                        >
                          ({buildPortSummary(node.portNumbers)})
                        </span>
                      )}
                      {nodePid != null && (
                        <button
                          type="button"
                          className="kill-btn compact"
                          disabled={isProcessKilling || loading}
                          onClick={(event) => {
                            event.preventDefault();
                            event.stopPropagation();
                            requestKillProcess(nodePid, node.processName, node.portNumbers);
                          }}
                        >
                          {isProcessKilling ? "终止中..." : "终止进程"}
                        </button>
                      )}
                    </div>
                  </div>
                </summary>
                {node.children.length > 0 && renderProcessTree(node.children, depth + 1)}
                {node.ports.length > 0 && (
                  <ul className="tree-port-list">
                    {node.ports.map((port, index) => {
                      const portPid = port.pid ?? null;
                      const key = `${portPid ?? "unknown"}-${port.localAddress}-${port.localPort}-${index}`;
                      const isKilling = portPid != null && killingPids.has(portPid);
                      const parentTitle = getProcessTitle(port.parentProcessName);
                      const parentDisplay = getProcessDisplayName(port.parentProcessName);
                      return (
                        <li key={key} className="tree-port-item">
                          <div className="tree-port-main">
                            <div className="tree-port-info">
                              <span className="tree-port-protocol">{port.protocol}</span>
                              <span className="tree-port-endpoint tree-port-endpoint-local">
                                {formatEndpoint(port.localAddress, port.localPort)}
                              </span>
                              <span className="tree-port-arrow" aria-hidden>
                                →
                              </span>
                              <span className="tree-port-endpoint tree-port-endpoint-remote">
                                {formatEndpoint(port.remoteAddress, port.remotePort)}
                              </span>
                            </div>
                            <div className="tree-port-meta-row">
                              <span className="tree-port-pid">PID {portPid ?? "-"}</span>
                              {port.parentProcessName && (
                                <span className="tree-port-parent" title={parentTitle}>
                                  来自 {parentDisplay}
                                </span>
                              )}
                            </div>
                          </div>
                          <div className="tree-port-actions">
                            <button
                              type="button"
                              className={`favorite-btn${isFavorite(port) ? " active" : ""}`}
                              onClick={() => toggleFavorite(port)}
                              aria-pressed={isFavorite(port)}
                              aria-label={isFavorite(port) ? "取消收藏" : "收藏该端口"}
                            >
                              {isFavorite(port) ? "★" : "☆"}
                            </button>
                            <button
                              type="button"
                              className="kill-btn"
                              disabled={portPid == null || isKilling || loading}
                              onClick={() => requestKillPort(port)}
                            >
                              {isKilling ? "终止中..." : "终止"}
                            </button>
                          </div>
                        </li>
                      );
                    })}
                  </ul>
                )}
              </details>
            </li>
          );
        })}
      </ul>
    ),
    [
      computePortCount,
      formatEndpoint,
      isFavorite,
      killingPids,
      loading,
      requestKillPort,
      requestKillProcess,
      toggleFavorite,
    ],
  );

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

        <div className="filter-group layout-mode">
          <span className="filter-label">布局</span>
          <div className="layout-toggle">
            <button
              type="button"
              className={`layout-toggle-btn${layoutMode === "table" ? " active" : ""}`}
              onClick={() => setLayoutMode("table")}
            >
              表格
            </button>
            <button
              type="button"
              className={`layout-toggle-btn${layoutMode === "tree" ? " active" : ""}`}
              onClick={() => setLayoutMode("tree")}
            >
              树形
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

      {layoutMode === "table" ? (
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
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              {filteredPorts.length === 0 ? (
                <tr>
                  <td colSpan={7} className="empty">
                    {loading ? "正在加载端口信息..." : "没有符合筛选条件的数据"}
                  </td>
                </tr>
              ) : (
                filteredPorts.map((item, index) => {
                  const pid = item.pid ?? null;
                  const isKilling = pid != null && killingPids.has(pid);

                  return (
                    <tr key={`${pid ?? "unknown"}-${item.localAddress}-${item.localPort}-${index}`}>
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
                    <td>
                      <span className="process-name" title={getProcessTitle(item.processName)}>
                        {getProcessDisplayName(item.processName)}
                      </span>
                    </td>
                      <td>{pid ?? "-"}</td>
                      <td>
                        <button
                          type="button"
                          className="kill-btn"
                          disabled={pid == null || isKilling || loading}
                          onClick={() => requestKillPort(item)}
                        >
                          {isKilling ? "终止中..." : "终止"}
                        </button>
                      </td>
                    </tr>
                  );
                })
              )}
            </tbody>
          </table>
        </section>
      ) : (
        <section className="tree-wrapper">
          {filteredPorts.length === 0 ? (
            <div className="tree-empty">
              {loading ? "正在加载端口信息..." : "没有符合筛选条件的数据"}
            </div>
          ) : (
            renderProcessTree(processTree)
          )}
        </section>
      )}

      <footer>
        <p className="note">
          Windows 上的数据依赖 netstat/tasklist，若权限不足可能无法完整获取。
        </p>
      </footer>

      {pendingKill && (
        <div className="kill-dialog-backdrop" role="presentation">
          <div className="kill-dialog" role="dialog" aria-modal="true" aria-labelledby="kill-dialog-title">
            <h3 id="kill-dialog-title">确认终止进程</h3>
            {pendingKill.kind === "port" ? (
              <>
                <p>
                  即将终止进程
                  {pendingKill.port.processName ? ` ${pendingKill.port.processName}` : ""}
                  {pendingKill.port.pid != null ? ` (PID ${pendingKill.port.pid})` : ""}，对应端口
                  {" "}
                  {formatEndpoint(pendingKill.port.localAddress, pendingKill.port.localPort)}。
                </p>
                <p>该操作无法撤销，请确认当前进程已不再需要。</p>
              </>
            ) : (
              <>
                <p>
                  即将终止父进程
                  {pendingKill.processName ? ` ${pendingKill.processName}` : ""} (PID {pendingKill.pid})。
                  {pendingKill.portNumbers.length > 0 ? (
                    <span>
                      其直接或子进程共占用 {pendingKill.portNumbers.length} 个端口：
                      <span title={pendingKill.portNumbers.join(", ")}>
                        {buildPortSummary(pendingKill.portNumbers)}
                      </span>
                      。
                    </span>
                  ) : (
                    <span> 当前未检测到活跃端口。</span>
                  )}
                </p>
                <p>终止后该进程及其子进程会被一并结束，请谨慎操作。</p>
              </>
            )}
            <div className="kill-dialog-actions">
              <button
                type="button"
                className="kill-dialog-confirm"
                onClick={confirmKill}
                disabled={loading}
              >
                确认终止
              </button>
              <button type="button" className="kill-dialog-cancel" onClick={cancelKill}>
                取消
              </button>
            </div>
          </div>
        </div>
      )}
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
