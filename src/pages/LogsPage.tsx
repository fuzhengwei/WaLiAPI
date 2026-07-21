import { useEffect, useState, useCallback, useMemo } from "react";
import { logApi } from "../lib/api";
import type { RequestLog, SecurityFinding } from "../types";
import { formatTime, formatDuration, formatNumber } from "../lib/constants";
import {
  ScrollText, RefreshCw, Trash2, ChevronDown, ChevronRight, AlertCircle,
  Bot, User, Wrench, Terminal, Eye, FileCode2, Image, ArrowRightLeft, ArrowUp, ArrowDown, Shield, Timer, Coins,
  Search, X, Calendar, Key, Server, Box, ShieldAlert,
} from "lucide-react";

const PAGE_SIZE = 20;

const RISK_META: Record<string, { label: string; cls: string }> = {
  clean: { label: "安全", cls: "bg-emerald-50 text-emerald-700 border-emerald-200" },
  info: { label: "提示", cls: "bg-slate-50 text-slate-600 border-slate-200" },
  low: { label: "低风险", cls: "bg-blue-50 text-blue-700 border-blue-200" },
  medium: { label: "中风险", cls: "bg-amber-50 text-amber-700 border-amber-200" },
  high: { label: "高风险", cls: "bg-orange-50 text-orange-700 border-orange-200" },
  critical: { label: "严重", cls: "bg-red-50 text-red-700 border-red-200" },
};
function getRiskMeta(level?: string) {
  return RISK_META[level || "clean"] || RISK_META.clean;
}

// ─── Helpers ────────────────────────────────────────────────────────────────

interface ToolCall {
  id?: string;
  type?: string;
  function?: {
    name?: string;
    arguments?: string;
  };
}

/** Extract all tool/function names from a message object */
function extractToolNames(msg: Record<string, unknown>): string[] {
  const names: string[] = [];
  // Anthropic tool_use blocks in content array
  if (Array.isArray(msg.content)) {
    for (const block of msg.content as Array<Record<string, unknown>>) {
      if (block.type === "tool_use" && typeof block.name === "string") {
        names.push(block.name);
      }
    }
  }
  // OpenAI tool_calls
  if (Array.isArray(msg.tool_calls)) {
    for (const tc of msg.tool_calls as Array<Record<string, unknown>>) {
      const fn = tc.function as Record<string, unknown> | undefined;
      if (fn && typeof fn.name === "string") names.push(fn.name);
    }
  }
  // legacy function_call
  if (msg.function_call && typeof msg.function_call === "object") {
    const fc = msg.function_call as Record<string, unknown>;
    if (typeof fc.name === "string") names.push(fc.name);
  }
  return names;
}

/** Extract tool_calls details from a message object */
function extractToolCalls(msg: Record<string, unknown>): ToolCall[] {
  const result: ToolCall[] = [];
  if (Array.isArray(msg.tool_calls)) {
    for (const tc of msg.tool_calls as Array<Record<string, unknown>>) {
      const fn = tc.function as Record<string, unknown> | undefined;
      if (fn && typeof fn.name === "string") {
        result.push({
          id: typeof tc.id === "string" ? tc.id : undefined,
          type: typeof tc.type === "string" ? tc.type : undefined,
          function: {
            name: fn.name,
            arguments: typeof fn.arguments === "string" ? fn.arguments : undefined,
          },
        });
      }
    }
  }
  return result;
}

function formatArguments(args?: string): string {
  if (!args) return "{}";
  try {
    return JSON.stringify(JSON.parse(args), null, 2);
  } catch {
    return args;
  }
}

/** Get a short preview of message content */
function getContentPreview(msg: Record<string, unknown>, maxLen: number = 140): string {
  const content = msg.content;
  if (typeof content === "string") {
    const compacted = content.replace(/\n+/g, " ").trim();
    return compacted.length > maxLen ? compacted.slice(0, maxLen) + "…" : compacted;
  }
  if (Array.isArray(content)) {
    // Anthropic-style content blocks
    const parts: string[] = [];
    for (const block of content as Array<Record<string, unknown>>) {
      if (block.type === "text" && typeof block.text === "string") {
        const compacted = block.text.replace(/\n+/g, " ").trim();
        const t = compacted.length > maxLen ? compacted.slice(0, maxLen) + "…" : compacted;
        parts.push(t);
      } else if (block.type === "tool_use") {
        parts.push(`🔧 ${block.name}`);
      } else if (block.type === "tool_result") {
        let rc: string;
        if (typeof block.content === "string") {
          const compacted = block.content.replace(/\n+/g, " ").trim();
          rc = compacted.slice(0, 40) + (compacted.length > 40 ? "…" : "");
        } else {
          rc = "tool_result";
        }
        parts.push(`📤 ${rc}`);
      } else if (block.type === "image") {
        parts.push("🖼 image");
      } else {
        parts.push(String(block.type));
      }
    }
    const joined = parts.join(" | ");
    const compacted = joined.replace(/\n+/g, " ").trim();
    return compacted.length > maxLen ? compacted.slice(0, maxLen) + "…" : compacted;
  }
  if (msg.tool_calls) return `🔧 ${extractToolNames(msg).join(", ")}`;
  if (msg.function_call) return `🔧 ${(msg.function_call as Record<string, unknown>).name as string}`;
  const str = JSON.stringify(content);
  const compacted = str.replace(/\n+/g, " ").trim();
  return compacted.length > maxLen ? compacted.slice(0, maxLen) + "…" : compacted;
}

/** Collect distinct tool names across all messages */
function collectAllToolNames(messages: Array<Record<string, unknown>>): string[] {
  const set = new Set<string>();
  for (const msg of messages) {
    for (const name of extractToolNames(msg)) set.add(name);
  }
  return Array.from(set);
}

/** Role icon + color mapping */
const ROLE_META: Record<string, { icon: typeof Bot; color: string; bg: string; label: string }> = {
  system:    { icon: Terminal,   color: "text-slate-600",    bg: "bg-slate-100",  label: "System" },
  user:      { icon: User,       color: "text-blue-600",     bg: "bg-blue-50",    label: "User" },
  assistant: { icon: Bot,        color: "text-emerald-600",  bg: "bg-emerald-50", label: "AI" },
  tool:      { icon: Wrench,     color: "text-amber-600",    bg: "bg-amber-50",   label: "Tool" },
  function:  { icon: Wrench,     color: "text-amber-600",    bg: "bg-amber-50",   label: "Func" },
};
function getRoleMeta(role: string) {
  return ROLE_META[role] || { icon: FileCode2, color: "text-purple-600", bg: "bg-purple-50", label: role };
}

// ─── Main Page ──────────────────────────────────────────────────────────────

