import { KnowledgeConnectionPanel } from "../components/KnowledgeConnectionPanel";
import { Select } from "../components/Select";
import { Fragment, useEffect, useState, useCallback, useRef, useMemo, type ReactNode } from "react";
import { Link as RouterLink, useLocation } from "react-router-dom";
import { onFileDrop, isTauriRuntime } from "../lib/runtime";
import { pickFileAsBase64 } from "../lib/web";
import {
  KnowledgeBase,
  KbDocument,
  type PdfTextExtraction,
  KbSearchResult,
  KbRagAnswer,
  KbRetrievalDetail,
  KbSource,
  KbIndexMeta,
  KbTag,
  ConversationMessage,
  kbApi,
  channelApi,
  apiKeyApi,
  settingsApi,
  serviceApi,
  serverApi,
  wikiApi,
  type WikiProject,
  type WikiPage,
  type WikiSource,
  type WikiSearchResult,
  type WikiGraphData,
  type WikiTag,
  type WikiExportResult,
  type WikiImportResult,
  type WikiIndexRebuildResult,
  type ServiceStatus,
} from "../lib/api";
import type { Channel, ApiKey } from "../types";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeRaw from "rehype-raw";
import rehypeSanitize from "rehype-sanitize";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import {
  BookOpen,
  Plus,
  Trash2,
  Upload,
  Search,
  MessageCircle,
  RefreshCw,
  FileText,
  CheckCircle2,
  Loader2,
  XCircle,
  Clock,
  Hash,
  ChevronRight,
  ChevronDown,
  Check,
  Settings as SettingsIcon,
  Terminal,
  Server,
  Wifi,
  Copy,
  Layers,
  GitBranch,
  Link,
  FolderOpen,
  FolderInput,
  Sparkles,
  Database,
  Tag,
  Sliders,
  ChevronUp,
  Code,
  ExternalLink,
  Package,
  Rocket,
  Puzzle,
  Network,
  Edit3,
  AlertTriangle,
  Save,
  Inbox,
Play,
  KeyRound,
  Download,
  FileArchive,
  FolderSearch,
  ListTree,
} from "lucide-react";

import { PromptTemplatesSection } from "../components/PromptTemplatesSection";
import { SoftwareSection } from "./SoftwarePage";

type ServiceTab = "knowledge" | "wiki" | "mcp" | "software" | "skills" | "prompts";
type KbTab = "documents" | "sources" | "search" | "ask" | "settings" | "index" | "mcp";

// ── Wiki 摄入进度（模块级内存状态）────────────────────────────────
// 进度提升到模块级：切换一级侧边栏（仪表盘/设置/日志）时「服务」页面整体卸载，
// 组件内 state 会丢失 → 返回时进度条"有概率隐藏"（依赖实时事件流，挂载瞬间无新事件即空白）。
// 模块级状态在卸载期间持续存活，回到「源」tab 立即恢复显示；完成/失败/取消后自动删除。
type WikiProgressEntry = { stage: string; progress: number; detail: string; filename: string };
let moduleWikiProgress: Record<string, WikiProgressEntry> = {};
const wikiProgressSubs = new Set<() => void>();
function patchWikiProgress(updater: (prev: Record<string, WikiProgressEntry>) => Record<string, WikiProgressEntry>) {
  moduleWikiProgress = updater(moduleWikiProgress);
  wikiProgressSubs.forEach((fn) => fn());
}
function useModuleWikiProgress(): Record<string, WikiProgressEntry> {
  const [, setTick] = useState(0);
  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    wikiProgressSubs.add(fn);
    return () => { wikiProgressSubs.delete(fn); };
  }, []);
  return moduleWikiProgress;
}

// ── 通用确认弹窗 ──────────────────────────────────────────────
// Tauri WebView2 会抑制原生 window.confirm（直接继续执行），破坏性操作必须用
// 自绘弹窗二次确认（项目约定：弹窗期间锁定 body 滚动）。
function ConfirmDialog({ message, confirmLabel = "删除", onCancel, onConfirm }: {
  message: string; confirmLabel?: string; onCancel: () => void; onConfirm: () => void;
}) {
  useEffect(() => {
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => { document.body.style.overflow = prev; };
  }, []);
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30" onClick={onCancel}>
      <div role="dialog" aria-modal="true" className="w-full max-w-sm rounded-2xl bg-white p-6 shadow-xl" onClick={e => e.stopPropagation()}>
        <h3 className="text-sm font-semibold text-slate-900">确认删除</h3>
        <p className="mt-2 text-sm text-slate-500">{message}</p>
        <div className="mt-5 flex justify-end gap-2">
          <button onClick={onCancel} className="action-secondary">取消</button>
          <button onClick={onConfirm} className="action-primary bg-red-600 hover:bg-red-700">{confirmLabel}</button>
        </div>
      </div>
    </div>
  );
}

export function KnowledgeBasePage() {
  const location = useLocation();
  const initialTab: ServiceTab = location.pathname.includes("/mcp") ? "mcp" : location.pathname.includes("/software") ? "software" : location.pathname.includes("/skills") ? "skills" : location.pathname.includes("/prompts") ? "prompts" : location.pathname.includes("/wiki") ? "wiki" : "knowledge";
  const [serviceTab, setServiceTab] = useState<ServiceTab>(initialTab);

  // 实时测量 page-header 高度 → CSS 变量 --wali-page-header-h。
  // 详情页（RAG/Wiki）子菜单用 sticky top:var(...) 做吸顶阈值，与 header 实际高度
  // 精确一致（窗口缩放/字体变化时 ResizeObserver 同步），避免吸顶后"先跟随滚动一小段"。
  const headerRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = headerRef.current;
    if (!el) return;
    const apply = () => {
      document.documentElement.style.setProperty("--wali-page-header-h", `${el.offsetHeight}px`);
    };
    apply();
    const ro = new ResizeObserver(apply);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const serviceTabs: { key: ServiceTab; label: string; icon: typeof BookOpen }[] = [
    { key: "knowledge", label: "RAG", icon: BookOpen },
    { key: "wiki", label: "Wiki", icon: Network },
    { key: "software", label: "Link", icon: Link },
    { key: "mcp", label: "MCP", icon: Terminal },
    { key: "skills", label: "Skills", icon: Puzzle },
    { key: "prompts", label: "Prompt 模板", icon: FileText },
  ];

  return (
    <div className="page-shell space-y-6">
      {/* Page Header */}
      <div className="page-header sticky top-0 z-30 flex-wrap -mx-7 -mt-7 mb-2 bg-white/90 px-7 py-5 backdrop-blur-md border-b border-slate-100" ref={headerRef}>
        <div className="min-w-0">
          <h1 className="page-title">服务</h1>
          <p className="page-subtitle">本地 RAG 知识库 · Wiki 知识图谱 · Link · MCP · Prompt 模板 · 支持 AI Agent 对接</p>
        </div>
        <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1">
          {serviceTabs.map(({ key, label, icon: Icon }) => (
            <Fragment key={key}>
              <button
                onClick={() => setServiceTab(key)}
                title={key === "software" ? "测试功能 · 开发迭代中" : undefined}
                className={`flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-xl px-3 py-2 text-sm font-medium transition-all ${
                  serviceTab === key
                    ? "border border-blue-100 bg-white text-slate-900 shadow-[0_8px_18px_rgba(15,23,42,0.05)]"
                    : "text-slate-500 hover:bg-white/70 hover:text-slate-900"
                }`}
              >
                <Icon size={16} />
                {label}
                {key === "software" && (
                  <span className="rounded-full bg-amber-100 px-1.5 py-0.5 text-[10px] font-semibold leading-none text-amber-600">
                    Beta
                  </span>
                )}
              </button>
              {/* Software 属测试功能：分割线与其后「对外能力」类入口（MCP / Skills / Prompt 模板）分组 */}
              {key === "software" && <div aria-hidden className="mx-1 h-6 w-px shrink-0 bg-slate-300" />}
            </Fragment>
          ))}
        </div>
      </div>

      <div>
        {serviceTab === "knowledge" ? <KnowledgeBaseSection /> : serviceTab === "wiki" ? <WikiSection /> : serviceTab === "mcp" ? <McpSection /> : serviceTab === "software" ? <SoftwareSection /> : serviceTab === "prompts" ? <PromptTemplatesSection /> : <SkillsSection />}
      </div>
    </div>
  );
}


// ─── MCP Service Section ─────────────────────────────────────────────────

const TOOL_ICONS: Record<string, typeof Terminal> = {
  search_knowledge_base: Search,
  list_knowledge_bases: BookOpen,
  read_document: FileText,
  ask_knowledge_base: MessageCircle,
  get_knowledge_base_stats: Database,
  create_knowledge_base: Plus,
  update_knowledge_base: SettingsIcon,
  delete_knowledge_base: Trash2,
  upload_document: Upload,
  delete_document: Trash2,
  list_documents: Layers,
  build_index: Sparkles,
  import_source: GitBranch,
};

/** 轻量 JSON 语法高亮（零依赖）：key 蓝、string 绿、number 琥珀、bool/null 玫红、标点灰 */
const JSON_TOKEN_RE =
  /("(?:\\.|[^"\\])*")(\s*:)?|\b(true|false|null)\b|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g;

function HighlightedJson({ text }: { text: string }) {
  const nodes: ReactNode[] = [];
  const re = new RegExp(JSON_TOKEN_RE.source, "g");
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) {
      nodes.push(
        <span key={`p${last}`} className="text-slate-500">
          {text.slice(last, m.index)}
        </span>,
      );
    }
    if (m[1] !== undefined) {
      if (m[2] !== undefined) {
        nodes.push(
          <span key={`k${m.index}`}>
            <span className="text-sky-700">{m[1]}</span>
            <span className="text-slate-400">{m[2]}</span>
          </span>,
        );
      } else {
        nodes.push(
          <span key={`s${m.index}`} className="text-emerald-700">
            {m[1]}
          </span>,
        );
      }
    } else if (m[3] !== undefined) {
      nodes.push(
        <span key={`b${m.index}`} className="font-medium text-rose-600">
          {m[3]}
        </span>,
      );
    } else if (m[4] !== undefined) {
      nodes.push(
        <span key={`n${m.index}`} className="text-amber-600">
          {m[4]}
        </span>,
      );
    }
    last = re.lastIndex;
    if (m[0] === "") re.lastIndex++;
  }
  if (last < text.length) {
    nodes.push(
      <span key={`p${last}`} className="text-slate-500">
        {text.slice(last)}
      </span>,
    );
  }
  return <>{nodes}</>;
}

