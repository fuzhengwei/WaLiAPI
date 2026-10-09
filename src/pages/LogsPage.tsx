import { useEffect, useLayoutEffect, useState, useCallback, useMemo, useRef } from "react";
import { Link } from "react-router-dom";
import { logApi, channelApi } from "../lib/api";
import type { DeleteLogsInput, DeleteLogsReport, CleanupChannelInput, CleanupChannelReport } from "../lib/api";
import type { Channel } from "../types";
import type { RequestLog, SecurityFinding } from "../types";
import { formatTime, formatDuration, formatNumber } from "../lib/constants";
import { writeClipboard } from "../lib/runtime";
import { contentToString, requestLogSource, responseLogSource } from "../lib/auditLog";
import AuditTextPanel from "../components/logs/AuditTextPanel";
import AuditToolCall from "../components/logs/AuditToolCall";
import {
  ScrollText, RefreshCw, Trash2, ChevronDown, ChevronRight, AlertCircle,
  Bot, User, Wrench, Terminal, Eye, FileCode2, Image, ArrowRightLeft, ArrowUp, ArrowDown, ArrowDownLeft, ArrowUpRight, Shield, Timer, Coins,
  Search, X, Calendar, Key, Server, Box, ShieldAlert, Brain,
} from "lucide-react";

const PAGE_SIZE = 20;

// 与 src-tauri/src/audit_log.rs 的 BRIEF_MARKER_KEY 保持一致：「简要」级别裁断
// 请求消息列表后写回 JSON 顶层的标记字段名（前导下划线避开厂商真实字段）。
const BRIEF_MARKER_KEY = "_wali_brief";

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

type ResponsesInputItem = Record<string, unknown> & {
  type?: string;
  id?: string;
  call_id?: string;
  role?: string;
  content?: unknown;
  input?: unknown;
  output?: unknown;
  name?: string;
  arguments?: string;
};

function isResponsesToolCall(item: Record<string, unknown>): boolean {
  return item.type === "function_call" || item.type === "custom_tool_call";
}

function normalizeResponsesInputItem(item: ResponsesInputItem, index: number): Record<string, unknown> {
  const type = typeof item.type === "string" ? item.type : "";
  const role = typeof item.role === "string" ? item.role : type || "item";
  let content = item.content;

  if (type === "reasoning" && typeof item.encrypted_content === "string") {
    content = "[encrypted reasoning]";
  } else if (item.content === undefined && item.output !== undefined) {
    content = item.output;
  } else if (item.content === undefined && item.input !== undefined) {
    content = item.input;
  }

  return {
    ...item,
    role,
    content,
    _source: "responses" as const,
    _index: index,
  };
}