export function LogsPage() {
  const [logs, setLogs] = useState<RequestLog[]>([]);
  const [loading, setLoading] = useState(false);
  const [page, setPage] = useState(0);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [showCleanModal, setShowCleanModal] = useState(false);
  const [showTraceColumn, setShowTraceColumn] = useState(false);

  // Search filters
  const [showSearch, setShowSearch] = useState(false);
  const [keyword, setKeyword] = useState("");
  const [filterApiKey, setFilterApiKey] = useState("");
  const [filterChannel, setFilterChannel] = useState("");
  const [filterModel, setFilterModel] = useState("");
  const [filterDateFrom, setFilterDateFrom] = useState("");
  const [filterDateTo, setFilterDateTo] = useState("");
  const [filterTraceId, setFilterTraceId] = useState("");

  const hasActiveFilters = keyword || filterApiKey || filterChannel || filterModel || filterDateFrom || filterDateTo || filterTraceId;

  const load = useCallback((p: number = 0) => {
    setLoading(true);
    logApi.getAll({
      limit: PAGE_SIZE,
      offset: p * PAGE_SIZE,
      keyword: keyword || undefined,
      api_key_name: filterApiKey || undefined,
      channel_name: filterChannel || undefined,
      model: filterModel || undefined,
      // created_at 存储为 UTC 完整时间戳（如 2026-07-20T12:34:56.789Z），
      // 需把本地日期转换为覆盖当天 [00:00, 24:00) 的 UTC 时刻，否则
      // "created_at <= 'YYYY-MM-DD'" 的字符串比较会漏掉结束日当天的全部日志
      date_from: filterDateFrom ? new Date(`${filterDateFrom}T00:00:00`).toISOString() : undefined,
      date_to: filterDateTo ? new Date(`${filterDateTo}T23:59:59.999`).toISOString() : undefined,
        trace_id: filterTraceId || undefined,
    })
      .then(setLogs)
      .catch(() => {})
      .finally(() => setLoading(false));
  }, [keyword, filterApiKey, filterChannel, filterModel, filterDateFrom, filterDateTo, filterTraceId]);

  useEffect(() => { load(0); }, [load]);

  const clearFilters = () => {
    setKeyword("");
    setFilterApiKey("");
    setFilterChannel("");
    setFilterModel("");
    setFilterDateFrom("");
    setFilterDateTo("");
    setFilterTraceId("");
    setPage(0);
  };

  const handleDeleteLog = async (id: string) => {
    try {
      await logApi.delete(id);
      setLogs(prev => prev.filter(l => l.id !== id));
    } catch (e) {
      console.error("Failed to delete log:", e);
    }
  };

  const handleCleanLogs = async (type: "all" | "7d" | "30d") => {
    try {
      if (type === "all") {
        await logApi.deleteAll();
      } else {
        const days = type === "7d" ? 7 : 30;
        const before = new Date(Date.now() - days * 86400000).toISOString();
        await logApi.deleteBefore(before);
      }
      setShowCleanModal(false);
      setPage(0);
      load(0);
    } catch (e) {
      console.error("Failed to clean logs:", e);
    }
  };

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex items-center justify-between gap-4 px-7 pt-7 pb-4 shrink-0">
        <div>
          <h1 className="text-[28px] font-bold leading-tight tracking-tight">审计日志</h1>
          <p className="mt-1.5 text-sm text-muted-foreground">查看请求结果、Token 消耗、工具调用与网关路由详情</p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setShowSearch(!showSearch)}
            className={`action-secondary ${hasActiveFilters ? "text-blue-600 bg-blue-50" : ""}`}
            title="搜索"
          >
            <Search size={16} />
            {hasActiveFilters && <span className="ml-1 text-xs">筛选中</span>}
          </button>
          <button onClick={() => setShowCleanModal(true)} className="action-secondary text-red-500">
            <Trash2 size={16} /> 清理
          </button>
          <button onClick={() => load(page)} disabled={loading} className="action-secondary">
            <RefreshCw size={16} className={loading ? "animate-spin" : ""} /> 刷新
          </button>
        </div>
      </div>

      {/* Search Panel */}
      {showSearch && (
        <div className="mx-7 mb-4 p-4 surface rounded-[16px] shrink-0">
          <div className="flex items-center justify-between mb-3">
            <h3 className="text-sm font-medium text-slate-700 flex items-center gap-2">
              <Search size={14} /> 搜索筛选
            </h3>
            <div className="flex items-center gap-2">
              {hasActiveFilters && (
                <button onClick={clearFilters} className="text-xs text-slate-500 hover:text-slate-700 flex items-center gap-1">
                  <X size={12} /> 清除筛选
                </button>
              )}
              <button onClick={() => setShowSearch(false)} className="text-slate-400 hover:text-slate-600">
                <X size={16} />
              </button>
            </div>
          </div>
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-3">
            {/* Keyword search */}
            <div className="relative">
              <Search size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-400" />
              <input
                type="text"
                placeholder="关键词搜索 (密钥/渠道/模型/ID)"
                value={keyword}
                onChange={(e) => { setKeyword(e.target.value); setPage(0); }}
                className="w-full pl-9 pr-3 py-2 text-sm rounded-lg border border-slate-200 focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500"
              />
            </div>
            {/* API Key filter */}
            <div className="relative">
              <Key size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-400" />
              <input
                type="text"
                placeholder="密钥名称"
                value={filterApiKey}
                onChange={(e) => { setFilterApiKey(e.target.value); setPage(0); }}
                className="w-full pl-9 pr-3 py-2 text-sm rounded-lg border border-slate-200 focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500"
              />
            </div>
            {/* Channel filter */}
            <div className="relative">
              <Server size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-400" />
              <input
                type="text"
                placeholder="渠道名称"
                value={filterChannel}
                onChange={(e) => { setFilterChannel(e.target.value); setPage(0); }}
                className="w-full pl-9 pr-3 py-2 text-sm rounded-lg border border-slate-200 focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500"
              />
            </div>
            {/* Model filter */}
            <div className="relative">
              <Box size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-400" />
              <input
                type="text"
                placeholder="模型名称"
                value={filterModel}
                onChange={(e) => { setFilterModel(e.target.value); setPage(0); }}
                className="w-full pl-9 pr-3 py-2 text-sm rounded-lg border border-slate-200 focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500"
              />
            </div>
            {/* Trace ID filter */}
            <div className="relative">
              <Search size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-400" />
              <input
                type="text"
                placeholder="Trace ID"
                value={filterTraceId}
                onChange={(e) => { setFilterTraceId(e.target.value); setPage(0); }}
                className="w-full pl-9 pr-3 py-2 text-sm rounded-lg border border-slate-200 focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500"
              />
            </div>
            {/* Date from */}
            <div className="relative">
              <Calendar size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-400" />
              <input
                type="date"
                placeholder="开始日期"
                value={filterDateFrom}
                onChange={(e) => { setFilterDateFrom(e.target.value); setPage(0); }}
                className="w-full pl-9 pr-3 py-2 text-sm rounded-lg border border-slate-200 focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500"
              />
            </div>
            {/* Date to */}
            <div className="relative">
              <Calendar size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-400" />
              <input
                type="date"
                placeholder="结束日期"
                value={filterDateTo}
                onChange={(e) => { setFilterDateTo(e.target.value); setPage(0); }}
                className="w-full pl-9 pr-3 py-2 text-sm rounded-lg border border-slate-200 focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500"
              />
            </div>
          </div>
        </div>
      )}

      {/* Table area — fills remaining height, scrolls internally */}
      <div className="flex-1 overflow-hidden px-7 pb-7 min-h-0">
        <div className="surface h-full overflow-hidden rounded-[24px] flex flex-col">
          {logs.length === 0 ? (
            <div className="flex flex-1 flex-col items-center justify-center gap-2 text-center">
              <ScrollText className="h-12 w-12 text-muted-foreground/70" />
              <p className="text-base font-medium">暂无审计日志</p>
              <p className="text-sm text-muted-foreground">当有模型请求经过网关后，这里会显示调用记录</p>
            </div>
          ) : (
            <>
              {/* Table header + body share the scroll area */}
              <div className="flex-1 overflow-auto">
                <table className="w-full table-fixed text-sm">
                  <thead className="sticky top-0 z-10 border-b border-border bg-white/90 backdrop-blur text-muted-foreground">
                    <tr>
                      <th className="w-8 px-2 py-3"></th>
                      <th className="w-12 px-2 py-3 text-left font-medium">#</th>
                      <th className="w-36 px-2 py-3 text-left font-medium">时间</th>
                      {showTraceColumn && <th className="w-28 px-2 py-3 text-left font-medium">Trace ID</th>}
                      <th className="w-24 px-2 py-3 text-left font-medium">密钥</th>
                      <th className="w-20 px-2 py-3 text-left font-medium">渠道</th>
                      <th className="px-2 py-3 text-left font-medium">模型</th>
                      <th className="w-20 px-2 py-3 text-left font-medium">状态</th>
                      <th className="w-28 px-2 py-3 text-right font-medium">安全</th>
                      <th className="w-24 px-2 py-3 text-right font-medium">Token</th>
                      <th className="w-16 px-2 py-3 text-right font-medium">耗时</th>
                      <th className="w-20 px-2 py-3">
                        <button
                          onClick={() => setShowTraceColumn(!showTraceColumn)}
                          className={`flex items-center gap-1 text-[11px] font-medium transition-colors whitespace-nowrap ${showTraceColumn ? "text-blue-500" : "text-slate-400 hover:text-slate-600"}`}
                          title={showTraceColumn ? "隐藏 Trace ID 列" : "显示 Trace ID 列"}
                        >
                          {showTraceColumn ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                          <span>更多</span>
                        </button>
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {logs.map(log => (
                      <LogRow
                        key={log.id}
                        log={log}
                        expanded={expandedId === log.id}
                        onToggle={() => setExpandedId(expandedId === log.id ? null : log.id)}
                        onDelete={() => handleDeleteLog(log.id)}
                        showTraceColumn={showTraceColumn}
                      />
                    ))}
                  </tbody>
                </table>
              </div>

              {/* Pagination — fixed at bottom of table card */}
              <div className="flex items-center justify-between border-t border-border px-4 py-2.5 bg-white/60">
                <button
                  onClick={() => { const p = Math.max(0, page - 1); setPage(p); load(p); }}
                  disabled={page === 0 || loading}
                  className="action-secondary disabled:opacity-40 disabled:cursor-not-allowed"
                  style={{ padding: "6px 12px", fontSize: "13px" }}
                >
                  上一页
                </button>
                <span className="text-sm text-muted-foreground">第 {page + 1} 页</span>
                <button
                  onClick={() => { const p = page + 1; setPage(p); load(p); }}
                  disabled={logs.length < PAGE_SIZE || loading}
                  className="action-secondary disabled:opacity-40 disabled:cursor-not-allowed"
                  style={{ padding: "6px 12px", fontSize: "13px" }}
                >
                  下一页
                </button>
              </div>
            </>
          )}
        </div>
      </div>

      {/* Clean modal */}
      {showCleanModal && (
        <CleanLogsModal onConfirm={handleCleanLogs} onCancel={() => setShowCleanModal(false)} />
      )}
    </div>
  );
}