function McpSection() {
  const [services, setServices] = useState<ServiceStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    serviceApi.getStatuses()
      .then(setServices)
      .catch(console.error)
      .finally(() => setLoading(false));
  }, []);

  const mcpService = services.find(s => s.id === "mcp");
  const kbService = services.find(s => s.id === "knowledge");
  const [serverUrl, setServerUrl] = useState("http://127.0.0.1:8777");

  useEffect(() => {
    serverApi.getStatus().then(s => {
      if (s.running) setServerUrl(`http://127.0.0.1:${s.port}`);
    }).catch(() => {});
  }, []);

  const baseUrl = serverUrl;
  const mcpEndpoint = `${baseUrl}/mcp`;
  const sseEndpoint = `${baseUrl}/mcp/sse`;
  const tools = (mcpService?.stats?.tools as { name: string; label: string; desc: string }[]) || [];

  const handleCopy = (text: string) => {
    navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  // ── 在线调用示例（左示例 / 右真实调用）──
  const [mcpAuth, setMcpAuth] = useState(() => localStorage.getItem("waliapi-mcp-auth") ?? "");
  // 密钥下拉选择：值为 API Key id 或 "manual"（手动输入 MCP Token 等）
  const [mcpApiKeys, setMcpApiKeys] = useState<ApiKey[]>([]);
  const [authChoice, setAuthChoice] = useState<string>(() => localStorage.getItem("waliapi-mcp-auth-choice") ?? "manual");
  const [authLoading, setAuthLoading] = useState(false);
  useEffect(() => {
    apiKeyApi.getAll().then((keys) => {
      setMcpApiKeys(keys.filter((k) => k.status === 1));
    }).catch(() => {});
  }, []);
  // 选择已保存的 API Key 时自动取完整密钥填入；手动输入则保留现有值
  useEffect(() => {
    if (!authChoice || authChoice === "manual") return;
    let cancelled = false;
    setAuthLoading(true);
    apiKeyApi.getFull(authChoice)
      .then((full) => {
        if (cancelled) return;
        setMcpAuth(full);
        localStorage.setItem("waliapi-mcp-auth", full);
      })
      .catch(() => {})
      .finally(() => { if (!cancelled) setAuthLoading(false); });
    return () => { cancelled = true; };
  }, [authChoice]);

  const authOptions = [
    ...mcpApiKeys.map((k) => ({
      value: k.id,
      label: `${k.name}（…${(k.key || "").slice(-4)}）`,
    })),
    { value: "manual", label: "手动输入（MCP Token 等）" },
  ];
  const handleAuthChoice = (v: string) => {
    setAuthChoice(v);
    localStorage.setItem("waliapi-mcp-auth-choice", v);
  };
  const [calling, setCalling] = useState(false);
  // 最近一次响应（无需调用记录，只保留最后一次）
  const [lastResp, setLastResp] = useState<{ ok: boolean; status: number | "err"; ms: number; time: string; response: string } | null>(null);

  const curlExample = `curl -X POST ${mcpEndpoint} \\
  -H "Authorization: Bearer $WALIAPI_API_KEY" \\
  -H "Content-Type: application/json" \\
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tools/list",
    "params": {}
  }'`;

  const runExample = async () => {
    const started = performance.now();
    const time = new Date().toLocaleTimeString("zh-CN", { hour12: false });
    setCalling(true);
    const rpcBody = JSON.stringify({ jsonrpc: "2.0", id: Date.now(), method: "tools/list", params: {} });
    let resp: { ok: boolean; status: number | "err"; ms: number; time: string; response: string };
    try {
      let status: number;
      let text: string;
      if (isTauriRuntime()) {
        // 桌面端：Rust 侧代理请求，绕开 WKWebView 对回环地址的混合内容/CORS 限制
        const r = await serverApi.mcpOnlineCall(mcpAuth, rpcBody);
        status = r.status;
        text = r.body;
      } else {
        // 非 Tauri 环境（浏览器同源/常规 fetch）：直接请求
        // WebKit 对安全上下文里的 127.0.0.1 字面 IP 不豁免混合内容，fetch 用 localhost
        const fetchEndpoint = mcpEndpoint.replace("127.0.0.1", "localhost");
        const res = await fetch(fetchEndpoint, {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            Accept: "application/json, text/event-stream",
            Authorization: `Bearer ${mcpAuth}`,
          },
          body: rpcBody,
        });
        status = res.status;
        text = await res.text();
      }
      const ms = Math.round(performance.now() - started);
      // SSE 响应折叠出 data: 行；纯 JSON 直接美化
      if (text.startsWith("data:")) {
        text = text
          .split("\n")
          .filter((l) => l.startsWith("data:"))
          .map((l) => l.slice(5).trim())
          .join("\n");
      }
      let ok = status >= 200 && status < 300;
      try {
        const parsed = JSON.parse(text);
        ok = ok && !parsed.error;
        text = JSON.stringify(parsed, null, 2);
      } catch {
        /* 非 JSON 保留原文 */
      }
      resp = { ok, status, ms, time, response: text };
    } catch (e) {
      const ms = Math.round(performance.now() - started);
      resp = {
        ok: false,
        status: "err",
        ms,
        time,
        response: `请求失败：${String(e)}\n\n可能原因：服务未启动或端口不正确；也可能是桌面端仍在运行旧版本（需重启使在线调用生效）。`,
      };
    } finally {
      setCalling(false);
    }
    setLastResp(resp);
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-20">
        <Loader2 className="h-8 w-8 animate-spin text-slate-400" />
      </div>
    );
  }

  return (
    <div className="grid gap-4 lg:grid-cols-2">
      {/* Service Status */}
      <div className="surface data-card rounded-2xl">
        <div className="mb-4 flex items-center gap-2">
          <Server size={18} className="text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">服务状态</h3>
        </div>
        <div className="space-y-3">
          {kbService && (
            <div className="rounded-xl border border-slate-100 bg-slate-50 p-4">
              <div className="flex items-center justify-between">
                <span className="text-sm font-medium text-slate-700">RAG 服务</span>
                <span className={`flex items-center gap-1.5 text-xs ${kbService.running ? "text-emerald-600" : "text-red-500"}`}>
                  <Wifi size={12} /> {kbService.running ? "运行中" : "已停止"}
                </span>
              </div>
              <div className="mt-2 text-xs text-slate-500">
                RAG: {String(kbService.stats.knowledge_bases || 0)} · 文档: {String(kbService.stats.documents || 0)} · 切片: {String(kbService.stats.chunks || 0)}
              </div>
            </div>
          )}
          {mcpService && (
            <div className="rounded-xl border border-slate-100 bg-slate-50 p-4">
              <div className="flex items-center justify-between">
                <span className="text-sm font-medium text-slate-700">MCP</span>
                <span className={`flex items-center gap-1.5 text-xs ${mcpService.running ? "text-emerald-600" : "text-red-500"}`}>
                  <Wifi size={12} /> {mcpService.running ? "运行中" : "已停止"}
                </span>
              </div>
              <div className="mt-2 text-xs text-slate-500">
                可用 RAG: {String(mcpService.stats.available_knowledge_bases || 0)} · 工具: {tools.length}
              </div>
            </div>
          )}
        </div>
      </div>

      {/* MCP Endpoints */}
      <div className="surface data-card rounded-2xl">
        <div className="mb-4 flex items-center gap-2">
          <Terminal size={18} className="text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">MCP 端点</h3>
        </div>
        <div className="space-y-3">
          <div>
            <label className="mb-1 block text-xs font-medium text-slate-500">JSON-RPC 端点（仅 POST，浏览器直接访问无效）</label>
            <div className="flex items-center gap-2">
              <code className="flex-1 truncate rounded-lg bg-slate-50 border border-slate-200 px-3 py-2 text-xs font-mono text-slate-800">{mcpEndpoint}</code>
              <button
                onClick={() => handleCopy(mcpEndpoint)}
                className="rounded-lg border border-slate-200 p-2 hover:bg-slate-50"
              >
                {copied ? <CheckCircle2 size={14} className="text-emerald-500" /> : <Copy size={14} className="text-slate-400" />}
              </button>
            </div>
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-slate-500">SSE 端点（GET，可用于 EventSource）</label>
            <div className="flex items-center gap-2">
              <code className="flex-1 truncate rounded-lg bg-slate-50 border border-slate-200 px-3 py-2 text-xs font-mono text-slate-800">{sseEndpoint}</code>
              <button
                onClick={() => handleCopy(sseEndpoint)}
                className="rounded-lg border border-slate-200 p-2 hover:bg-slate-50"
              >
                {copied ? <CheckCircle2 size={14} className="text-emerald-500" /> : <Copy size={14} className="text-slate-400" />}
              </button>
            </div>
          </div>
          <div className="rounded-lg bg-amber-50 border border-amber-100 px-3 py-2 text-xs text-amber-700">
            RAG 查询使用在“密钥”页授权的 API Key，通过 HTTP POST /mcp 接入。管理工具、Wiki 和旧版 SSE 使用 WALIAPI_MCP_TOKEN；MCP 开关不代表已经授权。
          </div>
        </div>
      </div>

      {/* 调用示例：左 curl 示例 / 右在线调用 + 调用记录 */}
      <div className="surface data-card rounded-2xl lg:col-span-2">
        <div className="mb-4 flex items-center gap-2">
          <Terminal size={18} className="text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">调用示例</h3>
        </div>
        <div className="grid gap-5 lg:grid-cols-2">
          {/* 左：curl 示例 */}
          <div>
            <div className="mb-1.5 flex items-center justify-between">
              <span className="text-xs font-medium text-slate-500">curl 示例（tools/list）</span>
              <button
                onClick={() => handleCopy(curlExample)}
                className="flex items-center gap-1 text-[11px] text-slate-400 hover:text-slate-600"
              >
                {copied ? <CheckCircle2 size={12} className="text-emerald-500" /> : <Copy size={12} />}
                复制
              </button>
            </div>
            <pre className="scroll-thin max-h-72 overflow-auto rounded-xl border border-slate-200 bg-slate-50 p-3.5 font-mono text-[11px] leading-relaxed text-slate-800">
              <code>{curlExample}</code>
            </pre>
            <p className="mt-2 text-[11px] leading-relaxed text-slate-400">
              RAG 查询用「密钥」页签发的 API Key；管理工具与 Wiki 用 MCP Token。在右侧下拉选择密钥即可在本页直接试调。
            </p>
          </div>

          {/* 右：在线调用 */}
          <div className="flex flex-col">
            <span className="mb-1.5 text-xs font-medium text-slate-500">在线调用（真实请求 /mcp）</span>
            <div className="flex items-center gap-2">
              <Select
                size="md"
                className="flex-1"
                ariaLabel="选择密钥"
                placeholder="选择 API Key"
                value={authChoice}
                onChange={handleAuthChoice}
                options={authOptions}
              />
              <button
                className="action-primary flex shrink-0 items-center gap-1.5 text-xs disabled:opacity-40"
                disabled={calling || authLoading || !mcpAuth.trim()}
                onClick={() => void runExample()}
              >
                {calling ? <Loader2 size={13} className="animate-spin" /> : <Play size={13} />}
                {calling ? "调用中..." : "调用 tools/list"}
              </button>
            </div>
            {authChoice === "manual" && (
              <div className="relative mt-2">
                <KeyRound size={13} className="absolute left-2.5 top-1/2 -translate-y-1/2 text-slate-400" />
                <input
                  type="password"
                  autoComplete="off"
                  placeholder="API Key 或 MCP Token（Bearer）"
                  className="w-full rounded-lg border border-slate-200 bg-white py-2 pl-8 pr-2 text-xs text-slate-800 outline-none focus:border-blue-400"
                  value={mcpAuth}
                  onChange={(e) => {
                    setMcpAuth(e.target.value);
                    localStorage.setItem("waliapi-mcp-auth", e.target.value);
                  }}
                />
              </div>
            )}

            {/* 最近一次响应 */}
            <div className="mt-3">
              <div className="mb-1.5 flex items-center gap-2 text-[11px] text-slate-400">
                最近一次响应
                {lastResp && (
                  <span className="flex items-center gap-1">
                    <span className={`h-1.5 w-1.5 rounded-full ${lastResp.ok ? "bg-emerald-500" : "bg-rose-500"}`} />
                    <span className={lastResp.ok ? "text-emerald-600" : "text-rose-500"}>
                      {lastResp.status === "err" ? "网络错误" : `HTTP ${lastResp.status}`}
                    </span>
                    <span>· {lastResp.ms}ms · {lastResp.time}</span>
                  </span>
                )}
              </div>
              {lastResp ? (
                <pre className="scroll-thin max-h-72 overflow-auto rounded-xl border border-slate-200 bg-slate-50 p-3.5 font-mono text-[11px] leading-relaxed">
                  <code>
                    {lastResp.response.startsWith("{") || lastResp.response.startsWith("[") ? (
                      <HighlightedJson text={lastResp.response} />
                    ) : (
                      lastResp.response
                    )}
                  </code>
                </pre>
              ) : (
                <div className="flex flex-col items-center justify-center gap-1.5 rounded-xl border border-dashed border-slate-200 bg-slate-50/50 px-4 py-8 text-center">
                  <Play size={16} className="text-slate-300" />
                  <span className="text-[11px] text-slate-400">
                    选择密钥后点击「调用 tools/list」，这里会显示真实响应
                  </span>
                </div>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* 可用工具（表格列表） */}
      <div className="surface data-card rounded-2xl lg:col-span-2">
        <div className="mb-4 flex items-center gap-2">
          <Terminal size={18} className="text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">可用工具 ({tools.length})</h3>
        </div>
        <div className="overflow-x-auto rounded-xl border border-slate-100">
          <table className="w-full text-left text-xs">
            <thead className="bg-slate-50 text-slate-500">
              <tr>
                <th className="px-3 py-2 font-medium">工具</th>
                <th className="px-3 py-2 font-medium">MCP 名称</th>
                <th className="px-3 py-2 font-medium">说明</th>
              </tr>
            </thead>
            <tbody>
              {tools.map((tool) => {
                const Icon = TOOL_ICONS[tool.name] || Terminal;
                return (
                  <tr key={tool.name} className="border-t border-slate-100 transition-colors hover:bg-slate-50">
                    <td className="whitespace-nowrap px-3 py-2">
                      <div className="flex items-center gap-2">
                        <span className="rounded-lg bg-slate-100 p-1.5 text-slate-600">
                          <Icon size={13} />
                        </span>
                        <span className="font-semibold text-slate-800">{tool.label}</span>
                      </div>
                    </td>
                    <td className="whitespace-nowrap px-3 py-2">
                      <code className="font-mono text-[11px] text-slate-400">{tool.name}</code>
                    </td>
                    <td className="px-3 py-2 leading-relaxed text-slate-500">{tool.desc}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}

// ─── Skills Section ─────────────────────────────────────────────────────

function SkillsSection() {
  const [serverUrl, setServerUrl] = useState("http://127.0.0.1:8777");

  useEffect(() => {
    serverApi.getStatus().then(s => {
      if (s.running) setServerUrl(`http://127.0.0.1:${s.port}`);
    }).catch(() => {});
  }, []);

  const mcpEndpoint = `${serverUrl}/mcp`;

  return (
    <div className="space-y-4">
      {/* 头部介绍 */}
      <div className="surface data-card rounded-2xl">
        <div className="flex items-start gap-4">
          <div className="flex-shrink-0 rounded-2xl bg-gradient-to-br from-blue-500 to-indigo-600 p-3 text-white shadow-lg shadow-blue-500/20">
            <Puzzle size={24} />
          </div>
          <div className="flex-1">
            <div className="flex items-center gap-3">
              <h3 className="text-base font-semibold text-slate-900">WaLiAPI Skills</h3>
              <span className="rounded-full bg-emerald-50 px-2 py-0.5 text-[11px] font-medium text-emerald-600 border border-emerald-100">v2.0.0</span>
            </div>
            <p className="mt-1 text-sm leading-relaxed text-slate-600">
              即装即用的 Agent Skill 技能包，通过 MCP 协议连接 WaLiAPI 本地知识服务。安装后 AI Agent 可直接执行 RAG 语义搜索、RAG 问答、文档管理、Wiki 搜索与问答、知识图谱等操作，无需手写提示词。
            </p>
            <div className="mt-3 flex flex-wrap items-center gap-2">
              <a
                href="https://github.com/fuzhengwei/waliapi-skills"
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-1.5 rounded-lg bg-slate-800 px-4 py-2 text-xs font-medium text-white transition-all hover:bg-slate-700"
              >
                <Code size={13} />
                GitHub 仓库
                <ExternalLink size={11} className="text-slate-300" />
              </a>
              <a
                href="https://github.com/fuzhengwei/waliapi-skills#readme"
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-1.5 rounded-lg border border-slate-200 px-4 py-2 text-xs font-medium text-slate-600 transition-all hover:border-slate-300 hover:bg-slate-50"
              >
                <FileText size={13} />
                使用文档
                <ExternalLink size={11} className="text-slate-400" />
              </a>
            </div>
          </div>
        </div>
      </div>

      {/* 安装步骤 + 使用方式 */}
      <div className="grid gap-4 lg:grid-cols-2">
        {/* 安装步骤 */}
        <div className="surface data-card rounded-2xl">
          <div className="mb-4 flex items-center gap-2">
            <Rocket size={18} className="text-slate-700" />
            <h3 className="text-sm font-semibold text-slate-900">安装步骤</h3>
          </div>
          <div className="space-y-3">
            <div className="flex items-start gap-3 rounded-lg border border-slate-100 bg-slate-50 px-4 py-4">
              <span className="flex h-5 w-5 flex-shrink-0 items-center justify-center rounded-full bg-slate-800 text-[10px] font-bold text-white">1</span>
              <div className="min-w-0 flex-1">
                <p className="text-xs font-medium text-slate-700">下载技能包</p>
                <p className="mt-1 text-[11px] leading-relaxed text-slate-500">从 GitHub 仓库克隆技能包到本地。克隆完成后，技能包会自动安装到 <code className="rounded bg-slate-200 px-1 py-0.5 text-[10px] font-mono text-slate-700">waliapi-skills</code> 目录。</p>
                <pre className="mt-2 overflow-x-auto rounded-lg border border-slate-200 bg-white px-2.5 py-1.5 text-[11px] font-mono text-slate-800">git clone https://github.com/fuzhengwei/waliapi-skills.git</pre>
                <p className="mt-2 text-[11px] leading-relaxed text-slate-500">安装完成后，在 Agent 客户端（如 WaLiCode、Codex、Claude Code）中即可直接使用以下能力：</p>
                <div className="mt-1.5 space-y-1">
                  <p className="text-[11px] text-slate-600">🔍 <span className="font-medium text-slate-700">RAG 语义搜索</span> — 向量+关键词混合检索，支持 hybrid/vector/keyword 三种模式</p>
                  <p className="text-[11px] text-slate-600">💬 <span className="font-medium text-slate-700">RAG 问答</span> — 基于知识库内容生成回答，附带来源引用</p>
                  <p className="text-[11px] text-slate-600">📖 <span className="font-medium text-slate-700">Wiki 搜索与问答</span> — 结构化知识页面搜索、标签导航、Wiki Q&A</p>
                  <p className="text-[11px] text-slate-600">🗺️ <span className="font-medium text-slate-700">知识图谱</span> — 页面关联可视化、wikilinks 网络</p>
                  <p className="text-[11px] text-slate-600">📁 <span className="font-medium text-slate-700">文档管理</span> — 上传、删除、列举知识库中的文档</p>
                  <p className="text-[11px] text-slate-600">📦 <span className="font-medium text-slate-700">批量导入</span> — 导入 Git 仓库、URL 或本地目录到知识库</p>
                </div>
              </div>
            </div>
            <div className="flex items-start gap-3 rounded-lg border border-slate-100 bg-slate-50 px-4 py-4">
              <span className="flex h-5 w-5 flex-shrink-0 items-center justify-center rounded-full bg-slate-800 text-[10px] font-bold text-white">2</span>
              <div className="min-w-0 flex-1">
                <p className="text-xs font-medium text-slate-700">重启 Agent 客户端</p>
                <p className="mt-1 text-[11px] leading-relaxed text-slate-500">重启 WaLiCode / Codex / Claude Code 等 Agent 客户端，技能会在启动时自动加载。首次使用时 AI 会自动询问 MCP 地址（<code className="rounded bg-slate-200 px-1 py-0.5 text-[10px] font-mono text-slate-700">${mcpEndpoint}</code>），无需手动配置。</p>
              </div>
            </div>
          </div>
        </div>

        {/* 使用方式 */}
        <div className="surface data-card rounded-2xl">
          <div className="mb-4 flex items-center gap-2">
            <Terminal size={18} className="text-slate-700" />
            <h3 className="text-sm font-semibold text-slate-900">使用方式</h3>
          </div>
          <div className="space-y-2.5">
            <div className="rounded-lg border border-slate-100 bg-gradient-to-br from-white to-slate-50 px-3 py-2.5">
              <div className="flex items-center gap-2">
                <Search size={14} className="text-blue-500" />
                <p className="text-xs font-semibold text-slate-800">语义搜索</p>
              </div>
              <p className="mt-1 text-[11px] text-slate-500">「搜索 RAG 中关于渠道配置的内容」</p>
              <p className="mt-0.5 text-[11px] text-slate-400">调用 search_knowledge_base，支持 hybrid/vector/keyword 三种模式</p>
            </div>
            <div className="rounded-lg border border-slate-100 bg-gradient-to-br from-white to-slate-50 px-3 py-2.5">
              <div className="flex items-center gap-2">
                <MessageCircle size={14} className="text-emerald-500" />
                <p className="text-xs font-semibold text-slate-800">RAG 问答</p>
              </div>
              <p className="mt-1 text-[11px] text-slate-500">「问一下 RAG，WaLiAPI 支持哪些协议？」</p>
              <p className="mt-0.5 text-[11px] text-slate-400">调用 ask_knowledge_base，检索 + LLM 生成回答 + 来源引用</p>
            </div>
            <div className="rounded-lg border border-slate-100 bg-gradient-to-br from-white to-slate-50 px-3 py-2.5">
              <div className="flex items-center gap-2">
                <Upload size={14} className="text-amber-500" />
                <p className="text-xs font-semibold text-slate-800">文档管理</p>
              </div>
              <p className="mt-1 text-[11px] text-slate-500">「把这份 PDF 上传到 RAG」</p>
              <p className="mt-0.5 text-[11px] text-slate-400">调用 upload_document，自动解析 → 分块 → 向量化 → 索引</p>
            </div>
            <div className="rounded-lg border border-slate-100 bg-gradient-to-br from-white to-slate-50 px-3 py-2.5">
              <div className="flex items-center gap-2">
                <GitBranch size={14} className="text-purple-500" />
                <p className="text-xs font-semibold text-slate-800">批量导入</p>
              </div>
              <p className="mt-1 text-[11px] text-slate-500">「把这个 Git 仓库导入 RAG」</p>
              <p className="mt-0.5 text-[11px] text-slate-400">调用 import_source，支持 Git 仓库 / URL / 本地目录</p>
            </div>
          </div>
        </div>
      </div>

      {/* 技术细节 + 工具覆盖 */}
      <div className="grid gap-4 lg:grid-cols-2">
        {/* 技术细节 */}
        <div className="surface data-card rounded-2xl">
          <div className="mb-4 flex items-center gap-2">
            <Package size={18} className="text-slate-700" />
            <h3 className="text-sm font-semibold text-slate-900">技术细节</h3>
          </div>
          <div className="space-y-2.5">
            <div className="flex items-center justify-between rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
              <span className="text-xs font-medium text-slate-500">通信协议</span>
              <span className="text-xs text-slate-800">MCP JSON-RPC (SSE + POST)</span>
            </div>
            <div className="flex items-center justify-between rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
              <span className="text-xs font-medium text-slate-500">运行依赖</span>
              <span className="text-xs text-slate-800">Python 3.8+（零第三方依赖）</span>
            </div>
            <div className="flex items-center justify-between rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
              <span className="text-xs font-medium text-slate-500">兼容客户端</span>
              <span className="text-xs text-slate-800">WaLiCode · Codex · Claude Code</span>
            </div>
            <div className="flex items-center justify-between rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
              <span className="text-xs font-medium text-slate-500">MCP 工具数</span>
              <span className="text-xs text-slate-800">13 个（5 只读 + 8 写入）</span>
            </div>
            <div className="flex items-center justify-between rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
              <span className="text-xs font-medium text-slate-500">许可证</span>
              <span className="text-xs text-slate-800">MIT</span>
            </div>
          </div>
        </div>

        {/* MCP 工具覆盖 */}
        <div className="surface data-card rounded-2xl">
          <div className="mb-4 flex items-center gap-2">
            <Terminal size={18} className="text-slate-700" />
            <h3 className="text-sm font-semibold text-slate-900">MCP 工具覆盖（13 个）</h3>
          </div>
          <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-2">
            {[
              { name: "search_knowledge_base", label: "语义搜索", icon: Search, color: "text-blue-500" },
              { name: "ask_knowledge_base", label: "RAG 问答", icon: MessageCircle, color: "text-emerald-500" },
              { name: "list_knowledge_bases", label: "列出 RAG", icon: BookOpen, color: "text-slate-500" },
              { name: "read_document", label: "读取文档", icon: FileText, color: "text-slate-500" },
              { name: "get_knowledge_base_stats", label: "RAG 统计", icon: Database, color: "text-slate-500" },
              { name: "create_knowledge_base", label: "创建 RAG", icon: Plus, color: "text-indigo-500" },
              { name: "update_knowledge_base", label: "更新 RAG", icon: SettingsIcon, color: "text-indigo-500" },
              { name: "delete_knowledge_base", label: "删除 RAG", icon: Trash2, color: "text-red-400" },
              { name: "upload_document", label: "上传文档", icon: Upload, color: "text-amber-500" },
              { name: "delete_document", label: "删除文档", icon: Trash2, color: "text-red-400" },
              { name: "list_documents", label: "文档列表", icon: Layers, color: "text-slate-500" },
              { name: "build_index", label: "构建索引", icon: Sparkles, color: "text-purple-500" },
              { name: "import_source", label: "导入源", icon: GitBranch, color: "text-purple-500" },
            ].map((tool) => {
              const Icon = tool.icon;
              return (
                <div key={tool.name} className="flex items-center gap-2 rounded-lg border border-slate-100 bg-white px-2.5 py-1.5">
                  <Icon size={12} className={tool.color} />
                  <span className="text-[11px] font-medium text-slate-700">{tool.label}</span>
                  <code className="ml-auto truncate text-[10px] text-slate-400">{tool.name}</code>
                </div>
              );
            })}
          </div>
        </div>
      </div>

      {/* 快速验证 */}
      <div className="surface data-card rounded-2xl">
        <div className="mb-4 flex items-center gap-2">
          <Terminal size={18} className="text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">快速验证</h3>
        </div>
        <p className="mb-3 text-xs text-slate-500">安装完成后，在 Agent 客户端中发送以下消息验证技能是否生效：</p>
        <div className="space-y-2">
          <div className="rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
            <p className="text-[11px] font-medium text-slate-400">验证连接</p>
            <code className="text-xs text-slate-700">列出所有 RAG</code>
          </div>
          <div className="rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
            <p className="text-[11px] font-medium text-slate-400">验证搜索</p>
            <code className="text-xs text-slate-700">搜索 RAG 中关于配置的内容</code>
          </div>
          <div className="rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
            <p className="text-[11px] font-medium text-slate-400">验证 RAG 问答</p>
            <code className="text-xs text-slate-700">问一下 RAG，WaLiAPI 支持哪些协议？</code>
          </div>
        </div>
      </div>
    </div>
  );
}

// ─── Knowledge Base Section ──────────────────────────────────────────────

function KnowledgeBaseSection() {
  const [kbs, setKbs] = useState<KnowledgeBase[]>([]);
  const [selectedKbId, setSelectedKbId] = useState<string | null>(null);
  const [kbTab, setKbTab] = useState<KbTab>("documents");
  const [loading, setLoading] = useState(false);
  const [showCreate, setShowCreate] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [kbDeleteTarget, setKbDeleteTarget] = useState<{ message: string; run: () => void } | null>(null);
  const selectedKb = useMemo(
    () => selectedKbId ? kbs.find((kb) => kb.id === selectedKbId) ?? null : null,
    [kbs, selectedKbId],
  );

  // 本地变更计数（NEW-2 竞态守卫）：在途的列表请求返回后若期间发生过本地
  // 原地更新（保存设置等），旧的服务器快照不得整体覆盖新状态；跳过本次
  // 设置，下次 fetch 自愈。与 LogsPage 的请求序号守卫同族但语义不同：
  // 这里防的是「旧响应覆盖本地新写入」，不是「旧响应覆盖新响应」。
  const kbMutations = useRef(0);

  const fetchKbs = useCallback(async () => {
    setLoading(true);
    const mutationsAtStart = kbMutations.current;
    try {
      const data = await kbApi.getAll();
      if (kbMutations.current === mutationsAtStart) setKbs(data);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchKbs();
  }, [fetchKbs]);

  const handleSelectKb = (kb: KnowledgeBase) => {
    setSelectedKbId(kb.id);
    setKbTab("documents");
  };

  const handleKbUpdated = useCallback((updated: KnowledgeBase) => {
    kbMutations.current += 1;
    setKbs((current) => current.map((kb) => kb.id === updated.id ? updated : kb));
  }, []);

  const handleDelete = async (id: string) => {
    setKbDeleteTarget({
      message: "确定删除此 RAG 知识库？所有文档和切片将一并删除。",
      run: async () => {
        try {
          await kbApi.delete(id);
          await fetchKbs();
          if (selectedKbId === id) setSelectedKbId(null);
        } catch (e) {
          setError(String(e));
        }
      },
    });
  };

  // Toggle KB status (enable/disable) from list view
  const handleToggleStatus = async (kb: KnowledgeBase, newStatus: number) => {
    try {
      await kbApi.update(kb.id, { status: newStatus });
      await fetchKbs();
    } catch (e) {
      setError(String(e));
    }
  };

  // Toggle MCP exposure from list view
  const handleToggleMcp = async (kb: KnowledgeBase, newMcp: number) => {
    try {
      await kbApi.update(kb.id, { mcp_enabled: newMcp });
      await fetchKbs();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <>
      {error && (
        <div className="rounded-xl border border-red-200 bg-red-50 p-4 text-sm text-red-600">
          {error}
          <button onClick={() => setError(null)} className="ml-2 text-red-400 hover:text-red-600">✕</button>
        </div>
      )}

      {selectedKb ? (
        <KbDetail
          kb={selectedKb}
          tab={kbTab}
          setTab={setKbTab}
          onBack={() => { setSelectedKbId(null); setKbTab("documents"); }}
          onRefresh={fetchKbs}
          onUpdated={handleKbUpdated}
        />
      ) : (
        <KbList
          kbs={kbs}
          loading={loading}
          onSelect={handleSelectKb}
          onDelete={handleDelete}
          onCreate={() => setShowCreate(true)}
          onToggleStatus={handleToggleStatus}
          onToggleMcp={handleToggleMcp}
        />
      )}

      {showCreate && (
        <CreateKbModal
          onClose={() => setShowCreate(false)}
          onCreated={async () => {
            setShowCreate(false);
            await fetchKbs();
          }}
        />
      )}

      {kbDeleteTarget && (
        <ConfirmDialog
          message={kbDeleteTarget.message}
          onCancel={() => setKbDeleteTarget(null)}
          onConfirm={() => { kbDeleteTarget.run(); setKbDeleteTarget(null); }}
        />
      )}
    </>
  );
}

// ─── KB Tags Bar (high-frequency words) ─────────────────────────────

function KbTagsBar({ kbId, chunkCount }: { kbId: string; chunkCount: number }) {
  const [tags, setTags] = useState<KbTag[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (chunkCount === 0) return;
    let active = true;
    setLoading(true);
    kbApi.getTags(kbId, 12)
      .then((data) => { if (active) setTags(data); })
      .catch(() => {})
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [kbId, chunkCount]);

  if (loading && tags.length === 0) {
    return (
      <div className="mt-2.5 flex items-center gap-1.5">
        {[...Array(5)].map((_, i) => (
          <div key={i} className="h-5 w-12 animate-pulse rounded-full bg-slate-100" />
        ))}
      </div>
    );
  }

  if (tags.length === 0) return null;

  // Color palette for tags - gradient blues/purples for visual appeal
  const tagColors = [
    "bg-blue-50 text-blue-600 border-blue-100",
    "bg-violet-50 text-violet-600 border-violet-100",
    "bg-emerald-50 text-emerald-600 border-emerald-100",
    "bg-amber-50 text-amber-600 border-amber-100",
    "bg-rose-50 text-rose-500 border-rose-100",
    "bg-cyan-50 text-cyan-600 border-cyan-100",
    "bg-indigo-50 text-indigo-600 border-indigo-100",
    "bg-teal-50 text-teal-600 border-teal-100",
  ];

  return (
    <div className="mt-2.5 flex flex-wrap items-center gap-1.5">
      <Tag size={11} className="text-slate-400 shrink-0" />
      {tags.map((tag, i) => (
        <span
          key={tag.word}
          className={`inline-flex items-center rounded-full border px-2 py-0.5 text-[10px] font-medium ${tagColors[i % tagColors.length]}`}
        >
          {tag.word}
        </span>
      ))}
    </div>
  );
}

// ─── KB List ────────────────────────────────────────────────────────────

function KbList({
  kbs,
  loading,
  onSelect,
  onDelete,
  onCreate,
  onToggleStatus,
  onToggleMcp,
}: {
  kbs: KnowledgeBase[];
  loading: boolean;
  onSelect: (kb: KnowledgeBase) => void;
  onDelete: (id: string) => void;
  onCreate: () => void;
  onToggleStatus: (kb: KnowledgeBase, newStatus: number) => void;
  onToggleMcp: (kb: KnowledgeBase, newMcp: number) => void;
}) {
  if (loading && kbs.length === 0) {
    return (
      <div className="surface empty-state">
        <Loader2 className="h-8 w-8 animate-spin text-slate-400" />
      </div>
    );
  }

  if (kbs.length === 0) {
    return (
      <div className="surface empty-state">
        <BookOpen className="h-12 w-12 text-slate-300" />
        <p className="text-sm text-slate-500">还没有 RAG</p>
        <button onClick={onCreate} className="action-primary mt-2">
          <Plus size={16} />
          新建 RAG
        </button>
      </div>
    );
  }

  return (
    <>
      <div className="flex justify-end mb-4">
        <button onClick={onCreate} className="action-primary">
          <Plus size={16} />
          新建 RAG
        </button>
      </div>
      <div className="space-y-3">
        {kbs.map((kb) => (
          <div
            key={kb.id}
            className="surface group rounded-2xl p-5 transition-all hover:shadow-[0_8px_24px_rgba(15,23,42,0.06)] border border-slate-100"
          >
            <div className="flex items-start gap-4">
              {/* Icon */}
              <div className={`flex h-10 w-10 shrink-0 items-center justify-center rounded-xl ${kb.status === 1 ? "bg-blue-50" : "bg-slate-100"}`}>
                <BookOpen className={`h-5 w-5 ${kb.status === 1 ? "text-blue-600" : "text-slate-400"}`} />
              </div>

              {/* Main content - clickable */}
              <div
                className="min-w-0 flex-1 cursor-pointer"
                onClick={() => onSelect(kb)}
              >
                <div className="flex items-center gap-2">
                  <h3 className="text-base font-semibold text-slate-900">{kb.name}</h3>
                  {kb.status === 1 ? (
                    <span className="rounded-full bg-emerald-50 px-2 py-0.5 text-[10px] font-medium text-emerald-600">活跃</span>
                  ) : (
                    <span className="rounded-full bg-slate-100 px-2 py-0.5 text-[10px] font-medium text-slate-500">已禁用</span>
                  )}
                </div>
                <p className="mt-0.5 text-xs text-slate-500 line-clamp-1">
                  {kb.description || "暂无描述"}
                </p>
                <div className="mt-2 flex items-center gap-4 text-xs text-slate-500">
                  <span className="flex items-center gap-1">
                    <FileText size={12} /> {kb.doc_count} 文档
                  </span>
                  <span className="flex items-center gap-1">
                    <Hash size={12} /> {kb.chunk_count} 切片
                  </span>
                  {kb.embedding_model && (
                    <span className="truncate" title={kb.embedding_model}>
                      {kb.embedding_model}
                    </span>
                  )}
                </div>
                {/* Tags */}
                <KbTagsBar kbId={kb.id} chunkCount={kb.chunk_count} />
              </div>

              {/* Right side: toggles + actions */}
              <div className="flex flex-col items-end gap-2 shrink-0">
                <div className="flex items-center gap-3">
                  {/* MCP toggle */}
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      onToggleMcp(kb, kb.mcp_enabled === 1 ? 0 : 1);
                    }}
                    className={`flex items-center gap-1.5 rounded-lg px-2 py-1 text-[10px] font-medium transition-colors ${
                      kb.mcp_enabled === 1
                        ? "bg-violet-50 text-violet-600 hover:bg-violet-100"
                        : "bg-slate-100 text-slate-400 hover:bg-slate-200"
                    }`}
                    title="MCP 暴露开关"
                  >
                    <Terminal size={11} />
                    MCP {kb.mcp_enabled === 1 ? "已暴露" : "未暴露"}
                  </button>

                  {/* Status toggle */}
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      onToggleStatus(kb, kb.status === 1 ? 0 : 1);
                    }}
                    className={`relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors ${
                      kb.status === 1 ? "bg-emerald-500" : "bg-slate-300"
                    }`}
                    title="RAG 开关"
                  >
                    <span
                      className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white shadow transition-transform ${
                        kb.status === 1 ? "translate-x-4" : "translate-x-1"
                      }`}
                    />
                  </button>

                  {/* Delete */}
                  <button
                    onClick={(e) => { e.stopPropagation(); onDelete(kb.id); }}
                    className="rounded-lg p-1.5 text-slate-400 opacity-0 transition-opacity group-hover:opacity-100 hover:bg-red-50 hover:text-red-500"
                  >
                    <Trash2 className="h-4 w-4" />
                  </button>
                </div>
                <ChevronRight size={16} className="text-slate-300 group-hover:text-blue-500" />
              </div>
            </div>
          </div>
        ))}
      </div>
    </>
  );
}

// ─── KB Detail ───────────────────────────────────────────────────────────

function KbDetail({
  kb,
  tab,
  setTab,
  onBack,
  onRefresh,
  onUpdated,
}: {
  kb: KnowledgeBase;
  tab: KbTab;
  setTab: (t: KbTab) => void;
  onBack: () => void;
  onRefresh: () => void;
  onUpdated: (kb: KnowledgeBase) => void;
}) {
  const tabs: { key: KbTab; label: string; icon: typeof FileText }[] = [
    { key: "documents", label: "文档", icon: FileText },
    { key: "sources", label: "来源", icon: GitBranch },
    { key: "search", label: "检索", icon: Search },
    { key: "ask", label: "问答", icon: MessageCircle },
    { key: "index", label: "索引", icon: Database },
    { key: "settings", label: "设置", icon: SettingsIcon },
    { key: "mcp", label: "MCP", icon: Terminal },
  ];

  return (
    <div className="space-y-4">
      {/* 顶部操作行 + 子菜单：滚动时钉在 page-header（sticky top-0，约 88px 高）下方 */}
      <div className="sticky top-[var(--wali-page-header-h,88px)] z-20 -mx-1 space-y-4 rounded-2xl border border-slate-100 bg-white/95 px-4 py-3 backdrop-blur">
        <div className="flex items-center gap-3">
          <button
            onClick={onBack}
            className="flex items-center gap-1 rounded-lg px-3 py-1.5 text-sm text-slate-500 hover:bg-slate-100"
          >
            ← 返回
          </button>
          <div className="h-4 w-px bg-slate-200" />
          <h2 className="text-lg font-semibold text-slate-900">{kb.name}</h2>
          <span className="rounded-full bg-emerald-50 px-2 py-0.5 text-[10px] font-medium text-emerald-600">
            {kb.doc_count} 文档 · {kb.chunk_count} 切片
          </span>
        </div>

        {/* Tabs */}
        <div className="flex gap-1 border-b border-slate-200">
          {tabs.map(({ key, label, icon: Icon }) => (
            <button
              key={key}
              onClick={() => setTab(key)}
              className={`flex items-center gap-2 border-b-2 px-4 py-2.5 text-sm transition-colors ${
                tab === key
                  ? "border-blue-600 text-blue-600"
                  : "border-transparent text-slate-500 hover:text-slate-700"
              }`}
            >
              <Icon size={15} />
              {label}
            </button>
          ))}
        </div>
      </div>

      {tab === "documents" && <DocumentsTab kb={kb} onRefresh={onRefresh} />}
      {tab === "sources" && <SourcesTab kb={kb} onRefresh={onRefresh} />}
      {tab === "search" && <SearchTab kb={kb} />}
      {tab === "ask" && <AskTab kb={kb} />}
      {tab === "index" && <IndexTab kb={kb} />}
      {tab === "settings" && <SettingsTab kb={kb} onUpdated={onUpdated} />}
      {tab === "mcp" && <McpTab kb={kb} />}
    </div>
  );
}

// ─── Sources Tab (Multi-source import) ────────────────────────────────

function SourcesTab({ kb, onRefresh }: { kb: KnowledgeBase; onRefresh: () => void }) {
  const [sources, setSources] = useState<KbSource[]>([]);
  const [loading, setLoading] = useState(false);
  const [showImport, setShowImport] = useState(false);
  const [progressMap, setProgressMap] = useState<Record<string, { progress: number; detail: string }>>({});
  const [sourceDeleteTarget, setSourceDeleteTarget] = useState<{ message: string; run: () => void } | null>(null);

  const fetchSources = useCallback(async () => {
    setLoading(true);
    try {
      const data = await kbApi.getSources(kb.id);
      setSources(data);
    } catch (e) {
      console.error(e);
    } finally {
      setLoading(false);
    }
  }, [kb.id]);

  useEffect(() => {
    fetchSources();
    const interval = setInterval(fetchSources, 3000);
    return () => clearInterval(interval);
  }, [fetchSources]);

  // Listen for import progress
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      unlisten = await listen<{ kb_id: string; source_id: string; progress: number; detail: string }>(
        "kb-import-progress",
        (event) => {
          if (!active) return;
          const p = event.payload;
          if (p.kb_id !== kb.id) return;
          if (p.progress >= 100) {
            setProgressMap((prev) => {
              const next = { ...prev };
              delete next[p.source_id];
              return next;
            });
            fetchSources();
            onRefresh();
          } else {
            setProgressMap((prev) => ({
              ...prev,
              [p.source_id]: { progress: p.progress, detail: p.detail },
            }));
          }
        }
      );
    })();
    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, [kb.id, fetchSources, onRefresh]);

  const handleDelete = async (sourceId: string) => {
    setSourceDeleteTarget({
      message: "删除此来源？关联的文档将保留但不再标记来源。",
      run: async () => {
        try {
          await kbApi.deleteSource(sourceId, kb.id);
          await fetchSources();
          onRefresh();
        } catch (e) {
          alert(`删除失败: ${e}`);
        }
      },
    });
  };

  return (
    <div className="space-y-4">
      <div className="flex justify-end">
        <button onClick={() => setShowImport(true)} className="action-primary">
          <Plus size={16} />
          导入来源
        </button>
      </div>

      {loading && sources.length === 0 ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="h-6 w-6 animate-spin text-slate-400" />
        </div>
      ) : sources.length === 0 ? (
        <div className="surface empty-state rounded-2xl">
          <GitBranch className="h-8 w-8 text-slate-300" />
          <p className="text-sm text-slate-500">暂无导入来源</p>
          <p className="text-xs text-slate-400 mt-1">从 Git 仓库、URL 或本地目录导入文档</p>
        </div>
      ) : (
        <div className="space-y-2">
          {sources.map((src) => {
            const prog = progressMap[src.id];
            return (
              <div key={src.id} className="surface flex items-center gap-3 rounded-xl px-4 py-3">
                <SourceIcon type={src.source_type} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate text-sm font-medium text-slate-900">
                      {src.source_url || src.source_path || src.source_type}
                    </span>
                    {src.branch && src.source_type === "git" && (
                      <span className="rounded bg-slate-100 px-1.5 py-0.5 text-[10px] text-slate-500">
                        {src.branch}
                      </span>
                    )}
                  </div>
                  {prog ? (
                    <div className="mt-1.5">
                      <div className="flex items-center gap-2">
                        <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-slate-200">
                          <div
                            className="h-full rounded-full bg-blue-500 transition-all duration-300"
                            style={{ width: `${prog.progress}%` }}
                          />
                        </div>
                        <span className="shrink-0 text-[11px] text-blue-600">
                          {prog.detail} · {prog.progress}%
                        </span>
                      </div>
                    </div>
                  ) : (
                    <div className="mt-1 flex items-center gap-3 text-xs text-slate-500">
                      <SourceStatusBadge status={src.status} />
                      {src.file_count > 0 && <span>{src.file_count} 文件</span>}
                      {src.error && (
                        <span className="text-red-500 truncate" title={src.error}>
                          {src.error.slice(0, 60)}
                        </span>
                      )}
                    </div>
                  )}
                </div>
                <button
                  onClick={() => handleDelete(src.id)}
                  className="rounded-lg p-1.5 text-slate-400 hover:bg-red-50 hover:text-red-500"
                  title="删除"
                >
                  <Trash2 size={15} />
                </button>
              </div>
            );
          })}
        </div>
      )}

      {showImport && (
        <ImportSourceModal
          kbId={kb.id}
          onClose={() => setShowImport(false)}
          onImported={async () => {
            setShowImport(false);
            await fetchSources();
            onRefresh();
          }}
        />
      )}

      {sourceDeleteTarget && (
        <ConfirmDialog
          message={sourceDeleteTarget.message}
          onCancel={() => setSourceDeleteTarget(null)}
          onConfirm={() => { sourceDeleteTarget.run(); setSourceDeleteTarget(null); }}
        />
      )}
    </div>
  );
}

function SourceIcon({ type }: { type: string }) {
  const cls = "h-5 w-5 shrink-0 text-slate-400";
  switch (type) {
    case "git":
      return <GitBranch className={cls} />;
    case "url":
      return <Link className={cls} />;
    case "local_dir":
      return <FolderOpen className={cls} />;
    default:
      return <FileText className={cls} />;
  }
}

function SourceStatusBadge({ status }: { status: string }) {
  switch (status) {
    case "done":
      return <span className="flex items-center gap-1 text-emerald-600"><CheckCircle2 size={12} /> 完成</span>;
    case "processing":
      return <span className="flex items-center gap-1 text-blue-600"><Loader2 size={12} className="animate-spin" /> 处理中</span>;
    case "error":
      return <span className="flex items-center gap-1 text-red-500"><XCircle size={12} /> 失败</span>;
    default:
      return <span className="flex items-center gap-1 text-slate-400"><Clock size={12} /> 等待中</span>;
  }
}

function IndexStatusBadge({ status }: { status: string }) {
  switch (status) {
    case "ready":
      return <span className="flex items-center gap-1 text-emerald-600"><CheckCircle2 size={12} /> 就绪</span>;
    case "building":
      return <span className="flex items-center gap-1 text-blue-600"><Loader2 size={12} className="animate-spin" /> 构建中</span>;
    case "error":
      return <span className="flex items-center gap-1 text-red-500"><XCircle size={12} /> 失败</span>;
    case "none":
      return <span className="flex items-center gap-1 text-slate-400"><Clock size={12} /> 未构建</span>;
    default:
      return <span className="flex items-center gap-1 text-slate-400"><Clock size={12} /> {status}</span>;
  }
}

function ImportSourceModal({
  kbId,
  onClose,
  onImported,
}: {
  kbId: string;
  onClose: () => void;
  onImported: () => void;
}) {
  const [sourceType, setSourceType] = useState<"git" | "url" | "local_dir">("git");
  const [repoUrl, setRepoUrl] = useState("");
  const [branch, setBranch] = useState("main");
  const [token, setToken] = useState("");
  const [url, setUrl] = useState("");
  const [dirPath, setDirPath] = useState("");
  const [excludedDirs, setExcludedDirs] = useState("");
  const [includedFiles, setIncludedFiles] = useState("");
  const [maxFileSize, setMaxFileSize] = useState(1);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleImport = async () => {
    setImporting(true);
    setError(null);
    try {
      const input: Record<string, unknown> = {
        source_type: sourceType,
        excluded_dirs: excludedDirs ? excludedDirs.split(",").map((s) => s.trim()) : [],
        included_files: includedFiles ? includedFiles.split(",").map((s) => s.trim()) : [],
        max_file_size: maxFileSize * 1024 * 1024,
      };

      if (sourceType === "git") {
        if (!repoUrl.trim()) { setError("请输入仓库 URL"); setImporting(false); return; }
        input.repo_url = repoUrl.trim();
        input.branch = branch.trim() || "main";
        if (token.trim()) input.token = token.trim();
      } else if (sourceType === "url") {
        if (!url.trim()) { setError("请输入 URL"); setImporting(false); return; }
        input.url = url.trim();
      } else if (sourceType === "local_dir") {
        if (!dirPath.trim()) { setError("请输入目录路径"); setImporting(false); return; }
        input.dir_path = dirPath.trim();
      }

      await kbApi.importSource(kbId, input as Parameters<typeof kbApi.importSource>[1]);
      onImported();
    } catch (e) {
      setError(String(e));
    } finally {
      setImporting(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30" onClick={onClose}>
      <div className="w-full max-w-lg rounded-2xl bg-white p-6 shadow-xl" onClick={(e) => e.stopPropagation()}>
        <h3 className="text-lg font-semibold text-slate-900">导入来源</h3>

        {/* Source type tabs */}
        <div className="mt-4 flex gap-2">
          {([
            { key: "git" as const, label: "Git 仓库", icon: GitBranch },
            { key: "url" as const, label: "URL", icon: Link },
            { key: "local_dir" as const, label: "本地目录", icon: FolderOpen },
          ]).map(({ key, label, icon: Icon }) => (
            <button
              key={key}
              onClick={() => setSourceType(key)}
              className={`flex items-center gap-2 rounded-xl px-4 py-2.5 text-sm font-medium transition-all ${
                sourceType === key
                  ? "border border-blue-100 bg-white text-slate-900 shadow-sm"
                  : "text-slate-500 hover:bg-white/70"
              }`}
            >
              <Icon size={15} />
              {label}
            </button>
          ))}
        </div>

        <div className="mt-4 space-y-4">
          {sourceType === "git" && (
            <>
              <div>
                <label className="mb-1 block text-sm font-medium text-slate-700">仓库 URL</label>
                <input
                  type="text"
                  value={repoUrl}
                  onChange={(e) => setRepoUrl(e.target.value)}
                  placeholder="https://github.com/owner/repo"
                  className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
                />
              </div>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="mb-1 block text-sm font-medium text-slate-700">分支</label>
                  <input
                    type="text"
                    value={branch}
                    onChange={(e) => setBranch(e.target.value)}
                    placeholder="main"
                    className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
                  />
                </div>
                <div>
                  <label className="mb-1 block text-sm font-medium text-slate-700">Access Token（可选）</label>
                  <input
                    type="password"
                    value={token}
                    onChange={(e) => setToken(e.target.value)}
                    placeholder="私有仓库需要"
                    className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
                  />
                </div>
              </div>
            </>
          )}

          {sourceType === "url" && (
            <div>
              <label className="mb-1 block text-sm font-medium text-slate-700">URL</label>
              <input
                type="text"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                placeholder="https://example.com/doc.md"
                className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
              />
            </div>
          )}

          {sourceType === "local_dir" && (
            <div>
              <label className="mb-1 block text-sm font-medium text-slate-700">目录路径</label>
              <div className="flex gap-2">
                <input
                  type="text"
                  value={dirPath}
                  onChange={(e) => setDirPath(e.target.value)}
                  placeholder="/path/to/project/docs"
                  className="flex-1 rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
                />
                <button
                  type="button"
                  onClick={async () => {
                    try {
                      const { open } = await import("@tauri-apps/plugin-dialog");
                      const selected = await open({
                        directory: true,
                        multiple: false,
                        title: "选择导入目录",
                      });
                      if (typeof selected === "string") {
                        setDirPath(selected);
                      }
                    } catch {
                      // 对话框取消或不可用，忽略
                    }
                  }}
                  className="flex items-center gap-1.5 rounded-xl border border-slate-200 bg-slate-50 px-3 py-2 text-sm font-medium text-slate-600 transition-colors hover:bg-slate-100 hover:text-slate-900"
                >
                  <FolderInput size={15} />
                  浏览
                </button>
              </div>
            </div>
          )}

          {/* Common filter options */}
          <div className="rounded-xl bg-slate-50 p-3 space-y-3">
            <div className="text-xs font-semibold text-slate-500">过滤选项（可选）</div>
            <div className="grid grid-cols-2 gap-3">
              <div>
                <label className="mb-1 block text-xs text-slate-600">排除目录（逗号分隔）</label>
                <input
                  type="text"
                  value={excludedDirs}
                  onChange={(e) => setExcludedDirs(e.target.value)}
                  placeholder="tests, examples, docs"
                  className="w-full rounded-lg border border-slate-200 px-2.5 py-1.5 text-xs outline-none focus:border-blue-400"
                />
              </div>
              <div>
                <label className="mb-1 block text-xs text-slate-600">包含文件类型（逗号分隔，空=全部）</label>
                <input
                  type="text"
                  value={includedFiles}
                  onChange={(e) => setIncludedFiles(e.target.value)}
                  placeholder="md, rs, ts, py"
                  className="w-full rounded-lg border border-slate-200 px-2.5 py-1.5 text-xs outline-none focus:border-blue-400"
                />
              </div>
            </div>
            <div>
              <label className="mb-1 block text-xs text-slate-600">最大文件大小 (MB)</label>
              <input
                type="number"
                value={maxFileSize}
                onChange={(e) => setMaxFileSize(Number(e.target.value) || 1)}
                min={0.1}
                step={0.1}
                className="w-24 rounded-lg border border-slate-200 px-2.5 py-1.5 text-xs outline-none focus:border-blue-400"
              />
            </div>
          </div>

          {error && (
            <div className="rounded-lg bg-red-50 p-3 text-sm text-red-600">{error}</div>
          )}
        </div>

        <div className="mt-6 flex justify-end gap-2">
          <button onClick={onClose} className="rounded-xl px-4 py-2 text-sm text-slate-500 hover:bg-slate-100">
            取消
          </button>
          <button
            onClick={handleImport}
            disabled={importing}
            className="action-primary disabled:opacity-50"
          >
            {importing ? "导入中..." : "开始导入"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── Index Tab ─────────────────────────────────────────────────────────

function IndexTab({ kb }: { kb: KnowledgeBase }) {
  const [indexMeta, setIndexMeta] = useState<KbIndexMeta | null>(null);
  const [loading, setLoading] = useState(true);
  const [building, setBuilding] = useState(false);
  const [buildMsg, setBuildMsg] = useState("");
  const [buildProgress, setBuildProgress] = useState(0);
  const [indexDeleteTarget, setIndexDeleteTarget] = useState<{ run: () => void } | null>(null);

  const fetchIndex = useCallback(async () => {
    setLoading(true);
    try {
      const data = await kbApi.getIndexStatus(kb.id);
      setIndexMeta(data);
      // Sync building state with DB status
      if (data?.status === "building") setBuilding(true);
      else setBuilding(false);
    } catch (e) {
      console.error(e);
    } finally {
      setLoading(false);
    }
  }, [kb.id]);

  useEffect(() => {
    fetchIndex();

    // Listen for real-time index build progress
    let unlisten: (() => void) | undefined;
    (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      unlisten = await listen<{ kb_id: string; status: string; message: string; progress?: number; current?: number; total?: number }>(
        "kb-index-progress",
        (event) => {
          const payload = event.payload;
          if (payload.kb_id !== kb.id) return;

          setBuildMsg(payload.message);

          if (payload.status === "ready") {
            setBuilding(false);
            setBuildProgress(100);
            setBuildMsg("");
            fetchIndex();
          } else if (payload.status === "error") {
            setBuilding(false);
            setBuildProgress(0);
            // Keep error message visible
          } else if (payload.status === "building") {
            setBuilding(true);
            setBuildProgress(payload.progress ?? 0);
          }
        }
      );
    })();

    return () => {
      if (unlisten) unlisten();
    };
  }, [fetchIndex, kb.id]);

  const handleBuild = async () => {
    setBuilding(true);
    setBuildProgress(0);
    setBuildMsg("正在构建 HNSW 向量索引…");
    try {
      await kbApi.buildIndex(kb.id);
      // Progress will come via Tauri event listener
    } catch (e) {
      setBuilding(false);
      setBuildMsg("");
      alert(`构建失败: ${e}`);
    }
  };

  const handleDrop = async () => {
    setIndexDeleteTarget({
      run: async () => {
        try {
          await kbApi.dropIndex(kb.id);
          await fetchIndex();
        } catch (e) {
          alert(`删除失败: ${e}`);
        }
      },
    });
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 className="h-6 w-6 animate-spin text-slate-400" />
      </div>
    );
  }

  return (
    <div className="grid gap-4 lg:grid-cols-2">
      {/* Index status */}
      <div className="surface data-card rounded-2xl">
        <div className="mb-4 flex items-center gap-2">
          <Database size={18} className="text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">索引状态</h3>
        </div>
        {indexMeta ? (
          <div className="space-y-3">
            <div className="grid grid-cols-2 gap-3">
              <div className="rounded-xl bg-slate-50 p-3">
                <div className="text-xs text-slate-500">索引类型</div>
                <div className="text-sm font-medium text-slate-900 mt-1">{indexMeta.index_type || "linear"}</div>
              </div>
              <div className="rounded-xl bg-slate-50 p-3">
                <div className="text-xs text-slate-500">状态</div>
                <div className="text-sm font-medium mt-1">
                  <IndexStatusBadge status={indexMeta.status} />
                </div>
              </div>
              <div className="rounded-xl bg-slate-50 p-3">
                <div className="text-xs text-slate-500">Embedding 维度</div>
                <div className="text-sm font-medium text-slate-900 mt-1">{indexMeta.embedding_dim || "未检测"}</div>
              </div>
              <div className="rounded-xl bg-slate-50 p-3">
                <div className="text-xs text-slate-500">切片数量</div>
                <div className="text-sm font-medium text-slate-900 mt-1">{indexMeta.chunk_count}</div>
              </div>
            </div>
            {indexMeta.built_at && (
              <div className="text-xs text-slate-400">构建时间: {indexMeta.built_at}</div>
            )}
          </div>
        ) : (
          <div className="text-sm text-slate-500">暂无索引信息</div>
        )}
      </div>

      {/* Actions */}
      <div className="surface data-card rounded-2xl">
        <div className="mb-4 flex items-center gap-2">
          <SettingsIcon size={18} className="text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">索引操作</h3>
        </div>
        <div className="space-y-3">
          {building && buildMsg && (
            <div className="rounded-lg bg-blue-50 border border-blue-100 px-3 py-2.5 text-xs text-blue-600">
              <div className="flex items-center gap-2 mb-1.5">
                <Loader2 className="h-3 w-3 animate-spin shrink-0" />
                <span>{buildMsg}</span>
              </div>
              {buildProgress >= 0 && buildProgress < 100 && (
                <div className="mt-1 h-1.5 w-full rounded-full bg-blue-100 overflow-hidden">
                  <div
                    className="h-full bg-blue-500 rounded-full transition-all duration-300"
                    style={{ width: `${Math.max(buildProgress, 3)}%` }}
                  />
                </div>
              )}
            </div>
          )}
          {!building && buildMsg && (
            <div className="rounded-lg bg-red-50 border border-red-100 px-3 py-2 text-xs text-red-600">
              {buildMsg}
            </div>
          )}
          <button
            onClick={handleBuild}
            disabled={building}
            className="action-primary w-full disabled:opacity-50"
          >
            {building ? (
              <><Loader2 className="h-4 w-4 animate-spin" /> 构建中...</>
            ) : (
              <><Database size={16} /> 构建索引</>
            )}
          </button>
          <button
            onClick={handleDrop}
            disabled={!indexMeta || indexMeta.status === "none"}
            className="w-full rounded-xl border border-red-200 px-4 py-2 text-sm text-red-600 hover:bg-red-50 disabled:opacity-50"
          >
            <Trash2 size={16} />
            删除索引
          </button>
          <div className="rounded-lg bg-blue-50 border border-blue-100 px-3 py-2 text-xs text-blue-600">
            ℹ️ 使用 HNSW 图索引，平均查询复杂度 O(log n)。构建后自动用于检索加速。
          </div>
        </div>
      </div>

      {indexDeleteTarget && (
        <ConfirmDialog
          message="确定删除索引？删除后需重新构建。"
          onCancel={() => setIndexDeleteTarget(null)}
          onConfirm={() => { indexDeleteTarget.run(); setIndexDeleteTarget(null); }}
        />
      )}
    </div>
  );
}

// ─── Documents Tab ────────────────────────────────────────────────────────

function DocumentsTab({ kb, onRefresh }: { kb: KnowledgeBase; onRefresh: () => void }) {
  const [docs, setDocs] = useState<KbDocument[]>([]);
  const [loading, setLoading] = useState(false);
  const [uploadingCount, setUploadingCount] = useState(0);
  const [uploadTotal, setUploadTotal] = useState(0);
  const [errorNotices, setErrorNotices] = useState<{ doc_id: string; filename: string; error: string }[]>([]);
  const [progressMap, setProgressMap] = useState<Record<string, { stage: string; progress: number; detail: string }>>({});
  const [docDeleteTarget, setDocDeleteTarget] = useState<{ message: string; run: () => void } | null>(null);

  const fetchDocs = useCallback(async () => {
    setLoading(true);
    try {
      const data = await kbApi.getDocuments(kb.id);
      setDocs(data);
    } catch (e) {
      console.error(e);
    } finally {
      setLoading(false);
    }
  }, [kb.id]);

  useEffect(() => {
    fetchDocs();
    const interval = setInterval(fetchDocs, 3000);
    return () => clearInterval(interval);
  }, [fetchDocs]);

  // Listen for document processing errors from backend
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      unlisten = await listen<{ doc_id: string; kb_id: string; filename: string; error: string }>(
        "kb-document-error",
        (event) => {
          if (!active) return;
          const payload = event.payload;
          if (payload.kb_id !== kb.id) return;
          setErrorNotices((prev) => [...prev, payload]);
          setProgressMap((prev) => {
            const next = { ...prev };
            delete next[payload.doc_id];
            return next;
          });
          setTimeout(() => {
            setErrorNotices((prev) => prev.filter((n) => n.doc_id !== payload.doc_id));
          }, 8000);
          fetchDocs();
          onRefresh();
        }
      );
    })();
    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, [kb.id, fetchDocs, onRefresh]);

  // 桌面端文件拖放：tauri OS 级拖放只给路径（HTML5 drop 不触发），
  // 故用 runtime.onFileDrop 拿路径 → 后端按路径读取并入库。浏览器端为 no-op（走 HTML5 onDrop）。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    onFileDrop(async (paths) => {
      if (!active) return;
      setUploadTotal(paths.length);
      setUploadingCount(0);
      for (const p of paths) {
        try {
          await kbApi.uploadDocumentFromPath(kb.id, p);
        } catch (e) {
          alert(`上传失败: ${e}`);
        }
        setUploadingCount((prev) => prev + 1);
      }
      setUploadTotal(0);
      setUploadingCount(0);
      await fetchDocs();
      onRefresh();
    }).then((un) => {
      if (active) unlisten = un;
      else un();
    });
    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, [kb.id, fetchDocs, onRefresh]);

  // Listen for document processing progress from backend
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      unlisten = await listen<{ doc_id: string; kb_id: string; filename: string; stage: string; progress: number; detail: string }>(
        "kb-document-progress",
        (event) => {
          if (!active) return;
          const p = event.payload;
          if (p.kb_id !== kb.id) return;
          if (p.stage === "done") {
            setProgressMap((prev) => {
              const next = { ...prev };
              delete next[p.doc_id];
              return next;
            });
            fetchDocs();
            onRefresh();
          } else {
            setProgressMap((prev) => ({
              ...prev,
              [p.doc_id]: { stage: p.stage, progress: p.progress, detail: p.detail },
            }));
          }
        }
      );
    })();
    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, [kb.id, fetchDocs, onRefresh]);

  const handleUploadBatch = async (files: File[]) => {
    if (files.length === 0) return;
    setUploadTotal(files.length);
    setUploadingCount(0);
    for (const file of files) {
      try {
        const content = await fileToBase64(file);
        await kbApi.uploadDocument({
          kb_id: kb.id,
          filename: file.name,
          content,
        });
      } catch (e) {
        console.error(`Upload failed for ${file.name}:`, e);
        alert(`上传失败 ${file.name}: ${e}`);
      }
      setUploadingCount(prev => prev + 1);
    }
    setUploadTotal(0);
    setUploadingCount(0);
    await fetchDocs();
    onRefresh();
  };

  const handleDelete = async (docId: string) => {
    setDocDeleteTarget({
      message: "删除此文档？",
      run: async () => {
        try {
          await kbApi.deleteDocument(docId, kb.id);
          await fetchDocs();
          onRefresh();
        } catch (e) {
          alert(`删除失败: ${e}`);
        }
      },
    });
  };

  const handleReindex = async (docId: string) => {
    try {
      await kbApi.reindexDocument(docId);
      await fetchDocs();
    } catch (e) {
      alert(`重新索引失败: ${e}`);
    }
  };

  return (
    <div className="space-y-4">
      {/* Upload zone */}
      <label
        className="flex cursor-pointer items-center justify-center rounded-2xl border-2 border-dashed border-slate-300 bg-white px-6 py-8 transition-colors hover:border-blue-400 hover:bg-blue-50/30"
        onDragOver={(e) => { e.preventDefault(); e.dataTransfer.dropEffect = "copy"; }}
        onDrop={(e) => {
          e.preventDefault();
          const files = Array.from(e.dataTransfer.files || []);
          if (files.length > 0) handleUploadBatch(files);
        }}
      >
        <input
          type="file"
          className="hidden"
          multiple
          accept=".md,.txt,.json,.yaml,.yml,.rs,.ts,.tsx,.js,.py,.go,.java,.c,.cpp,.h,.sh,.toml,.xml,.html,.css,.pdf"
          onChange={(e) => {
            const files = Array.from(e.target.files || []);
            if (files.length > 0) handleUploadBatch(files);
            e.target.value = "";
          }}
          disabled={uploadTotal > 0}
        />
        {uploadTotal > 0 ? (
          <div className="flex items-center gap-2 text-sm text-blue-600">
            <Loader2 className="h-5 w-5 animate-spin" />
            上传中 {uploadingCount}/{uploadTotal}...
          </div>
        ) : (
          <div className="flex flex-col items-center gap-2 text-sm text-slate-500">
            <Upload className="h-6 w-6" />
            <span>点击上传文件到 RAG（支持多选）</span>
            <span className="text-xs text-slate-400">支持 md/txt/code/json/yaml/pdf</span>
          </div>
        )}
      </label>

      {/* Error notices */}
      {errorNotices.length > 0 && (
        <div className="space-y-2">
          {errorNotices.map((notice) => (
            <div
              key={notice.doc_id}
              className="flex items-start gap-3 rounded-xl border border-red-200 bg-red-50 px-4 py-3"
            >
              <XCircle className="mt-0.5 h-5 w-5 shrink-0 text-red-500" />
              <div className="min-w-0 flex-1">
                <div className="text-sm font-medium text-red-800">
                  {notice.filename} 处理失败
                </div>
                <div className="mt-0.5 text-xs text-red-600">{notice.error}</div>
              </div>
              <button
                onClick={() =>
                  setErrorNotices((prev) =>
                    prev.filter((n) => n.doc_id !== notice.doc_id)
                  )
                }
                className="shrink-0 rounded-lg p-1 text-red-400 hover:bg-red-100 hover:text-red-600"
              >
                <Trash2 size={14} />
              </button>
            </div>
          ))}
        </div>
      )}

      {/* Documents list */}
      {loading && docs.length === 0 ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="h-6 w-6 animate-spin text-slate-400" />
        </div>
      ) : docs.length === 0 ? (
        <div className="surface empty-state rounded-2xl">
          <FileText className="h-8 w-8 text-slate-300" />
          <p className="text-sm text-slate-500">暂无文档</p>
        </div>
      ) : (
        <div className="space-y-2">
          {docs.map((doc) => {
            const prog = progressMap[doc.id];
            const ocrFailedPages = formatOcrFailedPages(doc.ocr_failed_pages);
            const pdfQuality = doc.file_type === "pdf" ? formatPdfTextExtraction(doc.doc_meta) : null;
            return (
            <div
              key={doc.id}
              className="surface flex items-center gap-3 rounded-xl px-4 py-3"
            >
              <DocStatusIcon status={prog ? "processing" : doc.status} />

              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="truncate text-sm font-medium text-slate-900">
                    {doc.filename}
                  </span>
                  <span className="rounded bg-slate-100 px-1.5 py-0.5 text-[10px] text-slate-500">
                    {doc.file_type}
                  </span>
                  {doc.ocr_engine === "vlm" && (
                    <span
                      className="rounded bg-violet-100 px-1.5 py-0.5 text-[10px] font-medium text-violet-600"
                      title={doc.page_count > 0 ? `经 LLM OCR 识别，共 ${doc.page_count} 页` : "经 LLM OCR 识别"}
                    >
                      OCR
                    </span>
                  )}
                </div>
                {prog ? (
                  <div className="mt-1.5">
                    <div className="flex items-center gap-2">
                      <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-slate-200">
                        <div
                          className="h-full rounded-full bg-blue-500 transition-all duration-300"
                          style={{ width: `${prog.progress}%` }}
                        />
                      </div>
                      <span className="shrink-0 text-[11px] text-blue-600">
                        {prog.detail} · {prog.progress}%
                      </span>
                    </div>
                  </div>
                ) : (
                  <div className="mt-1 flex items-center gap-3 text-xs text-slate-500">
                    <span>{formatSize(doc.file_size)}</span>
                    {doc.file_type === "pdf" && <span>{doc.page_count > 0 ? `${doc.page_count} 页` : "页数未记录"}</span>}
                    {doc.chunk_count > 0 && <span>{doc.chunk_count} 切片</span>}
                    {doc.token_count > 0 && <span>{doc.token_count} tokens</span>}
                    {doc.error_message && (
                      <span
                        className="text-red-500"
                        title={ocrFailedPages ? `${doc.error_message}；${ocrFailedPages}` : doc.error_message}
                      >
                        {doc.error_message.slice(0, 50)}
                        {ocrFailedPages && `（${ocrFailedPages}）`}
                      </span>
                    )}
                  </div>
                )}
                {!prog && pdfQuality && (
                  <div className={`mt-1 text-xs ${pdfQuality.warning ? "text-amber-700" : "text-slate-500"}`} title={pdfQuality.detail}>
                    {pdfQuality.label}
                  </div>
                )}
              </div>

              <div className="flex items-center gap-1">
                <button
                  onClick={() => handleReindex(doc.id)}
                  className="rounded-lg p-1.5 text-slate-400 hover:bg-slate-100 hover:text-slate-600"
                  title="重新索引"
                >
                  <RefreshCw size={15} />
                </button>
                <button
                  onClick={() => handleDelete(doc.id)}
                  className="rounded-lg p-1.5 text-slate-400 hover:bg-red-50 hover:text-red-500"
                  title="删除"
                >
                  <Trash2 size={15} />
                </button>
              </div>
            </div>
            );
          })}
        </div>
      )}

      {docDeleteTarget && (
        <ConfirmDialog
          message={docDeleteTarget.message}
          onCancel={() => setDocDeleteTarget(null)}
          onConfirm={() => { docDeleteTarget.run(); setDocDeleteTarget(null); }}
        />
      )}
    </div>
  );
}

// ─── Search Tab ──────────────────────────────────────────────────────────

function SearchTab({ kb }: { kb: KnowledgeBase }) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<KbSearchResult[]>([]);
  const [searching, setSearching] = useState(false);
  const [searched, setSearched] = useState(false);
  const [tags, setTags] = useState<KbTag[]>([]);
  const [tagsLoading, setTagsLoading] = useState(false);

  // Load tags for preset search terms
  useEffect(() => {
    if (kb.chunk_count === 0) return;
    let active = true;
    setTagsLoading(true);
    kbApi.getTags(kb.id, 8)
      .then((data) => { if (active) setTags(data); })
      .catch(() => {})
      .finally(() => { if (active) setTagsLoading(false); });
    return () => { active = false; };
  }, [kb.id, kb.chunk_count]);

  const handleSearch = async (searchQuery?: string) => {
    const q = (searchQuery ?? query).trim();
    if (!q) return;
    if (searchQuery) setQuery(searchQuery);
    setSearching(true);
    setSearched(true);
    try {
      const data = await kbApi.search({ query: q, kb_id: kb.id, top_k: 10 });
      setResults(data);
    } catch (e) {
      console.error(e);
    } finally {
      setSearching(false);
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex gap-2">
        <input
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && !e.nativeEvent.isComposing && e.keyCode !== 229 && handleSearch()}
          placeholder="输入搜索内容..."
          className="flex-1 rounded-xl border border-slate-200 bg-white px-4 py-2.5 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
        />
        <button
          onClick={() => handleSearch()}
          disabled={searching || !query.trim()}
          className="action-primary disabled:opacity-50"
        >
          {searching ? <Loader2 className="h-4 w-4 animate-spin" /> : <Search size={16} />}
          搜索
        </button>
      </div>

      {/* Preset search terms */}
      {(tagsLoading || tags.length > 0) && (
        <div className="flex flex-wrap items-center gap-2">
          <span className="flex items-center gap-1 text-[11px] font-medium text-slate-400">
            <Sparkles size={12} />
            快速检索
          </span>
          {tagsLoading ? (
            <>
              {[...Array(5)].map((_, i) => (
                <div key={i} className="h-6 w-16 animate-pulse rounded-full bg-slate-100" />
              ))}
            </>
          ) : (
            tags.map((tag) => (
              <button
                key={tag.word}
                onClick={() => setQuery(tag.word)}
                className="inline-flex items-center rounded-full border border-slate-200 bg-gradient-to-br from-slate-50 to-white px-3 py-1 text-xs font-medium text-slate-600 transition-all hover:border-blue-200 hover:bg-blue-50 hover:text-blue-600 hover:shadow-sm"
              >
                {tag.word}
              </button>
            ))
          )}
        </div>
      )}

      {searched && !searching && results.length === 0 && (
        <div className="surface empty-state rounded-2xl">
          <Search className="h-8 w-8 text-slate-300" />
          <p className="text-sm text-slate-500">未找到相关内容</p>
        </div>
      )}

      {results.length > 0 && (
        <div className="space-y-3">
          {results.map((r, i) => (
            <div key={r.chunk_id} className="surface rounded-xl p-4">
              <div className="mb-2 flex items-center gap-2">
                <span className="rounded bg-blue-50 px-2 py-0.5 text-[10px] font-medium text-blue-600">
                  #{i + 1}
                </span>
                <span className="text-xs font-medium text-slate-700">{r.filename}</span>
                <span className="text-xs text-slate-400">
                  相似度: {(r.score * 100).toFixed(1)}%
                </span>
              </div>
              <p className="text-sm text-slate-600 whitespace-pre-wrap line-clamp-6">
                {r.content}
              </p>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ─── Ask Tab (RAG) ──────────────────────────────────────────────────────

function AskTab({ kb }: { kb: KnowledgeBase }) {
  const [question, setQuestion] = useState("");
  const [answer, setAnswer] = useState<KbRagAnswer | null>(null);
  const [asking, setAsking] = useState(false);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [selectedChannelId, setSelectedChannelId] = useState<string>("");
  const [selectedModel, setSelectedModel] = useState<string>("");
  const [showChannelPicker, setShowChannelPicker] = useState(false);
  const [showModelPicker, setShowModelPicker] = useState(false);
  const [conversation, setConversation] = useState<Array<{ role: "user" | "assistant"; content: string; sources?: KbRagAnswer["sources"]; retrievalDetails?: KbRetrievalDetail[] | null }>>([]);
  const [deepResearch, setDeepResearch] = useState(false);
  const [showSearchConfig, setShowSearchConfig] = useState(false);
  const [searchMode, setSearchMode] = useState<"hybrid" | "vector" | "keyword">("hybrid");
  const [vectorWeight, setVectorWeight] = useState(0.7);
  const [keywordWeight, setKeywordWeight] = useState(0.3);
  const [topK, setTopK] = useState(5);
  const [showRetrievalDetails, setShowRetrievalDetails] = useState<number | null>(null);

  // Persistence key for this KB's ask preferences
  const storageKey = `kb_ask_prefs_${kb.id}`;

  useEffect(() => {
    channelApi.getAll().then((chs) => {
      const active = chs.filter((c) => c.status === 1);
      setChannels(active);

      // Load saved preferences from localStorage
      try {
        const saved = localStorage.getItem(storageKey);
        if (saved) {
          const prefs = JSON.parse(saved);
          // Validate that saved channel still exists and is active
          const savedCh = active.find(c => c.id === prefs.channelId);
          if (savedCh) {
            setSelectedChannelId(savedCh.id);
            // Validate saved model exists in that channel
            if (prefs.model && savedCh.models.includes(prefs.model)) {
              setSelectedModel(prefs.model);
            } else {
              setSelectedModel(savedCh.models[0] || "");
            }
            return;
          }
        }
      } catch {}

      // Fallback: auto-select first channel with models
      const first = active.find((c) => c.models.length > 0);
      if (first) {
        setSelectedChannelId(first.id);
        setSelectedModel(first.models[0]);
      }
    }).catch(console.error);
  }, [storageKey]);

  // Persist preferences when they change
  useEffect(() => {
    if (selectedChannelId && selectedModel) {
      localStorage.setItem(storageKey, JSON.stringify({
        channelId: selectedChannelId,
        model: selectedModel,
      }));
    }
  }, [storageKey, selectedChannelId, selectedModel]);

  // Models from selected channel
  const selectedChannel = channels.find((c) => c.id === selectedChannelId);
  const channelModels = selectedChannel?.models ?? [];

  const handleSelectChannel = (chId: string) => {
    setSelectedChannelId(chId);
    const ch = channels.find((c) => c.id === chId);
    if (ch && ch.models.length > 0) {
      setSelectedModel(ch.models[0]);
    } else {
      setSelectedModel("");
    }
    setShowChannelPicker(false);
  };

  const handleSelectModel = (model: string) => {
    setSelectedModel(model);
    setShowModelPicker(false);
  };

  const handleAsk = async () => {
    if (!question.trim()) return;
    setAsking(true);
    const userMsg = question;
    setQuestion("");
    setConversation((prev) => [...prev, { role: "user", content: userMsg }]);
    try {
      // Build history from current conversation (last 20 messages)
      const history: ConversationMessage[] = conversation.slice(-20).map((m) => ({
        role: m.role,
        content: m.content,
      }));

      const result = await kbApi.ask({
        question: userMsg,
        kb_id: kb.id,
        top_k: topK,
        model: selectedModel || undefined,
        history,
        deep_research: deepResearch,
        max_rounds: 5,
        vector_weight: searchMode === "hybrid" ? vectorWeight : undefined,
        keyword_weight: searchMode === "hybrid" ? keywordWeight : undefined,
        search_mode: searchMode,
      });
      setAnswer(result);
      setConversation((prev) => [
        ...prev,
        { role: "assistant", content: result.answer, sources: result.sources, retrievalDetails: result.retrieval_details },
      ]);
    } catch (e) {
      const errMsg = `请求失败: ${e}`;
      setAnswer({ answer: errMsg, sources: [], usage: null, retrieval_details: null });
      setConversation((prev) => [...prev, { role: "assistant", content: errMsg }]);
    } finally {
      setAsking(false);
    }
  };

  return (
    <div className="flex flex-col h-[calc(100vh-300px)] min-h-[360px]">
      {/* Model selector bar — top fixed */}
      <div className="flex items-center gap-3 border-b border-border bg-background/60 rounded-t-2xl px-4 py-3 shrink-0">
          {/* Channel selector */}
          <div className="relative">
            <button
              type="button"
              onClick={() => { setShowChannelPicker(!showChannelPicker); setShowModelPicker(false); }}
              className="flex items-center gap-2 rounded-xl border border-border bg-white px-3 py-2 text-xs font-medium transition-all hover:border-primary/40 hover:shadow-sm"
            >
              <span className="text-muted-foreground">渠道</span>
              <span className={selectedChannel ? "text-foreground truncate max-w-[120px]" : "text-muted-foreground"}>
                {selectedChannel?.name ?? "选择渠道"}
              </span>
              <ChevronDown size={13} className={`shrink-0 text-muted-foreground transition-transform ${showChannelPicker ? "rotate-180" : ""}`} />
            </button>

            {showChannelPicker && (
              <>
                <div className="fixed inset-0 z-40" onClick={() => setShowChannelPicker(false)} />
                <div className="absolute left-0 top-full z-50 mt-1.5 w-56 rounded-2xl border border-border bg-white p-2 shadow-xl max-h-[280px] overflow-auto">
                  <div className="px-2 py-1.5 text-[11px] font-semibold text-muted-foreground/70 uppercase tracking-wide">活跃渠道</div>
                  {channels.length === 0 ? (
                    <div className="px-3 py-2 text-xs text-muted-foreground">暂无可用渠道</div>
                  ) : channels.map((ch) => (
                    <button
                      key={ch.id}
                      type="button"
                      onClick={() => handleSelectChannel(ch.id)}
                      className={`flex w-full items-center justify-between rounded-xl px-3 py-2.5 text-sm transition-all ${
                        selectedChannelId === ch.id
                          ? "bg-primary/8 text-primary font-semibold"
                          : "text-foreground hover:bg-muted/60"
                      }`}
                    >
                      <div className="flex items-center gap-2 min-w-0">
                        <span className="truncate">{ch.name}</span>
                        <span className="rounded-full bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground shrink-0">
                          {ch.type}
                        </span>
                      </div>
                      {selectedChannelId === ch.id && <Check size={14} className="shrink-0" />}
                    </button>
                  ))}
                </div>
              </>
            )}
          </div>

          {/* Arrow */}
          <ChevronRight size={14} className="shrink-0 text-muted-foreground/40" />

          {/* Model selector */}
          <div className="relative">
            <button
              type="button"
              onClick={() => { setShowModelPicker(!showModelPicker); setShowChannelPicker(false); }}
              disabled={!selectedChannelId}
              className="flex items-center gap-2 rounded-xl border border-border bg-white px-3 py-2 text-xs font-medium transition-all hover:border-primary/40 hover:shadow-sm disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <span className="text-muted-foreground">模型</span>
              <span className={selectedModel ? "text-foreground truncate max-w-[160px]" : "text-muted-foreground"}>
                {selectedModel || "选择模型"}
              </span>
              <ChevronDown size={13} className={`shrink-0 text-muted-foreground transition-transform ${showModelPicker ? "rotate-180" : ""}`} />
            </button>

            {showModelPicker && selectedChannelId && (
              <>
                <div className="fixed inset-0 z-40" onClick={() => setShowModelPicker(false)} />
                <div className="absolute left-0 top-full z-50 mt-1.5 w-56 rounded-2xl border border-border bg-white p-2 shadow-xl max-h-[280px] overflow-auto">
                  <div className="px-2 py-1.5 text-[11px] font-semibold text-muted-foreground/70 uppercase tracking-wide">
                    {selectedChannel?.name} 模型
                  </div>
                  {channelModels.length === 0 ? (
                    <div className="px-3 py-2 text-xs text-muted-foreground">该渠道未配置模型</div>
                  ) : channelModels.map((m) => (
                    <button
                      key={m}
                      type="button"
                      onClick={() => handleSelectModel(m)}
                      className={`flex w-full items-center justify-between rounded-xl px-3 py-2.5 text-sm font-mono transition-all ${
                        selectedModel === m
                          ? "bg-primary/8 text-primary font-semibold"
                          : "text-foreground hover:bg-muted/60"
                      }`}
                    >
                      <span className="truncate">{m}</span>
                      {selectedModel === m && <Check size={14} className="shrink-0" />}
                    </button>
                  ))}
                </div>
              </>
            )}
          </div>

          {/* Right side actions */}
          <div className="ml-auto flex items-center gap-2">
            {selectedModel && (
              <span className="hidden sm:inline-flex rounded-full bg-primary/8 px-2.5 py-1 text-[10px] font-medium text-primary">
                {selectedModel}
              </span>
            )}
            {/* Deep Research toggle */}
            <button
              onClick={() => setDeepResearch(!deepResearch)}
              className={`flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs font-medium transition-colors ${
                deepResearch
                  ? "bg-violet-50 text-violet-600 hover:bg-violet-100"
                  : "bg-slate-100 text-slate-400 hover:bg-slate-200"
              }`
              }
              title="Deep Research: 多轮迭代检索+综合分析"
            >
              <Sparkles size={12} />
              Deep Research
            </button>
            {/* Search config toggle */}
            <button
              onClick={() => setShowSearchConfig(!showSearchConfig)}
              className={`flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs font-medium transition-colors ${
                showSearchConfig
                  ? "bg-blue-50 text-blue-600 hover:bg-blue-100"
                  : "bg-slate-100 text-slate-400 hover:bg-slate-200"
              }`
              }
              title="检索配置: 模式/权重/top_k"
            >
              <Sliders size={12} />
              检索配置
            </button>
            {conversation.length > 0 && (
              <button
                onClick={() => { setConversation([]); setAnswer(null); }}
                className="rounded-lg px-2.5 py-1.5 text-xs text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
              >
                清空对话
              </button>
            )}
          </div>
        </div>

        {/* Search config panel */}
        {showSearchConfig && (
          <div className="border-b border-border bg-slate-50/50 px-4 py-3 space-y-3 shrink-0">
            <div className="flex items-center gap-4 flex-wrap">
              {/* Search mode */}
              <div className="flex items-center gap-2">
                <span className="text-xs font-medium text-muted-foreground">检索模式</span>
                <div className="flex rounded-lg border border-border overflow-hidden">
                  {(["hybrid", "vector", "keyword"] as const).map((m) => (
                    <button
                      key={m}
                      onClick={() => setSearchMode(m)}
                      className={`px-2.5 py-1 text-xs transition-colors ${
                        searchMode === m
                          ? "bg-primary text-white"
                          : "bg-white text-muted-foreground hover:bg-slate-100"
                      }`}
                    >
                      {m === "hybrid" ? "混合" : m === "vector" ? "向量" : "关键词"}
                    </button>
                  ))}
                </div>
              </div>
              {/* Top K */}
              <div className="flex items-center gap-2">
                <span className="text-xs font-medium text-muted-foreground">Top K</span>
                <input
                  type="number"
                  min={1}
                  max={20}
                  value={topK}
                  onChange={(e) => setTopK(Math.max(1, Math.min(20, Number(e.target.value) || 5)))}
                  className="w-14 rounded-lg border border-border bg-white px-2 py-1 text-xs text-center outline-none focus:border-primary"
                />
              </div>
            </div>
            {/* Weights (only for hybrid) */}
            {searchMode === "hybrid" && (
              <div className="flex items-center gap-4 flex-wrap">
                <div className="flex items-center gap-2">
                  <span className="text-xs font-medium text-muted-foreground">向量权重</span>
                  <input
                    type="range"
                    min={0}
                    max={1}
                    step={0.1}
                    value={vectorWeight}
                    onChange={(e) => {
                      const v = Number(e.target.value);
                      setVectorWeight(v);
                      setKeywordWeight(Math.round((1 - v) * 10) / 10);
                    }}
                    className="w-24 accent-primary"
                  />
                  <span className="text-xs text-muted-foreground w-8">{vectorWeight.toFixed(1)}</span>
                </div>
                <div className="flex items-center gap-2">
                  <span className="text-xs font-medium text-muted-foreground">关键词权重</span>
                  <input
                    type="range"
                    min={0}
                    max={1}
                    step={0.1}
                    value={keywordWeight}
                    onChange={(e) => {
                      const v = Number(e.target.value);
                      setKeywordWeight(v);
                      setVectorWeight(Math.round((1 - v) * 10) / 10);
                    }}
                    className="w-24 accent-primary"
                  />
                  <span className="text-xs text-muted-foreground w-8">{keywordWeight.toFixed(1)}</span>
                </div>
              </div>
            )}
          </div>
        )}

        {/* Conversation area — flexible middle, scrollable */}
        <div className="flex-1 min-h-0 overflow-y-auto px-4 py-4 space-y-4">
          {conversation.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12 text-muted-foreground">
              <MessageCircle className="h-10 w-10 text-muted-foreground/30" />
              <p className="mt-3 text-sm">向 RAG 提问，AI 将基于检索到的内容回答</p>
              <p className="mt-1 text-xs text-muted-foreground/70">
                {kb.doc_count} 文档 · {kb.chunk_count} 切片可供检索
              </p>
            </div>
          ) : (
            conversation.map((msg, i) => (
              <div key={i} className={`flex ${msg.role === "user" ? "justify-end" : "justify-start"}`}>
                <div
                  className={`max-w-[80%] rounded-2xl px-4 py-3 text-sm ${
                    msg.role === "user"
                      ? "bg-primary text-white"
                      : "bg-muted/50 text-foreground border border-border"
                  }`}
                >
                  <p className="whitespace-pre-wrap">{msg.content}</p>
                  {msg.sources && msg.sources.length > 0 && (
                    <div className="mt-3 space-y-1.5 border-t border-border/40 pt-3">
                      <div className="text-[10px] font-medium text-muted-foreground uppercase tracking-wide">引用来源</div>
                      {msg.sources.map((s, si) => (
                        <div key={si} className="rounded-lg bg-white/80 p-2 text-xs">
                          <div className="flex items-center justify-between">
                            <span className="font-medium text-foreground">{s.filename}</span>
                            <span className="text-muted-foreground">{(s.score * 100).toFixed(1)}%</span>
                          </div>
                          <p className="mt-0.5 text-muted-foreground line-clamp-2">{s.snippet}</p>
                        </div>
                      ))}
                    </div>
                  )}
                  {msg.retrievalDetails && msg.retrievalDetails.length > 0 && (
                    <div className="mt-2 border-t border-border/40 pt-2">
                      <button
                        onClick={() => setShowRetrievalDetails(showRetrievalDetails === i ? null : i)}
                        className="flex items-center gap-1 text-[10px] font-medium text-muted-foreground hover:text-foreground transition-colors"
                      >
                        {showRetrievalDetails === i ? <ChevronUp size={10} /> : <ChevronDown size={10} />}
                        检索详情 ({msg.retrievalDetails.length})
                      </button>
                      {showRetrievalDetails === i && (
                        <div className="mt-1.5 space-y-1">
                          {msg.retrievalDetails.map((rd, rdi) => (
                            <div key={rdi} className="rounded-lg bg-white/60 p-2 text-xs border border-border/40">
                              <div className="flex items-center justify-between gap-2">
                                <div className="flex items-center gap-1.5 min-w-0">
                                  <span className="font-medium text-foreground truncate">{rd.filename}</span>
                                  {rd.symbol_name && (
                                    <span className="shrink-0 rounded bg-primary/10 px-1 py-0.5 text-[9px] text-primary">
                                      {rd.symbol_name}
                                    </span>
                                  )}
                                </div>
                                <span className="shrink-0 text-muted-foreground">{(rd.score * 100).toFixed(1)}%</span>
                              </div>
                              <div className="mt-1 flex items-center gap-3 text-[9px] text-muted-foreground">
                                {rd.vector_score != null && (
                                  <span className="text-blue-500">向量: {(rd.vector_score * 100).toFixed(1)}%</span>
                                )}
                                {rd.keyword_score != null && (
                                  <span className="text-green-500">关键词: {(rd.keyword_score * 100).toFixed(1)}%</span>
                                )}
                              </div>
                              <p className="mt-0.5 text-muted-foreground line-clamp-2 text-[10px]">{rd.snippet}</p>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              </div>
            ))
          )}
          {asking && (
            <div className="flex justify-start">
              <div className="rounded-2xl bg-muted/50 border border-border px-4 py-3">
                <div className="flex items-center gap-2 text-sm text-muted-foreground">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  正在检索 RAG 并生成回答...
                </div>
              </div>
            </div>
          )}
        </div>

        {/* Input bar — bottom fixed */}
        <div className="border-t border-border bg-background/40 rounded-b-2xl px-4 py-3 shrink-0">
          <div className="flex items-end gap-2">
            <textarea
              value={question}
              onChange={(e) => setQuestion(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing && e.keyCode !== 229) {
                  e.preventDefault();
                  handleAsk();
                }
              }}
              placeholder="输入问题，Enter 发送，Shift+Enter 换行..."
              rows={1}
              className="flex-1 resize-none rounded-2xl border border-border bg-white px-3.5 py-2.5 text-sm outline-none focus:border-primary focus:ring-2 focus:ring-primary/20 max-h-32"
              style={{ minHeight: "42px" }}
              disabled={asking}
            />
            <button
              onClick={handleAsk}
              disabled={asking || !question.trim()}
              className="action-primary disabled:opacity-50 shrink-0"
            >
              {asking ? <Loader2 className="h-4 w-4 animate-spin" /> : <MessageCircle size={16} />}
              发送
            </button>
          </div>
          {/* Token usage */}
          {answer?.usage && (
            <div className="mt-2 flex items-center gap-3 text-[10px] text-muted-foreground">
              <span>Prompt: {answer.usage.prompt_tokens}</span>
              <span>Completion: {answer.usage.completion_tokens}</span>
              <span>Total: {answer.usage.total_tokens}</span>
            </div>
          )}
        </div>
    </div>
  );
}

// ─── OCR 模型选项 Hook ──────────────────────────────────────────────────

/** 读取全局 LLM OCR 总开关与可选视觉模型列表（聚合各自启用渠道声明的模型）。 */
function useOcrModelOptions() {
  const [ocrEnabled, setOcrEnabled] = useState(false);
  const [options, setOptions] = useState<string[]>([]);
  useEffect(() => {
    settingsApi.get().then(s => setOcrEnabled(Boolean(s.ocr_enabled))).catch(() => {});
    channelApi.getAll()
      .then(chs => setOptions(Array.from(new Set(chs.filter(c => c.status === 1).flatMap(c => c.models))).sort()))
      .catch(() => {});
  }, []);
  return { ocrEnabled, options };
}

/** 聚合所有启用渠道声明的模型（去重排序）。 */
function aggregateChannelModels(chs: Channel[]): string[] {
  return Array.from(new Set(chs.filter(c => c.status === 1).flatMap(c => c.models))).sort();
}

/** 名字上疑似向量（Embedding）模型的匹配规则：embed / bge / gte / m3e 等常见命名 */
const EMBEDDING_MODEL_PATTERN = /embed|bge|gte|m3e/i;

/** OCR（视觉）下拉选项：排除疑似 embedding 模型——识图请求发给向量模型必然失败。 */
function filterVisionModels(models: string[]): string[] {
  return models.filter((m) => !EMBEDDING_MODEL_PATTERN.test(m));
}

/** 默认可选的 Embedding 模型（OpenAI 系），与历史默认保持一致 */
const DEFAULT_EMBEDDING_MODELS = ["text-embedding-3-small", "text-embedding-3-large", "text-embedding-ada-002"];

/** Embedding 下拉选项：默认模型 + 渠道声明中疑似 embedding 的模型；当前值不在列表时保留，避免误清空。 */
function mergeEmbeddingOptions(channelModels: string[], current: string): string[] {
  const all = [...DEFAULT_EMBEDDING_MODELS, ...channelModels.filter((m) => EMBEDDING_MODEL_PATTERN.test(m))];
  if (current && !all.includes(current)) all.push(current);
  return Array.from(new Set(all));
}

// ─── Settings Tab ───────────────────────────────────────────────────────

function SettingsTab({ kb, onUpdated }: { kb: KnowledgeBase; onUpdated: (kb: KnowledgeBase) => void }) {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [name, setName] = useState(kb.name);
  const [description, setDescription] = useState(kb.description || "");
  const [embeddingModel, setEmbeddingModel] = useState(kb.embedding_model || "text-embedding-3-small");
  const [embeddingCapability, setEmbeddingCapability] = useState<{ available: boolean; message: string } | null>(null);
  const [capabilityRevision, setCapabilityRevision] = useState(0);
  const [embeddingChannelId, setEmbeddingChannelId] = useState(kb.embedding_channel_id || "");
  const [status, setStatus] = useState(kb.status);
  const [mcpEnabled, setMcpEnabled] = useState(kb.mcp_enabled ?? 1);
  const [chunkSize, setChunkSize] = useState(kb.chunk_size || 512);
  const [chunkOverlap, setChunkOverlap] = useState(kb.chunk_overlap || 64);
  const [embeddingBatchSize, setEmbeddingBatchSize] = useState(kb.embedding_batch_size || 32);
  const [excludedDirs, setExcludedDirs] = useState(kb.excluded_dirs || "");
  const [excludedFiles, setExcludedFiles] = useState(kb.excluded_files || "");
  const [includedFiles, setIncludedFiles] = useState(kb.included_files || "");
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [showChannelPicker, setShowChannelPicker] = useState(false);
  const [ocrModel, setOcrModel] = useState(kb.ocr_model || "");
  const [reasoningEffort, setReasoningEffort] = useState(kb.reasoning_effort || "");
  const [temperature, setTemperature] = useState(kb.temperature != null ? String(kb.temperature) : "");
  const { ocrEnabled, options: ocrModelOptionsRaw } = useOcrModelOptions();
  // OCR 只应选视觉模型：过滤掉疑似 embedding 的模型；
  // 当前已配置的模型即使被过滤也保留为可选值，避免误清空（可在下拉里改选正确的视觉模型）
  const visionOptions = filterVisionModels(ocrModelOptionsRaw);
  const ocrModelOptions = ocrModel && !visionOptions.includes(ocrModel)
    ? [...visionOptions, ocrModel]
    : visionOptions;

  useEffect(() => {
    channelApi.getAll().then(setChannels).catch(console.error);
  }, []);

  useEffect(() => {
    let active = true;
    setEmbeddingCapability(null);
    kbApi.getEmbeddingCapability(embeddingModel).then(result => {
      if (active) setEmbeddingCapability(result);
    }).catch(() => {
      if (active) setEmbeddingCapability({ available: false, message: "无法检查渠道能力，请刷新后重试" });
    });
    return () => { active = false; };
  }, [embeddingModel, capabilityRevision]);

  const activeChannels = channels.filter(c => c.status === 1);
  const selectedEmbeddingChannel = activeChannels.find(c => c.id === embeddingChannelId);
  // Embedding 下拉选项：默认模型 + 已配置渠道声明的模型（复用本页已有的 channels，不额外请求）
  const embeddingOptions = mergeEmbeddingOptions(aggregateChannelModels(channels), embeddingModel);

  const handleSave = async () => {
    setSaving(true);
    setSaved(false);
    try {
      const updated = await kbApi.update(kb.id, {
        name: name.trim(),
        description: description.trim() || undefined,
        embedding_model: embeddingModel.trim() || undefined,
        embedding_channel_id: embeddingChannelId || undefined,
        // ocr_model 总是提交：空字符串表示「不启用」（后端 null/缺省视为不修改，无法清空）
        ocr_model: ocrModel.trim(),
        // 推理档位：空串=跟随默认；温度：空=不修改
        reasoning_effort: reasoningEffort,
        temperature: temperature.trim() === "" ? undefined : Number(temperature),
        status,
        mcp_enabled: mcpEnabled,
        chunk_size: chunkSize,
        chunk_overlap: chunkOverlap,
        embedding_batch_size: embeddingBatchSize,
        excluded_dirs: excludedDirs,
        excluded_files: excludedFiles,
        included_files: includedFiles,
      });
      onUpdated(updated);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      alert(`保存失败: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="grid gap-4 lg:grid-cols-2">
      {/* Basic */}
      <div className="surface data-card rounded-2xl">
        <h3 className="mb-4 text-sm font-semibold text-slate-900">基本信息</h3>
        <div className="space-y-4">
          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">名称</label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
            />
          </div>
          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">描述</label>
            <textarea
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              rows={2}
              className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
            />
          </div>
          <div className="flex items-center gap-3">
            <label className="flex items-center gap-2 cursor-pointer">
              <input
                type="checkbox"
                checked={status === 1}
                onChange={(e) => setStatus(e.target.checked ? 1 : 0)}
                className="rounded"
              />
              <span className="text-sm text-slate-700">启用 RAG</span>
            </label>
            <label className="flex items-center gap-2 cursor-pointer ml-4">
              <input
                type="checkbox"
                checked={mcpEnabled === 1}
                onChange={(e) => setMcpEnabled(e.target.checked ? 1 : 0)}
                className="rounded"
              />
              <span className="text-sm text-slate-700">MCP 暴露</span>
            </label>
          </div>
          <p className="text-xs text-slate-400">
            关闭 MCP 暴露后，该 RAG 不会出现在 MCP 工具的列表中，也不会被全局搜索命中。仍可通过显式指定 kb_id 访问。
          </p>
        </div>
      </div>

      {/* Embedding config */}
      <div className="surface data-card rounded-2xl">
        <h3 className="mb-4 text-sm font-semibold text-slate-900">Embedding 配置</h3>
        <div className="space-y-4">
          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">Embedding 模型</label>
            <Select
              value={embeddingModel}
              onChange={(v) => setEmbeddingModel(v)}
              size="sm"
              options={embeddingOptions.map((m) => ({ value: m, label: m }))}
            />
            <p className="mt-1 text-xs text-slate-400">
              默认提供 OpenAI 系模型，已配置渠道声明的模型也会出现在列表中；请确保所选渠道支持 /embeddings 接口（DeepSeek 不提供 Embedding）
            </p>
            <div role="status" className={`mt-2 rounded-lg p-3 text-xs ${embeddingCapability?.available ? "bg-emerald-50 text-emerald-700" : "bg-amber-50 text-amber-800"}`}>
              <p>{embeddingCapability?.message || "正在检查 Embeddings 渠道配置…"}</p>
              <div className="mt-2 flex gap-3">
                <RouterLink to="/channels" className="underline">前往渠道配置</RouterLink>
                <button type="button" onClick={() => setCapabilityRevision(value => value + 1)} className="underline">刷新能力检查</button>
              </div>
            </div>
          </div>

          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">绑定渠道（可选）</label>
            <div className="relative">
              <button
                type="button"
                onClick={() => setShowChannelPicker(!showChannelPicker)}
                className="flex w-full items-center justify-between rounded-xl border border-slate-200 bg-white px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
              >
                <span className={selectedEmbeddingChannel ? "text-slate-900" : "text-slate-400"}>
                  {selectedEmbeddingChannel
                    ? `${selectedEmbeddingChannel.name} (${selectedEmbeddingChannel.type})`
                    : "自动选择（默认）"}
                </span>
                <ChevronDown size={15} className={`shrink-0 text-slate-400 transition-transform ${showChannelPicker ? "rotate-180" : ""}`} />
              </button>

              {showChannelPicker && (
                <>
                  <div className="fixed inset-0 z-40" onClick={() => setShowChannelPicker(false)} />
                  <div className="absolute left-0 top-full z-50 mt-1.5 w-full rounded-2xl border border-slate-200 bg-white p-2 shadow-xl max-h-[280px] overflow-auto">
                    <button
                      type="button"
                      onClick={() => {
                        setEmbeddingChannelId("");
                        setShowChannelPicker(false);
                      }}
                      className={`flex w-full items-center justify-between rounded-xl px-3 py-2.5 text-sm transition-all ${
                        embeddingChannelId === ""
                          ? "bg-blue-50 text-blue-600 font-semibold"
                          : "text-slate-700 hover:bg-slate-50"
                      }`}
                    >
                      <span>自动选择（默认）</span>
                      {embeddingChannelId === "" && <Check size={14} className="shrink-0" />}
                    </button>
                    {activeChannels.map((c) => (
                      <button
                        key={c.id}
                        type="button"
                        onClick={() => {
                          setEmbeddingChannelId(c.id);
                          setShowChannelPicker(false);
                        }}
                        className={`flex w-full items-center justify-between rounded-xl px-3 py-2.5 text-sm transition-all ${
                          embeddingChannelId === c.id
                            ? "bg-blue-50 text-blue-600 font-semibold"
                            : "text-slate-700 hover:bg-slate-50"
                        }`}
                      >
                        <div className="flex items-center gap-2 min-w-0">
                          <span className="truncate">{c.name}</span>
                          <span className="rounded-full bg-slate-100 px-1.5 py-0.5 text-[10px] text-slate-500 shrink-0">
                            {c.type}
                          </span>
                        </div>
                        {embeddingChannelId === c.id && <Check size={14} className="shrink-0" />}
                      </button>
                    ))}
                  </div>
                </>
              )}
            </div>
            <p className="mt-1 text-xs text-slate-400">
              指定后，embedding 请求会优先使用该渠道。不指定则自动调度。
            </p>
          </div>
        </div>
      </div>

      {/* OCR config */}
      <div className="surface data-card rounded-2xl">
        <h3 className="mb-4 text-sm font-semibold text-slate-900">OCR 配置</h3>
        <div className="space-y-4">
          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">OCR 视觉模型</label>
            <Select
              value={ocrModel}
              disabled={!ocrEnabled}
              onChange={(v) => setOcrModel(v)}
              size="sm"
              options={[
                { value: "", label: "不启用（默认）" },
                ...ocrModelOptions.map((m) => ({ value: m, label: m })),
              ]}
            />
            <p className="mt-1 text-xs text-slate-400">
              {ocrEnabled
                ? "用于识别扫描版 PDF，需有渠道声明支持该视觉模型（如 qwen-vl-max / glm-4v-flash）；留空则不启用"
                : "请先在设置中心开启 LLM OCR"}
            </p>
          </div>
        </div>
      </div>

      {/* 推理档位 & 温度 */}
      <div className="surface data-card rounded-2xl">
        <h3 className="mb-4 text-sm font-semibold text-slate-900">推理与温度</h3>
        <div className="space-y-4">
          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">推理档位（思考级别）</label>
            <Select
              value={reasoningEffort}
              onChange={(v) => setReasoningEffort(v)}
              size="sm"
              ariaLabel="推理档位（思考级别）"
              options={[
                { value: "", label: "跟随默认（不覆盖模型默认）" },
                { value: "default", label: "default（模型默认）" },
                { value: "none", label: "none（关闭推理）" },
                { value: "low", label: "low" },
                { value: "medium", label: "medium" },
                { value: "high", label: "high" },
              ]}
            />
            <p className="mt-1 text-xs text-slate-400">问答未显式传档位时使用该值；仅对支持 reasoning_effort 的上游有效。</p>
          </div>
          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">温度（0~2）</label>
            <input
              type="number"
              min={0}
              max={2}
              step={0.1}
              value={temperature}
              onChange={(e) => setTemperature(e.target.value)}
              placeholder="留空不修改"
              className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
            />
            <p className="mt-1 text-xs text-slate-400">问答未显式传温度时使用该值。</p>
          </div>
        </div>
      </div>

      {/* Chunking & Filtering config */}
      <div className="surface data-card rounded-2xl">
        <h3 className="mb-4 text-sm font-semibold text-slate-900">分块与过滤</h3>
        <div className="space-y-4">
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="mb-1 block text-sm font-medium text-slate-700">分块大小 (tokens)</label>
              <input
                type="number"
                value={chunkSize}
                onChange={(e) => setChunkSize(Number(e.target.value) || 512)}
                min={50}
                max={2000}
                step={50}
                className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
              />
              <p className="mt-1 text-xs text-slate-400">默认 512，越大上下文越完整但消耗更多 token</p>
            </div>
            <div>
              <label className="mb-1 block text-sm font-medium text-slate-700">分块重叠 (tokens)</label>
              <input
                type="number"
                value={chunkOverlap}
                onChange={(e) => setChunkOverlap(Number(e.target.value) || 64)}
                min={0}
                max={500}
                step={16}
                className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
              />
              <p className="mt-1 text-xs text-slate-400">默认 64，保持上下文连续性</p>
            </div>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="mb-1 block text-sm font-medium text-slate-700">Embedding 批次大小</label>
              <input
                type="number"
                value={embeddingBatchSize}
                onChange={(e) => setEmbeddingBatchSize(Number(e.target.value) || 32)}
                min={1}
                max={100}
                step={1}
                className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
              />
              <p className="mt-1 text-xs text-slate-400">默认 32，单次 API 调用处理的最大 chunk 数量</p>
            </div>
          </div>
          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">排除目录（逗号分隔）</label>
            <input
              type="text"
              value={excludedDirs}
              onChange={(e) => setExcludedDirs(e.target.value)}
              placeholder="tests, examples, vendor"
              className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
            />
            <p className="mt-1 text-xs text-slate-400">导入 Git/本地目录时跳过这些目录（默认排除 .git, node_modules 等）</p>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="mb-1 block text-sm font-medium text-slate-700">排除文件（逗号分隔）</label>
              <input
                type="text"
                value={excludedFiles}
                onChange={(e) => setExcludedFiles(e.target.value)}
                placeholder="*.lock, *.min.js"
                className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
              />
            </div>
            <div>
              <label className="mb-1 block text-sm font-medium text-slate-700">包含文件类型（逗号分隔，空=全部）</label>
              <input
                type="text"
                value={includedFiles}
                onChange={(e) => setIncludedFiles(e.target.value)}
                placeholder="md, rs, ts, py"
                className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
              />
            </div>
          </div>
        </div>
      </div>

      {/* Stats */}
      <div className="surface data-card rounded-2xl">
        <h3 className="mb-4 text-sm font-semibold text-slate-900">统计</h3>
        <div className="grid grid-cols-3 gap-4">
          <div className="rounded-xl bg-slate-50 p-3 text-center">
            <div className="text-2xl font-bold text-slate-900">{kb.doc_count}</div>
            <div className="text-xs text-slate-500">文档数</div>
          </div>
          <div className="rounded-xl bg-slate-50 p-3 text-center">
            <div className="text-2xl font-bold text-slate-900">{kb.chunk_count}</div>
            <div className="text-xs text-slate-500">切片数</div>
          </div>
          <div className="rounded-xl bg-slate-50 p-3 text-center">
            <div className="text-2xl font-bold text-slate-900">{kb.total_tokens}</div>
            <div className="text-xs text-slate-500">总 Tokens</div>
          </div>
        </div>
      </div>

      {/* Save */}
      <div className="surface data-card rounded-2xl flex items-center justify-end gap-3">
        <button
          onClick={handleSave}
          disabled={saving}
          className="action-primary disabled:opacity-50"
        >
          {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : <SettingsIcon size={16} />}
          保存设置
        </button>
        {saved && (
          <span className="flex items-center gap-1 text-sm text-emerald-600">
            <CheckCircle2 size={16} /> 已保存
          </span>
        )}
      </div>
    </div>
  );
}

// ─── MCP Tab (per-KB) ───────────────────────────────────────────────────

function McpTab({ kb }: { kb: KnowledgeBase }) {
  const [serverUrl, setServerUrl] = useState("http://127.0.0.1:8777");
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    serverApi.getStatus().then(s => {
      if (s.running) setServerUrl(`http://127.0.0.1:${s.port}`);
    }).catch(() => {});
  }, []);

  const baseUrl = serverUrl;
  const mcpEndpoint = `${baseUrl}/mcp`;

  const handleCopy = (text: string) => {
    navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const mcpTools = [
    { name: "search_knowledge_base", desc: "语义检索 RAG，返回匹配文本片段和相似度评分", required: ["query", "kb_id"] },
    { name: "list_knowledge_bases", desc: "列出已授权且开启 MCP 的 RAG（ID/名称/文档数）", required: [] },
    { name: "ask_knowledge_base", desc: "RAG 问答，基于检索内容生成回答并返回来源引用", required: ["question", "kb_id", "model"] },
    { name: "read_document", desc: "读取指定文档的已摄入正文", required: ["kb_id", "doc_id"] },
    { name: "get_knowledge_base_stats", desc: "获取 RAG 统计信息（文档数/切片数/token数）", required: ["kb_id"] },
  ];

  return (
    <div className="space-y-4">
      {/* 接入说明 */}
      <div className="surface data-card rounded-2xl">
        <div className="mb-4 flex items-center gap-2">
          <Terminal size={18} className="text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">MCP 对接</h3>
          {kb.mcp_enabled === 1 ? (
            <span className="ml-auto rounded-full bg-emerald-50 px-2 py-0.5 text-xs font-medium text-emerald-600">MCP 已开启（仍需授权）</span>
          ) : (
            <span className="ml-auto rounded-full bg-slate-100 px-2 py-0.5 text-xs font-medium text-slate-500">MCP 未开启</span>
          )}
        </div>

        <div className="space-y-3">
          {/* 端点地址 */}
          <div>
            <label className="mb-1 block text-xs font-medium text-slate-500">MCP 端点（JSON-RPC over HTTP）</label>
            <div className="flex items-center gap-2">
              <code className="flex-1 truncate rounded-lg bg-slate-50 border border-slate-200 px-3 py-2 text-xs font-mono text-slate-800">{mcpEndpoint}</code>
              <button onClick={() => handleCopy(mcpEndpoint)} className="rounded-lg border border-slate-200 p-2 hover:bg-slate-50">
                {copied ? <CheckCircle2 size={14} className="text-emerald-500" /> : <Copy size={14} className="text-slate-400" />}
              </button>
            </div>
          </div>

          {/* 协议说明 */}
          <div className="rounded-lg bg-blue-50 border border-blue-100 px-3 py-2.5 text-xs text-blue-700">
            <div className="font-medium mb-1">📡 MCP (Model Context Protocol) 对接</div>
            <div className="text-blue-600">
              将端点和已授权的 API Key 配置到支持 HTTP POST 与自定义 Authorization 请求头的 MCP 客户端，即可检索和问答此 RAG。
            </div>
          </div>

          {/* 未暴露提示 */}
          {kb.mcp_enabled !== 1 && (
            <div className="rounded-lg bg-amber-50 border border-amber-100 px-3 py-2 text-xs text-amber-700">
              ⚠️ 该 RAG 未开启 MCP 暴露。外部 Agent 无法检索到此 RAG。请在「设置」中开启「MCP 暴露」。
            </div>
          )}
        </div>
      </div>

      <KnowledgeConnectionPanel key={kb.id} kbId={kb.id} />

      {/* 可用工具列表 */}
      <div className="surface data-card rounded-2xl">
        <div className="mb-4 flex items-center gap-2">
          <Layers size={18} className="text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">可用 MCP 工具</h3>
          <span className="ml-auto text-xs text-slate-400">{mcpTools.length} 个工具</span>
        </div>
        <div className="space-y-2">
          {mcpTools.map((tool) => (
            <div key={tool.name} className="flex items-start gap-3 rounded-xl bg-slate-50 px-3 py-2.5">
              <code className="shrink-0 rounded bg-slate-200 px-1.5 py-0.5 text-[11px] font-mono font-medium text-slate-700">{tool.name}</code>
              <div className="min-w-0">
                <p className="text-xs text-slate-600">{tool.desc}</p>
                {tool.required.length > 0 && (
                  <p className="mt-0.5 text-[10px] text-slate-400">
                    必填: {tool.required.join(", ")}
                  </p>
                )}
              </div>
            </div>
          ))}
        </div>
      </div>

      {/* 调用示例 */}
      <div className="surface data-card rounded-2xl">
        <div className="mb-4 flex items-center gap-2">
          <Server size={18} className="text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">调用示例</h3>
        </div>

        <p className="mb-3 text-xs text-slate-500">将已授权的 API Key 设置为环境变量 WALIAPI_API_KEY，再执行以下命令。</p>
        <div className="space-y-3">
          <div>
            <label className="mb-1 block text-xs font-medium text-slate-500">1. 列出可用 RAG</label>
            <pre className="overflow-x-auto rounded-xl bg-slate-50 border border-slate-200 p-3 text-[11px]"><code className="text-slate-800">{`curl -X POST ${mcpEndpoint} \\
  -H "Authorization: Bearer $WALIAPI_API_KEY" \\
  -H "Content-Type: application/json" \\
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tools/call",
    "params": {
      "name": "list_knowledge_bases",
      "arguments": {}
    }
  }'`}</code></pre>
          </div>

          <div>
            <label className="mb-1 block text-xs font-medium text-slate-500">2. 语义检索</label>
            <pre className="overflow-x-auto rounded-xl bg-slate-50 border border-slate-200 p-3 text-[11px]"><code className="text-slate-800">{`curl -X POST ${mcpEndpoint} \\
  -H "Authorization: Bearer $WALIAPI_API_KEY" \\
  -H "Content-Type: application/json" \\
  -d '{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/call",
    "params": {
      "name": "search_knowledge_base",
      "arguments": {
        "query": "你的检索内容",
        "kb_id": "${kb.id}",
        "top_k": 5
      }
    }
  }'`}</code></pre>
          </div>

          <div>
            <label className="mb-1 block text-xs font-medium text-slate-500">3. RAG 问答</label>
            <pre className="overflow-x-auto rounded-xl bg-slate-50 border border-slate-200 p-3 text-[11px]"><code className="text-slate-800">{`curl -X POST ${mcpEndpoint} \\
  -H "Authorization: Bearer $WALIAPI_API_KEY" \\
  -H "Content-Type: application/json" \\
  -d '{
    "jsonrpc": "2.0",
    "id": 3,
    "method": "tools/call",
    "params": {
      "name": "ask_knowledge_base",
      "arguments": {
        "question": "你的问题",
        "model": "请替换为已授权的生成模型",
        "kb_id": "${kb.id}"
      }
    }
  }'`}</code></pre>
          </div>
        </div>

        <div className="mt-3 rounded-lg bg-slate-50 border border-slate-200 px-3 py-2 text-xs text-slate-500">
          API Key 使用无会话 HTTP POST /mcp，仅开放上方查询工具。旧版 SSE 和管理工具仍使用 WALIAPI_MCP_TOKEN。
        </div>
      </div>
    </div>
  );
}

// ─── Create KB Modal ────────────────────────────────────────────────────

function CreateKbModal({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: () => void;
}) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [embeddingModel, setEmbeddingModel] = useState("text-embedding-3-small");
  const [ocrModel, setOcrModel] = useState("");
  // channelModels = 所有启用渠道声明的模型聚合，OCR 与 Embedding 两个下拉共用
  const { ocrEnabled, options: channelModels } = useOcrModelOptions();
  const embeddingOptions = mergeEmbeddingOptions(channelModels, embeddingModel);
  // OCR 下拉排除疑似 embedding 模型（识图请求发给向量模型必然失败）
  const visionModelOptions = filterVisionModels(channelModels);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleCreate = async () => {
    if (!name.trim()) {
      setError("请输入 RAG 名称");
      return;
    }
    setCreating(true);
    setError(null);
    try {
      await kbApi.create({
        name: name.trim(),
        description: description.trim() || undefined,
        embedding_model: embeddingModel || undefined,
        ocr_model: ocrModel || undefined,
      });
      onCreated();
    } catch (e) {
      setError(String(e));
    } finally {
      setCreating(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30" onClick={onClose}>
      <div
        className="w-full max-w-md rounded-2xl bg-white p-6 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <h3 className="text-lg font-semibold text-slate-900">新建 RAG</h3>
        <p className="mt-2 text-xs text-slate-500">新建 RAG 默认向全部已有 API 密钥开放查询。创建后可在“密钥 → 知识库查询权限”中撤销授权；MCP 访问仍需开启此库的 MCP 开关。</p>

        <div className="mt-4 space-y-4">
          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">名称</label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="例如：项目文档库"
              className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
            />
          </div>

          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">描述（可选）</label>
            <textarea
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="RAG 用途描述..."
              rows={2}
              className="w-full rounded-xl border border-slate-200 px-3 py-2 text-sm outline-none focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
            />
          </div>

          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">Embedding 模型</label>
            <Select
              value={embeddingModel}
              onChange={(v) => setEmbeddingModel(v)}
              size="sm"
              options={embeddingOptions.map((m) => ({ value: m, label: m }))}
            />
            <p className="mt-1 text-xs text-slate-400">
              默认提供 OpenAI 系模型，已配置渠道声明的模型也会出现在列表中；请确保所选渠道支持 /embeddings 接口（DeepSeek 不提供 Embedding）
            </p>
          </div>

          <div>
            <label className="mb-1 block text-sm font-medium text-slate-700">OCR 视觉模型（可选）</label>
            <Select
              value={ocrModel}
              disabled={!ocrEnabled}
              onChange={(v) => setOcrModel(v)}
              size="sm"
              options={[
                { value: "", label: "不启用（默认）" },
                ...visionModelOptions.map((m) => ({ value: m, label: m })),
              ]}
            />
            <p className="mt-1 text-xs text-slate-400">
              {ocrEnabled
                ? "用于识别扫描版 PDF，需有渠道声明支持该视觉模型；留空则不启用"
                : "请先在设置中心开启 LLM OCR"}
            </p>
          </div>

          {error && (
            <div className="rounded-lg bg-red-50 p-3 text-sm text-red-600">{error}</div>
          )}
        </div>

        <div className="mt-6 flex justify-end gap-2">
          <button
            onClick={onClose}
            className="rounded-xl px-4 py-2 text-sm text-slate-500 hover:bg-slate-100"
          >
            取消
          </button>
          <button
            onClick={handleCreate}
            disabled={creating}
            className="action-primary disabled:opacity-50"
          >
            {creating ? "创建中..." : "创建"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── Helpers ────────────────────────────────────────────────────────────

function DocStatusIcon({ status }: { status: string }) {
  switch (status) {
    case "ready":
      return <CheckCircle2 className="h-5 w-5 shrink-0 text-emerald-500" />;
    case "processing":
      return <Loader2 className="h-5 w-5 shrink-0 animate-spin text-blue-500" />;
    case "failed":
      return <XCircle className="h-5 w-5 shrink-0 text-red-500" />;
    case "pending":
      return <Clock className="h-5 w-5 shrink-0 text-slate-400" />;
    default:
      return <FileText className="h-5 w-5 shrink-0 text-slate-400" />;
  }
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** 解析 KbDocument.ocr_failed_pages（JSON 数组字符串，如 "[3,7]"），返回「第 3、7 页识别失败」文案。 */
function formatOcrFailedPages(raw: string | null | undefined): string | null {
  if (!raw) return null;
  try {
    const pages: unknown = JSON.parse(raw);
    if (Array.isArray(pages) && pages.length > 0) {
      return `第 ${pages.join("、")} 页识别失败`;
    }
  } catch {
    // 忽略无法解析的历史数据
  }
  return null;
}

/** 兼容未记录质量的旧文档；只展示结构化页号，不显示解析库的原始错误。 */
function formatPdfTextExtraction(raw: string): { label: string; detail: string; warning: boolean } | null {
  try {
    const metadata = JSON.parse(raw);
    const info = metadata?.pdf_text_extraction as PdfTextExtraction | undefined;
    if (info?.version !== 1 || !Array.isArray(info.pages) || !["complete", "partial", "failed"].includes(info.status)) return null;
    const pages = info.pages.filter((page) => Number.isInteger(page.page_no) && page.page_no > 0);
    const insufficient = pages.filter((page) => page.status === "insufficient").map((page) => page.page_no);
    const failed = pages.filter((page) => page.status === "failed").map((page) => page.page_no);
    const extracted = pages.filter((page) => page.status === "extracted").length;
    const warning = info.status !== "complete";
    const details = [
      `文字层充足 ${extracted} 页；不足 ${insufficient.length} 页；失败 ${failed.length} 页`,
      insufficient.length ? `第 ${insufficient.join("、")} 页文字层少于 50 个非空白字符（可能为空白页或扫描页）` : "",
      failed.length ? `第 ${failed.join("、")} 页文字层提取失败` : "",
      info.page_count === 0 ? "页面树无法读取，真实页数未知" : "",
      "仅检查文字层，不代表资料内容完整；不会自动启用 OCR",
    ].filter(Boolean).join("；");
    const label = info.status === "failed"
      ? "未提取到可用文字层 · 查看提示"
      : warning
        ? `文字层待检查 · ${insufficient.length} 页不足 / ${failed.length} 页失败`
        : "文字层已提取 · 不代表资料内容完整";
    return { label, detail: details, warning };
  } catch {
    return null;
  }
}

function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result as string;
      const base64 = result.split(",")[1] || result;
      resolve(base64);
    };
    reader.onerror = reject;
    reader.readAsDataURL(file);
  });
}

// ─── Wiki Section ─────────────────────────────────────────────────────

// Strip YAML frontmatter from markdown content before rendering
function stripFrontmatter(content: string): string {
  if (content.startsWith("---\n")) {
    const end = content.indexOf("\n---\n", 4);
    if (end !== -1) return content.slice(end + 5);
  }
  return content;
}

// Reusable Markdown renderer for Wiki pages
/// 把 markdown 中 root-relative 的 `/api/wiki/...` URL 补成绝对地址。
/// 只匹配出现在括号/引号后（`(`、`"`、`'`）的路径，避免误伤已是 http://... 的链接。
function absolutizeWikiApiUrls(md: string, baseUrl: string): string {
  return md.replace(/([(("'])\/api\/wiki\//g, `$1${baseUrl}/api/wiki/`);
}

function WikiMarkdown({ content }: { content: string }) {
  // 桌面端（tauri WebView）无法解析 root-relative 的 /api/...（base 是 tauri://），
  // 渲染前把图像/链接 URL 补成本机服务绝对地址 http://127.0.0.1:{port}/api/...
  // Web 面板与 location.origin 同源，相对路径可直接解析，无需转换。
  const [serverBase, setServerBase] = useState<string | null>(null);
  useEffect(() => {
    if (!isTauriRuntime()) return;
    let active = true;
    serverApi.getStatus().then((s) => {
      if (active && s.port > 0) setServerBase(`http://127.0.0.1:${s.port}`);
    }).catch(() => {});
    return () => { active = false; };
  }, []);
  const body = stripFrontmatter(serverBase ? absolutizeWikiApiUrls(content, serverBase) : content);
  return (
    <div className="wiki-markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[
          // 先让 rehype-raw 解析内嵌 HTML，再用 sanitize 按 GitHub 白名单过滤，
          // 阻止 Wiki 内容中的 iframe/form/script/事件属性等直接进入 DOM（XSS 面）
          rehypeRaw,
          rehypeSanitize,
        ]}
        components={{
          code({ className, children, ...props }) {
            const match = /language-(\w+)/.exec(className || "");
            const codeStr = String(children).replace(/\n$/, "");
            if (match) {
              return (
                <SyntaxHighlighter
                  language={match[1]}
                  style={oneDark}
                  customStyle={{ margin: 0, borderRadius: "0.75rem", fontSize: "0.75rem", lineHeight: "1.6rem" }}
                  wrapLongLines={false}
                >
                  {codeStr}
                </SyntaxHighlighter>
              );
            }
            return <code className="rounded bg-slate-100 px-1.5 py-0.5 text-[0.85em] text-pink-600" {...props}>{children}</code>;
          },
          img({ src, alt, ...props }) {
            return <img src={src as string} alt={alt || ""} loading="lazy" className="my-3 max-w-full rounded-xl border border-slate-100" {...props} />;
          },
          a({ href, children, ...props }) {
            return <a href={href} target="_blank" rel="noopener noreferrer" className="text-blue-600 underline decoration-blue-200 underline-offset-2 hover:decoration-blue-500" {...props}>{children}</a>;
          },
          table({ children, ...props }) {
            return <div className="my-3 overflow-x-auto rounded-xl border border-slate-100"><table className="w-full text-sm" {...props}>{children}</table></div>;
          },
          th({ children, ...props }) {
            return <th className="border-b border-slate-100 bg-slate-50 px-3 py-2 text-left font-semibold text-slate-700" {...props}>{children}</th>;
          },
          td({ children, ...props }) {
            return <td className="border-b border-slate-50 px-3 py-2 text-slate-600" {...props}>{children}</td>;
          },
          blockquote({ children, ...props }) {
            return <blockquote className="my-3 border-l-3 border-blue-200 bg-blue-50/40 py-2 pl-4 text-slate-600" {...props}>{children}</blockquote>;
          },
          h1({ children, ...props }) {
            return <h1 className="mb-3 mt-5 text-xl font-bold text-slate-900" {...props}>{children}</h1>;
          },
          h2({ children, ...props }) {
            return <h2 className="mb-2 mt-4 text-lg font-bold text-slate-900" {...props}>{children}</h2>;
          },
          h3({ children, ...props }) {
            return <h3 className="mb-2 mt-3 text-base font-semibold text-slate-800" {...props}>{children}</h3>;
          },
          p({ children, ...props }) {
            return <p className="my-2 text-sm leading-6 text-slate-700" {...props}>{children}</p>;
          },
          ul({ children, ...props }) {
            return <ul className="my-2 ml-5 list-disc space-y-1 text-sm text-slate-700" {...props}>{children}</ul>;
          },
          ol({ children, ...props }) {
            return <ol className="my-2 ml-5 list-decimal space-y-1 text-sm text-slate-700" {...props}>{children}</ol>;
          },
          hr({ ...props }) {
            return <hr className="my-4 border-slate-100" {...props} />;
          },
        }}
      >
        {body}
      </ReactMarkdown>
    </div>
  );
}

function WikiSection() {
  const [projects, setProjects] = useState<WikiProject[]>([]);
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [showCreate, setShowCreate] = useState(false);
  const [showExport, setShowExport] = useState(false);
  const [showImport, setShowImport] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [wikiDeleteTarget, setWikiDeleteTarget] = useState<{ message: string; run: () => void } | null>(null);
  const [wikiTab, setWikiTab] = useState<"overview" | "pages" | "sources" | "search" | "graph" | "settings">("overview");
  const selectedProject = useMemo(
    () => selectedProjectId ? projects.find((project) => project.id === selectedProjectId) ?? null : null,
    [projects, selectedProjectId],
  );

  // 同 KB 列表的 NEW-2 竞态守卫（见上方 kbMutations 注释）。
  const projectMutations = useRef(0);

  const fetchProjects = useCallback(async () => {
    setLoading(true);
    const mutationsAtStart = projectMutations.current;
    try {
      const data = await wikiApi.getProjects();
      if (projectMutations.current === mutationsAtStart) setProjects(data);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { fetchProjects(); }, [fetchProjects]);

  const handleProjectUpdated = useCallback((updated: WikiProject) => {
    projectMutations.current += 1;
    setProjects((current) => current.map((project) => project.id === updated.id ? updated : project));
  }, []);

  const handleDelete = async (id: string) => {
    setWikiDeleteTarget({
      message: "确定删除此 Wiki 项目？所有页面和源数据将一并删除。",
      run: async () => {
        try {
          await wikiApi.deleteProject(id);
          await fetchProjects();
          if (selectedProjectId === id) setSelectedProjectId(null);
        } catch (e) { setError(String(e)); }
      },
    });
  };

  const handleToggleStatus = async (p: WikiProject, newStatus: number) => {
    try {
      await wikiApi.updateProject(p.id, { status: newStatus });
      await fetchProjects();
    } catch (e) { setError(String(e)); }
  };

  const handleToggleMcp = async (p: WikiProject, newMcp: number) => {
    try {
      await wikiApi.updateProject(p.id, { mcp_enabled: newMcp });
      await fetchProjects();
    } catch (e) { setError(String(e)); }
  };

  if (error) {
    return (
      <div className="rounded-xl border border-red-200 bg-red-50 p-4 text-sm text-red-600">
        {error}
        <button onClick={() => setError(null)} className="ml-2 text-red-400 hover:text-red-600">✕</button>
      </div>
    );
  }

  if (selectedProject) {
    return (
      <WikiProjectDetail
        project={selectedProject}
        tab={wikiTab}
        setTab={setWikiTab}
        onBack={() => { setSelectedProjectId(null); setWikiTab("overview"); }}
        onRefresh={fetchProjects}
        onUpdated={handleProjectUpdated}
      />
    );
  }

  if (loading && projects.length === 0) {
    return (
      <div className="surface empty-state">
        <Loader2 className="h-8 w-8 animate-spin text-slate-400" />
      </div>
    );
  }

  if (projects.length === 0) {
    return (
      <div className="surface empty-state">
        <Network className="h-12 w-12 text-slate-300" />
        <p className="text-sm text-slate-500">还没有 Wiki 项目</p>
        <p className="text-xs text-slate-400">LLM 增量 RAG：摄入文档 → 生成结构化页面 → 知识图谱</p>
        <div className="mt-3 flex items-center gap-2">
          <button onClick={() => setShowImport(true)} className="action-secondary">
            <Download size={16} />
            导入备份
          </button>
          <button onClick={() => setShowCreate(true)} className="action-primary">
            <Plus size={16} />
            新建 Wiki 项目
          </button>
        </div>
        {showCreate && (
          <CreateWikiProjectModal
            onClose={() => setShowCreate(false)}
            onCreated={async () => { setShowCreate(false); await fetchProjects(); }}
          />
        )}
        {showImport && (
          <WikiImportDialog
            onClose={() => setShowImport(false)}
            onImported={async () => { setShowImport(false); await fetchProjects(); }}
          />
        )}
      </div>
    );
  }

  return (
    <>
      <div className="flex justify-end gap-2 mb-4">
        <button
          onClick={() => setShowExport(true)}
          className="action-secondary"
          title="把项目导出为 zip 备份（含页面、源文件、图片与 token）"
        >
          <Upload size={16} />
          导出
        </button>
        <button
          onClick={() => setShowImport(true)}
          className="action-secondary"
          title="从 zip 备份导入并恢复项目"
        >
          <Download size={16} />
          导入
        </button>
        <button onClick={() => setShowCreate(true)} className="action-primary">
          <Plus size={16} />
          新建 Wiki 项目
        </button>
      </div>
      <div className="space-y-3">
        {projects.map((p) => (
          <div key={p.id} className="surface group rounded-2xl p-5 transition-all hover:shadow-[0_8px_24px_rgba(15,23,42,0.06)] border border-slate-100">
            <div className="flex items-start gap-4">
              <div className={`flex h-10 w-10 shrink-0 items-center justify-center rounded-xl ${p.status === 1 ? "bg-violet-50" : "bg-slate-100"}`}>
                <Network className={`h-5 w-5 ${p.status === 1 ? "text-violet-600" : "text-slate-400"}`} />
              </div>
              <div className="min-w-0 flex-1 cursor-pointer" onClick={() => setSelectedProjectId(p.id)}>
                <div className="flex items-center gap-2">
                  <h3 className="text-base font-semibold text-slate-900">{p.name}</h3>
                  {p.status === 1 ? (
                    <span className="rounded-full bg-emerald-50 px-2 py-0.5 text-[10px] font-medium text-emerald-600">活跃</span>
                  ) : (
                    <span className="rounded-full bg-slate-100 px-2 py-0.5 text-[10px] font-medium text-slate-500">已禁用</span>
                  )}
                </div>
                <p className="mt-0.5 text-xs text-slate-500 line-clamp-1">{p.description || "暂无描述"}</p>
                <div className="mt-2 flex items-center gap-4 text-xs text-slate-500">
                  <span className="flex items-center gap-1"><FileText size={12} /> {p.page_count} 页面</span>
                  <span className="flex items-center gap-1"><Layers size={12} /> {p.source_count} 源</span>
                  {p.ingest_model && <span className="truncate" title={p.ingest_model}>{p.ingest_model}</span>}
                </div>
              </div>
              <div className="flex flex-col items-end gap-2 shrink-0">
                <div className="flex items-center gap-3">
                  <button
                    onClick={(e) => { e.stopPropagation(); handleToggleMcp(p, p.mcp_enabled === 1 ? 0 : 1); }}
                    className={`flex items-center gap-1.5 rounded-lg px-2 py-1 text-[10px] font-medium transition-colors ${
                      p.mcp_enabled === 1 ? "bg-violet-50 text-violet-600 hover:bg-violet-100" : "bg-slate-100 text-slate-400 hover:bg-slate-200"
                    }`}
                    title="MCP 暴露开关"
                  >
                    <Terminal size={11} />
                    MCP {p.mcp_enabled === 1 ? "已暴露" : "未暴露"}
                  </button>
                  <button
                    onClick={(e) => { e.stopPropagation(); handleToggleStatus(p, p.status === 1 ? 0 : 1); }}
                    className={`relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors ${p.status === 1 ? "bg-emerald-500" : "bg-slate-300"}`}
                    title="项目开关"
                  >
                    <span className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white shadow transition-transform ${p.status === 1 ? "translate-x-4" : "translate-x-1"}`} />
                  </button>
                  <button
                    onClick={(e) => { e.stopPropagation(); handleDelete(p.id); }}
                    className="rounded-lg p-1.5 text-slate-400 opacity-0 transition-opacity group-hover:opacity-100 hover:bg-red-50 hover:text-red-500"
                  >
                    <Trash2 className="h-4 w-4" />
                  </button>
                </div>
              </div>
            </div>
          </div>
        ))}
      </div>
      {showCreate && (
        <CreateWikiProjectModal
          onClose={() => setShowCreate(false)}
          onCreated={async () => { setShowCreate(false); await fetchProjects(); }}
        />
      )}
      {showExport && (
        <WikiExportDialog projects={projects} onClose={() => setShowExport(false)} />
      )}
      {showImport && (
        <WikiImportDialog
          onClose={() => setShowImport(false)}
          onImported={async () => { setShowImport(false); await fetchProjects(); }}
        />
      )}

      {wikiDeleteTarget && (
        <ConfirmDialog
          message={wikiDeleteTarget.message}
          onCancel={() => setWikiDeleteTarget(null)}
          onConfirm={() => { wikiDeleteTarget.run(); setWikiDeleteTarget(null); }}
        />
      )}
    </>
  );
}

// ─── Wiki 项目备份：大小格式化 ─────────────────────────────────────
function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(2)} MB`;
}

// ─── Wiki 导出弹窗：选项目 + 本地目录（可打开本地路径选择）→ 打 zip ──
function WikiExportDialog({ projects, onClose }: { projects: WikiProject[]; onClose: () => void }) {
  const [projectId, setProjectId] = useState(projects[0]?.id ?? "");
  const [targetDir, setTargetDir] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<WikiExportResult | null>(null);

  // 弹窗期间锁定页面滚动（关闭时恢复）
  useEffect(() => {
    document.body.style.overflow = "hidden";
    return () => { document.body.style.overflow = ""; };
  }, []);

  const handlePickDir = async () => {
    setError(null);
    try {
      const dir = await wikiApi.pickExportDir();
      if (dir) setTargetDir(dir);
    } catch (e) {
      setError(String(e));
    }
  };

  const handleExport = async () => {
    if (!projectId) { setError("请选择要导出的 Wiki 项目"); return; }
    const dir = targetDir.trim();
    if (!dir) { setError("请输入或选择导出目录"); return; }
    setBusy(true);
    setError(null);
    setDone(null);
    try {
      const res = await wikiApi.exportProject(projectId, dir);
      setDone(res);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4 backdrop-blur-sm" onClick={onClose}>
      <div className="relative w-full max-w-lg rounded-3xl bg-white p-7 shadow-2xl max-h-[70vh] overflow-y-auto" onClick={e => e.stopPropagation()}>
        <button onClick={onClose} className="absolute right-5 top-5 rounded-full p-1 text-slate-400 hover:bg-slate-100 hover:text-slate-600">
          <XCircle className="h-5 w-5" />
        </button>
        <div className="flex items-center gap-2">
          <div className="rounded-2xl border border-violet-100 bg-violet-50 p-2.5">
            <Upload className="h-5 w-5 text-violet-600" />
          </div>
          <div>
            <h2 className="text-lg font-semibold text-slate-900">导出 Wiki 备份</h2>
            <p className="text-xs text-slate-500">打包页面 MD、源文件、图片资产（含 token）与核心数据为 zip</p>
          </div>
        </div>
        <div className="mt-5 space-y-4">
          <div>
            <label className="mb-1.5 block text-xs font-medium text-slate-700">选择项目 *</label>
            <select
              value={projectId}
              onChange={e => { setProjectId(e.target.value); setDone(null); }}
              className="w-full rounded-xl border border-slate-200 bg-white px-3.5 py-2.5 text-sm"
            >
              {projects.map(p => (
                <option key={p.id} value={p.id}>{p.name}（{p.page_count} 页面 · {p.source_count} 源）</option>
              ))}
            </select>
          </div>
          <div>
            <label className="mb-1.5 block text-xs font-medium text-slate-700">导出目录 *</label>
            <p className="mb-1.5 text-[11px] text-slate-400">
              {isTauriRuntime() ? "备份以 wiki-backup-<项目名>-<时间戳>.zip 保存到该目录" : "填写服务端存放备份的目录路径"}
            </p>
            <div className="flex gap-2">
              <input
                value={targetDir}
                onChange={e => setTargetDir(e.target.value)}
                placeholder={isTauriRuntime() ? "例如 D:\\Backups\\wiki 或 /home/user/backups" : "例如 /data/waliapi/backups"}
                className="min-w-0 flex-1 rounded-xl border border-slate-200 px-3.5 py-2.5 text-sm"
              />
              {isTauriRuntime() && (
                <button onClick={handlePickDir} className="action-secondary shrink-0" title="打开本地路径选择目录">
                  <FolderSearch size={16} />
                  打开本地路径
                </button>
              )}
            </div>
          </div>

          {error && (
            <div className="flex items-start gap-2 rounded-xl border border-red-200 bg-red-50 px-3.5 py-3 text-xs text-red-600">
              <XCircle size={14} className="mt-0.5 shrink-0" />
              <span>导出失败：{error}</span>
            </div>
          )}
          {done && (
            <div className="flex items-start gap-2 rounded-xl border border-emerald-200 bg-emerald-50 px-3.5 py-3 text-xs text-emerald-700">
              <CheckCircle2 size={14} className="mt-0.5 shrink-0" />
              <div className="min-w-0 space-y-0.5">
                <p className="font-medium">导出成功</p>
                <p className="break-all">{done.path}</p>
                <p>{done.page_count} 页面 · {done.source_count} 源 · {done.file_count} 个文件 · {formatBytes(done.size_bytes)}</p>
              </div>
            </div>
          )}
        </div>
        <div className="mt-5 flex justify-end gap-2">
          <button onClick={onClose} className="action-secondary">关闭</button>
          <button onClick={handleExport} disabled={busy || projects.length === 0} className="action-primary">
            {busy ? <Loader2 className="h-4 w-4 animate-spin" /> : <Upload className="h-4 w-4" />}
            {busy ? "导出中..." : "开始导出"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── Wiki 导入弹窗：选 zip 备份 → 恢复为新项目（不覆盖现有数据） ────────
function WikiImportDialog({ onClose, onImported }: { onClose: () => void; onImported: () => void }) {
  const [picked, setPicked] = useState<{ label: string; path?: string; base64?: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<WikiImportResult | null>(null);

  // 弹窗期间锁定页面滚动（关闭时恢复）
  useEffect(() => {
    document.body.style.overflow = "hidden";
    return () => { document.body.style.overflow = ""; };
  }, []);

  const handlePickFile = async () => {
    setError(null);
    try {
      if (isTauriRuntime()) {
        const path = await wikiApi.pickImportFile();
        if (path) setPicked({ label: path.split(/[\\/]/).pop() || path, path });
      } else {
        const pickedFile = await pickFileAsBase64(".zip,application/zip");
        if (pickedFile) setPicked({ label: pickedFile.name, base64: pickedFile.base64 });
      }
    } catch (e) {
      setError(String(e));
    }
  };

  const handleImport = async () => {
    if (!picked) { setError("请先选择要导入的 zip 备份文件"); return; }
    setBusy(true);
    setError(null);
    setDone(null);
    try {
      const res = picked.path
        ? await wikiApi.importBackup(picked.path)
        : await wikiApi.importBackupBytes(picked.base64 || "");
      setDone(res);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4 backdrop-blur-sm" onClick={onClose}>
      <div className="relative w-full max-w-lg rounded-3xl bg-white p-7 shadow-2xl max-h-[70vh] overflow-y-auto" onClick={e => e.stopPropagation()}>
        <button onClick={onClose} className="absolute right-5 top-5 rounded-full p-1 text-slate-400 hover:bg-slate-100 hover:text-slate-600">
          <XCircle className="h-5 w-5" />
        </button>
        <div className="flex items-center gap-2">
          <div className="rounded-2xl border border-violet-100 bg-violet-50 p-2.5">
            <FileArchive className="h-5 w-5 text-violet-600" />
          </div>
          <div>
            <h2 className="text-lg font-semibold text-slate-900">导入 Wiki 备份</h2>
            <p className="text-xs text-slate-500">从导出 zip 恢复项目</p>
          </div>
        </div>
        <div className="mt-5 space-y-4">
          <div className="rounded-2xl border border-slate-100 px-4 py-3 text-[11px] text-slate-500 space-y-1">
            <p>• 备份包含页面 MD、源文件、图片资产（token）与核心数据库记录，导入后可直接使用，无需重新摄入。</p>
            <p>• 导入会创建为新项目（全新 ID），同名项目允许共存，不会覆盖任何现有数据。</p>
            <p>• API Key 的 Wiki 查询授权不随备份恢复，导入后请在「密钥」页重新授权。</p>
          </div>

          <div className="flex gap-2">
            <input
              readOnly
              value={picked?.label ?? ""}
              placeholder={isTauriRuntime() ? "尚未选择备份文件" : "尚未选择备份文件（浏览器选择）"}
              className="min-w-0 flex-1 rounded-xl border border-slate-200 bg-slate-50 px-3.5 py-2.5 text-sm text-slate-600"
            />
            <button onClick={handlePickFile} disabled={busy} className="action-secondary shrink-0">
              <FolderOpen size={16} />
              选择文件
            </button>
          </div>

          {error && (
            <div className="flex items-start gap-2 rounded-xl border border-red-200 bg-red-50 px-3.5 py-3 text-xs text-red-600">
              <XCircle size={14} className="mt-0.5 shrink-0" />
              <span>导入失败：{error}</span>
            </div>
          )}
          {done && (
            <div className="flex items-start gap-2 rounded-xl border border-emerald-200 bg-emerald-50 px-3.5 py-3 text-xs text-emerald-700">
              <CheckCircle2 size={14} className="mt-0.5 shrink-0" />
              <div className="min-w-0 space-y-0.5">
                <p className="font-medium">导入成功：已恢复为项目「{done.project_name}」</p>
                <p>{done.page_count} 页面 · {done.source_count} 源 · {done.file_count} 个文件（含图片与 token）</p>
              </div>
            </div>
          )}
        </div>
        <div className="mt-5 flex justify-end gap-2">
          {done ? (
            <button onClick={onImported} className="action-primary">
              <Check className="h-4 w-4" />
              完成
            </button>
          ) : (
            <>
              <button onClick={onClose} className="action-secondary">取消</button>
              <button onClick={handleImport} disabled={busy || !picked} className="action-primary">
                {busy ? <Loader2 className="h-4 w-4 animate-spin" /> : <Download className="h-4 w-4" />}
                {busy ? "导入中..." : "开始导入"}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function CreateWikiProjectModal({ onClose, onCreated }: { onClose: () => void; onCreated: () => void }) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [schemaText, setSchemaText] = useState("");
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [selectedChannel, setSelectedChannel] = useState("");
  const [selectedModel, setSelectedModel] = useState("");
  const [chatChannel, setChatChannel] = useState("");
  const [chatModel, setChatModel] = useState("");
  const [pdfOcrChannel, setPdfOcrChannel] = useState("");
  const [pdfOcrModel, setPdfOcrModel] = useState("");
  // 推理档位（思考级别）：新建时即可配置（摄入/问答 LLM 调用联动 thinking）
  const [reasoningEffort, setReasoningEffort] = useState("");

  useEffect(() => {
    channelApi.getAll().then(list => {
      const active = list.filter(c => c.status === 1);
      setChannels(active);
      if (active.length > 0) {
        setSelectedChannel(active[0].id);
        setChatChannel(active[0].id);
        const firstModel = active[0].models[0];
        if (firstModel) { setSelectedModel(firstModel); setChatModel(firstModel); }
      }
    }).catch(() => {});
  }, []);

  const handleCreate = async () => {
    if (!name.trim()) { setError("请输入项目名称"); return; }
    if (!selectedChannel || !selectedModel) { setError("请选择摄入渠道和模型"); return; }
    setCreating(true);
    setError(null);
    try {
      await wikiApi.createProject({
        name: name.trim(),
        description: description.trim() || undefined,
        schema_text: schemaText.trim() || undefined,
        ingest_channel_id: selectedChannel,
        ingest_model: selectedModel,
        chat_channel_id: chatChannel || selectedChannel,
        chat_model: chatModel || selectedModel,
        pdf_ocr_channel_id: pdfOcrChannel || undefined,
        pdf_ocr_model: pdfOcrModel || undefined,
        reasoning_effort: reasoningEffort || undefined,
      });
      onCreated();
    } catch (e) {
      setError(String(e));
    } finally {
      setCreating(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4 backdrop-blur-sm" onClick={onClose}>
      <div className="relative w-full max-w-lg rounded-3xl bg-white p-7 shadow-2xl max-h-[60vh] overflow-y-auto" onClick={e => e.stopPropagation()}>
        <button onClick={onClose} className="absolute right-5 top-5 rounded-full p-1 text-slate-400 hover:bg-slate-100 hover:text-slate-600">
          <XCircle className="h-5 w-5" />
        </button>
        <div className="flex items-center gap-2">
          <div className="rounded-2xl border border-violet-100 bg-violet-50 p-2.5">
            <Network className="h-5 w-5 text-violet-600" />
          </div>
          <div>
            <h2 className="text-lg font-semibold text-slate-900">新建 Wiki 项目</h2>
            <p className="text-xs text-slate-500">LLM 增量 RAG</p>
          </div>
        </div>
        <div className="mt-5 space-y-4">
          <div>
            <label className="mb-1.5 block text-xs font-medium text-slate-700">项目名称 *</label>
            <input
              value={name}
              onChange={e => setName(e.target.value)}
              placeholder="例如：项目文档 Wiki"
              className="w-full rounded-xl border border-slate-200 px-3.5 py-2.5 text-sm"
            />
          </div>
          <div>
            <label className="mb-1.5 block text-xs font-medium text-slate-700">描述</label>
            <input
              value={description}
              onChange={e => setDescription(e.target.value)}
              placeholder="简单描述这个 Wiki 的用途"
              className="w-full rounded-xl border border-slate-200 px-3.5 py-2.5 text-sm"
            />
          </div>
          <div className="rounded-2xl border border-slate-100 p-4">
            <label className="mb-1.5 block text-xs font-medium text-slate-700">摄入渠道 & 模型 *</label>
            <p className="mb-2 text-[11px] text-slate-400">用于 LLM 解析文档并生成 Wiki 页面</p>
            <ChannelModelPicker
              channels={channels}
              channelId={selectedChannel}
              onChannelChange={setSelectedChannel}
              model={selectedModel}
              onModelChange={setSelectedModel}
            />
          </div>
          <div className="rounded-2xl border border-slate-100 p-4">
            <label className="mb-1.5 block text-xs font-medium text-slate-700">对话渠道 & 模型</label>
            <p className="mb-2 text-[11px] text-slate-400">用于 Wiki 问答，默认同摄入渠道</p>
            <ChannelModelPicker
              channels={channels}
              channelId={chatChannel}
              onChannelChange={setChatChannel}
              model={chatModel}
              onModelChange={setChatModel}
              allowAuto
              autoLabel="同摄入渠道"
            />
          </div>
          <div className="rounded-2xl border border-slate-100 p-4">
            <label className="mb-1.5 block text-xs font-medium text-slate-700">PDF 视觉模型</label>
            <p className="mb-2 text-[11px] text-slate-400">摄入含扫描页/图文页的 PDF 时识别整页文字，纯文字层 PDF 不需要。可留空稍后在项目设置中配置。</p>
            <ChannelModelPicker
              channels={channels}
              channelId={pdfOcrChannel}
              onChannelChange={setPdfOcrChannel}
              model={pdfOcrModel}
              onModelChange={setPdfOcrModel}
            />
          </div>
          <div className="rounded-2xl border border-slate-100 p-4">
            <label className="mb-1.5 block text-xs font-medium text-slate-700">推理档位（思考级别）</label>
            <p className="mb-2 text-[11px] text-slate-400">摄入与问答使用的 LLM 推理档位，选择后自动联动 thinking 参数。未配置或选 default 时跟随模型默认。</p>
            <Select
              value={reasoningEffort}
              onChange={v => setReasoningEffort(v)}
              size="sm"
              ariaLabel="推理档位（思考级别）"
              options={[
                { value: "", label: "跟随模型默认（不写档位/thinking）" },
                { value: "default", label: "default（模型默认）" },
                { value: "none", label: "none（关闭推理）" },
                { value: "low", label: "low" },
                { value: "medium", label: "medium" },
                { value: "high", label: "high" },
              ]}
            />
          </div>
          <div>
            <label className="mb-1.5 block text-xs font-medium text-slate-700">Wiki Schema (CLAUDE.md)</label>
            <p className="mb-1.5 text-[11px] text-slate-400">定义 LLM 维护 Wiki 的规则。留空使用默认模板。</p>
            <textarea
              value={schemaText}
              onChange={e => setSchemaText(e.target.value)}
              placeholder="留空使用默认 Schema..."
              rows={4}
              className="w-full rounded-xl border border-slate-200 px-3.5 py-2.5 text-xs font-mono"
            />
          </div>
          {error && <p className="text-xs text-red-500">{error}</p>}
        </div>
        <div className="mt-5 flex justify-end gap-2">
          <button onClick={onClose} className="action-secondary">取消</button>
          <button onClick={handleCreate} disabled={creating} className="action-primary">
            {creating ? <Loader2 className="h-4 w-4 animate-spin" /> : <Check className="h-4 w-4" />}
            创建
          </button>
        </div>
      </div>
    </div>
  );
}

function WikiProjectDetail({
  project,
  tab,
  setTab,
  onBack,
  onRefresh,
  onUpdated,
}: {
  project: WikiProject;
  tab: "overview" | "pages" | "sources" | "search" | "graph" | "settings";
  setTab: (t: "overview" | "pages" | "sources" | "search" | "graph" | "settings") => void;
  onBack: () => void;
  onRefresh: () => void;
  onUpdated: (project: WikiProject) => void;
}) {
  const [initialSearchQuery, setInitialSearchQuery] = useState<string | null>(null);

  // ── 目录重建（程序 / LLM）：工具栏置顶 sticky，仅「页面」tab 可见 ──
  const [rebuildMode, setRebuildMode] = useState<"program" | "llm">("program");
  const [rebuildConfirm, setRebuildConfirm] = useState(false);
  const [rebuildBusy, setRebuildBusy] = useState(false);
  const [rebuildError, setRebuildError] = useState<string | null>(null);
  const [rebuildDone, setRebuildDone] = useState<WikiIndexRebuildResult | null>(null);
  // 页面列表搜索词（过滤标题/路径）
  const [pageSearchQuery, setPageSearchQuery] = useState("");

  // 实时测量次级菜单行（Breadcrumb + Tabs）高度 → CSS 变量 --wali-tabs-h。
  // Wiki 页面详情里的"返回列表/编辑/删除"操作行 sticky top 用它做阈值（header + tabs 高度），
  // 保证操作行钉在 Tabs 正下方而不重叠（Tabs 高度随窗口宽度/换行变化）。
  const tabsRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = tabsRef.current;
    if (!el) return;
    const apply = () => {
      document.documentElement.style.setProperty("--wali-tabs-h", `${el.offsetHeight}px`);
    };
    apply();
    const ro = new ResizeObserver(apply);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // 摄入进度使用模块级状态：切换一级侧边栏（服务页整体卸载）不丢失，
  // 回到「源」tab 时未完成任务进度条立即恢复显示；完成/失败/取消后删除条目并 bump 版本刷新列表。
  const wikiProgress = useModuleWikiProgress();
  const [wikiSourcesVersion, setWikiSourcesVersion] = useState(0);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      unlisten = await listen<{ source_id: string; project_id: string; filename: string; stage: string; progress: number; detail: string }>(
        "wiki-source-progress",
        (event) => {
          if (!active) return;
          const p = event.payload;
          if (p.project_id !== project.id) return;
          if (p.stage === "done" || p.stage === "error" || p.stage === "cancelled") {
            patchWikiProgress((prev) => {
              const next = { ...prev };
              delete next[p.source_id];
              return next;
            });
            setWikiSourcesVersion((v) => v + 1);
            if (p.stage === "done") onRefresh();
          } else {
            patchWikiProgress((prev) => ({
              ...prev,
              [p.source_id]: { stage: p.stage, progress: p.progress, detail: p.detail, filename: p.filename || prev[p.source_id]?.filename || "" },
            }));
          }
        }
      );
    })();
    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, [project.id, onRefresh]);
  const tabs = [
    { key: "overview" as const, label: "概览", icon: Layers },
    { key: "pages" as const, label: "页面", icon: FileText },
    { key: "sources" as const, label: "源", icon: FolderOpen },
    { key: "search" as const, label: "搜索", icon: Search },
    { key: "graph" as const, label: "图谱", icon: Network },
    { key: "settings" as const, label: "设置", icon: SettingsIcon },
  ];

  return (
    <div className="space-y-4">
      {/* Breadcrumb + Tabs：滚动时钉在 page-header（sticky top-0，约 88px 高）下方 */}
      <div ref={tabsRef} className="sticky top-[var(--wali-page-header-h,88px)] z-20 -mx-1 flex items-center justify-between rounded-2xl border border-slate-100 bg-white/95 px-4 py-2.5 backdrop-blur">
        <div className="flex items-center gap-2">
          <button onClick={onBack} className="flex items-center gap-1 text-xs text-slate-500 hover:text-slate-900">
            <ChevronRight className="h-3 w-3 rotate-180" /> 返回
          </button>
          <span className="text-slate-300">/</span>
          <Network className="h-4 w-4 text-violet-600" />
          <h2 className="text-lg font-semibold text-slate-900">{project.name}</h2>
        </div>
        <div className="flex items-center gap-1">
          {tabs.map(({ key, label, icon: Icon }) => (
            <button
              key={key}
              onClick={() => setTab(key)}
              className={`flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-medium transition-all ${
                tab === key ? "bg-slate-900 text-white" : "text-slate-500 hover:bg-slate-100 hover:text-slate-900"
              }`}
            >
              <Icon size={13} />
              {label}
            </button>
          ))}
        </div>
      </div>

      {/* 目录重建工具栏：仅在「页面」tab 显示；置顶 sticky，钉在 Breadcrumb + Tabs（设置按钮所在行）正下方，
          不随页面滚动隐藏（log 缺失时一并回溯修复） */}
      {tab === "pages" && (
      <div className="sticky top-[calc(var(--wali-page-header-h,88px)+var(--wali-tabs-h,40px))] z-20 -mx-1 flex flex-wrap items-center gap-3 rounded-2xl border border-slate-100 bg-white/95 px-4 py-2.5 backdrop-blur">
        <div className="flex items-center gap-2">
          <ListTree className="h-4 w-4 text-slate-400" />
          <span className="text-xs font-medium text-slate-700">重建目录</span>
        </div>
        <Select
          value={rebuildMode}
          onChange={(v) => { setRebuildMode(v as "program" | "llm"); setRebuildDone(null); setRebuildError(null); }}
          size="sm"
          ariaLabel="重建目录方式"
          className="w-56 shrink-0"
          options={[
            { value: "program", label: "程序化（按类型分组，零消耗）" },
            { value: "llm", label: "LLM 智能归组（消耗 token）" },
          ]}
        />
        <button
          onClick={() => setRebuildConfirm(true)}
          disabled={rebuildBusy}
          className="action-secondary shrink-0"
          title="按所选方式重建 wiki/index.md；log 缺失时自动回溯修复"
        >
          {rebuildBusy ? <Loader2 className="h-4 w-4 animate-spin" /> : <RefreshCw className="h-4 w-4" />}
          {rebuildBusy ? "重建中..." : "重建"}
        </button>
        {rebuildError && (
          <span className="text-xs text-red-600">重建失败：{rebuildError}</span>
        )}
        {rebuildDone && (
          <span className="text-xs text-emerald-700">
            重建完成：{rebuildDone.page_count} 个页面{rebuildDone.log_rebuilt ? "，并已回溯修复日志" : ""}
          </span>
        )}
        {/* 搜索文档：过滤页面列表（标题/路径关键字） */}
        <div className="ml-auto flex shrink-0 items-center gap-1.5">
          <Search size={14} className="text-slate-400" />
          <input
            value={pageSearchQuery}
            onChange={(e) => setPageSearchQuery(e.target.value)}
            placeholder="搜索文档"
            className="w-52 rounded-lg border border-slate-200 bg-white px-2.5 py-1.5 text-xs text-slate-700 outline-none transition-all placeholder:text-slate-400 hover:border-blue-400 focus:border-blue-400 focus:ring-2 focus:ring-blue-100"
          />
        </div>
      </div>
      )}

      {/* Tab Content */}
      {tab === "overview" && <WikiOverview project={project} onTagClick={(tag) => { setInitialSearchQuery(tag); setTab("search"); }} />}
      {tab === "pages" && <WikiPagesTab project={project} searchQuery={pageSearchQuery} />}
      {tab === "sources" && <WikiSourcesTab project={project} onRefresh={onRefresh} onNavigateSettings={() => setTab("settings")} progressMap={wikiProgress} sourcesVersion={wikiSourcesVersion} />}
      {tab === "search" && <WikiSearchTab project={project} initialQuery={initialSearchQuery} onInitialQueryConsumed={() => setInitialSearchQuery(null)} />}
      {tab === "graph" && <WikiGraphTab project={project} />}
      {tab === "settings" && <WikiSettingsTab project={project} onUpdated={onUpdated} />}

      {/* 目录重建二次确认弹窗（锁定页面滚动；LLM 消耗说明仅在弹窗内提示） */}
      {rebuildConfirm && (
        <RebuildIndexConfirmDialog
          mode={rebuildMode}
          onClose={() => setRebuildConfirm(false)}
          onConfirm={async () => {
            setRebuildConfirm(false);
            setRebuildBusy(true);
            setRebuildError(null);
            setRebuildDone(null);
            try {
              const res = await wikiApi.rebuildIndex(project.id, rebuildMode);
              setRebuildDone(res);
            } catch (e) {
              setRebuildError(String(e));
            } finally {
              setRebuildBusy(false);
            }
          }}
        />
      )}
    </div>
  );
}

function WikiTagsBar({ projectId, onTagClick }: { projectId: string; onTagClick?: (tag: string) => void }) {
  const [tags, setTags] = useState<WikiTag[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let active = true;
    setLoading(true);
    wikiApi.getTags(projectId, 16)
      .then((data) => { if (active) setTags(data); })
      .catch(() => {})
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [projectId]);

  if (loading && tags.length === 0) {
    return (
      <div className="surface data-card rounded-2xl animate-pulse">
        <div className="h-4 w-24 rounded bg-slate-100" />
        <div className="mt-3 flex flex-wrap gap-1.5">
          {Array.from({ length: 6 }).map((_, i) => (
            <div key={i} className="h-5 w-16 rounded-full bg-slate-100" />
          ))}
        </div>
      </div>
    );
  }

  if (tags.length === 0) return null;

  const tagColors = [
    "border-violet-200 bg-violet-50 text-violet-700 hover:bg-violet-100",
    "border-blue-200 bg-blue-50 text-blue-700 hover:bg-blue-100",
    "border-emerald-200 bg-emerald-50 text-emerald-700 hover:bg-emerald-100",
    "border-amber-200 bg-amber-50 text-amber-700 hover:bg-amber-100",
    "border-rose-200 bg-rose-50 text-rose-700 hover:bg-rose-100",
    "border-indigo-200 bg-indigo-50 text-indigo-700 hover:bg-indigo-100",
    "border-cyan-200 bg-cyan-50 text-cyan-700 hover:bg-cyan-100",
    "border-fuchsia-200 bg-fuchsia-50 text-fuchsia-700 hover:bg-fuchsia-100",
  ];

  return (
    <div className="surface data-card rounded-2xl">
      <div className="mb-2 flex items-center gap-1.5">
        <Tag size={13} className="text-slate-400" />
        <span className="text-xs font-medium text-slate-500">标签</span>
        <span className="text-[10px] text-slate-400">·</span>
        <span className="text-[10px] text-slate-400">从页面 frontmatter 自动提取</span>
        {onTagClick && (
          <span className="ml-auto text-[10px] text-slate-400">点击标签快速搜索</span>
        )}
      </div>
      <div className="flex flex-wrap gap-1.5">
        {tags.map((tag, i) => (
          <button
            key={tag.word}
            onClick={() => onTagClick?.(tag.word)}
            className={`inline-flex items-center rounded-full border px-2 py-0.5 text-[10px] font-medium transition-all ${onTagClick ? "cursor-pointer hover:shadow-sm" : "cursor-default"} ${tagColors[i % tagColors.length]}`}
            title={`${tag.count} 个页面`}
          >
            {tag.word}
            <span className="ml-1 text-[9px] opacity-60">{tag.count}</span>
          </button>
        ))}
      </div>
    </div>
  );
}

function WikiOverview({ project, onTagClick }: { project: WikiProject; onTagClick?: (tag: string) => void }) {
  const [stats, setStats] = useState<Record<string, unknown> | null>(null);

  useEffect(() => {
    wikiApi.getStats(project.id).then(setStats).catch(() => {});
  }, [project.id]);

  const metrics = [
    { label: "页面数", value: stats?.pages ?? project.page_count, icon: FileText, color: "text-violet-600", tone: "bg-violet-50" },
    { label: "源资料", value: stats?.sources ?? project.source_count, icon: Layers, color: "text-blue-600", tone: "bg-blue-50" },
    { label: "页面类型", value: "-", icon: Layers, color: "text-emerald-600", tone: "bg-emerald-50" },
  ];

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        {metrics.map(({ label, value, icon: Icon, color, tone }) => (
          <div key={label} className="surface data-card">
            <div className="flex items-center justify-between">
              <div className={`rounded-xl ${tone} p-2`}><Icon className={`h-4 w-4 ${color}`} /></div>
            </div>
            <div className="mt-3 text-2xl font-semibold tracking-tight text-slate-900">{String(value)}</div>
            <div className="text-xs text-slate-500">{label}</div>
          </div>
        ))}
      </div>

      <WikiTagsBar projectId={project.id} onTagClick={onTagClick} />

      <div className="surface data-card rounded-2xl">
        <div className="mb-3 flex items-center gap-2">
          <Network className="h-4 w-4 text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">项目信息</h3>
        </div>
        <div className="space-y-2">
          <div className="flex items-center justify-between rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
            <span className="text-xs font-medium text-slate-500">项目 ID</span>
            <code className="text-xs text-slate-700">{project.id.slice(0, 8)}...</code>
          </div>
          <div className="flex items-center justify-between rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
            <span className="text-xs font-medium text-slate-500">摄入模型</span>
            <span className="text-xs text-slate-800">{project.ingest_model || "默认路由"}</span>
          </div>
          <div className="flex items-center justify-between rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
            <span className="text-xs font-medium text-slate-500">查询模型</span>
            <span className="text-xs text-slate-800">{project.chat_model || "默认路由"}</span>
          </div>
          <div className="flex items-center justify-between rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
            <span className="text-xs font-medium text-slate-500">MCP 状态</span>
            <span className={`text-xs font-medium ${project.mcp_enabled === 1 ? "text-violet-600" : "text-slate-400"}`}>
              {project.mcp_enabled === 1 ? "已暴露" : "未暴露"}
            </span>
          </div>
          <div className="flex items-center justify-between rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
            <span className="text-xs font-medium text-slate-500">创建时间</span>
            <span className="text-xs text-slate-800">{project.created_at.slice(0, 19).replace("T", " ")}</span>
          </div>
          {project.last_ingest_at && (
            <div className="flex items-center justify-between rounded-lg border border-slate-100 bg-slate-50 px-3 py-2">
              <span className="text-xs font-medium text-slate-500">最后摄入</span>
              <span className="text-xs text-slate-800">{project.last_ingest_at.slice(0, 19).replace("T", " ")}</span>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function WikiPagesTab({ project, searchQuery }: { project: WikiProject; searchQuery: string }) {
  const [pages, setPages] = useState<WikiPage[]>([]);
  const [loading, setLoading] = useState(true);
  const [selectedPage, setSelectedPage] = useState<string | null>(null);
  const [pageContent, setPageContent] = useState("");
  const [editing, setEditing] = useState(false);
  const [editContent, setEditContent] = useState("");
  const [saving, setSaving] = useState(false);
  const [pageDeleteTarget, setPageDeleteTarget] = useState<string | null>(null);

  // 列表 ↔ MD 详情的滚动位置管理（滚动容器为 Layout 的 [data-wali-scroll]）：
  // 进入详情始终从顶部显示；返回列表恢复到点击时的位置，避免双向继承滚动位置。
  const listScrollRef = useRef(0);
  const needRestoreScrollRef = useRef(false);
  const getScroller = () => document.querySelector<HTMLElement>("[data-wali-scroll]");

  useEffect(() => {
    const scroller = getScroller();
    if (!scroller) return;
    if (selectedPage) {
      scroller.scrollTo({ top: 0 });
    } else if (needRestoreScrollRef.current) {
      needRestoreScrollRef.current = false;
      scroller.scrollTo({ top: listScrollRef.current });
    }
  }, [selectedPage]);

  const fetchPages = useCallback(async () => {
    setLoading(true);
    try { const data = await wikiApi.getPages(project.id); setPages(data); }
    catch { /* ignore */ } finally { setLoading(false); }
  }, [project.id]);

  useEffect(() => { fetchPages(); }, [fetchPages]);

  const openPage = async (path: string) => {
    listScrollRef.current = getScroller()?.scrollTop ?? 0;
    setSelectedPage(path);
    setEditing(false);
    try {
      const page = await wikiApi.getPage(project.id, path);
      setPageContent(page.content || "");
    } catch { setPageContent("无法加载页面内容"); }
  };

  const backToList = () => {
    needRestoreScrollRef.current = true;
    setSelectedPage(null);
  };

  const doDeletePage = async (path: string) => {
    try {
      await wikiApi.deletePage(project.id, path);
      setSelectedPage(null);
      setEditing(false);
      await fetchPages();
    } catch { /* ignore */ }
  };

  const handleSave = async () => {
    if (!selectedPage) return;
    setSaving(true);
    try {
      await wikiApi.savePage(project.id, selectedPage, editContent);
      setPageContent(editContent);
      setEditing(false);
      await fetchPages();
    } catch { /* ignore */ } finally { setSaving(false); }
  };

  if (loading) {
    return <div className="surface empty-state"><Loader2 className="h-8 w-8 animate-spin text-slate-400" /></div>;
  }

  if (pages.length === 0 && !selectedPage) {
    return (
      <div className="surface empty-state">
        <FileText className="h-10 w-10 text-slate-300" />
        <p className="text-sm text-slate-500">还没有 Wiki 页面</p>
        <p className="text-xs text-slate-400">摄入源文档后 LLM 会自动生成页面</p>
      </div>
    );
  }

  if (selectedPage) {
    return (
      <>
        <div className="space-y-3">
        {/* 次级菜单（返回列表/编辑/删除）：滚动时钉在 Tabs（设置按钮所在菜单）正下方。
            阈值 = page-header 高度 + Tabs 行高度（两变量均由 ResizeObserver 实时测量）。 */}
        <div className="sticky top-[calc(var(--wali-page-header-h,88px)+var(--wali-tabs-h,40px))] z-20 -mx-1 flex items-center justify-between gap-3 rounded-2xl border border-slate-100 bg-white/95 px-4 py-2.5 backdrop-blur">
          <div className="flex min-w-0 flex-1 items-center gap-2">
            <button
              onClick={backToList}
              className="inline-flex shrink-0 items-center gap-1 text-xs text-slate-500 hover:text-slate-900"
            >
              <ChevronRight className="h-3 w-3 rotate-180" /> 返回列表
            </button>
            <span className="shrink-0 text-slate-300">/</span>
            <code className="min-w-0 truncate text-xs text-slate-700" title={selectedPage}>{selectedPage}</code>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {editing ? (
              <>
                <button onClick={() => setEditing(false)} className="action-secondary">取消</button>
                <button onClick={handleSave} disabled={saving} className="action-primary">
                  {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : <Save className="h-4 w-4" />}
                  保存
                </button>
              </>
            ) : (
              <>
                <button onClick={() => { setEditContent(pageContent); setEditing(true); }} className="action-secondary inline-flex items-center gap-1">
                  <Edit3 size={14} /> 编辑
                </button>
                <button onClick={() => setPageDeleteTarget(selectedPage)} className="inline-flex items-center gap-1 whitespace-nowrap rounded-xl border border-red-200 px-3 py-1.5 text-sm text-red-600 hover:bg-red-50">
                  <Trash2 size={14} /> 删除
                </button>
              </>
            )}
          </div>
        </div>
        {editing ? (
          <textarea
            value={editContent}
            onChange={e => setEditContent(e.target.value)}
            className="w-full h-[60vh] rounded-xl border border-slate-200 bg-white p-4 text-xs font-mono"
          />
        ) : (
          <div className="surface rounded-2xl p-6">
            <WikiMarkdown content={pageContent} />
          </div>
        )}
      </div>

      {pageDeleteTarget && (
        <ConfirmDialog
          message={`确定删除页面 "${pageDeleteTarget}"？此操作不可恢复。`}
          onCancel={() => setPageDeleteTarget(null)}
          onConfirm={() => { const p = pageDeleteTarget; setPageDeleteTarget(null); doDeletePage(p); }}
        />
      )}
      </>
    );
  }

  const typeColors: Record<string, string> = {
    entity: "bg-blue-50 text-blue-600",
    concept: "bg-violet-50 text-violet-600",
    summary: "bg-emerald-50 text-emerald-600",
    index: "bg-amber-50 text-amber-600",
    log: "bg-slate-50 text-slate-500",
  };

  // 按搜索词过滤（标题/路径包含匹配，大小写不敏感）
  const q = searchQuery.trim().toLowerCase();
  const visiblePages = q
    ? pages.filter(p => p.title.toLowerCase().includes(q) || p.path.toLowerCase().includes(q))
    : pages;

  return (
    <div className="surface rounded-2xl overflow-hidden">
      <div className="divide-y divide-slate-50">
        {visiblePages.map(p => (
          <div key={p.id} className="flex items-center gap-3 px-4 py-3 hover:bg-slate-50 cursor-pointer" onClick={() => openPage(p.path)}>
            <span className={`rounded-full px-2 py-0.5 text-[10px] font-medium ${typeColors[p.page_type] || "bg-slate-50 text-slate-500"}`}>{p.page_type}</span>
            <div className="min-w-0 flex-1">
              <p className="text-sm font-medium text-slate-900 truncate">{p.title}</p>
              <code className="text-[11px] text-slate-400">{p.path}</code>
            </div>
            <span className="text-[10px] text-slate-400">{p.token_count} tokens</span>
            <ChevronRight className="h-4 w-4 text-slate-300" />
          </div>
        ))}
        {visiblePages.length === 0 && (
          <div className="px-4 py-10 text-center text-sm text-slate-400">
            {q ? `没有匹配「${searchQuery.trim()}」的页面` : "还没有 Wiki 页面"}
          </div>
        )}
      </div>
    </div>
  );
}

// ─── 目录重建二次确认弹窗（程序 / LLM；锁定页面滚动）─────────────────────
function RebuildIndexConfirmDialog({ mode, onClose, onConfirm }: { mode: "program" | "llm"; onClose: () => void; onConfirm: () => void }) {
  // 弹窗期间锁定页面滚动（关闭时恢复）
  useEffect(() => {
    document.body.style.overflow = "hidden";
    return () => { document.body.style.overflow = ""; };
  }, []);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4 backdrop-blur-sm" onClick={onClose}>
      <div className="relative w-full max-w-md rounded-3xl bg-white p-7 shadow-2xl" onClick={e => e.stopPropagation()}>
        <button onClick={onClose} className="absolute right-5 top-5 rounded-full p-1 text-slate-400 hover:bg-slate-100 hover:text-slate-600">
          <XCircle className="h-5 w-5" />
        </button>
        <div className="flex items-center gap-2">
          <div className="rounded-2xl border border-violet-100 bg-violet-50 p-2.5">
            <RefreshCw className="h-5 w-5 text-violet-600" />
          </div>
          <h2 className="text-lg font-semibold text-slate-900">重建 Wiki 目录</h2>
        </div>
        <div className="mt-4 space-y-2 text-xs text-slate-600">
          {mode === "program" ? (
            <>
              <p>• 按页面类型（实体/概念/摘要/评审）重新生成 wiki/index.md，覆盖旧目录。</p>
              <p>• 零 LLM 消耗、毫秒级完成；log.md 缺失时自动从源资料回溯修复。</p>
            </>
          ) : (
            <>
              <p>• 将调用一次 LLM，对全部页面做语义归组后重写 wiki/index.md（覆盖旧目录）。</p>
              <p>• 会消耗 token（使用项目问答/摄入模型与渠道）。</p>
              <p>• LLM 漏掉的页面会自动补到《未分组》，不会丢失任何页面；失败则自动回退为程序化重建。</p>
            </>
          )}
        </div>
        <div className="mt-5 flex justify-end gap-2">
          <button onClick={onClose} className="action-secondary">取消</button>
          <button onClick={onConfirm} className="action-primary">
            <RefreshCw className="h-4 w-4" />
            确认重建
          </button>
        </div>
      </div>
    </div>
  );
}

function WikiSourcesTab({ project, onRefresh, onNavigateSettings, progressMap, sourcesVersion }: { project: WikiProject; onRefresh: () => void; onNavigateSettings: () => void; progressMap: Record<string, { stage: string; progress: number; detail: string; filename: string }>; sourcesVersion: number }) {
  const [sources, setSources] = useState<WikiSource[]>([]);
  const [loading, setLoading] = useState(true);
  const [ingesting, setIngesting] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [uploadTotal, setUploadTotal] = useState(0);
  const [uploadingCount, setUploadingCount] = useState(0);
  const [sourceFileDeleteTarget, setSourceFileDeleteTarget] = useState<{ message: string; run: () => void } | null>(null);

  const fetchSources = useCallback(async () => {
    setLoading(true);
    try { const data = await wikiApi.getSources(project.id); setSources(data); }
    catch { /* ignore */ } finally { setLoading(false); }
  }, [project.id]);

  // 首次加载 + 父组件进度监听在任务完成/失败/取消时 bump sourcesVersion → 刷新列表
  useEffect(() => { fetchSources(); }, [fetchSources, sourcesVersion]);

  // 桌面端文件拖放：tauri OS 级拖放只给路径（HTML5 drop 被接管不触发），
  // 故用 runtime.onFileDrop 拿路径 → 后端按路径读文件落盘 → 自动摄入。浏览器端为 no-op。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    onFileDrop(async (paths) => {
      if (!active) return;
      setUploadTotal(paths.length);
      setUploadingCount(0);
      for (const p of paths) {
        try {
          const source = await wikiApi.addSourceFromPath(project.id, p);
          setUploadingCount((prev) => prev + 1);
          setIngesting(source.id);
          wikiApi.ingestSource(project.id, source.id).catch((e) => {
            setError(`摄入失败: ${e}`);
            setIngesting(null);
          });
        } catch (e) {
          setError(`上传失败: ${e}`);
        }
      }
      setUploadTotal(0);
      setUploadingCount(0);
      await fetchSources();
      onRefresh();
    }).then((un) => {
      if (active) unlisten = un;
      else un();
    });
    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, [project.id, fetchSources, onRefresh]);

  const handleDelete = async (id: string) => {
    setSourceFileDeleteTarget({
      message: "确定删除此源文件？",
      run: async () => {
        await wikiApi.deleteSource(id);
        await fetchSources();
        onRefresh();
      },
    });
  };

  // 停止处理：取消进行中的摄入（OCR/LLM 检查点尽快退出，不再继续后续步骤）
  const handleCancelIngest = async (id: string) => {
    try {
      await wikiApi.cancelIngest(project.id, id);
      setIngesting(null);
      await fetchSources();
    } catch (e) {
      setError(`停止失败: ${e}`);
    }
  };

  const handleUploadBatch = async (files: File[]) => {
    if (files.length === 0) return;
    setUploadTotal(files.length);
    setUploadingCount(0);
    for (const file of files) {
      try {
        // PDF 是二进制：base64 传输，后端解码为原始字节再落盘（否则 UTF-8 解码会变乱码）；
        // 其余纯文本照旧按文本读取。
        const ext = (file.name.split('.').pop() || 'txt').toLowerCase();
        const content = ext === 'pdf' ? await fileToBase64(file) : await file.text();
        const source = await wikiApi.addSource(project.id, {
          source_type: ext,
          filename: file.name,
          content,
        });
        setUploadingCount(prev => prev + 1);
        // Auto-ingest after upload
        setIngesting(source.id);
        wikiApi.ingestSource(project.id, source.id).catch(e => {
          setError(`摄入失败: ${e}`);
          setIngesting(null);
        });
      } catch (e) {
        setError(`上传失败 ${file.name}: ${e}`);
      }
    }
    setUploadTotal(0);
    setUploadingCount(0);
    await fetchSources();
    onRefresh();
  };

  const handleIngest = async (sourceId: string) => {
    setIngesting(sourceId);
    try {
      await wikiApi.ingestSource(project.id, sourceId);
      await fetchSources();
      onRefresh();
    } catch (e) {
      setError(`摄入失败: ${e}`);
    } finally {
      setIngesting(null);
    }
  };

  const handleRescanAll = async () => {
    setIngesting('rescan');
    try {
      await wikiApi.rescanSources(project.id);
      await fetchSources();
      onRefresh();
    } catch (e) {
      setError(`重新扫描失败: ${e}`);
    } finally {
      setIngesting(null);
    }
  };

  const isChannelError = error && (error.includes('No channel available') || error.includes('No active channel'));

  if (loading) {
    return <div className="surface empty-state"><Loader2 className="h-8 w-8 animate-spin text-slate-400" /></div>;
  }

  const statusColors: Record<string, string> = {
    pending: "bg-amber-50 text-amber-600",
    ingested: "bg-emerald-50 text-emerald-600",
    failed: "bg-red-50 text-red-500",
    cancelled: "bg-slate-100 text-slate-500",
  };

  // Collect active progress entries (not done, not error)
  const activeProgresses = Object.entries(progressMap);

  return (
    <div className="space-y-4">
      {/* Upload zone — matches RAG style */}
      <label
        className="flex cursor-pointer items-center justify-center rounded-2xl border-2 border-dashed border-slate-300 bg-white px-6 py-8 transition-colors hover:border-violet-400 hover:bg-violet-50/30"
        onDragOver={(e) => { e.preventDefault(); e.dataTransfer.dropEffect = "copy"; }}
        onDrop={(e) => {
          e.preventDefault();
          const files = Array.from(e.dataTransfer.files || []);
          if (files.length > 0) handleUploadBatch(files);
        }}
      >
        <input
          type="file"
          className="hidden"
          multiple
          accept=".md,.txt,.json,.yaml,.yml,.rs,.ts,.tsx,.js,.py,.go,.java,.c,.cpp,.h,.sh,.toml,.xml,.html,.css,.pdf"
          onChange={(e) => {
            const files = Array.from(e.target.files || []);
            if (files.length > 0) handleUploadBatch(files);
            e.target.value = "";
          }}
          disabled={uploadTotal > 0}
        />
        {uploadTotal > 0 ? (
          <div className="flex items-center gap-2 text-sm text-violet-600">
            <Loader2 className="h-5 w-5 animate-spin" />
            上传中 {uploadingCount}/{uploadTotal}...
          </div>
        ) : (
          <div className="flex flex-col items-center gap-2 text-sm text-slate-500">
            <Upload className="h-6 w-6" />
            <span>点击上传文件（支持多选）</span>
            <span className="text-xs text-slate-400">支持 md/txt/code/json/yaml/pdf，上传后自动摄入</span>
          </div>
        )}
      </label>

      {/* Error notice */}
      {error && (
        <div className={`rounded-2xl border p-4 ${isChannelError ? 'border-amber-200 bg-amber-50' : 'border-red-200 bg-red-50'}`}>
          <div className="flex items-start gap-3">
            <AlertTriangle className={`h-5 w-5 shrink-0 ${isChannelError ? 'text-amber-500' : 'text-red-500'}`} />
            <div className="flex-1">
              <p className="text-sm font-medium text-slate-900">{isChannelError ? '渠道未配置' : '操作失败'}</p>
              <p className="mt-0.5 text-xs text-slate-500">{error}</p>
              {isChannelError && (
                <button onClick={() => { setError(null); onNavigateSettings(); }} className="mt-2 inline-flex items-center gap-1.5 rounded-lg bg-slate-900 px-3 py-1.5 text-xs font-medium text-white hover:bg-slate-800">
                  <SettingsIcon size={13} /> 前往设置配置渠道
                </button>
              )}
              {!isChannelError && (
                <button onClick={() => setError(null)} className="mt-2 text-xs text-slate-500 hover:text-slate-900">关闭</button>
              )}
            </div>
          </div>
        </div>
      )}

      {/* Active ingest progress bars */}
      {activeProgresses.length > 0 && (
        <div className="space-y-2">
          {activeProgresses.map(([sid, prog]) => (
            <div key={sid} className="surface flex items-center gap-3 rounded-xl px-4 py-3">
              <Loader2 className="h-5 w-5 shrink-0 animate-spin text-violet-500" />
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="truncate text-sm font-medium text-slate-900">
                    {prog.filename || sources.find(s => s.id === sid)?.filename || "摄入中..."}
                  </span>
                </div>
                <div className="mt-1.5">
                  <div className="flex items-center gap-2">
                    <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-slate-200">
                      <div
                        className="h-full rounded-full bg-violet-500 transition-all duration-300"
                        style={{ width: `${prog.progress}%` }}
                      />
                    </div>
                    <span className="shrink-0 text-[11px] text-violet-600">
                      {prog.detail} · {prog.progress}%
                    </span>
                  </div>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Toolbar */}
      <div className="flex justify-end gap-2">
        <button onClick={() => fetchSources()} disabled={loading} className="action-secondary">
          {loading ? <Loader2 className="h-4 w-4 animate-spin" /> : <RefreshCw className="h-4 w-4" />}
          刷新
        </button>
        {(sources.some(s => s.status === 'pending' || s.status === 'failed')) && (
          <button onClick={handleRescanAll} disabled={!!ingesting} className="action-secondary">
            {ingesting === 'rescan' ? <Loader2 className="h-4 w-4 animate-spin" /> : <RefreshCw className="h-4 w-4" />}
            全部处理
          </button>
        )}
      </div>

      {/* Sources list */}
      {sources.length === 0 ? (
        <div className="surface empty-state">
          <FolderOpen className="h-10 w-10 text-slate-300" />
          <p className="text-sm text-slate-500">还没有源文件</p>
          <p className="text-xs text-slate-400">上传文档后 LLM 会自动摄入并生成 Wiki 页面</p>
        </div>
      ) : (
        <div className="surface rounded-2xl overflow-hidden">
          <div className="divide-y divide-slate-50">
            {sources.map(s => (
              <div key={s.id} className="flex items-center gap-3 px-4 py-3">
                <FileText className="h-4 w-4 text-slate-400 shrink-0" />
                <div className="min-w-0 flex-1">
                  <p className="text-sm font-medium text-slate-900 truncate">{s.filename}</p>
                  <div className="flex items-center gap-3 text-[11px] text-slate-400">
                    <span>{(s.file_size / 1024).toFixed(1)} KB</span>
                    {s.page_count > 0 && <span>{s.page_count} 页面</span>}
                    {s.ingested_at && <span>{s.ingested_at.slice(0, 10)}</span>}
                    {s.status === 'failed' && s.error_message && (
                      <span className="text-red-500" title={s.error_message}>{s.error_message.slice(0, 50)}</span>
                    )}
                  </div>
                </div>
                <span className={`rounded-full px-2 py-0.5 text-[10px] font-medium ${statusColors[s.status] || "bg-slate-50 text-slate-500"}`}>{s.status}</span>
                {(ingesting === s.id || s.status === 'pending') && (
                  <button onClick={() => handleCancelIngest(s.id)} className="rounded-lg p-1.5 text-red-500 hover:bg-red-50" title="停止处理">
                    <XCircle className="h-3.5 w-3.5" />
                  </button>
                )}
                {(s.status === 'pending' || s.status === 'failed' || s.status === 'cancelled') && (
                  <button onClick={() => handleIngest(s.id)} disabled={!!ingesting} className="rounded-lg p-1.5 text-violet-500 hover:bg-violet-50 disabled:opacity-50" title={s.status === 'failed' || s.status === 'cancelled' ? '重新摄入' : '摄入'}>
                    {ingesting === s.id ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Sparkles className="h-3.5 w-3.5" />}
                  </button>
                )}
                <button onClick={() => handleDelete(s.id)} className="rounded-lg p-1.5 text-slate-400 hover:bg-red-50 hover:text-red-500">
                  <Trash2 className="h-3.5 w-3.5" />
                </button>
              </div>
            ))}
          </div>
        </div>
      )}

      {sourceFileDeleteTarget && (
        <ConfirmDialog
          message={sourceFileDeleteTarget.message}
          onCancel={() => setSourceFileDeleteTarget(null)}
          onConfirm={() => { sourceFileDeleteTarget.run(); setSourceFileDeleteTarget(null); }}
        />
      )}
    </div>
  );
}

function WikiSearchTab({ project, initialQuery, onInitialQueryConsumed }: { project: WikiProject; initialQuery?: string | null; onInitialQueryConsumed?: () => void }) {
  const [query, setQueryDirect] = useState(initialQuery ?? "");
  const [results, setResults] = useState<WikiSearchResult[]>([]);
  const [searching, setSearching] = useState(false);
  const [searched, setSearched] = useState(false);
  const [tags, setTags] = useState<WikiTag[]>([]);
  const [tagsLoading, setTagsLoading] = useState(false);

  // Load tags for preset search terms
  useEffect(() => {
    let active = true;
    setTagsLoading(true);
    wikiApi.getTags(project.id, 12)
      .then((data) => { if (active) setTags(data); })
      .catch(() => {})
      .finally(() => { if (active) setTagsLoading(false); });
    return () => { active = false; };
  }, [project.id]);

  // Auto-trigger search when initialQuery arrives from tag click in overview
  useEffect(() => {
    if (initialQuery) {
      setQueryDirect(initialQuery);
      handleSearch(initialQuery);
      onInitialQueryConsumed?.();
    }
   // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialQuery]);

  const handleSearch = async (searchQuery?: string) => {
    const q = (searchQuery ?? query).trim();
    if (!q) return;
    if (searchQuery) setQueryDirect(searchQuery);
    setSearching(true);
    setSearched(true);
    try { const data = await wikiApi.search(project.id, q, 10); setResults(data); }
    catch { /* ignore */ } finally { setSearching(false); }
  };

  const typeColors: Record<string, string> = {
    entity: "bg-blue-50 text-blue-600",
    concept: "bg-violet-50 text-violet-600",
    summary: "bg-emerald-50 text-emerald-600",
  };

  return (
    <div className="space-y-4">
      <div className="surface rounded-2xl p-5">
        <div className="flex gap-2">
          <input
            value={query}
            onChange={e => setQueryDirect(e.target.value)}
            onKeyDown={e => e.key === 'Enter' && !e.nativeEvent.isComposing && e.keyCode !== 229 && handleSearch()}
            placeholder="搜索 Wiki 页面..."
            className="flex-1 rounded-xl border border-slate-200 px-4 py-2.5 text-sm"
          />
          <button onClick={() => handleSearch()} disabled={searching} className="action-primary">
            {searching ? <Loader2 className="h-4 w-4 animate-spin" /> : <Search className="h-4 w-4" />}
            搜索
          </button>
        </div>

        {/* Preset search terms from tags */}
        {(tagsLoading || tags.length > 0) && (
          <div className="mt-3 flex flex-wrap items-center gap-2">
            <span className="flex items-center gap-1 text-[11px] font-medium text-slate-400">
              <Sparkles size={12} />
              快速搜索
            </span>
            {tagsLoading ? (
              <>
                {[...Array(5)].map((_, i) => (
                  <div key={i} className="h-6 w-16 animate-pulse rounded-full bg-slate-100" />
                ))}
              </>
            ) : (
              tags.map((tag) => (
                <button
                  key={tag.word}
                  onClick={() => handleSearch(tag.word)}
                  className="inline-flex items-center rounded-full border border-slate-200 bg-gradient-to-br from-slate-50 to-white px-3 py-1 text-xs font-medium text-slate-600 transition-all hover:border-blue-200 hover:bg-blue-50 hover:text-blue-600 hover:shadow-sm"
                  title={`${tag.count} 个页面`}
                >
                  {tag.word}
                  <span className="ml-1 text-[9px] opacity-50">{tag.count}</span>
                </button>
              ))
            )}
          </div>
        )}
      </div>

      {results.length > 0 && (
        <div className="surface rounded-2xl overflow-hidden">
          <div className="divide-y divide-slate-50">
            {results.map((r) => (
              <div key={r.page_id} className="px-4 py-3 hover:bg-slate-50">
                <div className="flex items-center gap-2">
                  <span className={`rounded-full px-2 py-0.5 text-[10px] font-medium ${typeColors[r.page_type] || "bg-slate-50 text-slate-500"}`}>{r.page_type}</span>
                  <p className="text-sm font-medium text-slate-900">{r.title}</p>
                  {r.score > 0 && (
                    <span className="ml-auto text-[10px] text-slate-400">{r.score.toFixed(2)}</span>
                  )}
                </div>
                <code className="text-[11px] text-slate-400">{r.path}</code>
                {r.snippet && (
                  <p className="mt-1.5 text-xs leading-relaxed text-slate-500 line-clamp-2">{r.snippet}</p>
                )}
              </div>
            ))}
          </div>
        </div>
      )}
      {searched && !searching && results.length === 0 && (
        <div className="surface empty-state">
          <Inbox className="h-10 w-10 text-slate-300" />
          <p className="text-sm text-slate-500">未找到匹配的页面</p>
          <p className="text-xs text-slate-400">试试点击下方标签快速搜索</p>
        </div>
      )}
    </div>
  );
}

const NODE_COLORS: Record<string, string> = {
  entity: "#3b82f6",
  concept: "#8b5cf6",
  summary: "#10b981",
  index: "#f59e0b",
  log: "#94a3b8",
};

const NODE_COLOR_BG: Record<string, string> = {
  entity: "bg-blue-500",
  concept: "bg-violet-500",
  summary: "bg-emerald-500",
  index: "bg-amber-500",
  log: "bg-slate-400",
};

interface SimNode {
  id: string;
  label: string;
  node_type: string;
  link_count: number;
  x: number;
  y: number;
  vx: number;
  vy: number;
  fx?: number | null;
  fy?: number | null;
}

function useForceSimulation(graph: WikiGraphData | null) {
  const [tick, setTick] = useState(0);
  const dragRef = useRef<SimNode | null>(null);
  const hoverRef = useRef<string | null>(null);
  const animRef = useRef<number>(0);
  const nodesRef = useRef<SimNode[]>([]);

  // Initialize nodes when graph changes
  useEffect(() => {
    if (!graph || graph.nodes.length === 0) { nodesRef.current = []; return; }
    const cx = 400, cy = 225;
    nodesRef.current = graph.nodes.map((n, i) => {
      const angle = (i / graph.nodes.length) * Math.PI * 2;
      return {
        id: n.id,
        label: n.label,
        node_type: n.node_type,
        link_count: n.link_count,
        x: cx + Math.cos(angle) * 180 + (Math.random() - 0.5) * 20,
        y: cy + Math.sin(angle) * 120 + (Math.random() - 0.5) * 20,
        vx: 0, vy: 0,
        fx: null, fy: null,
      };
    });
    setTick(t => t + 1);
  }, [graph]);

  // Build adjacency list
  const adjacency = useMemo(() => {
    const adj = new Map<string, Set<string>>();
    if (!graph) return adj;
    graph.nodes.forEach(n => adj.set(n.id, new Set()));
    graph.edges.forEach(e => {
      adj.get(e.source)?.add(e.target);
      adj.get(e.target)?.add(e.source);
    });
    return adj;
  }, [graph]);

  // Animation loop
  useEffect(() => {
    if (!graph || graph.nodes.length === 0) return;
    const W = 800, H = 450;
    const repulsion = 1500; // Charge strength
    const linkDistance = 120;
    const linkStrength = 0.08;
    const centerForce = 0.015;
    const damping = 0.82;
    const maxSpeed = 8;

    const edges = graph.edges;
    const nodeMap = new Map<string, SimNode>();
    nodesRef.current.forEach(n => nodeMap.set(n.id, n));

    let running = true;
    let alpha = 1.0;
    const alphaDecay = 0.008;
    const alphaMin = 0.02;

    const step = () => {
      if (!running) return;
      const ns = nodesRef.current;

      // Repulsion (Coulomb's law approximation)
      for (let i = 0; i < ns.length; i++) {
        for (let j = i + 1; j < ns.length; j++) {
          const a = ns[i], b = ns[j];
          let dx = a.x - b.x;
          let dy = a.y - b.y;
          let dist2 = dx * dx + dy * dy;
          if (dist2 < 0.01) { dist2 = 0.01; dx = Math.random() - 0.5; dy = Math.random() - 0.5; }
          const dist = Math.sqrt(dist2);
          const force = repulsion / dist2;
          const fx = (dx / dist) * force;
          const fy = (dy / dist) * force;
          a.vx += fx; a.vy += fy;
          b.vx -= fx; b.vy -= fy;
        }
      }

      // Link attraction (spring)
      for (const edge of edges) {
        const s = nodeMap.get(edge.source);
        const t = nodeMap.get(edge.target);
        if (!s || !t) continue;
        const dx = t.x - s.x;
        const dy = t.y - s.y;
        const dist = Math.sqrt(dx * dx + dy * dy) || 1;
        const diff = dist - linkDistance;
        const force = diff * linkStrength * alpha;
        const fx = (dx / dist) * force;
        const fy = (dy / dist) * force;
        s.vx += fx; s.vy += fy;
        t.vx -= fx; t.vy -= fy;
      }

      // Center gravity
      for (const n of ns) {
        n.vx += (400 - n.x) * centerForce * alpha;
        n.vy += (225 - n.y) * centerForce * alpha;
      }

      // Apply velocity with damping and boundary
      for (const n of ns) {
        if (n.fx != null) { n.x = n.fx; n.vx = 0; }
        else {
          n.vx *= damping;
          n.vy *= damping;
          const speed = Math.sqrt(n.vx * n.vx + n.vy * n.vy);
          if (speed > maxSpeed) { n.vx = (n.vx / speed) * maxSpeed; n.vy = (n.vy / speed) * maxSpeed; }
          n.x += n.vx;
          n.y += n.vy;
          n.x = Math.max(30, Math.min(W - 30, n.x));
          n.y = Math.max(30, Math.min(H - 30, n.y));
        }
        if (n.fy != null) { n.y = n.fy; n.vy = 0; }
      }

      alpha = Math.max(alphaMin, alpha - alphaDecay);
      setTick(t => t + 1);

      if (alpha > alphaMin) {
        animRef.current = requestAnimationFrame(step);
      } else {
        running = false;
      }
    };

    animRef.current = requestAnimationFrame(step);
    return () => { running = false; cancelAnimationFrame(animRef.current); };
  }, [graph, adjacency]);

  // Drag handlers
  const handleDragStart = (node: SimNode) => {
    dragRef.current = node;
    node.fx = node.x;
    node.fy = node.y;
  };
  const handleDragMove = (node: SimNode, x: number, y: number) => {
    if (dragRef.current === node) {
      node.fx = x;
      node.fy = y;
      setTick(t => t + 1);
    }
  };
  const handleDragEnd = (node: SimNode) => {
    dragRef.current = null;
    node.fx = null;
    node.fy = null;
    // Reheat
    const ns = nodesRef.current;
    for (const n of ns) { n.vx += (Math.random() - 0.5) * 2; n.vy += (Math.random() - 0.5) * 2; }
  };

  const setHover = (id: string | null) => { hoverRef.current = id; setTick(t => t + 1); };

  return { nodes: nodesRef.current, tick, handleDragStart, handleDragMove, handleDragEnd, setHover, hoverId: hoverRef.current, adjacency };
}

function WikiGraphTab({ project }: { project: WikiProject }) {
  const [graph, setGraph] = useState<WikiGraphData | null>(null);
  const [loading, setLoading] = useState(true);
  const svgRef = useRef<SVGSVGElement>(null);

  useEffect(() => {
    wikiApi.getGraph(project.id).then(setGraph).catch(() => {}).finally(() => setLoading(false));
  }, [project.id]);

  const sim = useForceSimulation(graph);
  const [selectedNode, setSelectedNode] = useState<string | null>(null);

  const maxLinks = graph ? Math.max(...graph.nodes.map(n => n.link_count), 1) : 1;

  const handleNodeMouseDown = (node: SimNode) => (e: React.MouseEvent) => {
    e.preventDefault();
    sim.handleDragStart(node);
    const moveHandler = (ev: MouseEvent) => {
      const svg = svgRef.current;
      if (!svg) return;
      const pt = svg.createSVGPoint();
      pt.x = ev.clientX; pt.y = ev.clientY;
      const ctm = svg.getScreenCTM();
      if (!ctm) return;
      const transformed = pt.matrixTransform(ctm.inverse());
      sim.handleDragMove(node, transformed.x, transformed.y);
    };
    const upHandler = () => {
      sim.handleDragEnd(node);
      document.removeEventListener("mousemove", moveHandler);
      document.removeEventListener("mouseup", upHandler);
    };
    document.addEventListener("mousemove", moveHandler);
    document.addEventListener("mouseup", upHandler);
  };

  if (loading) {
    return <div className="surface empty-state"><Loader2 className="h-8 w-8 animate-spin text-slate-400" /></div>;
  }

  if (!graph || graph.nodes.length === 0) {
    return (
      <div className="surface empty-state">
        <Network className="h-10 w-10 text-slate-300" />
        <p className="text-sm text-slate-500">还没有知识图谱</p>
        <p className="text-xs text-slate-400">摄入文档并生成页面后，图谱会自动构建</p>
      </div>
    );
  }

  const hoveredId = sim.hoverId;

  // Highlight neighbors
  const isHighlighted = (id: string): boolean => {
    if (!hoveredId && !selectedNode) return false;
    const active = hoveredId || selectedNode;
    if (id === active) return true;
    return sim.adjacency.get(active || "")?.has(id) ?? false;
  };

  const isEdgeHighlighted = (edge: { source: string; target: string }): boolean => {
    if (!hoveredId && !selectedNode) return false;
    const active = hoveredId || selectedNode;
    return edge.source === active || edge.target === active;
  };

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-3 gap-3">
        <div className="surface data-card">
          <div className="text-2xl font-semibold text-slate-900">{graph.nodes.length}</div>
          <div className="text-xs text-slate-500">节点</div>
        </div>
        <div className="surface data-card">
          <div className="text-2xl font-semibold text-slate-900">{graph.edges.length}</div>
          <div className="text-xs text-slate-500">边</div>
        </div>
        <div className="surface data-card">
          <div className="text-2xl font-semibold text-slate-900">{new Set(graph.edges.map(e => e.edge_type)).size}</div>
          <div className="text-xs text-slate-500">边类型</div>
        </div>
      </div>
      <div className="surface rounded-2xl p-6">
        <div className="mb-3 flex items-center gap-2">
          <Network className="h-4 w-4 text-slate-700" />
          <h3 className="text-sm font-semibold text-slate-900">知识图谱</h3>
          <span className="ml-auto text-[11px] text-slate-400">力导向布局 · 拖拽节点 · Hover 高亮关联</span>
        </div>
        <svg
          ref={svgRef}
          viewBox="0 0 800 450"
          className="w-full select-none"
          style={{ width: "100%", height: "420px", margin: "0 auto", cursor: "default" }}
        >
          <defs>
            {Object.entries(NODE_COLORS).map(([type, color]) => (
              <radialGradient key={type} id={`grad-${type}`} cx="35%" cy="35%">
                <stop offset="0%" stopColor={color} stopOpacity="1" />
                <stop offset="100%" stopColor={color} stopOpacity="0.6" />
              </radialGradient>
            ))}
            <marker id="arrow" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
              <path d="M 0 0 L 10 5 L 0 10 z" fill="rgba(99, 102, 241, 0.5)" />
            </marker>
            <marker id="arrow-active" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
              <path d="M 0 0 L 10 5 L 0 10 z" fill="rgba(59, 130, 246, 0.9)" />
            </marker>
          </defs>
          {/* Edges */}
          {graph.edges.map((edge, i) => {
            const s = sim.nodes.find(n => n.id === edge.source);
            const t = sim.nodes.find(n => n.id === edge.target);
            if (!s || !t) return null;
            const highlighted = isEdgeHighlighted(edge);
            const dimmed = (hoveredId || selectedNode) && !highlighted;
            // Shorten edge to not overlap node circle
            const dx = t.x - s.x;
            const dy = t.y - s.y;
            const dist = Math.sqrt(dx * dx + dy * dy) || 1;
            const sRadius = 10 + (s.link_count / maxLinks) * 16;
            const tRadius = 10 + (t.link_count / maxLinks) * 16;
            const x1 = s.x + (dx / dist) * sRadius;
            const y1 = s.y + (dy / dist) * sRadius;
            const x2 = t.x - (dx / dist) * (tRadius + 4);
            const y2 = t.y - (dy / dist) * (tRadius + 4);
            // Curve control point — perpendicular offset for visual appeal
            const mx = (x1 + x2) / 2;
            const my = (y1 + y2) / 2;
            const curveOffset = Math.min(dist * 0.12, 20);
            const cx = mx + (-dy / dist) * curveOffset;
            const cy = my + (dx / dist) * curveOffset;
            return (
              <path
                key={i}
                d={`M ${x1} ${y1} Q ${cx} ${cy} ${x2} ${y2}`}
                fill="none"
                stroke={highlighted ? "rgba(59, 130, 246, 0.7)" : "rgba(99, 102, 241, 0.35)"}
                strokeWidth={highlighted ? Math.max(2, edge.weight * 2.5) : Math.max(1.2, edge.weight * 1.5)}
                markerEnd={highlighted ? "url(#arrow-active)" : "url(#arrow)"}
                opacity={dimmed ? 0.12 : 1}
                style={{ transition: "opacity 0.2s, stroke 0.2s, stroke-width 0.2s" }}
              />
            );
          })}
          {/* Nodes */}
          {sim.nodes.map((node) => {
            const radius = 10 + (node.link_count / maxLinks) * 16;
            const color = NODE_COLORS[node.node_type] || "#64748b";
            const highlighted = isHighlighted(node.id);
            const dimmed = (hoveredId || selectedNode) && !highlighted;
            const isSelected = selectedNode === node.id;
            return (
              <g
                key={node.id}
                style={{ cursor: "grab" }}
                onMouseDown={handleNodeMouseDown(node)}
                onMouseEnter={() => sim.setHover(node.id)}
                onMouseLeave={() => sim.setHover(null)}
                onClick={(e) => { e.stopPropagation(); setSelectedNode(isSelected ? null : node.id); }}
              >
                {/* Highlight ring */}
                {(highlighted || isSelected) && (
                  <circle cx={node.x} cy={node.y} r={radius + 4} fill="none" stroke={color} strokeWidth="2" opacity="0.3" className="animate-pulse" />
                )}
                {/* Node glow */}
                <circle
                  cx={node.x} cy={node.y} r={radius + 2}
                  fill={color}
                  opacity={dimmed ? 0.05 : 0.15}
                  style={{ transition: "opacity 0.2s" }}
                />
                <circle
                  cx={node.x} cy={node.y} r={radius}
                  fill={`url(#grad-${node.node_type})`}
                  opacity={dimmed ? 0.3 : 0.92}
                  stroke={dimmed ? "rgba(148, 163, 184, 0.2)" : "rgba(255, 255, 255, 0.8)"}
                  strokeWidth="1.5"
                  style={{ transition: "opacity 0.2s" }}
                />
                {/* Label: always show when few nodes, otherwise on hover/selected */}
                {(highlighted || isSelected || graph.nodes.length <= 25) && (
                  <text
                    x={node.x} y={node.y - radius - 5}
                    textAnchor="middle"
                    className={highlighted || isSelected ? "fill-slate-900" : "fill-slate-600"}
                    style={{
                      fontSize: highlighted || isSelected ? "14px" : "11px",
                      fontWeight: highlighted || isSelected ? 700 : 500,
                      pointerEvents: "none",
                      paintOrder: "stroke",
                      stroke: "white",
                      strokeWidth: 3,
                      opacity: dimmed ? 0.3 : 1,
                      transition: "opacity 0.2s",
                    }}
                  >
                    {node.label.length > 16 ? node.label.slice(0, 14) + "…" : node.label}
                  </text>
                )}
                <title>{node.label} ({node.link_count} links)</title>
              </g>
            );
          })}
        </svg>
        {/* Legend */}
        <div className="mt-3 flex flex-wrap items-center gap-3">
          {Object.entries({ entity: "实体", concept: "概念", summary: "摘要", index: "索引", log: "日志" }).map(([type, label]) => (
            <span key={type} className="flex items-center gap-1.5 text-[11px] text-slate-500">
              <span className={`h-2.5 w-2.5 rounded-full ${NODE_COLOR_BG[type]}`} />
              {label}
            </span>
          ))}
          <span className="ml-auto text-[11px] text-slate-400">点击节点高亮 · 拖拽节点调整布局</span>
        </div>
      </div>
    </div>
  );
}

function ChannelModelPicker({
  channels,
  channelId,
  onChannelChange,
  model,
  onModelChange,
  allowAuto = false,
  autoLabel = "同摄入渠道",
  stacked = false,
}: {
  channels: Channel[];
  channelId: string;
  onChannelChange: (id: string) => void;
  model: string;
  onModelChange: (m: string) => void;
  allowAuto?: boolean;
  autoLabel?: string;
  stacked?: boolean;
}) {
  const [showChannelPicker, setShowChannelPicker] = useState(false);
  const [showModelPicker, setShowModelPicker] = useState(false);
  const selectedChannel = channels.find(c => c.id === channelId);
  const channelModels = selectedChannel?.models ?? [];

  const handleChannel = (id: string) => {
    onChannelChange(id);
    const ch = channels.find(c => c.id === id);
    onModelChange(ch?.models[0] || "");
    setShowChannelPicker(false);
  };

  return (
    <div className={stacked ? "flex flex-col gap-2" : "flex gap-2"}>{/* Channel picker */}
      <div className="relative flex-1">
        <button
          type="button"
          onClick={() => { setShowChannelPicker(!showChannelPicker); setShowModelPicker(false); }}
          className="flex w-full items-center justify-between rounded-xl border border-slate-200 bg-white px-3 py-2 text-sm outline-none transition-all hover:border-blue-400 hover:shadow-sm"
        >
          <span className={selectedChannel ? "text-slate-900 truncate" : "text-slate-400"}>
            {selectedChannel ? selectedChannel.name : (allowAuto ? autoLabel : "选择渠道")}
          </span>
          <ChevronDown size={14} className={`shrink-0 text-slate-400 transition-transform ${showChannelPicker ? "rotate-180" : ""}`} />
        </button>
        {showChannelPicker && (
          <>
            <div className="fixed inset-0 z-40" onClick={() => setShowChannelPicker(false)} />
            <div className="absolute left-0 top-full z-50 mt-1.5 w-full rounded-2xl border border-slate-200 bg-white p-2 shadow-xl max-h-[280px] overflow-auto">
              {allowAuto && (
                <button
                  type="button"
                  onClick={() => { onChannelChange(""); onModelChange(""); setShowChannelPicker(false); }}
                  className={`flex w-full items-center justify-between rounded-xl px-3 py-2.5 text-sm transition-all ${
                    channelId === "" ? "bg-blue-50 text-blue-600 font-semibold" : "text-slate-700 hover:bg-slate-50"
                  }`
                  }
                >
                  <span>{autoLabel}</span>
                  {channelId === "" && <Check size={14} className="shrink-0" />}
                </button>
              )}
              {channels.length === 0 ? (
                <div className="px-3 py-2 text-xs text-slate-400">暂无可用渠道</div>
              ) : channels.map(c => (
                <button
                  key={c.id}
                  type="button"
                  onClick={() => handleChannel(c.id)}
                  className={`flex w-full items-center justify-between rounded-xl px-3 py-2.5 text-sm transition-all ${
                    channelId === c.id ? "bg-blue-50 text-blue-600 font-semibold" : "text-slate-700 hover:bg-slate-50"
                  }`}
                >
                  <div className="flex items-center gap-2 min-w-0">
                    <span className="truncate">{c.name}</span>
                    <span className="rounded-full bg-slate-100 px-1.5 py-0.5 text-[10px] text-slate-500 shrink-0">{c.type}</span>
                  </div>
                  {channelId === c.id && <Check size={14} className="shrink-0" />}
                </button>
              ))}
            </div>
          </>
        )}
      </div>

      {/* Model picker */}
      <div className="relative flex-1">
        <button
          type="button"
          onClick={() => { setShowModelPicker(!showModelPicker); setShowChannelPicker(false); }}
          disabled={!channelId}
          className="flex w-full items-center justify-between rounded-xl border border-slate-200 bg-white px-3 py-2 text-sm outline-none transition-all hover:border-blue-400 hover:shadow-sm disabled:opacity-50 disabled:cursor-not-allowed"
        >
          <span className={model ? "text-slate-900 truncate font-mono" : "text-slate-400"}>
            {model || "选择模型"}
          </span>
          <ChevronDown size={14} className={`shrink-0 text-slate-400 transition-transform ${showModelPicker ? "rotate-180" : ""}`} />
        </button>
        {showModelPicker && channelId && (
          <>
            <div className="fixed inset-0 z-40" onClick={() => setShowModelPicker(false)} />
            <div className="absolute left-0 top-full z-50 mt-1.5 w-full rounded-2xl border border-slate-200 bg-white p-2 shadow-xl max-h-[280px] overflow-auto">
              <div className="px-2 py-1.5 text-[11px] font-semibold text-slate-400/70 uppercase tracking-wide">
                {selectedChannel?.name} 模型
              </div>
              {channelModels.length === 0 ? (
                <div className="px-3 py-2 text-xs text-slate-400">该渠道未配置模型</div>
              ) : channelModels.map(m => (
                <button
                  key={m}
                  type="button"
                  onClick={() => { onModelChange(m); setShowModelPicker(false); }}
                  className={`flex w-full items-center justify-between rounded-xl px-3 py-2.5 text-sm font-mono transition-all ${
                    model === m ? "bg-blue-50 text-blue-600 font-semibold" : "text-slate-700 hover:bg-slate-50"
                  }`}
                >
                  <span className="truncate">{m}</span>
                  {model === m && <Check size={14} className="shrink-0" />}
                </button>
              ))}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function WikiSettingsTab({ project, onUpdated }: { project: WikiProject; onUpdated: (project: WikiProject) => void }) {
  const [name, setName] = useState(project.name);
  const [description, setDescription] = useState(project.description || "");
  const [ingestModel, setIngestModel] = useState(project.ingest_model || "");
  const [chatModel, setChatModel] = useState(project.chat_model || "");
  const [ingestChannelId, setIngestChannelId] = useState(project.ingest_channel_id || "");
  const [chatChannelId, setChatChannelId] = useState(project.chat_channel_id || "");
  const [pdfOcrChannelId, setPdfOcrChannelId] = useState(project.pdf_ocr_channel_id || "");
  const [pdfOcrModel, setPdfOcrModel] = useState(project.pdf_ocr_model || "");
  const [reasoningEffort, setReasoningEffort] = useState(project.reasoning_effort || "");
  const [temperature, setTemperature] = useState(project.temperature != null ? String(project.temperature) : "");
  const [schemaText, setSchemaText] = useState(project.schema_text || "");
  const [mcpEnabled, setMcpEnabled] = useState(project.mcp_enabled ?? 1);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [channels, setChannels] = useState<Channel[]>([]);

  useEffect(() => {
    channelApi.getAll().then(list => setChannels(list.filter(c => c.status === 1))).catch(() => {});
  }, []);

  const handleSave = async () => {
    setSaving(true);
    try {
      const updated = await wikiApi.updateProject(project.id, {
        name, description, ingest_model: ingestModel || undefined,
        chat_model: chatModel || undefined, schema_text: schemaText || undefined,
        ingest_channel_id: ingestChannelId || undefined,
        chat_channel_id: chatChannelId || undefined,
        // 空串=显式「不启用」（后端以 Some("") 覆盖旧值；摄入时按未配置处理）
        pdf_ocr_channel_id: pdfOcrChannelId,
        pdf_ocr_model: pdfOcrModel,
        // 推理档位：空串=跟随默认（摄入未配置时默认 high）；温度：空=不修改
        reasoning_effort: reasoningEffort,
        temperature: temperature.trim() === "" ? undefined : Number(temperature),
        mcp_enabled: mcpEnabled,
      });
      onUpdated(updated);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch { /* ignore */ } finally { setSaving(false); }
  };

  return (
    <div className="surface rounded-2xl p-6 space-y-5">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-slate-900">项目设置</h3>
        <button onClick={handleSave} disabled={saving} className="action-primary">
          {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : saved ? <Check className="h-4 w-4" /> : <Save className="h-4 w-4" />}
          {saved ? "已保存" : "保存"}
        </button>
      </div>
      {/* 两列六项布局：行1 基础信息+摄入；行2 对话+PDF 视觉；行3 推理档位+MCP。
          Wiki Schema 在 grid 外独占整行宽度（位置保持在最底部）。 */}
      <div className="grid grid-cols-1 gap-5 md:grid-cols-2">
        <div className="rounded-2xl border border-slate-100 p-4 space-y-3">
          <h4 className="text-xs font-semibold text-slate-900">项目基础信息</h4>
          <div>
            <label className="mb-1.5 block text-xs font-medium text-slate-700">项目名称</label>
            <input value={name} onChange={e => setName(e.target.value)} className="w-full rounded-xl border border-slate-200 px-3.5 py-2.5 text-sm" />
          </div>
          <div>
            <label className="mb-1.5 block text-xs font-medium text-slate-700">描述</label>
            <input value={description} onChange={e => setDescription(e.target.value)} className="w-full rounded-xl border border-slate-200 px-3.5 py-2.5 text-sm" />
          </div>
        </div>
        <div className="rounded-2xl border border-slate-100 p-4">
          <label className="mb-1.5 block text-xs font-medium text-slate-700">摄入渠道 & 模型</label>
          <p className="mb-2 text-[11px] text-slate-400">用于 LLM 解析文档并生成 Wiki 页面</p>
          <ChannelModelPicker
            channels={channels}
            channelId={ingestChannelId}
            onChannelChange={setIngestChannelId}
            model={ingestModel}
            onModelChange={setIngestModel}
            allowAuto
            autoLabel="自动选择"
            stacked
          />
        </div>
        <div className="rounded-2xl border border-slate-100 p-4">
          <label className="mb-1.5 block text-xs font-medium text-slate-700">对话渠道 & 模型</label>
          <p className="mb-2 text-[11px] text-slate-400">用于 Wiki 问答</p>
          <ChannelModelPicker
            channels={channels}
            channelId={chatChannelId}
            onChannelChange={setChatChannelId}
            model={chatModel}
            onModelChange={setChatModel}
            allowAuto
            autoLabel="同摄入渠道"
            stacked
          />
        </div>
        <div className="rounded-2xl border border-slate-100 p-4">
          <div className="mb-1.5 flex items-center justify-between">
            <label className="text-xs font-medium text-slate-700">PDF 视觉模型</label>
            {(pdfOcrChannelId || pdfOcrModel) && (
              <button
                type="button"
                onClick={() => { setPdfOcrChannelId(""); setPdfOcrModel(""); }}
                className="text-[11px] text-slate-400 hover:text-slate-600"
              >
                不启用
              </button>
            )}
          </div>
          <p className="mb-2 text-[11px] text-slate-400">摄入含扫描页/图文页的 PDF 时用该模型识别整页文字（复用渠道视觉模型）；纯文字层 PDF 不需要配置。未配置时扫描版 PDF 摄入会报错提示。</p>
          <ChannelModelPicker
            channels={channels}
            channelId={pdfOcrChannelId}
            onChannelChange={setPdfOcrChannelId}
            model={pdfOcrModel}
            onModelChange={setPdfOcrModel}
            stacked
          />
        </div>
        <div className="rounded-2xl border border-slate-100 p-4 space-y-3">
          <div>
            <label className="mb-1.5 block text-xs font-medium text-slate-700">推理档位（思考级别）</label>
            <p className="mb-2 text-[11px] text-slate-400">摄入与问答使用的 LLM 推理档位。未配置或选 default 时不写档位字段与 thinking，完全跟随模型默认（对严格校验的上游最安全）；显式选择档位时自动联动 thinking。</p>
            <Select
              value={reasoningEffort}
              onChange={v => setReasoningEffort(v)}
              size="sm"
              ariaLabel="推理档位（思考级别）"
              options={[
                { value: "", label: "跟随模型默认（不写档位/thinking）" },
                { value: "default", label: "default（模型默认）" },
                { value: "none", label: "none（关闭推理）" },
                { value: "low", label: "low" },
                { value: "medium", label: "medium" },
                { value: "high", label: "high" },
              ]}
            />
          </div>
          <div>
            <label className="mb-1.5 block text-xs font-medium text-slate-700">温度（0~2）</label>
            <p className="mb-2 text-[11px] text-slate-400">留空不修改；摄入默认 0.3，问答默认 0.4。</p>
            <input
              type="number"
              min={0}
              max={2}
              step={0.1}
              value={temperature}
              onChange={e => setTemperature(e.target.value)}
              placeholder="留空"
              className="w-full rounded-xl border border-slate-200 px-3.5 py-2.5 text-sm"
            />
          </div>
        </div>
        <div className="rounded-2xl border border-slate-100 p-4">
          <label className="flex items-center gap-2 cursor-pointer">
            <input type="checkbox" checked={mcpEnabled === 1} onChange={(e) => setMcpEnabled(e.target.checked ? 1 : 0)} className="rounded" />
            <span className="text-sm text-slate-700">MCP 暴露</span>
          </label>
          <p className="mt-1.5 text-[11px] text-slate-400">关闭后该 Wiki 项目不出现在 MCP 工具列表中。仍需在密钥页为 Key 授权该项目，Key 才有查询权限。</p>
        </div>
      </div>
      <div>
        <label className="mb-1.5 block text-xs font-medium text-slate-700">Wiki Schema (CLAUDE.md)</label>
        <p className="mb-1.5 text-[11px] text-slate-400">定义 LLM 维护 Wiki 的规则</p>
        <textarea value={schemaText} onChange={e => setSchemaText(e.target.value)} rows={10} className="w-full rounded-xl border border-slate-200 px-3.5 py-2.5 text-xs font-mono" />
      </div>
    </div>
  );
}