/** Extract all tool/function names from a message object */
function extractToolNames(msg: Record<string, unknown>): string[] {
  const names: string[] = [];
  if (msg._source === "responses" && isResponsesToolCall(msg)) {
    const name = typeof msg.name === "string" && msg.name ? msg.name : String(msg.type);
    names.push(name);
  }
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
  if (msg._source === "responses" && isResponsesToolCall(msg)) {
    const argumentsValue = typeof msg.arguments === "string" ? msg.arguments : msg.input;
    result.push({
      id: typeof msg.id === "string" ? msg.id : (typeof msg.call_id === "string" ? msg.call_id : undefined),
      type: typeof msg.type === "string" ? msg.type : undefined,
      function: {
        name: typeof msg.name === "string" && msg.name ? msg.name : String(msg.type),
        arguments: typeof argumentsValue === "string" ? argumentsValue : JSON.stringify(argumentsValue),
      },
    });
    return result;
  }
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
      if ((block.type === "text" || block.type === "input_text" || block.type === "output_text") && typeof block.text === "string") {
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
  if (msg._source === "responses" && isResponsesToolCall(msg)) {
    return `🔧 ${extractToolNames(msg).join(", ")}`;
  }
  // content 为 undefined 时 JSON.stringify 返回 undefined，需兜底为空串
  const str = JSON.stringify(content) ?? "";
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
/** 兼容旧版 Responses 逐帧审计：从原始 SSE 恢复标准响应卡片数据。 */
function choicesFromStreamSegments(segments: Array<{ seq: number; content: string }>): Array<Record<string, unknown>> {
  const contentParts: string[] = [];
  const reasoningParts: string[] = [];
  const toolCalls: Array<Record<string, unknown>> = [];
  const seenToolIds = new Set<string>();

  for (const segment of segments) {
    for (const line of segment.content.split("\n")) {
      if (!line.startsWith("data:")) continue;
      const payload = line.slice(5).trim();
      if (!payload || payload === "[DONE]") continue;
      let event: Record<string, unknown>;
      try { event = JSON.parse(payload) as Record<string, unknown>; } catch { continue; }

      const type = typeof event.type === "string" ? event.type : "";
      if ((type === "response.output_text.delta" || type === "response.text.delta") && typeof event.delta === "string") {
        contentParts.push(event.delta);
      } else if (
        (type === "response.reasoning_summary_text.delta" || type === "response.reasoning_text.delta") &&
        typeof event.delta === "string"
      ) {
        reasoningParts.push(event.delta);
      } else if (type === "response.output_item.done" && event.item && typeof event.item === "object") {
        const item = event.item as Record<string, unknown>;
        const itemType = typeof item.type === "string" ? item.type : "";
        if (itemType === "function_call" || itemType === "custom_tool_call") {
          const id = typeof item.call_id === "string" ? item.call_id : String(item.id ?? "");
          if (!id || seenToolIds.has(id)) continue;
          seenToolIds.add(id);
          const args = typeof item.arguments === "string" ? item.arguments : item.input;
          toolCalls.push({
            id,
            type: "function",
            function: {
              name: typeof item.name === "string" ? item.name : itemType,
              arguments: typeof args === "string" ? args : JSON.stringify(args),
            },
          });
        }
      } else if ((type === "response.completed" || type === "response.done") && event.response && typeof event.response === "object") {
        const response = event.response as Record<string, unknown>;
        const output = Array.isArray(response.output) ? response.output as Array<Record<string, unknown>> : [];
        for (const item of output) {
          const blocks = Array.isArray(item.content) ? item.content as Array<Record<string, unknown>> : [];
          for (const block of blocks) {
            if (typeof block.text === "string" && !contentParts.length) contentParts.push(block.text);
          }
        }
      }
    }
  }

  const message: Record<string, unknown> = { role: "assistant" };
  if (contentParts.length) message.content = contentParts.join("");
  if (reasoningParts.length) message.reasoning_content = reasoningParts.join("");
  if (toolCalls.length) message.tool_calls = toolCalls;
  if (!contentParts.length && !reasoningParts.length && !toolCalls.length) return [];
  return [{ index: 0, message, finish_reason: "stop" }];
}

const ROLE_META: Record<string, { icon: typeof Bot; color: string; bg: string; label: string }> = {
  system:    { icon: Terminal,   color: "text-slate-600",    bg: "bg-slate-100",  label: "System" },
  developer: { icon: Terminal,   color: "text-purple-600",   bg: "bg-purple-50",  label: "Developer" },
  user:      { icon: User,       color: "text-blue-600",     bg: "bg-blue-50",    label: "User" },
  assistant: { icon: Bot,        color: "text-emerald-600",  bg: "bg-emerald-50", label: "AI" },
  tool:      { icon: Wrench,     color: "text-amber-600",    bg: "bg-amber-50",   label: "Tool" },
  function:  { icon: Wrench,     color: "text-amber-600",    bg: "bg-amber-50",   label: "Func" },
  reasoning: { icon: Brain,      color: "text-violet-600",   bg: "bg-violet-50",  label: "Reasoning" },
  custom_tool_call: { icon: Wrench, color: "text-amber-600", bg: "bg-amber-50",   label: "Custom Tool" },
  function_call:    { icon: Wrench, color: "text-amber-600", bg: "bg-amber-50",   label: "Function" },
  custom_tool_call_output: { icon: Wrench, color: "text-amber-600", bg: "bg-amber-50", label: "Tool Output" },
  function_call_output:    { icon: Wrench, color: "text-amber-600", bg: "bg-amber-50", label: "Function Output" },
  additional_tools: { icon: FileCode2, color: "text-purple-600", bg: "bg-purple-50", label: "Additional Tools" },
};
function getRoleMeta(role: string) {
  return ROLE_META[role] || { icon: FileCode2, color: "text-purple-600", bg: "bg-purple-50", label: role };
}

// ─── Main Page ──────────────────────────────────────────────────────────────

export function LogsPage() {
  const [logs, setLogs] = useState<RequestLog[]>([]);
  const [details, setDetails] = useState<Record<string, RequestLog>>({});
  const [detailLoadingId, setDetailLoadingId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState(false);
  const [page, setPage] = useState(0);
  const [totalCount, setTotalCount] = useState(0);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [showCleanModal, setShowCleanModal] = useState(false);
  // 「渠道清理」tab 所需的渠道列表(下拉选择目标渠道)。
  const [channels, setChannels] = useState<Channel[]>([]);
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
  const [filterUpstreamType, setFilterUpstreamType] = useState<"" | "channel" | "auth_account">("");

  const hasActiveFilters = keyword || filterApiKey || filterChannel || filterModel || filterDateFrom || filterDateTo || filterTraceId || filterUpstreamType;

  // 请求序号：过滤条件快速变化时只接受最新一次请求的响应，
  // 乱序返回的陈旧数据不得覆盖新数据（FIX-15/NEW-2 统一模式）。
  const loadSeq = useRef(0);

  const load = useCallback((p: number = 0, silent: boolean = false) => {
    if (!silent) setLoading(true);
    const seq = ++loadSeq.current;
    const input = {
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
      upstream_type: filterUpstreamType || undefined,
    };
    logApi.getAll(input)
      .then(items => { if (seq === loadSeq.current) setLogs(items); })
      .catch(() => { if (seq === loadSeq.current) setLoadError(true); })
      .finally(() => { if (!silent && seq === loadSeq.current) setLoading(false); });
    // 总数查询与列表并行；静默轮询时同样刷新，保证页数与最新数据一致
    const { limit: _l, offset: _o, ...countInput } = input;
    logApi.count(countInput)
      .then(n => { if (seq === loadSeq.current) setTotalCount(n); })
      .catch(() => {});
  }, [keyword, filterApiKey, filterChannel, filterModel, filterDateFrom, filterDateTo, filterTraceId, filterUpstreamType]);

  // 过滤条件变化防抖 300ms 再重载（FIX-15：此前每个按键直接触发请求）。
  useEffect(() => {
    const timer = setTimeout(() => load(0), 300);
    return () => clearTimeout(timer);
  }, [load]);

  // 渠道清理 tab 需要渠道清单(仅打开弹窗时点按需拉取,避免常驻请求)。
  const loadChannels = useCallback(async () => {
    try {
      setChannels(await channelApi.getAll());
    } catch (e) {
      console.error("Failed to load channels:", e);
    }
  }, []);

  // ─── Auto-refresh: poll every 5s when page is visible ───────────────────
  // 默认开启保持既有语义，可关闭（偏好持久化，GAP-06，上游 #28 的跟进）。
  // 有日志行展开时不做静默轮询——新日志会把旧行顶出当前页导致详情
  // 悄然收起；正在阅读的详情优先（GAP-06）。
  const [autoRefresh, setAutoRefresh] = useState(() => localStorage.getItem("waliapi:logs-auto-refresh") !== "off");
  const pageRef = useRef(page);
  pageRef.current = page;
  const expandedIdRef = useRef(expandedId);
  expandedIdRef.current = expandedId;

  // 展开行的「置顶」需要表头的实际高度作为 sticky 偏移：Trace 列开关、字体缩放、
  // 换行都会改变表头高度，写死像素值会露出下方滚过的内容，故实测后写进 CSS 变量。
  const tableWrapRef = useRef<HTMLDivElement | null>(null);
  useLayoutEffect(() => {
    const wrap = tableWrapRef.current;
    const head = wrap?.querySelector("thead");
    if (!wrap || !head) return;
    const apply = () =>
      wrap.style.setProperty("--logs-thead-h", `${Math.ceil(head.getBoundingClientRect().height)}px`);
    apply();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(apply);
    ro.observe(head);
    return () => ro.disconnect();
  }, [showTraceColumn, logs.length]);

  const toggleAutoRefresh = () => {
    setAutoRefresh(prev => {
      localStorage.setItem("waliapi:logs-auto-refresh", prev ? "off" : "on");
      return !prev;
    });
  };

  useEffect(() => {
    if (!autoRefresh) return;
    const interval = setInterval(() => {
      if (document.visibilityState === "visible" && expandedIdRef.current === null) {
        load(pageRef.current, true);
      }
    }, 5000);
    return () => clearInterval(interval);
  }, [autoRefresh, load]);

  const clearFilters = () => {
    setKeyword("");
    setFilterApiKey("");
    setFilterChannel("");
    setFilterModel("");
    setFilterDateFrom("");
    setFilterDateTo("");
    setFilterTraceId("");
    setFilterUpstreamType("");
    setPage(0);
  };

  const handleDeleteLog = async (id: string) => {
    try {
      await logApi.delete(id);
      setLogs(prev => prev.filter(l => l.id !== id));
      setDetails(prev => { const next = { ...prev }; delete next[id]; return next; });
      if (expandedId === id) setExpandedId(null);
    } catch (e) {
      console.error("Failed to delete log:", e);
    }
  };

  const handleToggleLog = async (id: string) => {
    if (expandedId === id) {
      setExpandedId(null);
      return;
    }
    setExpandedId(id);
    if (details[id]) return;
    const summary = logs.find(log => log.id === id);
    // 基本模式明确没有正文时无需再请求详情接口；展开直接展示说明。
    if (summary?.detail_available === false || (summary?.detail_level === "basic" && !summary.request_body)) return;
    setDetailLoadingId(id);
    try {
      const detail = await logApi.get(id);
      setDetails(prev => ({ ...prev, [id]: detail }));
    } catch (e) {
      console.error("Failed to load log detail:", e);
    } finally {
      setDetailLoadingId(current => current === id ? null : current);
    }
  };

  const [cleanConfirm, setCleanConfirm] = useState<{
    kind: "logs" | "channel";
    input: DeleteLogsInput | CleanupChannelInput;
    preview: DeleteLogsReport | CleanupChannelReport;
    channelName?: string;
  } | null>(null);
  const [cleanRunning, setCleanRunning] = useState(false);

  const handleCleanLogs = async (input: DeleteLogsInput) => {
    // 两步确认:先 dry_run 预览影响行数,展示给用户确认后才真正删除。
    try {
      const preview = await logApi.deleteMany({ ...input, dry_run: true });
      setCleanConfirm({ kind: "logs", input, preview });
    } catch (e) {
      console.error("Failed to preview clean:", e);
    }
  };

  const handleCleanupChannel = async (input: CleanupChannelInput) => {
    try {
      const preview = await logApi.cleanupChannel({ ...input, dry_run: true });
      const ch = channels.find(c => c.id === input.channel_id);
      setCleanConfirm({ kind: "channel", input, preview, channelName: ch?.name });
    } catch (e) {
      console.error("Failed to preview channel cleanup:", e);
    }
  };

  const handleConfirmClean = async () => {
    if (!cleanConfirm) return;
    setCleanRunning(true);
    try {
      if (cleanConfirm.kind === "logs") {
        await logApi.deleteMany(cleanConfirm.input as DeleteLogsInput);
      } else {
        await logApi.cleanupChannel(cleanConfirm.input as CleanupChannelInput);
      }
      setCleanConfirm(null);
      setShowCleanModal(false);
      setPage(0);
      load(0);
    } catch (e) {
      console.error("Failed to clean logs:", e);
    } finally {
      setCleanRunning(false);
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
        <div className="flex min-w-0 items-center gap-2">
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
          <button
            onClick={toggleAutoRefresh}
            className={`action-secondary ${autoRefresh ? "text-blue-600 bg-blue-50" : ""}`}
            title={autoRefresh ? "自动刷新开启中（每 5 秒；展开详情或离开页面时暂停），点击关闭" : "自动刷新已关闭，点击开启"}
          >
            <Timer size={16} /> 自动刷新{autoRefresh ? "·开" : "·关"}
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
            <div className="relative">
              <Server size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-400 pointer-events-none" />
              <select
                aria-label="来源类型"
                value={filterUpstreamType}
                onChange={(e) => { setFilterUpstreamType(e.target.value as "" | "channel" | "auth_account"); setPage(0); }}
                className="w-full appearance-none bg-white pl-9 pr-3 py-2 text-sm rounded-lg border border-slate-200 focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500"
              >
                <option value="">全部来源</option>
                <option value="channel">API 渠道</option>
                <option value="auth_account">Auth 账号</option>
              </select>
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
          {loading && logs.length === 0 ? (
            <div className="flex flex-1 flex-col items-center justify-center gap-2 text-center">
              <RefreshCw className="h-10 w-10 animate-spin text-muted-foreground/60" />
              <p className="text-base font-medium">正在加载审计日志…</p>
            </div>
          ) : logs.length === 0 ? (
            <div className="flex flex-1 flex-col items-center justify-center gap-2 text-center">
              {loadError ? (
                <>
                  <AlertCircle className="h-12 w-12 text-red-400/70" />
                  <p className="text-base font-medium">日志加载失败</p>
                  <p className="text-sm text-muted-foreground">请检查服务是否已启动</p>
                  <button onClick={() => load(page)} className="mt-1 rounded-lg bg-blue-600 px-4 py-2 text-xs font-medium text-white hover:bg-blue-700">重新加载</button>
                </>
              ) : (
                <>
                  <ScrollText className="h-12 w-12 text-muted-foreground/70" />
                  <p className="text-base font-medium">暂无审计日志</p>
                  <p className="text-sm text-muted-foreground">当有模型请求经过网关后，这里会显示调用记录</p>
                </>
              )}
            </div>
          ) : (
            <>
              {/* Table header + body share the scroll area */}
              {/* 固定表格布局 + 百分比列宽：列宽只由 colgroup 决定，展开行的
                  colSpan 大块内容不再能把「模型」列撑长、也不再让表头错位；
                  总和 <100% 时余量全部给「模型」列，故永远不会出现横向滚动条。 */}
              <div ref={tableWrapRef} className="flex-1 overflow-auto">
                <table className="w-full table-fixed text-sm">
                  <thead className="sticky top-0 z-20 border-b border-border bg-white/90 backdrop-blur text-muted-foreground">
                    <tr>
                      <th className="w-[4%] px-2 py-3"></th>
                      <th className="w-[5%] truncate px-2 py-3 text-left font-medium whitespace-nowrap">#</th>
                      <th className="w-[10%] truncate px-2 py-3 text-left font-medium whitespace-nowrap">时间</th>
                      {showTraceColumn && <th className="w-[8%] truncate px-2 py-3 text-left font-medium whitespace-nowrap">Trace ID</th>}
                      <th className="w-[8%] truncate px-2 py-3 text-left font-medium whitespace-nowrap">密钥</th>
                      <th className="w-[10%] truncate px-2 py-3 text-left font-medium whitespace-nowrap">上游</th>
                      <th className="px-2 py-3 text-left font-medium">模型</th>
                      <th className="w-[8%] truncate px-2 py-3 text-left font-medium whitespace-nowrap">推理档位</th>
                      <th className="w-[7%] truncate px-2 py-3 text-left font-medium whitespace-nowrap">状态</th>
                      <th className="w-[7%] truncate px-2 py-3 text-right font-medium whitespace-nowrap">安全</th>
                      <th className="w-[9%] truncate px-2 py-3 text-right font-medium whitespace-nowrap">Token</th>
                      <th className="w-[7%] truncate px-2 py-3 text-right font-medium whitespace-nowrap">耗时</th>
                      <th className="w-[6%] px-2 py-3">
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
                        detail={details[log.id]}
                        detailLoading={detailLoadingId === log.id}
                        onToggle={() => handleToggleLog(log.id)}
                        onDelete={() => handleDeleteLog(log.id)}
                        showTraceColumn={showTraceColumn}
                      />
                    ))}
                  </tbody>
                </table>
              </div>

              {/* Pagination — fixed at bottom of table card */}
              {(() => {
                const totalPages = Math.max(1, Math.ceil(totalCount / PAGE_SIZE));
                const current = Math.min(page, totalPages - 1);
                // 生成可点击页码：首尾页 + 当前页附近 ±2，其余折叠为省略号
                const items: (number | "…")[] = [];
                for (let i = 0; i < totalPages; i++) {
                  if (i === 0 || i === totalPages - 1 || Math.abs(i - current) <= 2) {
                    items.push(i);
                  } else if (items[items.length - 1] !== "…") {
                    items.push("…");
                  }
                }
                const goto = (p: number) => {
                  const target = Math.min(Math.max(0, p), totalPages - 1);
                  setPage(target);
                  load(target);
                };
                return (
                  <div className="flex items-center justify-between gap-3 border-t border-border px-4 py-2.5 bg-white/60">
                    <span className="text-sm text-muted-foreground whitespace-nowrap">
                      共 {formatNumber(totalCount)} 条 · 第 {current + 1} / {totalPages} 页
                    </span>
                    <div className="flex items-center gap-1">
                      <button
                        onClick={() => goto(current - 1)}
                        disabled={current === 0 || loading}
                        className="action-secondary disabled:opacity-40 disabled:cursor-not-allowed"
                        style={{ padding: "6px 12px", fontSize: "13px" }}
                      >
                        上一页
                      </button>
                      {items.map((item, idx) =>
                        item === "…" ? (
                          <span key={`ellipsis-${idx}`} className="px-1.5 text-sm text-muted-foreground select-none">…</span>
                        ) : (
                          <button
                            key={item}
                            onClick={() => goto(item)}
                            disabled={loading}
                            className={`min-w-[30px] rounded-md px-1.5 py-1 text-[13px] transition-colors disabled:cursor-not-allowed ${
                              item === current
                                ? "bg-primary text-white font-medium"
                                : "text-muted-foreground hover:bg-slate-100 hover:text-foreground"
                            }`}
                          >
                            {item + 1}
                          </button>
                        ),
                      )}
                      <button
                        onClick={() => goto(current + 1)}
                        disabled={current >= totalPages - 1 || loading}
                        className="action-secondary disabled:opacity-40 disabled:cursor-not-allowed"
                        style={{ padding: "6px 12px", fontSize: "13px" }}
                      >
                        下一页
                      </button>
                    </div>
                  </div>
                );
              })()}
            </>
          )}
        </div>
      </div>

      {/* Clean modal(第一步:条件选择,含「清理日志/渠道清理」双 tab) */}
      {showCleanModal && (
        <CleanLogsModal
          channels={channels}
          onLoadChannels={loadChannels}
          onConfirmLogs={handleCleanLogs}
          onConfirmChannel={handleCleanupChannel}
          onCancel={() => setShowCleanModal(false)}
        />
      )}

      {/* 清理确认(第二步:预览影响 + 备份提醒 + 二次确认) */}
      {cleanConfirm && (
        <CleanConfirmModal
          kind={cleanConfirm.kind}
          channelName={cleanConfirm.channelName}
          preview={cleanConfirm.preview}
          onConfirm={handleConfirmClean}
          onCancel={() => setCleanConfirm(null)}
          running={cleanRunning}
        />
      )}
    </div>
  );
}

// ─── LogRow ──────────────────────────────────────────────────────────────────

function LogRow({
  log,
  detail,
  detailLoading,
  expanded,
  onToggle,
  onDelete,
  showTraceColumn,
}: {
  log: RequestLog;
  detail?: RequestLog;
  detailLoading: boolean;
  expanded: boolean;
  onToggle: () => void;
  onDelete: () => void;
  showTraceColumn: boolean;
}) {
  return (
    <>
      <tr
        className={`border-b border-white/6 transition-colors hover:bg-white/4 ${
          // 展开时把这一行钉在表头正下方：详情面板很长，滚到底后 ﹀ 会跟着滑出视口，
          // 就没法再点它收纳。sticky 在 tr 上于 Chromium/WKWebView 表现不一致，
          // 故逐格钉住，并用 inset shadow 保留随行移动的底部细分隔线。
          expanded
            ? "[&>td]:sticky [&>td]:top-[var(--logs-thead-h,44px)] [&>td]:z-10 [&>td]:bg-white [&>td]:shadow-[inset_0_-1px_0_rgba(15,23,42,0.08)]"
            : ""
        }`}
      >
        <td className="px-2 py-2.5">
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
        <td className="px-2 py-2.5 text-xs overflow-hidden max-w-[180px]">
          <div className="flex flex-col gap-1 truncate">
            <span className="truncate" title={log.channel_name || undefined}>{log.channel_name || "-"}</span>
            <UpstreamTypeBadge upstreamType={log.upstream_type} />
          </div>
        </td>
        <td className="px-2 py-2.5 text-[13px] font-mono overflow-hidden truncate max-w-[220px]" title={log.model}>
          <div className="flex flex-col gap-0.5">
            <span className="truncate font-medium text-foreground">{log.model}</span>
            {log.upstream_model && log.upstream_model !== log.model && (
              <span className="text-[10px] text-blue-500 leading-tight truncate">
                → {log.upstream_model}
              </span>
            )}
          </div>
        </td>
        <td className="px-2 py-2.5 text-xs overflow-hidden truncate" title={log.reasoning_effort || undefined}>
          {log.reasoning_effort ? (
            <span className="rounded-md bg-violet-500/10 px-1.5 py-0.5 text-[11px] font-medium text-violet-500">{log.reasoning_effort}</span>
          ) : (
            <span className="text-muted-foreground/50">-</span>
          )}
        </td>
        <td className="overflow-hidden px-2 py-2.5 text-xs">
          <div className="flex items-center gap-1.5">
            <span className={`rounded-full px-2 py-0.5 ${log.status_code === 200 ? "bg-emerald-500/12 text-emerald-300" : "bg-red-500/12 text-red-300"}`}>
              {log.status_code}
            </span>
            {log.is_stream && <span className="text-blue-400 text-[10px]">stream</span>}
            {log.is_retry && <span className="text-amber-400 text-[10px]">retry</span>}
          </div>
        </td>
        <td className="overflow-hidden px-2 py-2.5 text-xs">
          <div className="flex justify-end">
            <RiskBadge log={log} />
          </div>
        </td>
        <td className="overflow-hidden px-2 py-2.5 text-right text-xs align-middle">
          <div className="flex flex-col items-end gap-1" title={`Prompt: ${log.prompt_tokens}, Completion: ${log.completion_tokens}, Cached: ${log.cached_tokens ?? 0}`}>
            <div className="text-base font-semibold text-foreground tabular-nums tracking-tight leading-none">
              {log.total_tokens > 0 ? formatNumber(log.total_tokens) : <span className="text-muted-foreground/50">0</span>}
            </div>
            <div className="text-[10px] text-muted-foreground/75 whitespace-nowrap">
              <span className="inline-flex items-center gap-0.5">
                <ArrowDownLeft size={10} /> {formatNumber(log.prompt_tokens)}
              </span>
              <span className="mx-1 text-slate-300">·</span>
              <span className="inline-flex items-center gap-0.5">
                <ArrowUpRight size={10} /> {formatNumber(log.completion_tokens)}
              </span>
            </div>
            {log.cached_tokens > 0 && (
              <div className="inline-flex items-center rounded-full border border-emerald-200 bg-emerald-50 px-1.5 py-0.5 text-[10px] font-medium text-emerald-700 whitespace-nowrap">
                命中 {formatNumber(log.cached_tokens)} · {log.prompt_tokens > 0 ? Math.round((log.cached_tokens / log.prompt_tokens) * 100) : 0}%
              </div>
            )}
          </div>
        </td>
        <td className="px-3 py-2.5 text-right text-xs text-muted-foreground align-middle whitespace-nowrap">{formatDuration(log.duration_ms)}</td>
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
          <td colSpan={showTraceColumn ? 13 : 12} className="px-4 py-4 bg-slate-50/80 border-b border-border align-top">
            <div className="min-w-0 max-w-full overflow-hidden">
              {detailLoading ? (
                <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
                  <RefreshCw size={15} className="animate-spin" /> 正在加载日志详情…
                </div>
              ) : detail ? (
                <LogDetail log={detail} />
              ) : log.detail_available === false || (!log.request_body && log.detail_level === "basic") ? (
                <div className="rounded-xl border border-blue-100 bg-blue-50 px-4 py-3 text-sm text-blue-800">
                  该记录使用“基本”日志级别，未保存请求与响应正文；网络状态、渠道、模型、推理档位、响应码和 Token 等摘要信息仍可查看。
                  <Link to="/settings#general" className="ml-2 inline-flex items-center gap-0.5 font-semibold text-indigo-600 underline decoration-indigo-300 underline-offset-2 transition-colors hover:text-indigo-800 hover:decoration-indigo-500">
                    前往「设置 → 通用设置」开启详细日志级别
                  </Link>
                </div>
              ) : (
                <div className="rounded-xl border border-amber-100 bg-amber-50 px-4 py-3 text-sm text-amber-800">
                  日志详情暂时不可用，请稍后重试。
                </div>
              )}
            </div>
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

function UpstreamTypeBadge({ upstreamType }: { upstreamType: RequestLog["upstream_type"] }) {
  const isAuth = upstreamType === "auth_account";
  return (
    <span className={`w-fit rounded-full px-1.5 py-0.5 text-[10px] font-medium ${
      isAuth ? "bg-violet-50 text-violet-700" : "bg-blue-50 text-blue-700"
    }`}>
      {isAuth ? "Auth" : "API"}
    </span>
  );
}

// ─── LogDetail ────────────────────────────────────────────────────────────────

function LogDetail({ log }: { log: RequestLog }) {
  const [jsonExpanded, setJsonExpanded] = useState(false);
  const [responseJsonExpanded, setResponseJsonExpanded] = useState(false);
  const [sourceView, setSourceView] = useState(false);
  const [findings, setFindings] = useState<SecurityFinding[]>([]);
  const [expandedChoices, setExpandedChoices] = useState<Set<string>>(new Set());
  const [expandedMessages, setExpandedMessages] = useState<Set<number>>(new Set());
  const [copyingMessageKey, setCopyingMessageKey] = useState<string | null>(null);
  const [copyingThinkingKey, setCopyingThinkingKey] = useState<string | null>(null);
  const [copyingContentKey, setCopyingContentKey] = useState<string | null>(null);
  const [streamSegments, setStreamSegments] = useState<Array<{ seq: number; content: string }>>([]);
  const [segmentsLoading, setSegmentsLoading] = useState(log.is_stream);
  const [segmentsError, setSegmentsError] = useState(false);
  const [segmentsReload, setSegmentsReload] = useState(0);

  useEffect(() => {
    if (log.risk_score > 0) {
      logApi.getSecurityFindings(log.id).then(setFindings).catch(() => setFindings([]));
    } else {
      setFindings([]);
    }
  }, [log.id, log.risk_score]);

  // 流式请求懒加载已生成内容段（detailed 策略下由服务端随落账写入溢出表）
  useEffect(() => {
    let cancelled = false;
    setStreamSegments([]);
    setSegmentsError(false);
    setSegmentsLoading(log.is_stream);
    if (log.is_stream) {
      logApi.getStreamSegments(log.id)
        .then(segments => { if (!cancelled) setStreamSegments(segments); })
        .catch(() => { if (!cancelled) setSegmentsError(true); })
        .finally(() => { if (!cancelled) setSegmentsLoading(false); });
    }
    return () => { cancelled = true; };
  }, [log.id, log.is_stream, segmentsReload]);
  const requestSource = requestLogSource(log.request_body);
  const responseSource = useMemo(() => responseLogSource(log.response_choices, streamSegments), [log.response_choices, streamSegments]);
  const streamSegmentsText = streamSegments.length > 0 ? responseSource.text || "" : "";

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

  // 「简要」级别把请求消息列表裁到最新 N 条后，会在 JSON 顶层留一个 _wali_brief 标记
  // （与后端 BRIEF_MARKER_KEY 同名）。这里读出来告诉用户少看到了什么。
  const briefRaw = parsed?.[BRIEF_MARKER_KEY] as Record<string, unknown> | undefined;
  const briefMarker =
    briefRaw && typeof briefRaw === "object"
      ? {
          omitted: Number(briefRaw.omitted_messages) || 0,
          kept: Number(briefRaw.kept_messages) || 0,
          originalBytes:
            typeof briefRaw.original_bytes === "number" ? briefRaw.original_bytes : null,
        }
      : null;

  // Support Chat Completions (messages), Responses array input, and Responses string input.
  const rawMessages: Array<Record<string, unknown>> = parsed && Array.isArray(parsed.messages) ? parsed.messages : [];
  const rawInput: Array<ResponsesInputItem> = parsed && Array.isArray(parsed.input) ? parsed.input : [];
  const messages: Array<Record<string, unknown>> = rawMessages.length > 0
    ? rawMessages
    : typeof parsed?.input === "string"
      ? [{ role: "user", content: parsed.input, _source: "responses" as const, _index: 0 }]
      : rawInput.map(normalizeResponsesInputItem);
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
      const role = typeof msg.role === "string" ? msg.role : "unknown";
      const type = typeof msg.type === "string" ? msg.type : "message";
      const r = type === "message" ? role : type;
      roles.set(r, (roles.get(r) || 0) + 1);
      if (typeof msg.content === "string") totalInputChars += msg.content.length;
      if (Array.isArray(msg.content)) {
        for (const b of msg.content as Array<Record<string, unknown>>) {
          if ((b.type === "text" || b.type === "input_text" || b.type === "output_text") && typeof b.text === "string") {
            totalInputChars += b.text.length;
          }
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
  const legacyChoices = useMemo(() => choicesFromStreamSegments(streamSegments), [streamSegments]);
  let parsedChoices: Array<Record<string, unknown>> | null = null;
  let prettyChoices = log.response_choices || "";
  let choicesParseError = false;
  try {
    if (log.response_choices) {
      const choices: unknown = JSON.parse(log.response_choices);
      parsedChoices = Array.isArray(choices) ? choices : null;
      prettyChoices = JSON.stringify(choices, null, 2);
    }
  } catch { choicesParseError = true; }
  if (!parsedChoices && legacyChoices.length) {
    parsedChoices = legacyChoices;
    prettyChoices = JSON.stringify(parsedChoices, null, 2);
  }

  const [responseChoicesExpanded, setResponseChoicesExpanded] = useState(
    !parsedChoices || parsedChoices.length <= 5
  );
  const [activeTab, setActiveTab] = useState<"request" | "response">("request");

  return (
    <div className="w-full min-w-0 space-y-4">
      {/* ── Gateway Metadata Cards ── */}
      {/* 指标方块：列数随可用宽度自动排布，minmax 的下界用 min(9rem,100%) 兜底，
          保证任何窗口宽度下都不会超出展开单元格从而顶出横向滚动条；
          [&>*]:min-w-0 让格子里的 truncate 真正生效（grid 子项默认 min-width:auto）。 */}
      <div className="grid grid-cols-[repeat(auto-fit,minmax(min(9rem,100%),1fr))] gap-3 [&>*]:min-w-0">
        {/* Token detail */}
        <div className="rounded-xl border border-slate-200 bg-white p-3 min-h-[120px] flex flex-col">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><Coins size={13} /> Token 消耗</div>
          <div className="mt-2 text-2xl font-semibold tracking-tight text-slate-900 tabular-nums leading-none">{formatNumber(log.total_tokens)}</div>
          <div className="mt-2 text-[11px] text-slate-500">
            <span className="inline-flex items-center gap-1">
              <ArrowDownLeft size={11} /> <span className="font-medium text-slate-700 tabular-nums">{formatNumber(log.prompt_tokens)}</span>
            </span>
            <span className="mx-2 text-slate-300">·</span>
            <span className="inline-flex items-center gap-1">
              <ArrowUpRight size={11} /> <span className="font-medium text-slate-700 tabular-nums">{formatNumber(log.completion_tokens)}</span>
            </span>
          </div>
          {log.cached_tokens > 0 && (
            <div className="mt-2 inline-flex items-center rounded-full border border-emerald-200 bg-emerald-50 px-2 py-0.5 text-[10px] font-medium text-emerald-700">
              命中 {formatNumber(log.cached_tokens)} · {log.prompt_tokens > 0 ? Math.round((log.cached_tokens / log.prompt_tokens) * 100) : 0}%
            </div>
          )}
        </div>

        {/* Duration */}
        <div className="rounded-xl border border-slate-200 bg-white p-3 min-h-[120px] flex flex-col">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><Timer size={13} /> 响应耗时</div>
          <div className="mt-1.5 text-sm font-semibold text-slate-900">{formatDuration(log.duration_ms)}</div>
          <div className="mt-0.5 text-[11px] text-slate-400">
            {log.is_stream ? "流式传输" : "非流式"}
          </div>
        </div>

        {/* Cost estimate */}
        <div className="rounded-xl border border-slate-200 bg-white p-3 min-h-[120px] flex flex-col">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><Coins size={13} /> 成本估算</div>
          <div className="mt-1.5 text-sm font-semibold text-slate-900">{costEstimate}</div>
          <div className="mt-0.5 text-[11px] text-slate-400">参考 GPT-4o 定价</div>
        </div>

        {/* Request size */}
        <div className="rounded-xl border border-slate-200 bg-white p-3 min-h-[120px] flex flex-col">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><FileCode2 size={13} /> 请求大小</div>
          <div className="mt-1.5 text-sm font-semibold text-slate-900">{sizeLabel}</div>
          <div className="mt-0.5 text-[11px] text-slate-400">
            {convStats.msgCount} 条消息 · {convStats.totalInputChars > 1000 ? `${(convStats.totalInputChars / 1000).toFixed(1)}K` : convStats.totalInputChars} 字符
          </div>
        </div>

        {/* Upstream route — simple card */}
        <div className="rounded-xl border border-slate-200 bg-white p-3 min-h-[120px] flex flex-col">
          <div className="flex items-center justify-between gap-1">
            <div className="flex items-center gap-1.5 text-xs text-slate-500"><Shield size={13} /> 上游路由</div>
            <UpstreamTypeBadge upstreamType={log.upstream_type} />
          </div>
          <div className="mt-1.5 truncate text-sm font-semibold text-slate-900" title={log.channel_name || "-"}>
            {log.channel_name || "-"}
          </div>
          <div className="mt-0.5 text-[11px] text-slate-400">
            {log.is_retry ? "⚠ 重试转发" : "✓ 首选渠道"}
          </div>
        </div>

        {/* Reasoning effort — simple card */}
        <div className="rounded-xl border border-slate-200 bg-white p-3 min-h-[120px] flex flex-col">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><Brain size={13} /> 推理档位</div>
          <div className="mt-1.5 truncate text-sm font-semibold text-slate-900" title={log.reasoning_effort || "-"}>
            {log.reasoning_effort || "-"}
          </div>
          <div className="mt-0.5 truncate text-[11px] text-slate-400" title={`协议: ${log.upstream_protocol || log.downstream_protocol || "-"}`}>
            {log.upstream_protocol || log.downstream_protocol || "-"}
          </div>
        </div>
        {/* Model mapping */}
        <div className="rounded-xl border border-slate-200 bg-white p-3 min-h-[120px] flex flex-col">
          <div className="flex items-center gap-1.5 text-xs text-slate-500"><ArrowRightLeft size={13} /> 模型映射</div>
          <div className="mt-1.5 text-sm font-semibold text-slate-900">
            {modelMappingDisplay || modelRequested}
          </div>
          <div className="mt-0.5 text-[11px] text-slate-400">
            {modelMappingDisplay ? "网关重映射" : "直传上游"}
          </div>
        </div>
      </div>

      {/* ── 简要级别的截断说明 ── */}
      {briefMarker && briefMarker.omitted > 0 && (
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-xl border border-sky-200 bg-sky-50 px-3 py-2 text-[11px] leading-relaxed text-sky-800">
          <span className="font-semibold">简要日志</span>
          <span>
            请求消息列表只保留最新 {briefMarker.kept} 条，已省略较早的 {briefMarker.omitted} 条；
            响应内容与 Token 用量、状态码等统计完整保留。
          </span>
          {briefMarker.originalBytes != null && (
            <span className="text-sky-600/90">
              （原始请求 {briefMarker.originalBytes > 1048576
                ? `${(briefMarker.originalBytes / 1048576).toFixed(1)} MB`
                : `${(briefMarker.originalBytes / 1024).toFixed(1)} KB`}）
            </span>
          )}
        </div>
      )}

      {/* ── Trace ID ── */}
      {(log.trace_id || log.downstream_endpoint) && (
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 rounded-xl border border-slate-200 bg-white p-3">
        {log.trace_id && (
          <>
            <Search size={14} className="text-slate-400" />
            <span className="text-xs text-slate-500">Trace ID:</span>
            <span className="text-xs font-mono text-slate-700 break-all">{log.trace_id}</span>
          </>
        )}
        {log.downstream_endpoint && (
          <>
            <span className="text-xs text-slate-500">端点:</span>
            <span className="text-xs font-mono text-slate-700 break-all">{log.downstream_endpoint}</span>
          </>
        )}
      </div>
      )}

      {/* ── Security Summary ── */}
      {(log.risk_score > 0 || log.risk_summary) && (
        <div className="rounded-xl border border-slate-200 bg-white p-3 min-h-[120px] flex flex-col">
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
          <div className="flex min-w-0 flex-wrap gap-1.5">
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
        <div className="flex min-w-0 items-center gap-2">
          <Eye size={14} className="text-blue-500" />
          <span className="text-xs text-slate-500 shrink-0">对话构成:</span>
          <div className="flex min-w-0 flex-wrap gap-1.5">
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
      <div className="flex min-w-0 flex-wrap gap-1.5">
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
        <div className="flex min-w-0 items-start gap-2 rounded-xl bg-red-50 border border-red-200 p-3 text-xs text-red-600">
          <AlertCircle size={14} className="mt-0.5 shrink-0" />
          <span className="min-w-0 break-words [overflow-wrap:anywhere]">{log.error_message}</span>
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
        </div>
        <div className="flex flex-wrap items-center gap-2 border-b border-slate-200 px-4 py-2">
          <button
            onClick={() => setSourceView(false)}
            aria-pressed={!sourceView}
            className={`rounded-md px-2 py-1 text-xs font-medium ${!sourceView ? "bg-blue-50 text-blue-600" : "text-slate-500 hover:text-slate-700"}`}
          >
            可读视图
          </button>
          <button
            onClick={() => setSourceView(true)}
            aria-pressed={sourceView}
            className={`rounded-md px-2 py-1 text-xs font-medium ${sourceView ? "bg-blue-50 text-blue-600" : "text-slate-500 hover:text-slate-700"}`}
          >
            源数据
          </button>
          <div className="flex-1" />
          {!sourceView && activeTab === "request" && log.request_body && messages.length > 0 && (
            <button
              onClick={() => setJsonExpanded(!jsonExpanded)}
              className="text-xs text-blue-500 hover:text-blue-600 transition-colors font-medium"
            >
              {jsonExpanded ? "返回消息列表" : "请求 JSON（格式化）"}
            </button>
          )}
          {!sourceView && activeTab === "response" && (log.response_choices || legacyChoices.length > 0) && (
            <button
              onClick={() => setResponseJsonExpanded(!responseJsonExpanded)}
              className="text-xs text-blue-500 hover:text-blue-600 transition-colors font-medium"
            >
              {responseJsonExpanded ? "返回响应列表" : "响应摘要 JSON"}
            </button>
          )}
        </div>

        {/* Tab content */}
        <div className="p-4">
          {sourceView && activeTab === "request" && <AuditTextPanel key="request-source" {...requestSource} />}
          {sourceView && activeTab === "response" && (
            segmentsLoading ? (
              <p className="text-xs text-slate-500">正在加载保存的下游 SSE…</p>
            ) : segmentsError ? (
              <div role="alert" className="flex items-center gap-2 text-xs text-red-600">
                源数据加载失败，不能确认是否保存了 SSE。
                <button onClick={() => setSegmentsReload(value => value + 1)} className="text-blue-600">重试</button>
              </div>
            ) : <AuditTextPanel key="response-source" {...responseSource} />
          )}
          {!sourceView && (
            <p className="mb-3 text-[11px] leading-relaxed text-slate-500">
              可读视图为格式化展示，长消息预览可能截断，工具参数可能展开转义；核对保存内容请切换「源数据」。
              {activeTab === "request" ? "请求入库前会进行日志脱敏，简要日志还可能裁剪消息。" : "响应列表和摘要 JSON 由保存的响应提取重组，不代表原始 HTTP 响应。"}
            </p>
          )}

      {/* ── Request Tab Content ── */}
      {!sourceView && activeTab === "request" && (
      <>
      {messages.length > 0 ? (
        <div>
          <div className="flex items-center justify-between mb-2">
            <span className="text-xs font-medium text-slate-600">{jsonExpanded ? "请求 JSON（格式化展示）" : `消息列表 (${messages.length} 条)`}</span>
          </div>

          {jsonExpanded ? (
            <AuditTextPanel title="格式化请求" text={pretty} copyLabel="复制格式化 JSON" />
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
                    const fullContent = contentToString(msg.content);
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
                                   await writeClipboard(fullContent);
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
                        <div className="px-3 py-2 min-w-0">
                          <div className="min-w-0 text-xs text-slate-700 leading-snug whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                            {isLongContent && !isExpanded ? preview : fullContent}
                          </div>
                          {/* Tool calls detail */}
                          {(() => {
                            let toolCalls: ToolCall[] = [];
                            if (Array.isArray(msg.tool_calls)) {
                              toolCalls = msg.tool_calls;
                            } else if (msg.tool_call) {
                              toolCalls = [msg.tool_call as ToolCall];
                            } else if (msg._source === "responses") {
                              toolCalls = extractToolCalls(msg);
                            }
                            if (toolCalls.length === 0) return null;

                            return (
                              <div className="mt-2 divide-y divide-slate-200">
                                {toolCalls.map((toolCall, toolIndex) => <AuditToolCall key={`msg-${i}-${toolIndex}`} toolCall={toolCall} />)}
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
        <AuditTextPanel title={parseError ? "保存的请求内容（无法解析）" : `请求 JSON（格式化展示，${sizeLabel}）`} text={pretty} copyLabel={parseError ? "复制原文" : "复制格式化 JSON"} />
      ) : (
        <div className="text-xs text-slate-400">无请求内容记录</div>
      )}

      {parseError && (
        <div className="text-xs text-amber-500">⚠ JSON 解析失败，显示保存的内容</div>
      )}
      </>
      )}

      {/* ── Response Tab Content ── */}
      {!sourceView && activeTab === "response" && (
      <>
      {/* ── Response Choices Timeline ── */}
      {parsedChoices && parsedChoices.length > 0 ? (
        <div>
          <div className="flex items-center justify-between mb-2">
            <span className="text-xs font-medium text-slate-600">{responseJsonExpanded ? "响应摘要 JSON（格式化展示）" : `响应列表 (${parsedChoices.length} 条)`}</span>
          </div>

          {responseJsonExpanded ? (
            <AuditTextPanel title="格式化响应摘要" text={prettyChoices} copyLabel="复制格式化 JSON" />
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

                    // content 可能是 undefined/数组（Anthropic 内容块），统一转字符串防止后续 .replace/.length 崩溃
                    const content = contentToString(message.content);
                    const reasoningContent = typeof message.reasoning_content === "string" ? message.reasoning_content : "";

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
                                       await writeClipboard(reasoningContent);
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
                            <div className="min-w-0 text-xs text-slate-700 leading-snug whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                              {reasoningContent.length > 200 ? (
                                reasoningExpanded ? (
                                  reasoningContent
                                ) : reasoningPreviewTruncated
                              ) : (
                                reasoningContent
                              )}
                            </div>
                          </div>
                        )}

                        {/* Content section */}
                        {(content.length > 0 || toolCalls.length === 0) && (
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
                                    await writeClipboard(content);
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
                          <div className="min-w-0 text-xs text-slate-700 leading-snug whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                            {content.length > 300 ? (
                              contentExpanded ? (
                                content
                              ) : getContentPreview(message, 300)
                            ) : (
                              content
                            )}
                          </div>
                        </div>
                        )}

                          {/* Tool calls detail */}
                          {toolCalls.length > 0 && (
                            <div className="px-3 py-2 min-w-0 divide-y divide-slate-200">
                              {toolCalls.map((toolCall, toolIndex) => <AuditToolCall key={`response-${i}-${toolIndex}`} toolCall={toolCall} />)}
                            </div>
                          )}
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
        <AuditTextPanel title={choicesParseError ? "保存的响应摘要（无法解析）" : "响应摘要 JSON（格式化展示）"} text={prettyChoices} copyLabel={choicesParseError ? "复制保存的摘要" : "复制格式化 JSON"} />
      ) : streamSegmentsText ? (
        <AuditTextPanel {...responseSource} />
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

// ─── CleanLogsModal(第一步:条件选择,含「清理日志/渠道清理」双 tab)──────────

function CleanLogsModal({
  channels,
  onLoadChannels,
  onConfirmLogs,
  onConfirmChannel,
  onCancel,
}: {
  channels: Channel[];
  onLoadChannels: () => void;
  onConfirmLogs: (input: DeleteLogsInput) => void;
  onConfirmChannel: (input: CleanupChannelInput) => void;
  onCancel: () => void;
}) {
  const [tab, setTab] = useState<"logs" | "channel">("logs");

  // 渠道下拉两个 tab 都用(全局日志的目标渠道 / 渠道日志的渠道选择),
  // 弹窗一挂载即拉取;channels 非空则跳过,避免重复请求。
  useEffect(() => {
    if (channels.length === 0) onLoadChannels();
  }, [channels.length, onLoadChannels]);

  const inputCls = "w-full px-3 py-2 text-sm rounded-lg border border-slate-200 focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500 bg-white";
  const tabCls = (active: boolean) =>
    `flex-1 rounded-lg px-4 py-2 text-sm font-medium transition-all ${active ? "bg-white text-slate-900 shadow-sm" : "text-slate-500 hover:text-slate-700"}`;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm" onClick={onCancel}>
      <div className="surface rounded-2xl p-6 max-w-lg w-full mx-4" onClick={e => e.stopPropagation()}>
        <h3 className="text-lg font-semibold mb-1">清理</h3>
        <p className="text-sm text-muted-foreground mb-4">下一步将显示预计影响并二次确认。</p>

        {/* 双 tab:全局日志(全局维度)/ 渠道日志(按渠道维度) */}
        <div className="mb-4 flex rounded-lg border border-slate-200 bg-slate-50 p-0.5">
          <button className={tabCls(tab === "logs")} onClick={() => setTab("logs")}>全局日志</button>
          <button className={tabCls(tab === "channel")} onClick={() => setTab("channel")}>渠道日志</button>
        </div>

        {tab === "logs" ? (
          <CleanLogsForm inputCls={inputCls} channels={channels} onConfirm={onConfirmLogs} />
        ) : (
          <CleanChannelForm inputCls={inputCls} channels={channels} onConfirm={onConfirmChannel} />
        )}

        <div className="mt-5">
          <button onClick={onCancel} className="w-full action-secondary justify-center">取消</button>
        </div>
      </div>
    </div>
  );
}

// 全局日志表单(全局维度:时间 + 请求结果 + 可选目标渠道)。
function CleanLogsForm({
  inputCls,
  channels,
  onConfirm,
}: {
  inputCls: string;
  channels: Channel[];
  onConfirm: (input: DeleteLogsInput) => void;
}) {
  const [retention, setRetention] = useState("30"); // 保留最近 N 天,空=全部
  const [isSuccess, setIsSuccess] = useState("");
  const [channelId, setChannelId] = useState(""); // 空=全部渠道
  const [clearStats, setClearStats] = useState(false);

  const submit = () => {
    const input: DeleteLogsInput = {
      keep_recent_days: retention ? Number(retention) : undefined,
      is_success: isSuccess ? isSuccess === "success" : undefined,
      channel_id: channelId || undefined,
      clear_stats: clearStats || undefined,
    };
    onConfirm(input);
  };

  return (
    <div className="space-y-3">
      <div>
        <label className="mb-1 block text-xs font-medium text-slate-600">时间范围</label>
        <select value={retention} onChange={e => setRetention(e.target.value)} className={inputCls}>
          <option value="">清理全部日志</option>
          <option value={1}>保留最近 1 天（清理更早的）</option>
          <option value={7}>保留最近 7 天（清理更早的）</option>
          <option value={30}>保留最近 30 天（清理更早的）</option>
          <option value={90}>保留最近 90 天（清理更早的）</option>
        </select>
      </div>
      <div>
        <label className="mb-1 block text-xs font-medium text-slate-600">请求结果（可选）</label>
        <select value={isSuccess} onChange={e => setIsSuccess(e.target.value)} className={inputCls}>
          <option value="">全部结果</option>
          <option value="success">仅成功（2xx）</option>
          <option value="failed">仅失败（非 2xx）</option>
        </select>
      </div>
      <div>
        <label className="mb-1 block text-xs font-medium text-slate-600">目标渠道（可选）</label>
        <select value={channelId} onChange={e => setChannelId(e.target.value)} className={inputCls}>
          <option value="">全部渠道</option>
          {channels.map(ch => (
            <option key={ch.id} value={ch.id}>{ch.name}（{ch.id}）</option>
          ))}
        </select>
      </div>
      <label className="flex items-start gap-2 rounded-xl border border-slate-200 bg-slate-50 px-3 py-2.5 cursor-pointer">
        <input type="checkbox" checked={clearStats} onChange={e => setClearStats(e.target.checked)} className="mt-0.5" />
        <span className="text-sm">
          <span className="font-medium text-slate-800">同时清除对应的历史统计数据</span>
          <span className="block text-xs text-red-500">默认不清除；勾选后对应时间段的调用量与 Token 统计将不可恢复。</span>
        </span>
      </label>

      <button onClick={submit} className="w-full rounded-lg bg-red-500 px-4 py-2 text-sm font-medium text-white hover:bg-red-600">
        下一步：预览影响
      </button>
    </div>
  );
}

// 渠道清理表单(按渠道维度:清日志 / 清统计 / 重置失败计数)。
function CleanChannelForm({
  inputCls,
  channels,
  onConfirm,
}: {
  inputCls: string;
  channels: Channel[];
  onConfirm: (input: CleanupChannelInput) => void;
}) {
  const [channelId, setChannelId] = useState("");
  const [retention, setRetention] = useState(""); // 空=全部
  const [clearLogs, setClearLogs] = useState(true);
  const [clearStats, setClearStats] = useState(false);
  const [resetFail, setResetFail] = useState(false);

  const canSubmit = channelId !== "";

  const submit = () => {
    if (!canSubmit) return;
    onConfirm({
      channel_id: channelId,
      keep_recent_days: retention ? Number(retention) : undefined,
      clear_logs: clearLogs || undefined,
      clear_stats: clearStats || undefined,
      reset_fail: resetFail || undefined,
    });
  };

  return (
    <div className="space-y-3">
      <div>
        <label className="mb-1 block text-xs font-medium text-slate-600">目标渠道</label>
        <select value={channelId} onChange={e => setChannelId(e.target.value)} className={inputCls}>
          <option value="">请选择渠道…</option>
          {channels.map(ch => (
            <option key={ch.id} value={ch.id}>{ch.name}（{ch.id}）</option>
          ))}
        </select>
      </div>
      <div>
        <label className="mb-1 block text-xs font-medium text-slate-600">时间范围</label>
        <select value={retention} onChange={e => setRetention(e.target.value)} className={inputCls}>
          <option value="">全部时间</option>
          <option value={1}>保留最近 1 天（清理更早的）</option>
          <option value={7}>保留最近 7 天（清理更早的）</option>
          <option value={30}>保留最近 30 天（清理更早的）</option>
          <option value={90}>保留最近 90 天（清理更早的）</option>
        </select>
      </div>
      <div className="space-y-2">
        <label className="flex items-start gap-2 rounded-xl border border-slate-200 bg-slate-50 px-3 py-2.5 cursor-pointer">
          <input type="checkbox" checked={clearLogs} onChange={e => setClearLogs(e.target.checked)} className="mt-0.5" />
          <span className="text-sm">
            <span className="font-medium text-slate-800">清理该渠道的日志</span>
            <span className="block text-xs text-slate-500">删除该渠道的审计日志（含流式内容段），不影响统计。</span>
          </span>
        </label>
        <label className="flex items-start gap-2 rounded-xl border border-slate-200 bg-slate-50 px-3 py-2.5 cursor-pointer">
          <input type="checkbox" checked={clearStats} onChange={e => setClearStats(e.target.checked)} className="mt-0.5" />
          <span className="text-sm">
            <span className="font-medium text-slate-800">清除该渠道的统计数据</span>
            <span className="block text-xs text-red-500">删除该渠道的调用量、成功/失败、Token 统计，不可恢复。</span>
          </span>
        </label>
        <label className="flex items-start gap-2 rounded-xl border border-slate-200 bg-slate-50 px-3 py-2.5 cursor-pointer">
          <input type="checkbox" checked={resetFail} onChange={e => setResetFail(e.target.checked)} className="mt-0.5" />
          <span className="text-sm">
            <span className="font-medium text-slate-800">重置失败计数（恢复成功率）</span>
            <span className="block text-xs text-slate-500">仅把该渠道的失败计数清零，请求数/成功数/Token 不动；成功率恢复为 ~100%。</span>
          </span>
        </label>
      </div>

      <button
        onClick={submit}
        disabled={!canSubmit}
        className="w-full rounded-lg bg-red-500 px-4 py-2 text-sm font-medium text-white hover:bg-red-600 disabled:opacity-40"
      >
        下一步：预览影响
      </button>
    </div>
  );
}

// ─── CleanConfirmModal(第二步:确认,含备份提醒;支持日志/渠道两种形态)────────

function CleanConfirmModal({
  kind,
  channelName,
  preview,
  onConfirm,
  onCancel,
  running,
}: {
  kind: "logs" | "channel";
  channelName?: string;
  preview: DeleteLogsReport | CleanupChannelReport;
  onConfirm: () => void;
  onCancel: () => void;
  running: boolean;
}) {
  const p = preview as DeleteLogsReport & CleanupChannelReport;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm" onClick={running ? undefined : onCancel}>
      <div className="surface rounded-2xl p-6 max-w-md w-full mx-4" onClick={e => e.stopPropagation()}>
        <h3 className="text-lg font-semibold mb-1 text-red-600">确认清理？</h3>
        <p className="text-sm text-muted-foreground mb-4">此操作不可撤销</p>

        <div className="space-y-2 rounded-xl border border-slate-200 bg-slate-50 p-4 text-sm">
          {kind === "channel" && channelName && (
            <div className="flex justify-between">
              <span className="text-slate-500">目标渠道</span>
              <span className="font-semibold text-slate-900">{channelName}</span>
            </div>
          )}
          {kind === "logs" && (
            <div className="flex justify-between">
              <span className="text-slate-500">将清理日志</span>
              <span className="font-semibold text-slate-900">{formatNumber(p.matched_logs)} 条</span>
            </div>
          )}
          {kind === "logs" && p.matched_stats > 0 && (
            <div className="flex justify-between">
              <span className="text-slate-500">将同步清除统计数据</span>
              <span className="font-semibold text-red-600">{formatNumber(p.matched_stats)} 行</span>
            </div>
          )}
          {kind === "channel" && p.matched_logs > 0 && (
            <div className="flex justify-between">
              <span className="text-slate-500">将清理该渠道日志</span>
              <span className="font-semibold text-slate-900">{formatNumber(p.matched_logs)} 条</span>
            </div>
          )}
          {kind === "channel" && p.matched_stats > 0 && (
            <div className="flex justify-between">
              <span className="text-slate-500">将清除该渠道统计数据</span>
              <span className="font-semibold text-red-600">{formatNumber(p.matched_stats)} 行</span>
            </div>
          )}
          {kind === "channel" && p.reset_fail_rows > 0 && (
            <div className="flex justify-between">
              <span className="text-slate-500">将重置失败计数</span>
              <span className="font-semibold text-amber-600">{formatNumber(p.reset_fail_rows)} 行</span>
            </div>
          )}
        </div>

        <div className="mt-3 rounded-xl border border-amber-200 bg-amber-50 p-3 text-xs leading-5 text-amber-800">
          清理前建议先备份数据库：应用启动迁移时会自动生成 <code className="font-mono">waliapi.db.pre-upgrade-*</code> 快照，也可手动复制数据库文件备份。清理后日志不可恢复；若勾选了清除统计数据，对应统计同样不可恢复。
        </div>

        <div className="mt-5 flex gap-2">
          <button onClick={onCancel} disabled={running} className="flex-1 action-secondary justify-center">取消</button>
          <button onClick={onConfirm} disabled={running} className="flex-1 rounded-lg bg-red-500 px-4 py-2 text-sm font-medium text-white hover:bg-red-600 disabled:opacity-50">
            {running ? "清理中…" : "确认清理"}
          </button>
        </div>
      </div>
    </div>
  );
}