// ─── LogRow ──────────────────────────────────────────────────────────────────

function LogRow({
  log,
  expanded,
  onToggle,
  onDelete,
  showTraceColumn,
}: {
  log: RequestLog;
  expanded: boolean;
  onToggle: () => void;
  onDelete: () => void;
  showTraceColumn: boolean;
}) {
  return (
    <>
      <tr className="border-b border-white/6 transition-colors hover:bg-white/4">
        <td className="px-3 py-2.5">
          <button onClick={onToggle} className="text-muted-foreground hover:text-foreground transition-colors">
            {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          </button>
        </td>
        <td className="px-2 py-2.5 text-xs text-muted-foreground/60 font-mono whitespace-nowrap">{log.seq != null ? `#${log.seq}` : "-"}</td>
        <td className="px-2 py-2.5 text-xs text-muted-foreground whitespace-nowrap overflow-hidden">{formatTime(log.created_at)}</td>
        {showTraceColumn && (
          <td className="px-2 py-2.5 text-xs font-mono text-slate-500 whitespace-nowrap overflow-hidden truncate max-w-[180px]" title={log.trace_id || undefined}>{log.trace_id || "-"}</td>
        )}
        <td className="px-2 py-2.5 text-xs overflow-hidden truncate">{log.api_key_name || "-"}</td>
        <td className="px-2 py-2.5 text-xs overflow-hidden truncate">{log.channel_name || "-"}</td>
        <td className="px-2 py-2.5 text-[13px] font-mono overflow-hidden truncate">
          <div className="flex flex-col gap-0.5">
            <span className="truncate font-medium text-foreground">{log.model}</span>
            {log.upstream_model && log.upstream_model !== log.model && (
              <span className="text-[10px] text-blue-500 leading-tight truncate">
                → {log.upstream_model}
              </span>
            )}
          </div>
        </td>
        <td className="px-2 py-2.5 text-xs">
          <div className="flex items-center gap-1.5">
            <span className={`rounded-full px-2 py-0.5 ${log.status_code === 200 ? "bg-emerald-500/12 text-emerald-300" : "bg-red-500/12 text-red-300"}`}>
              {log.status_code}
            </span>
            {log.is_stream && <span className="text-blue-400 text-[10px]">stream</span>}
            {log.is_retry && <span className="text-amber-400 text-[10px]">retry</span>}
          </div>
        </td>
        <td className="px-2 py-2.5 text-xs">
          <div className="flex justify-end">
            <RiskBadge log={log} />
          </div>
        </td>
        <td className="px-3 py-2.5 text-right text-xs">
          <span title={`Prompt: ${log.prompt_tokens}, Completion: ${log.completion_tokens}`}>
            {log.total_tokens > 0 ? formatNumber(log.total_tokens) : <span className="text-muted-foreground/50">0</span>}
          </span>
        </td>
        <td className="px-3 py-2.5 text-right text-xs text-muted-foreground">{formatDuration(log.duration_ms)}</td>
        <td className="px-3 py-2.5">
          <button
            onClick={onDelete}
            className="text-muted-foreground/40 hover:text-red-400 transition-colors"
            title="删除此日志"
          >
            <Trash2 size={13} />
          </button>
        </td>
      </tr>
      {expanded && (
        <tr>
          <td colSpan={12} className="px-4 py-4 bg-slate-50/80 border-b border-border">
            <LogDetail log={log} />
          </td>
        </tr>
      )}
    </>
  );
}


function RiskBadge({ log }: { log: RequestLog }) {
  const meta = getRiskMeta(log.risk_level);
  return (
    <span title={log.risk_summary || undefined} className={`inline-flex items-center gap-1 rounded-full border px-2.5 py-0.5 text-[11px] font-medium ${meta.cls}`}>
      <ShieldAlert size={11} />
      {meta.label}{log.risk_score > 0 ? ` ${log.risk_score}` : ""}
    </span>
  );
}

// ─── LogDetail ────────────────────────────────────────────────────────────────

function LogDetail({ log }: { log: RequestLog }) {
  const [jsonExpanded, setJsonExpanded] = useState(false);
  const [responseJsonExpanded, setResponseJsonExpanded] = useState(false);
  const [findings, setFindings] = useState<SecurityFinding[]>([]);
  const [expandedChoices, setExpandedChoices] = useState<Set<string>>(new Set());
  const [expandedMessages, setExpandedMessages] = useState<Set<number>>(new Set());
  const [copyingMessageKey, setCopyingMessageKey] = useState<string | null>(null);
  const [copyingThinkingKey, setCopyingThinkingKey] = useState<string | null>(null);
  const [copyingContentKey, setCopyingContentKey] = useState<string | null>(null);
  const [expandedToolCalls, setExpandedToolCalls] = useState<Set<string>>(new Set());
  const [copyingTool, setCopyingTool] = useState<string | null>(null);
  const [copiedJsonKey, setCopiedJsonKey] = useState<string | null>(null);

  useEffect(() => {
    if (log.risk_score > 0) {
      logApi.getSecurityFindings(log.id).then(setFindings).catch(() => setFindings([]));
    } else {
      setFindings([]);
    }
  }, [log.id, log.risk_score]);

  // Parse request body
  let parsed: Record<string, unknown> | null = null;
  let pretty = log.request_body || "";
  let parseError = false;
  try {
    if (log.request_body) {
      parsed = JSON.parse(log.request_body) as Record<string, unknown>;
      pretty = JSON.stringify(parsed, null, 2);
    }
  } catch { parseError = true; }

  const byteSize = log.request_body ? new Blob([log.request_body]).size : 0;
  const sizeLabel = byteSize > 1024 ? `${(byteSize / 1024).toFixed(1)} KB` : `${byteSize} B`;

  const messages: Array<Record<string, unknown>> = parsed && Array.isArray(parsed.messages) ? parsed.messages : [];
  const [messagesExpanded, setMessagesExpanded] = useState(messages.length <= 5);
  const allToolNames = useMemo(() => collectAllToolNames(messages), [messages]);
  const modelRequested = (parsed?.model as string) || log.model;
  const stream = (parsed?.stream as boolean) || false;
  const temperature = parsed?.temperature as number | undefined;
  const maxTokens = parsed?.max_tokens as number | undefined;

  // ── Conversation stats ──
  const convStats = useMemo(() => {
    const roles = new Map<string, number>();
    let totalInputChars = 0;
    let hasImage = false;
    for (const msg of messages) {
      const r = (msg.role as string) || "unknown";
      roles.set(r, (roles.get(r) || 0) + 1);
      if (typeof msg.content === "string") totalInputChars += msg.content.length;
      if (Array.isArray(msg.content)) {
        for (const b of msg.content as Array<Record<string, unknown>>) {
          if (b.type === "text" && typeof b.text === "string") totalInputChars += b.text.length;
          if (b.type === "image") hasImage = true;
        }
      }
    }
    return { roles, totalInputChars, hasImage, msgCount: messages.length };
  }, [messages]);

  // ── Cost estimate (rough, GPT-4o pricing as reference) ──
  const costEstimate = useMemo(() => {
    const p = log.prompt_tokens;
    const c = log.completion_tokens;
    // GPT-4o: $2.5/1M input, $10/1M output (rough)
    const inputCost = (p / 1_000_000) * 2.5;
    const outputCost = (c / 1_000_000) * 10;
    const total = inputCost + outputCost;
    if (total < 0.001 && total > 0) return "<$0.001";
    if (total === 0) return "$0";
    return `$${total.toFixed(3)}`;
  }, [log.prompt_tokens, log.completion_tokens]);

  // ── Model mapping display ──
  const modelMappingDisplay = log.upstream_model && log.upstream_model !== log.model
    ? `${log.model} → ${log.upstream_model}`
    : null;

  // Parse response choices
  let parsedChoices: Array<Record<string, unknown>> | null = null;
  let prettyChoices = log.response_choices || "";
  let choicesParseError = false;
  try {
    if (log.response_choices) {
      parsedChoices = JSON.parse(log.response_choices) as Array<Record<string, unknown>>;
      prettyChoices = JSON.stringify(parsedChoices, null, 2);
    }
  } catch { choicesParseError = true; }

  const [responseChoicesExpanded, setResponseChoicesExpanded] = useState(
    !parsedChoices || parsedChoices.length <= 5
  );
  const [activeTab, setActiveTab] = useState<"request" | "response">("request");

  return (
    <div className="space-y-4">
      {/* ── Gateway Metadata Cards ── */}
      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-6">
        {/* Token detail */}
        <div className="rounded-xl border border-slate-200 bg-white p-3">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><Coins size={13} /> Token 消耗</div>
          <div className="mt-1.5 text-sm font-semibold text-slate-900">{formatNumber(log.total_tokens)}</div>
          <div className="mt-0.5 text-[11px] text-slate-400">
            输入 {log.prompt_tokens} · 输出 {log.completion_tokens}
          </div>
        </div>

        {/* Duration */}
        <div className="rounded-xl border border-slate-200 bg-white p-3">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><Timer size={13} /> 响应耗时</div>
          <div className="mt-1.5 text-sm font-semibold text-slate-900">{formatDuration(log.duration_ms)}</div>
          <div className="mt-0.5 text-[11px] text-slate-400">
            {log.is_stream ? "流式传输" : "非流式"}
          </div>
        </div>

        {/* Cost estimate */}
        <div className="rounded-xl border border-slate-200 bg-white p-3">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><Coins size={13} /> 成本估算</div>
          <div className="mt-1.5 text-sm font-semibold text-slate-900">{costEstimate}</div>
          <div className="mt-0.5 text-[11px] text-slate-400">参考 GPT-4o 定价</div>
        </div>

        {/* Request size */}
        <div className="rounded-xl border border-slate-200 bg-white p-3">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><FileCode2 size={13} /> 请求大小</div>
          <div className="mt-1.5 text-sm font-semibold text-slate-900">{sizeLabel}</div>
          <div className="mt-0.5 text-[11px] text-slate-400">
            {convStats.msgCount} 条消息 · {convStats.totalInputChars > 1000 ? `${(convStats.totalInputChars / 1000).toFixed(1)}K` : convStats.totalInputChars} 字符
          </div>
        </div>

        {/* Channel routing */}
        <div className="rounded-xl border border-slate-200 bg-white p-3">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><Shield size={13} /> 渠道路由</div>
          <div className="mt-1.5 text-sm font-semibold text-slate-900">{log.channel_name || "-"}</div>
          <div className="mt-0.5 text-[11px] text-slate-400">
            {log.is_retry ? "⚠ 重试转发" : "✓ 首选渠道"}
          </div>
        </div>

        {/* Model mapping */}
        <div className="rounded-xl border border-slate-200 bg-white p-3">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><ArrowRightLeft size={13} /> 模型映射</div>
          <div className="mt-1.5 text-sm font-semibold text-slate-900">
            {modelMappingDisplay || modelRequested}
          </div>
          <div className="mt-0.5 text-[11px] text-slate-400">
            {modelMappingDisplay ? "网关重映射" : "直传上游"}
          </div>
        </div>
      </div>

      {/* ── Trace ID ── */}
      {log.trace_id && (
        <div className="flex items-center gap-2 rounded-xl border border-slate-200 bg-white p-3">
          <Search size={14} className="text-slate-400" />
          <span className="text-xs text-slate-500">Trace ID:</span>
          <span className="text-xs font-mono text-slate-700 break-all">{log.trace_id}</span>
        </div>
      )}

      {/* ── Security Summary ── */}
      {(log.risk_score > 0 || log.risk_summary) && (
        <div className="rounded-xl border border-slate-200 bg-white p-3">
          <div className="flex items-center justify-between gap-3">
            <div className="flex items-center gap-2">
              <ShieldAlert size={15} className="text-amber-500" />
              <span className="text-sm font-semibold text-slate-800">安全审计</span>
              <RiskBadge log={log} />
              <span className="rounded-md bg-slate-50 border border-slate-200 px-2 py-0.5 text-[11px] text-slate-500">动作: {log.security_action}</span>
            </div>
            {log.sanitized && <span className="text-xs text-blue-600">已脱敏</span>}
          </div>
          <p className="mt-2 text-xs text-slate-600">{log.risk_summary || "未发现明显风险"}</p>
          {log.blocked_reason && <p className="mt-1 text-xs text-red-600">阻断原因：{log.blocked_reason}</p>}
          {findings.length > 0 && (
            <div className="mt-3 space-y-2 max-h-[240px] overflow-y-auto pr-1">
              {findings.map(f => {
                const meta = getRiskMeta(f.severity);
                return (
                  <div key={f.id} className="rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-xs">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className={`rounded-full border px-2 py-0.5 text-[10px] font-medium ${meta.cls}`}>{meta.label}</span>
                      <span className="font-medium text-slate-800">{f.title}</span>
                      <span className="font-mono text-[10px] text-slate-400">{f.rule_id}</span>
                    </div>
                    {f.description && <div className="mt-1 text-slate-500">{f.description}</div>}
                    <div className="mt-1 flex flex-wrap gap-2 text-[11px] text-slate-400">
                      {f.location && <span>位置：{f.location}</span>}
                      {f.evidence_masked && <span>证据：{f.evidence_masked}</span>}
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}

      {/* ── Tool Tags ── */}
      {allToolNames.length > 0 && (
        <div className="flex items-center gap-2">
          <Wrench size={14} className="text-amber-500" />
          <span className="text-xs text-slate-500 shrink-0">涉及工具:</span>
          <div className="flex flex-wrap gap-1.5">
            {allToolNames.map(name => (
              <span
                key={name}
                className="inline-flex items-center gap-1 rounded-md bg-amber-50 border border-amber-200 px-2 py-0.5 text-[11px] font-medium text-amber-700"
              >
                <Wrench size={11} />
                {name}
              </span>
            ))}
          </div>
        </div>
      )}

      {/* ── Conversation composition summary ── */}
      {convStats.msgCount > 0 && (
        <div className="flex items-center gap-2">
          <Eye size={14} className="text-blue-500" />
          <span className="text-xs text-slate-500 shrink-0">对话构成:</span>
          <div className="flex flex-wrap gap-1.5">
            {Array.from(convStats.roles.entries()).map(([role, count]) => {
              const meta = getRoleMeta(role);
              const Icon = meta.icon;
              return (
                <span
                  key={role}
                  className={`inline-flex items-center gap-1 rounded-md ${meta.bg} border border-slate-200 px-2 py-0.5 text-[11px] font-medium ${meta.color}`}
                >
                  <Icon size={11} />
                  {meta.label} ×{count}
                </span>
              );
            })}
            {convStats.hasImage && (
              <span className="inline-flex items-center gap-1 rounded-md bg-purple-50 border border-purple-200 px-2 py-0.5 text-[11px] font-medium text-purple-700">
                <Image size={11} /> 含图片
              </span>
            )}
          </div>
        </div>
      )}

      {/* ── Request params tags ── */}
      <div className="flex flex-wrap gap-1.5">
        <span className="inline-flex items-center rounded-md bg-blue-50 border border-blue-200 px-2 py-0.5 text-[11px] font-mono font-medium text-blue-700">
          model: {modelRequested}
        </span>
        {stream && (
          <span className="inline-flex items-center rounded-md bg-emerald-50 border border-emerald-200 px-2 py-0.5 text-[11px] font-medium text-emerald-700">
            stream
          </span>
        )}
        {temperature !== undefined && (
          <span className="inline-flex items-center rounded-md bg-slate-50 border border-slate-200 px-2 py-0.5 text-[11px] font-mono text-slate-600">
            temp: {temperature}
          </span>
        )}
        {maxTokens !== undefined && (
          <span className="inline-flex items-center rounded-md bg-slate-50 border border-slate-200 px-2 py-0.5 text-[11px] font-mono text-slate-600">
            max: {maxTokens}
          </span>
        )}
        {log.is_retry && (
          <span className="inline-flex items-center rounded-md bg-amber-50 border border-amber-200 px-2 py-0.5 text-[11px] font-medium text-amber-700">
            ⚠ retry
          </span>
        )}
        {log.mode && (
          <span className="inline-flex items-center rounded-md bg-slate-50 border border-slate-200 px-2 py-0.5 text-[11px] font-mono text-slate-600">
            mode: {log.mode}
          </span>
        )}
      </div>

      {/* ── Error ── */}
      {log.error_message && (
        <div className="flex items-start gap-2 rounded-xl bg-red-50 border border-red-200 p-3 text-xs text-red-600">
          <AlertCircle size={14} className="mt-0.5 shrink-0" />
          <span className="break-all">{log.error_message}</span>
        </div>
      )}

      {/* ── Request / Response Tabs ── */}
      <div className="rounded-2xl border border-slate-200 bg-white overflow-hidden">
        {/* Tab bar */}
        <div className="flex items-center border-b border-slate-200 bg-slate-50/80">
          <button
            onClick={() => setActiveTab("request")}
            className={`relative flex items-center gap-2 px-5 py-2.5 text-sm font-medium transition-all duration-200 ${
              activeTab === "request"
                ? "text-blue-600 bg-white"
                : "text-slate-500 hover:text-slate-700 hover:bg-white/50"
            }`}
          >
            <ArrowUp size={14} className={activeTab === "request" ? "text-blue-500" : "text-slate-400"} />
            请求
            {messages.length > 0 && (
              <span className={`rounded-full px-1.5 py-0.5 text-[10px] font-mono ${activeTab === "request" ? "bg-blue-100 text-blue-600" : "bg-slate-200 text-slate-500"}`}>{messages.length}</span>
            )}
            {activeTab === "request" && <span className="absolute bottom-0 left-0 right-0 h-0.5 bg-blue-500" />}
          </button>
          <button
            onClick={() => setActiveTab("response")}
            className={`relative flex items-center gap-2 px-5 py-2.5 text-sm font-medium transition-all duration-200 ${
              activeTab === "response"
                ? "text-emerald-600 bg-white"
                : "text-slate-500 hover:text-slate-700 hover:bg-white/50"
            }`}
          >
            <ArrowDown size={14} className={activeTab === "response" ? "text-emerald-500" : "text-slate-400"} />
            响应
            {parsedChoices && parsedChoices.length > 0 && (
              <span className={`rounded-full px-1.5 py-0.5 text-[10px] font-mono ${activeTab === "response" ? "bg-emerald-100 text-emerald-600" : "bg-slate-200 text-slate-500"}`}>{parsedChoices.length}</span>
            )}
            {activeTab === "response" && <span className="absolute bottom-0 left-0 right-0 h-0.5 bg-emerald-500" />}
          </button>
          <div className="flex-1" />
          {/* Tab-specific actions */}
          {activeTab === "request" && messages.length > 0 && (
            <button
              onClick={() => setJsonExpanded(!jsonExpanded)}
              className="mr-3 text-xs text-blue-500 hover:text-blue-600 transition-colors font-medium"
            >
              {jsonExpanded ? "返回缩略视图" : "查看原始 JSON"}
            </button>
          )}
          {activeTab === "response" && parsedChoices && parsedChoices.length > 0 && (
            <button
              onClick={() => setResponseJsonExpanded(!responseJsonExpanded)}
              className="mr-3 text-xs text-blue-500 hover:text-blue-600 transition-colors font-medium"
            >
              {responseJsonExpanded ? "返回缩略视图" : "查看原始 JSON"}
            </button>
          )}
        </div>

        {/* Tab content */}
        <div className="p-4">

      {/* ── Request Tab Content ── */}
      {activeTab === "request" && (
      <>
      {messages.length > 0 ? (
        <div>
          <div className="flex items-center justify-between mb-2">
            <span className="text-xs font-medium text-slate-600">消息列表 ({messages.length} 条)</span>
          </div>

          {jsonExpanded ? (
            <div className="relative">
              <pre className="max-h-[420px] w-full overflow-auto rounded-xl bg-slate-50 border border-slate-200 p-3 pr-16 text-xs font-mono whitespace-pre-wrap break-all text-slate-700">
                {pretty}
              </pre>
              <button
                onClick={async () => {
                  try {
                    await navigator.clipboard.writeText(pretty);
                    setCopiedJsonKey('request-messages');
                    setTimeout(() => setCopiedJsonKey(null), 1500);
                  } catch {}
                }}
                className={`absolute top-2 right-2 text-[10px] px-1.5 py-0.5 rounded-full transition-all shadow-sm ${
                  copiedJsonKey === 'request-messages'
                    ? 'bg-emerald-100 border-emerald-300 text-emerald-700'
                    : 'bg-white/90 border border-slate-200 text-slate-600 hover:bg-blue-50 hover:text-blue-600 hover:border-blue-300'
                }`}
              >
                {copiedJsonKey === 'request-messages' ? '✅ 已复制' : '📋 复制'}
              </button>
            </div>
          ) : (
            <div className="border border-slate-200 rounded-xl overflow-hidden bg-white">
              {messagesExpanded && (
                <div className="space-y-2 max-h-[420px] overflow-y-auto pr-1 p-3">
                  {messages.map((msg, i) => {
                    const role = (msg.role as string) || "unknown";
                    const meta = getRoleMeta(role);
                    const Icon = meta.icon;
                    const isExpanded = expandedMessages.has(i);
                    const preview = getContentPreview(msg, 140);
                    // Check if getContentPreview truncated the content
                    const fullContent = (() => {
                      const content = msg.content;
                      if (typeof content === "string") {
                        // Convert escaped newline characters to actual newlines
                        let processed = content
                          .replace(/\\n/g, '\n')
                          .replace(/\\r/g, '\r')
                          .replace(/\\t/g, '\t');
                        // Normalize line endings
                        return processed.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
                      }
                      return JSON.stringify(content);
                    })();
                    const isLongContent = fullContent.length > 140;
                    const messageKey = `msg-${i}`;

                    return (
                      <div
                        key={i}
                        className={`rounded-lg border border-slate-200/70 bg-gradient-to-b from-white to-slate-50/30 overflow-hidden transition-all duration-300 hover:border-slate-300 hover:shadow-md hover:-translate-y-0.5 ${meta.bg}`}
                      >
                        {/* Header with role, index and expand button */}
                        <div className={`flex items-center justify-between gap-2 px-3 py-1.5 border-b border-slate-200/50 bg-gradient-to-r ${meta.bg} to-white`}>
                          <div className={`shrink-0 flex items-center gap-1.5 ${meta.color}`}>
                            <div className="p-0.5 rounded-md bg-white/60 shadow-sm">
                              <Icon size={12} className={meta.color} />
                            </div>
                            <span className={`text-xs font-semibold ${meta.color}`}>{meta.label}</span>
                          </div>
                          <div className="flex items-center gap-1.5">
                            <span className="text-[10px] text-slate-400 font-mono bg-white/60 px-1 py-0.5 rounded-full shadow-sm">#&nbsp;{i + 1}</span>
                            <button
                               onClick={async () => {
                                 try {
                                   await navigator.clipboard.writeText(fullContent);
                                   setCopyingMessageKey(messageKey);
                                   setTimeout(() => setCopyingMessageKey(null), 1000);
                                 } catch {
                                   setCopyingMessageKey(null);
                                 }
                               }}
                               className={`group relative text-[10px] px-1.5 py-0 rounded-full font-medium transition-all duration-200 flex items-center gap-0.5 overflow-hidden ${
                                 copyingMessageKey === messageKey
                                   ? 'bg-emerald-100 text-emerald-700'
                                   : 'bg-gradient-to-r from-slate-100 to-slate-200 text-slate-700 hover:from-indigo-50 hover:to-indigo-100 hover:text-indigo-700 hover:shadow-md hover:-translate-y-0.5'
                               }`}
                             >
                               {copyingMessageKey === messageKey ? '✅ 已复制' : '📋 复制'}
                             </button>
                            {isLongContent && (
                              <button
                                onClick={() => {
                                  const newSet = new Set(expandedMessages);
                                  if (newSet.has(i)) newSet.delete(i);
                                  else newSet.add(i);
                                  setExpandedMessages(newSet);
                                }}
                                className="group relative text-[10px] px-2 py-0.5 rounded-full font-medium transition-all duration-300 flex items-center gap-0.5 overflow-hidden bg-gradient-to-r from-slate-100 to-slate-200 text-slate-700 hover:from-indigo-50 hover:to-indigo-100 hover:text-indigo-700 hover:shadow-md hover:-translate-y-0.5"
                              >
                                {isExpanded ? (
                                  <>
                                    <span className="text-sm group-hover:-translate-y-0.5 transition-transform duration-200">↑</span>
                                    <span className="font-medium">收起</span>
                                  </>
                                ) : (
                                  <>
                                    <span className="text-sm group-hover:translate-y-0.5 transition-transform duration-200">↓</span>
                                    <span className="font-medium">展开</span>
                                  </>
                                )}
                              </button>
                            )}
                          </div>
                        </div>

                        {/* Content section */}
                        <div className="px-3 py-2">
                          <div className="text-xs text-slate-700 leading-snug whitespace-pre-wrap break-words">
                            {!isExpanded
                              ? preview
                              : fullContent
                            }
                          </div>
                          {/* Tool calls detail */}
                          {(() => {
                            // Extract tool calls from message
                            let toolCalls: any[] = [];
                            if (Array.isArray(msg.tool_calls)) {
                              toolCalls = msg.tool_calls;
                            } else if (msg.tool_call) {
                              toolCalls = [msg.tool_call];
                            }
                            if (toolCalls.length === 0) return null;

                            return (
                              <div className="mt-2 divide-y divide-slate-200">
                                {toolCalls.map((tc, tci) => {
                                  const toolKey = `msg-${i}-${tci}`;
                                  const isToolExpanded = expandedToolCalls.has(toolKey);
                                  const isCopyingToolKey = copyingTool === toolKey;
                                  const formattedArgs = (() => {
                                    try {
                                      if (typeof tc.function?.arguments === 'string') {
                                        return JSON.stringify(JSON.parse(tc.function.arguments), null, 2);
                                      }
                                      return JSON.stringify(tc.function?.arguments, null, 2);
                                    } catch {
                                      return String(tc.function?.arguments);
                                    }
                                  })();
                                  const fullJson = JSON.stringify({
                                    id: tc.id,
                                    type: tc.type,
                                    function: {
                                      name: tc.function?.name,
                                      arguments: tc.function?.arguments ? (typeof tc.function.arguments === 'string' ? JSON.parse(tc.function.arguments) : tc.function.arguments) : undefined,
                                    }
                                  }, null, 2);

                                  return (
                                    <div key={toolKey} className="py-2">
                                      <div className="flex items-center justify-between gap-2 mb-1">
                                        <div className="flex items-center gap-1.5 min-w-0">
                                          <span className="inline-flex items-center gap-0.5 rounded-md bg-slate-100 border border-slate-200 px-1.5 py-0.5 text-[10px] font-semibold text-slate-700">
                                            <span>🔧</span>
                                            {tc.function?.name}
                                          </span>
                                          {tc.id && (
                                            <span className="text-[10px] text-slate-400 font-mono truncate">{tc.id}</span>
                                          )}
                                        </div>
                                        <div className="flex items-center gap-1">
                                          <button
                                            onClick={async () => {
                                              setCopyingTool(toolKey);
                                              try {
                                                await navigator.clipboard.writeText(fullJson);
                                                setTimeout(() => setCopyingTool(null), 1000);
                                              } catch {
                                                setCopyingTool(null);
                                              }
                                            }}
                                            className={`group relative text-[8px] px-1.5 py-0 rounded-full font-medium transition-all duration-200 flex items-center gap-0.5 overflow-hidden ${
                                              isCopyingToolKey
                                                ? 'bg-emerald-100 text-emerald-700'
                                                : 'bg-gradient-to-r from-slate-100 to-slate-200 text-slate-700 hover:from-indigo-50 hover:to-indigo-100 hover:text-indigo-700 hover:shadow-md hover:-translate-y-0.5'
                                            }`}
                                          >
                                            {isCopyingToolKey ? '✅ 已复制' : '📋 复制'}
                                          </button>
                                          <button
                                            onClick={() => {
                                              const newSet = new Set(expandedToolCalls);
                                              if (newSet.has(toolKey)) newSet.delete(toolKey);
                                              else newSet.add(toolKey);
                                              setExpandedToolCalls(newSet);
                                            }}
                                            className="group relative text-[8px] px-1.5 py-0 rounded-full font-medium transition-all duration-200 flex items-center gap-0.5 overflow-hidden bg-gradient-to-r from-slate-100 to-slate-200 text-slate-700 hover:from-indigo-50 hover:to-indigo-100 hover:text-indigo-700 hover:shadow-md hover:-translate-y-0.5"
                                          >
                                            {isToolExpanded ? '↑ 收起' : '↓ 详情'}
                                          </button>
                                        </div>
                                      </div>
                                      <div className="bg-slate-50 rounded border border-slate-200 overflow-hidden">
                                        <pre className="text-[10px] font-mono text-slate-700 whitespace-pre-wrap break-all p-1.5 max-h-[120px] overflow-auto">
                                          {isToolExpanded ? fullJson : formattedArgs}
                                        </pre>
                                      </div>
                                    </div>
                                  );
                                })}
                              </div>
                            );
                          })()}
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}
              <div className="px-4 py-1.5 border-t border-slate-200 bg-slate-50/50 flex items-center justify-between">
                {!messagesExpanded && (
                  <div className="text-xs text-slate-600">
                    共 {messages.length} 条消息，点击展开查看详情
                  </div>
                )}
                <button
                  onClick={() => setMessagesExpanded(!messagesExpanded)}
                  className="text-slate-400 hover:text-slate-600 transition-colors"
                >
                  {messagesExpanded ? <ChevronDown size={16} /> : <ChevronRight size={16} />}
                </button>
              </div>
            </div>
          )}
        </div>
      ) : log.request_body ? (
        <div>
          <div className="flex items-center justify-between mb-2">
            <span className="text-xs text-slate-500">请求内容 ({sizeLabel})</span>
          </div>
          {jsonExpanded ? (
            <div className="relative">
              <pre className="max-h-[420px] w-full overflow-auto rounded-xl bg-slate-50 border border-slate-200 p-3 pr-16 text-xs font-mono whitespace-pre-wrap break-all text-slate-700">
                {pretty}
              </pre>
              <button
                onClick={async () => {
                  try {
                    await navigator.clipboard.writeText(pretty);
                    setCopiedJsonKey('request-body');
                    setTimeout(() => setCopiedJsonKey(null), 1500);
                  } catch {}
                }}
                className={`absolute top-2 right-2 text-[10px] px-1.5 py-0.5 rounded-full transition-all shadow-sm ${
                  copiedJsonKey === 'request-body'
                    ? 'bg-emerald-100 border-emerald-300 text-emerald-700'
                    : 'bg-white/90 border border-slate-200 text-slate-600 hover:bg-blue-50 hover:text-blue-600 hover:border-blue-300'
                }`}
              >
                {copiedJsonKey === 'request-body' ? '✅ 已复制' : '📋 复制'}
              </button>
            </div>
          ) : (
            <div className="text-xs text-slate-500">点击「展开全部」查看原始 JSON</div>
          )}
        </div>
      ) : (
        <div className="text-xs text-slate-400">无请求内容记录</div>
      )}

      {parseError && (
        <div className="text-xs text-amber-500">⚠ JSON 解析失败，显示原始内容</div>
      )}
      </>
      )}

      {/* ── Response Tab Content ── */}
      {activeTab === "response" && (
      <>
      {/* ── Response Choices Timeline ── */}
      {parsedChoices && parsedChoices.length > 0 ? (
        <div>
          <div className="flex items-center justify-between mb-2">
            <span className="text-xs font-medium text-slate-600">选择列表 ({parsedChoices.length} 条)</span>
          </div>

          {responseJsonExpanded ? (
            <div className="relative">
              <pre className="max-h-[420px] w-full overflow-auto rounded-xl bg-slate-50 border border-slate-200 p-3 pr-16 text-xs font-mono whitespace-pre-wrap break-all text-slate-700">
                {prettyChoices}
              </pre>
              <button
                onClick={async () => {
                  try {
                    await navigator.clipboard.writeText(prettyChoices);
                    setCopiedJsonKey('response-choices');
                    setTimeout(() => setCopiedJsonKey(null), 1500);
                  } catch {}
                }}
                className={`absolute top-2 right-2 text-[10px] px-1.5 py-0.5 rounded-full transition-all shadow-sm ${
                  copiedJsonKey === 'response-choices'
                    ? 'bg-emerald-100 border-emerald-300 text-emerald-700'
                    : 'bg-white/90 border border-slate-200 text-slate-600 hover:bg-blue-50 hover:text-blue-600 hover:border-blue-300'
                }`}
              >
                {copiedJsonKey === 'response-choices' ? '✅ 已复制' : '📋 复制'}
              </button>
            </div>
          ) : (
            <div className="border border-slate-200 rounded-xl overflow-hidden bg-white">
              {responseChoicesExpanded && (
                <div className="space-y-2 max-h-[500px] overflow-y-auto pr-1 pb-1 p-3">
                  {parsedChoices.map((choice, i) => {
                    const message = (choice.message || choice.delta || {}) as Record<string, unknown>;
                    const role = (message.role as string) || "assistant";
                    const meta = getRoleMeta(role);
                    const Icon = meta.icon;
                    const toolCalls = extractToolCalls(message);

                    const content = (message.content as string) || "";
                    const reasoningContent = (message.reasoning_content as string) || "";

                    const reasoningExpanded = expandedChoices.has(`${i}-reasoning`);
                    const contentExpanded = expandedChoices.has(`${i}-content`);

                    // Get preview for reasoning content (compact newlines)
                    const reasoningPreview = reasoningContent.replace(/\n+/g, " ").trim();
                    const reasoningPreviewTruncated = reasoningPreview.length > 200
                      ? `${reasoningPreview.slice(0, 200)}…`
                      : reasoningPreview;

                    const toggleReasoning = () => {
                      const newSet = new Set(expandedChoices);
                      const key = `${i}-reasoning`;
                      if (newSet.has(key)) newSet.delete(key);
                      else newSet.add(key);
                      setExpandedChoices(newSet);
                    };

                    const toggleContent = () => {
                      const newSet = new Set(expandedChoices);
                      const key = `${i}-content`;
                      if (newSet.has(key)) newSet.delete(key);
                      else newSet.add(key);
                      setExpandedChoices(newSet);
                    };

                    return (
                      <div
                        key={i}
                        className={`rounded-lg border border-slate-200/70 bg-gradient-to-b from-white to-slate-50/30 overflow-hidden transition-all duration-300 hover:border-slate-300 hover:shadow-md hover:-translate-y-0.5`}
                      >
                        {/* Header with role, index and actions */}
                        <div className={`flex items-center justify-between gap-2 px-3 py-1.5 border-b border-slate-200/50 bg-gradient-to-r ${meta.bg} to-white`}>
                          <div className={`flex items-center gap-1.5`}>
                            <div className="p-0.5 rounded-md bg-white/60 shadow-sm">
                              <Icon size={12} className={meta.color} />
                            </div>
                            <span className={`text-xs font-semibold ${meta.color}`}>{meta.label}</span>
                          </div>
                          <div className="flex items-center gap-1.5">
                            <span className="text-[10px] text-slate-400 font-mono bg-white/60 px-1 py-0.5 rounded-full shadow-sm">#&nbsp;{i + 1}</span>
                          </div>
                        </div>

                        {/* Thinking process section */}
                        {reasoningContent && (
                          <div className="px-3 py-2 border-b border-slate-200/30">
                            <div className="flex items-center justify-between gap-1.5 mb-1">
                              <div className="flex items-center gap-1.5">
                                <span className="text-sm">💭</span>
                                <span className="text-[11px] font-semibold text-slate-700">推理内容</span>
                              </div>
                              <div className="flex items-center gap-1">
                                <button
                                   onClick={async () => {
                                     const thinkingKey = `thinking-${i}`;
                                     try {
                                       let processed = reasoningContent
                                         .replace(/\\n/g, '\n')
                                         .replace(/\\r/g, '\r')
                                         .replace(/\\t/g, '\t');
                                       processed = processed.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
                                       await navigator.clipboard.writeText(processed);
                                       setCopyingThinkingKey(thinkingKey);
                                       setTimeout(() => setCopyingThinkingKey(null), 1000);
                                     } catch {
                                       setCopyingThinkingKey(null);
                                     }
                                   }}
                                   className={`group relative text-[10px] px-1.5 py-0 rounded-full font-medium transition-all duration-200 flex items-center gap-0.5 overflow-hidden ${
                                     copyingThinkingKey === `thinking-${i}`
                                       ? 'bg-emerald-100 text-emerald-700'
                                       : 'bg-gradient-to-r from-slate-100 to-slate-200 text-slate-700 hover:from-indigo-50 hover:to-indigo-100 hover:text-indigo-700 hover:shadow-md hover:-translate-y-0.5'
                                   }`}
                                  >
                                   {copyingThinkingKey === `thinking-${i}` ? '✅ 已复制' : '📋 复制'}
                                  </button>
                                {reasoningContent.length > 200 && (
                                  <button
                                    onClick={toggleReasoning}
                                    className="group relative text-[10px] px-2 py-0.5 rounded-full font-medium transition-all duration-300 flex items-center gap-0.5 overflow-hidden bg-gradient-to-r from-slate-100 to-slate-200 text-slate-700 hover:from-indigo-50 hover:to-indigo-100 hover:text-indigo-700 hover:shadow-md hover:-translate-y-0.5"
                                  >
                                    {reasoningExpanded ? (
                                      <>
                                        <span className="text-sm group-hover:-translate-y-0.5 transition-transform duration-200">↑</span>
                                        <span className="font-medium">收起</span>
                                      </>
                                    ) : (
                                      <>
                                        <span className="text-sm group-hover:translate-y-0.5 transition-transform duration-200">↓</span>
                                        <span className="font-medium">展开</span>
                                      </>
                                    )}
                                  </button>
                                )}
                              </div>
                            </div>
                            <div className="text-xs text-slate-700 leading-snug whitespace-pre-wrap break-words">
                              {reasoningContent.length > 200 ? (
                                reasoningExpanded ? (
                                  (() => {
                                    let processed = reasoningContent
                                      .replace(/\\n/g, '\n')
                                      .replace(/\\r/g, '\r')
                                      .replace(/\\t/g, '\t');
                                    return processed.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
                                  })()
                                ) : reasoningPreviewTruncated
                              ) : (
                                (() => {
                                  let processed = reasoningContent
                                    .replace(/\\n/g, '\n')
                                    .replace(/\\r/g, '\r')
                                    .replace(/\\t/g, '\t');
                                  return processed.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
                                })()
                              )}
                            </div>
                          </div>
                        )}

                        {/* Content section */}
                        <div className="px-3 py-2">
                          <div className="flex items-center justify-between gap-1.5 mb-1">
                            <div className="flex items-center gap-1.5">
                              <span className="text-sm">✍️</span>
                              <span className="text-[11px] font-semibold text-slate-700">正文内容</span>
                            </div>
                            <div className="flex items-center gap-1">
                              <button
                                onClick={async () => {
                                  const contentKey = `content-${i}`;
                                  try {
                                    let processed = content
                                      .replace(/\\n/g, '\n')
                                      .replace(/\\r/g, '\r')
                                      .replace(/\\t/g, '\t');
                                    processed = processed.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
                                    await navigator.clipboard.writeText(processed);
                                    setCopyingContentKey(contentKey);
                                    setTimeout(() => setCopyingContentKey(null), 1000);
                                  } catch {
                                    setCopyingContentKey(null);
                                  }
                                }}
                                className={`group relative text-[10px] px-1.5 py-0 rounded-full font-medium transition-all duration-200 flex items-center gap-0.5 overflow-hidden ${
                                  copyingContentKey === `content-${i}`
                                    ? 'bg-emerald-100 text-emerald-700'
                                    : 'bg-gradient-to-r from-slate-100 to-slate-200 text-slate-700 hover:from-indigo-50 hover:to-indigo-100 hover:text-indigo-700 hover:shadow-md hover:-translate-y-0.5'
                                }`}
                              >
                                {copyingContentKey === `content-${i}` ? '✅ 已复制' : '📋 复制'}
                              </button>
                              {content.length > 300 && (
                                <button
                                  onClick={toggleContent}
                                  className="group relative text-[10px] px-2 py-0.5 rounded-full font-medium transition-all duration-300 flex items-center gap-0.5 overflow-hidden bg-gradient-to-r from-slate-100 to-slate-200 text-slate-700 hover:from-indigo-50 hover:to-indigo-100 hover:text-indigo-700 hover:shadow-md hover:-translate-y-0.5"
                                >
                                  {contentExpanded ? (
                                    <>
                                       <span className="text-sm group-hover:-translate-y-0.5 transition-transform duration-200">↑</span>
                                       <span className="font-medium">收起</span>
                                     </>
                                   ) : (
                                     <>
                                       <span className="text-sm group-hover:translate-y-0.5 transition-transform duration-200">↓</span>
                                       <span className="font-medium">展开</span>
                                     </>
                                   )}
                                </button>
                              )}
                            </div>
                          </div>
                          <div className="text-xs text-slate-700 leading-snug whitespace-pre-wrap break-words">
                            {content.length > 300 ? (
                              contentExpanded ? (
                                (() => {
                                  let processed = content
                                    .replace(/\\n/g, '\n')
                                    .replace(/\\r/g, '\r')
                                    .replace(/\\t/g, '\t');
                                  return processed.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
                                })()
                              ) : getContentPreview(message, 300)
                            ) : (
                              (() => {
                                let processed = content
                                  .replace(/\\n/g, '\n')
                                  .replace(/\\r/g, '\r')
                                  .replace(/\\t/g, '\t');
                                return processed.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
                              })()
                            )}
                          </div>

                          {/* Tool calls detail */}
                          {toolCalls.length > 0 && (
                            <div className="mt-1 divide-y divide-slate-200">
                              {toolCalls.map((tc, tci) => {
                                const toolKey = `${i}-${tci}`;
                                const isToolExpanded = expandedToolCalls.has(toolKey);
                                const isCopyingToolKey = copyingTool === toolKey;
                                const formattedArgs = formatArguments(tc.function?.arguments);
                                const fullJson = JSON.stringify({
                                  id: tc.id,
                                  type: tc.type,
                                  function: {
                                    name: tc.function?.name,
                                    arguments: tc.function?.arguments ? JSON.parse(tc.function.arguments) : undefined,
                                  }
                                }, null, 2);

                                  return (
                                    <div key={toolKey} className="py-2">
                                      <div className="flex items-center justify-between gap-2 mb-1">
                                        <div className="flex items-center gap-1.5 min-w-0">
                                          <span className="inline-flex items-center gap-0.5 rounded-md bg-slate-100 border border-slate-200 px-1.5 py-0.5 text-[10px] font-semibold text-slate-700">
                                            <span>🔧</span>
                                            {tc.function?.name}
                                          </span>
                                          {tc.id && (
                                            <span className="text-[10px] text-slate-400 font-mono truncate">{tc.id}</span>
                                          )}
                                        </div>
                                        <div className="flex items-center gap-1">
                                          <button
                                            onClick={async () => {
                                              setCopyingTool(toolKey);
                                              try {
                                                await navigator.clipboard.writeText(fullJson);
                                                setTimeout(() => setCopyingTool(null), 1000);
                                              } catch {
                                                setCopyingTool(null);
                                              }
                                            }}
                                            className={`group relative text-[8px] px-1.5 py-0 rounded-full font-medium transition-all duration-200 flex items-center gap-0.5 overflow-hidden ${
                                              isCopyingToolKey
                                                ? 'bg-emerald-100 text-emerald-700'
                                                : 'bg-gradient-to-r from-slate-100 to-slate-200 text-slate-700 hover:from-indigo-50 hover:to-indigo-100 hover:text-indigo-700 hover:shadow-md hover:-translate-y-0.5'
                                            }`}
                                          >
                                            {isCopyingToolKey ? '✅ 已复制' : '📋 复制'}
                                          </button>
                                          <button
                                            onClick={() => {
                                              const newSet = new Set(expandedToolCalls);
                                              if (newSet.has(toolKey)) newSet.delete(toolKey);
                                              else newSet.add(toolKey);
                                              setExpandedToolCalls(newSet);
                                            }}
                                            className="group relative text-[8px] px-1.5 py-0 rounded-full font-medium transition-all duration-200 flex items-center gap-0.5 overflow-hidden bg-gradient-to-r from-slate-100 to-slate-200 text-slate-700 hover:from-indigo-50 hover:to-indigo-100 hover:text-indigo-700 hover:shadow-md hover:-translate-y-0.5"
                                          >
                                            {isToolExpanded ? '↑ 收起' : '↓ 详情'}
                                          </button>
                                        </div>
                                      </div>
                                      <div className="bg-slate-50 rounded border border-slate-200 overflow-hidden">
                                        <pre className="text-[10px] font-mono text-slate-700 whitespace-pre-wrap break-all p-1.5 max-h-[120px] overflow-auto">
                                          {isToolExpanded ? fullJson : formattedArgs}
                                        </pre>
                                      </div>
                                    </div>
                                  );
                                })}
                            </div>
                          )}
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}
              <div className="px-4 py-1.5 border-t border-slate-200 bg-slate-50/50 flex items-center justify-between">
                {!responseChoicesExpanded && (
                  <div className="text-xs text-slate-600">
                    共 {parsedChoices.length} 条响应，点击展开查看详情
                  </div>
                )}
                <button
                  onClick={() => setResponseChoicesExpanded(!responseChoicesExpanded)}
                  className="text-slate-400 hover:text-slate-600 transition-colors"
                >
                  {responseChoicesExpanded ? <ChevronDown size={16} /> : <ChevronRight size={16} />}
                </button>
              </div>
            </div>
          )}
        </div>
      ) : log.response_choices ? (
        <div>
          <div className="flex items-center justify-between mb-2">
            <span className="text-xs text-slate-500">响应内容</span>
          </div>
          {responseJsonExpanded ? (
            <div className="relative">
              <pre className="max-h-[420px] w-full overflow-auto rounded-xl bg-slate-50 border border-slate-200 p-3 pr-16 text-xs font-mono whitespace-pre-wrap break-all text-slate-700">
                {prettyChoices}
              </pre>
              <button
                onClick={async () => {
                  try {
                    await navigator.clipboard.writeText(prettyChoices);
                    setCopiedJsonKey('response-body');
                    setTimeout(() => setCopiedJsonKey(null), 1500);
                  } catch {}
                }}
                className={`absolute top-2 right-2 text-[10px] px-1.5 py-0.5 rounded-full transition-all shadow-sm ${
                  copiedJsonKey === 'response-body'
                    ? 'bg-emerald-100 border-emerald-300 text-emerald-700'
                    : 'bg-white/90 border border-slate-200 text-slate-600 hover:bg-blue-50 hover:text-blue-600 hover:border-blue-300'
                }`}
              >
                {copiedJsonKey === 'response-body' ? '✅ 已复制' : '📋 复制'}
              </button>
            </div>
          ) : (
            <div className="text-xs text-slate-500">点击「展开全部」查看原始 JSON</div>
          )}
        </div>
      ) : (
        <div className="flex flex-col items-center justify-center py-8 text-center">
          <ScrollText className="h-8 w-8 text-slate-300 mb-2" />
          <p className="text-xs text-slate-400">无响应内容记录</p>
        </div>
      )}

      {choicesParseError && (
        <div className="text-xs text-amber-500">⚠ 响应 JSON 解析失败，显示原始内容</div>
      )}
      </>
      )}

        </div>{/* end tab content */}
      </div>{/* end tab container */}
    </div>
  );
}

// ─── CleanLogsModal ──────────────────────────────────────────────────────────

function CleanLogsModal({
  onConfirm,
  onCancel,
}: {
  onConfirm: (type: "all" | "7d" | "30d") => void;
  onCancel: () => void;
}) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm" onClick={onCancel}>
      <div className="surface rounded-2xl p-6 max-w-sm w-full mx-4" onClick={e => e.stopPropagation()}>
        <h3 className="text-lg font-semibold mb-2">清理日志</h3>
        <p className="text-sm text-muted-foreground mb-4">选择要清理的日志范围，此操作不可撤销</p>
        <div className="space-y-2">
          <button
            onClick={() => onConfirm("7d")}
            className="w-full text-left px-4 py-3 rounded-xl border border-border hover:bg-white/60 transition-colors text-sm"
          >
            清理 7 天前的日志
          </button>
          <button
            onClick={() => onConfirm("30d")}
            className="w-full text-left px-4 py-3 rounded-xl border border-border hover:bg-white/60 transition-colors text-sm"
          >
            清理 30 天前的日志
          </button>
          <button
            onClick={() => onConfirm("all")}
            className="w-full text-left px-4 py-3 rounded-xl border border-red-200 text-red-500 hover:bg-red-50 transition-colors text-sm"
          >
            清理全部日志
          </button>
        </div>
        <button onClick={onCancel} className="mt-4 w-full action-secondary justify-center">
          取消
        </button>
      </div>
    </div>
  );
}
