import { useEffect, useState, useRef, useCallback } from "react";
import { channelApi, importExportApi } from "../lib/api";
import type { ChannelStats } from "../lib/api";
import type { Channel } from "../types";
import { getProtocolLabel, getChannelProviderLabel, formatTime, formatNumber, formatDuration } from "../lib/constants";
import { downloadTextFile, isWebRuntime } from "../lib/web";
import { Plus, Radio, Trash2, Zap, Power, Edit, Download, ChevronDown, Upload, Loader2, X, Activity, Clock, GripVertical, Eye, EyeOff, Copy, Check, AlertCircle, Terminal } from "lucide-react";
import { ChannelForm } from "../components/ChannelForm";
import { ImportDialog } from "../components/ImportDialog";
import { MappingChips } from "../components/MappingChips";
import { ChannelTabs } from "../components/layout/ChannelTabs";
import { writeClipboard } from "../lib/runtime";
import { buildChannelCurl } from "../lib/curl";

/** Key 启用/停用状态胶囊：主 Key 与额外 Keys 共用同一组件，保证样式永远一致。 */
function KeyStatusPill({ enabled, loading, onToggle }: { enabled: boolean; loading: boolean; onToggle: () => void }) {
  return (
    <button
      onClick={onToggle}
      disabled={loading}
      className="shrink-0 transition-colors disabled:opacity-50"
      title={enabled ? "停用（不参与负载均衡）" : "启用"}
    >
      {loading ? (
        <Loader2 size={11} className="animate-spin" />
      ) : (
        <span className={`inline-flex items-center gap-0.5 rounded-full px-1.5 py-0.5 text-[10px] font-medium leading-none ${enabled ? "bg-emerald-100 text-emerald-600" : "bg-slate-200 text-slate-400"}`}>
          <Power size={9} />
          {enabled ? "启用" : "已停用"}
        </span>
      )}
    </button>
  );
}

export function ChannelsPage() {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [channelStats, setChannelStats] = useState<Record<string, ChannelStats>>({});
  const [showForm, setShowForm] = useState(false);
  const [editing, setEditing] = useState<Channel | null>(null);
  const [duplicating, setDuplicating] = useState(false);
  const [testing, setTesting] = useState<string | null>(null);
  const [testResult, setTestResult] = useState<Record<string, { success: boolean; message: string; latency_ms: number }>>({});
  const [showImport, setShowImport] = useState(false);
  const [showImportMenu, setShowImportMenu] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<Channel | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [loadError, setLoadError] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [draggedId, setDraggedId] = useState<string | null>(null);
  const [dragOverId, setDragOverId] = useState<string | null>(null);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [showKeyMap, setShowKeyMap] = useState<Record<string, boolean>>({});
  const [fullKeyMap, setFullKeyMap] = useState<Record<string, string>>({});
  const [keyLoading, setKeyLoading] = useState<string | null>(null);
  const [extraKeyVisibleMap, setExtraKeyVisibleMap] = useState<Record<string, boolean>>({});
  const [extraKeyFullMap, setExtraKeyFullMap] = useState<Record<string, string>>({});
  const [extraKeyLoading, setExtraKeyLoading] = useState<string | null>(null);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const [copiedModel, setCopiedModel] = useState<string | null>(null);
  const [copiedCurl, setCopiedCurl] = useState<string | null>(null);
  const importMenuRef = useRef<HTMLDivElement>(null);
  const dragCounter = useRef(0);

  const load = useCallback(() => {
    channelApi.getAll().then(setChannels).catch(() => {});
    channelApi.getStats().then(stats => {
      const map: Record<string, ChannelStats> = {};
      stats.forEach(s => { map[s.channel_id] = s; });
      setChannelStats(map);
    }).catch(() => {});
    Promise.all([channelApi.getAll(), channelApi.getStats()])
      .then(() => setLoadError(false))
      .catch(() => setLoadError(true));
  }, []);

  useEffect(() => { load(); }, [load]);

  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (importMenuRef.current && !importMenuRef.current.contains(e.target as Node)) {
        setShowImportMenu(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, []);

  const handleExport = async () => {
    // 导出内容包含全部渠道的明文 API 密钥（GAP-05）：误点即把密钥落盘到
    // 下载目录，先确认再执行。
    if (!confirm("导出的 JSON 包含所有渠道的明文 API 密钥，请妥善保管。确定导出？")) return;
    setExporting(true);
    try {
      const content = await importExportApi.exportChannels();
      const timestamp = new Date().toISOString().slice(0, 10);
      const filename = `waliapi-export-${timestamp}.json`;
      if (isWebRuntime()) {
        downloadTextFile(content, filename);
      } else {
        await importExportApi.saveExportFile(content, filename);
      }
    } catch (e) {
      console.error("Export failed:", e);
    }
    setExporting(false);
  };

  const handleTest = async (id: string) => {
    setTesting(id);
    try {
      const result = await channelApi.test(id);
      setTestResult(prev => ({ ...prev, [id]: result }));
    } catch (e: any) {
      setTestResult(prev => ({ ...prev, [id]: { success: false, message: String(e), latency_ms: 0 } }));
    }
    setTesting(null);
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      await channelApi.delete(deleteTarget.id);
      setDeleteTarget(null);
      load();
    } catch (e) {
      setActionError(`删除渠道失败: ${String(e)}`);
    } finally {
      setDeleting(false);
    }
  };

  const handleToggle = async (ch: Channel) => {
    const newStatus = ch.status === 1 ? 0 : 1;
    try {
      await channelApi.toggle(ch.id, newStatus);
      load();
    } catch (e) {
      console.error("Failed to toggle channel:", e);
      setActionError(`切换渠道状态失败: ${String(e)}`);
    }
  };

  // 映射模型 开启/关闭（迁移 041）：乐观更新 + 即时保存，失败回滚
  const handleToggleMapping = async (ch: Channel, name: string, target: string, currentlyOff: boolean) => {
    const current = ch.model_mapping_disabled ?? [];
    const next = currentlyOff
      ? current.filter(d => !(d[0] === name && d[1] === target))
      : [...current.filter(d => !(d[0] === name && d[1] === target)), [name, target]];
    setChannels(prev => prev.map(c => (c.id === ch.id ? { ...c, model_mapping_disabled: next } : c)));
    try {
      await channelApi.update({ id: ch.id, model_mapping_disabled: next });
    } catch (e) {
      setChannels(prev => prev.map(c => (c.id === ch.id ? { ...c, model_mapping_disabled: current } : c)));
      console.error("Failed to toggle mapping:", e);
      setActionError(`切换映射状态失败: ${String(e)}`);
    }
  };

  // API Key 显示/隐藏切换
  const handleToggleKey = async (ch: Channel) => {    const next = !showKeyMap[ch.id];
    setShowKeyMap(prev => ({ ...prev, [ch.id]: next }));
    if (next && !fullKeyMap[ch.id]) {
      setKeyLoading(ch.id);
      try {
        const fullKey = await channelApi.getApiKey(ch.id);
        setFullKeyMap(prev => ({ ...prev, [ch.id]: fullKey }));
      } catch (e) {
        console.error("Failed to get API key:", e);
        setShowKeyMap(prev => ({ ...prev, [ch.id]: false }));
      } finally {
        setKeyLoading(null);
      }
    }
  };

  // 切换额外 Key 显隐
  const handleToggleExtraKey = async (keyId: string) => {
    const next = !extraKeyVisibleMap[keyId];
    setExtraKeyVisibleMap(prev => ({ ...prev, [keyId]: next }));
    if (next && !extraKeyFullMap[keyId]) {
      setExtraKeyLoading(keyId);
      try {
        const fullKey = await channelApi.getExtraKeyValue(keyId);
        setExtraKeyFullMap(prev => ({ ...prev, [keyId]: fullKey }));
      } catch (e) {
        console.error("Failed to get extra key value:", e);
        setExtraKeyVisibleMap(prev => ({ ...prev, [keyId]: false }));
      } finally {
        setExtraKeyLoading(null);
      }
    }
  };

  // 主 Key 是否参与调度：api_key_enabled（迁移 044，缺省 = 启用）
  const primaryKeyEnabled = (ch: Channel) => (ch.api_key_enabled ?? 1) !== 0;

  // 主 Key 启用/禁用（迁移 044 窄列更新，不触碰渠道身份）：乐观更新 + 失败回滚
  const [primaryKeyLoading, setPrimaryKeyLoading] = useState<string | null>(null);
  const handleTogglePrimaryKey = async (ch: Channel) => {
    const nextEnabled = !primaryKeyEnabled(ch);
    setPrimaryKeyLoading(ch.id);
    setChannels(prev => prev.map(c => (c.id === ch.id ? { ...c, api_key_enabled: nextEnabled ? 1 : 0 } : c)));
    try {
      await channelApi.togglePrimaryKey(ch.id, nextEnabled);
    } catch (e) {
      setChannels(prev => prev.map(c => (c.id === ch.id ? { ...c, api_key_enabled: nextEnabled ? 0 : 1 } : c)));
      console.error("Failed to toggle primary key:", e);
      setActionError(`切换主 Key 状态失败: ${String(e)}`);
    } finally {
      setPrimaryKeyLoading(null);
    }
  };

  // 额外 Key 启用/禁用（迁移 023 status 字段）：乐观更新 + 失败回滚
  const [extraKeyToggleLoading, setExtraKeyToggleLoading] = useState<string | null>(null);
  const handleSetExtraKeyStatus = async (ch: Channel, keyId: string, enable: boolean) => {
    const next = enable ? 1 : 0;
    const rollback = () =>
      setChannels(prev => prev.map(c => (c.id === ch.id
        ? { ...c, extra_keys: (c.extra_keys ?? []).map(k => (k.id === keyId ? { ...k, status: enable ? 0 : 1 } : k)) }
        : c)));
    setExtraKeyToggleLoading(keyId);
    setChannels(prev => prev.map(c => (c.id === ch.id
      ? { ...c, extra_keys: (c.extra_keys ?? []).map(k => (k.id === keyId ? { ...k, status: next } : k)) }
      : c)));
    try {
      await channelApi.toggleExtraKey(keyId, next);
    } catch (e) {
      rollback();
      console.error("Failed to toggle extra key status:", e);
      setActionError(`切换 Key 状态失败: ${String(e)}`);
    } finally {
      setExtraKeyToggleLoading(null);
    }
  };

  // 复制 API Key
  const handleCopyModel = async (model: string) => {
    try {
      await writeClipboard(model);
      setCopiedModel(model);
      setTimeout(() => setCopiedModel(null), 2000);
    } catch {}
  };

  const handleCopyKey = async (ch: Channel) => {
    try {
      // 列表里的 api_key 是掩码（****/前4后4），直接复制没有意义；
      // 未加载过全量时按需取一次再复制（FIX-24），缓存供后续显隐复用。
      let keyToCopy = fullKeyMap[ch.id];
      if (!keyToCopy) {
        keyToCopy = await channelApi.getApiKey(ch.id);
        setFullKeyMap(prev => ({ ...prev, [ch.id]: keyToCopy }));
      }
      await writeClipboard(keyToCopy);
      setCopiedKey(ch.id);
      setTimeout(() => setCopiedKey(null), 2000);
    } catch (e) {
      console.error("Failed to copy:", e);
      setActionError("复制失败：无法获取完整密钥");
    }
  };

  // 复制测试 curl：按渠道协议/URL/模型生成可直接执行的 curl 命令（含真实 Key）
  const handleCopyCurl = async (ch: Channel) => {
    try {
      let keyToCopy = fullKeyMap[ch.id];
      if (!keyToCopy) {
        keyToCopy = await channelApi.getApiKey(ch.id);
        setFullKeyMap(prev => ({ ...prev, [ch.id]: keyToCopy }));
      }
      const curl = buildChannelCurl({
        protocol: ch.protocol,
        baseUrl: ch.native_base_url || ch.base_url,
        models: ch.models,
        apiKey: keyToCopy,
        extraHeaders: (ch.request_headers ?? [])
          .filter(h => h.status === 1)
          .map(h => ({ name: h.name, value: h.value })),
      });
      await writeClipboard(curl);
      setCopiedCurl(ch.id);
      setTimeout(() => setCopiedCurl(null), 2000);
    } catch (e) {
      console.error("Failed to copy curl:", e);
      setActionError("复制 curl 失败：无法获取完整密钥");
    }
  };

  // 拖拽排序
  const handleDragStart = (e: React.DragEvent, id: string) => {
    setDraggedId(id);
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", id);
  };

  const handleDragEnd = () => {
    setDraggedId(null);
    setDragOverId(null);
    dragCounter.current = 0;
  };

  const handleDragOver = (e: React.DragEvent, id: string) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    if (id !== draggedId) setDragOverId(id);
  };

  const handleDrop = async (e: React.DragEvent, targetId: string) => {
    e.preventDefault();
    if (!draggedId || draggedId === targetId) return;
    const fromIdx = channels.findIndex(c => c.id === draggedId);
    const toIdx = channels.findIndex(c => c.id === targetId);
    if (fromIdx === -1 || toIdx === -1) return;
    const next = [...channels];
    const [moved] = next.splice(fromIdx, 1);
    next.splice(toIdx, 0, moved);
    setChannels(next);
    setDraggedId(null);
    setDragOverId(null);
    try {
      await channelApi.reorder(next.map(c => c.id));
    } catch (err) {
      console.error("reorder failed:", err);
      load(); // revert on failure
    }
  };

  return (
    <div className="page-shell space-y-6">
      <div className="page-header sticky top-0 z-30 -mx-7 -mt-7 mb-2 flex-col bg-white/90 px-7 pt-3 backdrop-blur-md">
        <div className="flex w-full items-start justify-between gap-4 pb-1.5">
          <div>
            <h1 className="page-title">渠道管理</h1>
            <p className="page-subtitle mt-0.5">配置上游 API 供应商与调度优先级</p>
          </div>
        <div className="flex items-center gap-2">
          <><button onClick={handleExport} disabled={exporting} className="action-secondary flex items-center gap-1.5">
            {exporting ? <Loader2 size={16} className="animate-spin" /> : <Download size={16} />}
            导出
          </button>
          <div className="relative" ref={importMenuRef}>
            <button onClick={() => setShowImportMenu(!showImportMenu)} className="action-secondary flex items-center gap-1.5">
              <Upload size={16} />
              导入
              <ChevronDown size={14} className={`transition-transform ${showImportMenu ? "rotate-180" : ""}`} />
            </button>
            {showImportMenu && (
              <div className="absolute right-0 top-full mt-1.5 z-40 w-64 rounded-2xl border border-border bg-white p-2 shadow-xl">
                <button
                  onClick={() => { setShowImportMenu(false); setShowImport(true); }}
                  className="flex w-full items-center gap-2.5 rounded-xl px-3 py-2.5 text-sm transition-all hover:bg-muted/60"
                >
                  <Upload size={16} className="text-muted-foreground" />
                  <div className="text-left">
                    <div>导入渠道</div>
                    <div className="text-xs text-muted-foreground">WaLiAPI 导出 / 扫描本地 / WaLiCode 备份</div>
                  </div>
                </button>
              </div>
            )}
          </div></>
          <button onClick={() => { setEditing(null); setDuplicating(false); setShowForm(true); }} className="action-primary">
            <Plus size={16} /> 新建渠道
          </button>
        </div>
        </div>
        <ChannelTabs />
      </div>

      {actionError && (
        <div className="flex items-center justify-between rounded-2xl border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
          <span>{actionError}</span>
          <button onClick={() => setActionError(null)} className="ml-3 shrink-0 text-red-400 transition-colors hover:text-red-600">
            <X size={16} />
          </button>
        </div>
      )}

      {channels.length === 0 ? (
        <div className="surface empty-state">
          {loadError ? (
            <>
              <AlertCircle className="h-12 w-12 text-red-400/70" />
              <p className="text-base font-medium">数据加载失败</p>
              <p className="text-sm text-muted-foreground">请检查服务是否已启动，或点击下方按钮重试</p>
              <button onClick={() => load()} className="mt-2 rounded-lg bg-blue-600 px-4 py-2 text-xs font-medium text-white hover:bg-blue-700">重新加载</button>
            </>
          ) : (
            <>
              <Radio className="h-12 w-12 text-muted-foreground/70" />
              <p className="text-base font-medium">还没有配置任何渠道</p>
              <p className="text-sm text-muted-foreground">先添加一个上游服务商，即可开始分发请求</p>
            </>
          )}
        </div>
      ) : (
        <div className="space-y-3">
          {channels.map((ch, idx) => {
            const result = testResult[ch.id];
            const stats = channelStats[ch.id];
            const isDragging = draggedId === ch.id;
            const isDragOver = dragOverId === ch.id;
            const isExpanded = expandedId === ch.id;
            const keyVisible = showKeyMap[ch.id];
            // 双标签（设计 3.5）：第一 [协议]，第二 [提供商]，来自规范化身份。
            // 后端 DTO 对旧配置也实时推断 protocol/provider，故直接显示真实协议；
            // 仅当协议值未知/缺失时才回退到 getProtocolLabel 的 "旧配置"。
            const protocolLabel = getProtocolLabel(ch.protocol);
            const providerLabel = getChannelProviderLabel(ch.provider);
            const displayBaseUrl = ch.native_base_url || ch.base_url;
            return (
              <div
                key={ch.id}
                draggable
                onDragStart={(e) => handleDragStart(e, ch.id)}
                onDragEnd={handleDragEnd}
                onDragOver={(e) => handleDragOver(e, ch.id)}
                onDrop={(e) => handleDrop(e, ch.id)}
                className={`group surface rounded-2xl p-4 transition-all ${
                  isDragging ? "opacity-40 scale-[0.98]" : ""
                } ${
                  isDragOver ? "ring-2 ring-blue-400 ring-offset-1" : ""
                }`}
              >
                <div className="flex items-center gap-3 cursor-pointer" onClick={() => setExpandedId(isExpanded ? null : ch.id)}>
                  {/* 拖拽手柄 */}
                  <div className="flex cursor-grab items-center text-slate-300 transition-colors hover:text-slate-400 active:cursor-grabbing" onClick={e => e.stopPropagation()}>
                    <GripVertical size={18} />
                  </div>

                  {/* 排序序号 */}
                  <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-lg bg-slate-100 text-xs font-bold text-slate-500">
                    {idx + 1}
                  </div>

                  {/* 状态点 */}
                  <span className={`h-2 w-2 shrink-0 rounded-full ${ch.status === 1 ? "bg-emerald-400 shadow-[0_0_8px_rgba(52,211,153,0.6)]" : "bg-zinc-400"}`} />

                  {/* 名称 + 双标签（[协议] [提供商]）+ 规范 Base URL */}
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <h3 className="truncate text-sm font-semibold tracking-tight">{ch.name}</h3>
                      <span className="shrink-0 rounded-full border border-blue-200 bg-blue-50 px-2 py-0.5 text-[10px] font-semibold text-blue-600">
                        [{protocolLabel}]
                      </span>
                      <span className="shrink-0 rounded-full border border-purple-200 bg-purple-50 px-2 py-0.5 text-[10px] font-semibold text-purple-600">
                        [{providerLabel}]
                      </span>
                    </div>
                    <div className="mt-0.5 truncate text-xs font-mono text-slate-400" title={displayBaseUrl}>
                      {displayBaseUrl}
                    </div>
                  </div>

                  {/* 快速统计 */}
                  {stats && stats.total_calls > 0 ? (
                    <div className="hidden items-center gap-3 lg:flex">
                      <div className="flex items-center gap-1 text-xs">
                        <Activity size={11} className="text-slate-400" />
                        <span className="font-semibold tabular-nums text-slate-700">{formatNumber(stats.total_calls)}</span>
                      </div>
                      <div className="flex items-center gap-1 text-xs">
                        <span className="text-slate-400">成功率</span>
                        <span className="font-semibold tabular-nums" style={{ color: (stats.success_calls + stats.failed_calls > 0 ? stats.success_calls / (stats.success_calls + stats.failed_calls) * 100 : 0) >= 95 ? "#10b981" : (stats.success_calls + stats.failed_calls > 0 ? stats.success_calls / (stats.success_calls + stats.failed_calls) * 100 : 0) >= 80 ? "#f59e0b" : "#ef4444" }}>
                          {(stats.success_calls + stats.failed_calls > 0 ? stats.success_calls / (stats.success_calls + stats.failed_calls) * 100 : 0).toFixed(0)}%
                        </span>
                      </div>
                      <div className="flex items-center gap-1 text-xs">
                        <Clock size={11} className="text-slate-400" />
                        <span className="font-semibold tabular-nums text-slate-700">{formatDuration(stats.avg_latency_ms)}</span>
                      </div>
                    </div>
                  ) : null}

                  {/* 调度信息 */}
                  <div className="hidden items-center gap-2 text-xs text-slate-400 md:flex">
                    <span className="rounded-md bg-slate-100 px-1.5 py-0.5">P{ch.priority}</span>
                    <span className="rounded-md bg-slate-100 px-1.5 py-0.5">W{ch.weight}</span>
                  </div>

                  {/* 操作按钮 */}
                  <div className="flex items-center gap-1" onClick={e => e.stopPropagation()}>
                    <button onClick={() => handleTest(ch.id)} disabled={testing === ch.id} className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-blue-50 hover:text-blue-600 disabled:opacity-50" title="测试连接">
                      {testing === ch.id ? <Loader2 size={15} className="animate-spin" /> : <Zap size={15} />}
                    </button>
                    <button onClick={() => handleCopyCurl(ch)} className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-slate-100 hover:text-slate-600" title="复制测试 curl 命令（含真实 API Key，粘贴到终端即可直接请求该渠道）">
                      {copiedCurl === ch.id ? <Check size={15} className="text-emerald-500" /> : <Terminal size={15} />}
                    </button>
                    <button onClick={() => { setEditing(ch); setShowForm(true); }} className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-slate-100 hover:text-slate-600" title="编辑">
                      <Edit size={15} />
                    </button>
                    <button onClick={() => { setEditing(ch); setDuplicating(true); setShowForm(true); }} className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-slate-100 hover:text-slate-600" title="复制">
                      <Copy size={15} />
                    </button>
                    <button onClick={() => handleToggle(ch)} className="rounded-lg p-1.5 transition-colors hover:bg-slate-100" title={ch.status === 1 ? "禁用" : "启用"}>
                      <Power size={15} className={ch.status === 1 ? "text-emerald-500" : "text-zinc-400"} />
                    </button>
                    <button onClick={() => setDeleteTarget(ch)} className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-red-50 hover:text-red-500" title="删除">
                      <Trash2 size={15} />
                    </button>
                    <button onClick={() => setExpandedId(isExpanded ? null : ch.id)} className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-slate-100 hover:text-slate-600" title={isExpanded ? "收起" : "展开"}>
                      <ChevronDown size={15} className={`transition-transform ${isExpanded ? "rotate-180" : ""}`} />
                    </button>
                  </div>
                </div>

                {/* 测试结果行（紧凑） */}
                {result && (
                  <div className={`mt-2 flex items-center gap-2 rounded-lg px-3 py-1.5 text-xs ${result.success ? "bg-emerald-50 text-emerald-700" : "bg-red-50 text-red-700"}`}>
                    {result.success ? <><span className="text-emerald-500">✓</span> 连接成功</> : <><span className="text-red-500">✗</span> {result.message}</>}
                    <span className={result.success ? "text-emerald-600" : "text-red-600"}>({result.latency_ms.toFixed(2)}ms)</span>
                  </div>
                )}

                {/* 展开区域 */}
                {isExpanded && (
                  <div className="mt-3 space-y-3 border-t border-slate-100 pt-3">
                    {/* 可用模型 */}
                    <div>
                      <div className="mb-1.5 text-xs font-semibold text-slate-500">可用模型 ({ch.models.length})</div>
                      <div className="flex flex-wrap gap-1.5">
                        {ch.models.map(m => (
                          <button
                            key={m}
                            onClick={() => handleCopyModel(m)}
                            className={`inline-flex items-center gap-0.5 rounded-full px-1.5 py-0 text-[11px] font-medium leading-5 transition-all active:scale-95 ${copiedModel === m ? "bg-emerald-50 text-emerald-700" : "bg-slate-100 text-slate-700 hover:bg-slate-200"}`}
                            title="点击复制"
                          >
                            {m}
                            {copiedModel === m && <Check size={9} className="text-emerald-500" />}
                          </button>
                        ))}
                      </div>
                    </div>

                    {/* 映射模型（与 Auth 账号共用 MappingChips，样式/交互一致） */}
                    {ch.model_mapping && Object.keys(ch.model_mapping).length > 0 && (
                      <div>
                        <div className="mb-1.5 text-xs font-semibold text-slate-500">映射模型</div>
                        <MappingChips
                          mapping={ch.model_mapping}
                          disabledPairs={ch.model_mapping_disabled}
                          onToggle={(name, t, isOff) => void handleToggleMapping(ch, name, t, isOff)}
                        />
                      </div>
                    )}

                    {/* API Key */}
                    <div>
                      <div className="mb-1.5 flex items-center justify-between">
                        <span className="text-xs font-semibold text-slate-500">API Key</span>
                        <div className="flex items-center gap-1">
                          <KeyStatusPill
                            enabled={primaryKeyEnabled(ch)}
                            loading={primaryKeyLoading === ch.id}
                            onToggle={() => handleTogglePrimaryKey(ch)}
                          />
                          <button
                            onClick={() => handleCopyKey(ch)}
                            className="rounded-md p-1 text-slate-400 transition-colors hover:bg-slate-100 hover:text-slate-600"
                            title="复制"
                          >
                            {copiedKey === ch.id ? <Check size={12} className="text-emerald-500" /> : <Copy size={12} />}
                          </button>
                          <button
                            onClick={() => handleToggleKey(ch)}
                            disabled={keyLoading === ch.id}
                            className="rounded-md p-1 text-slate-400 transition-colors hover:bg-slate-100 hover:text-slate-600 disabled:opacity-50"
                            title={keyVisible ? "隐藏" : "显示"}
                          >
                            {keyLoading === ch.id ? <Loader2 size={12} className="animate-spin" /> : keyVisible ? <EyeOff size={12} /> : <Eye size={12} />}
                          </button>
                        </div>
                      </div>
                      <div className={`flex items-center gap-2 rounded-lg bg-slate-50 px-3 py-2 text-xs font-mono text-slate-600 ${primaryKeyEnabled(ch) ? "" : "opacity-60"}`}>
                        <span className="truncate">
                          {keyVisible
                            ? (fullKeyMap[ch.id] || ch.api_key)
                            : ch.api_key}
                        </span>
                        {!primaryKeyEnabled(ch) && (
                          <span className="shrink-0 rounded-full bg-slate-200 px-1.5 py-0.5 font-sans text-[10px] text-slate-400">
                            已停用
                          </span>
                        )}
                        {keyVisible && fullKeyMap[ch.id] && (
                          <span className="shrink-0 rounded-full bg-slate-200 px-1.5 py-0.5 font-sans text-[10px] text-slate-500">
                            {fullKeyMap[ch.id].length} chars
                          </span>
                        )}
                      </div>
                      {copiedKey === ch.id && (
                        <div className="mt-1 text-[11px] text-emerald-600">✓ 已复制到剪贴板</div>
                      )}
                    </div>

                    {/* 额外 Keys（多 Key 负载均衡）*/}
                    {ch.extra_keys && ch.extra_keys.length > 0 && (
                      <div>
                        <div className="mb-1.5 flex items-center justify-between">
                          <span className="text-xs font-semibold text-slate-500">额外 Keys（负载均衡）</span>
                          <span className="rounded-full bg-slate-100 px-2 py-0.5 text-[10px] font-medium text-slate-500">
                            {ch.extra_keys.length} 个
                          </span>
                        </div>
                        <div className="space-y-1.5">
                          {ch.extra_keys.map((ek, ekIdx) => {
                            const ekVisible = extraKeyVisibleMap[ek.id];
                            const ekFull = extraKeyFullMap[ek.id];
                            const ekEnabled = ek.status === 1;
                            return (
                            <div key={ek.id} className={`flex items-center gap-2 rounded-lg bg-slate-50 px-3 py-2 text-xs font-mono text-slate-600 ${ekEnabled ? "" : "opacity-60"}`}>
                              <span className="shrink-0 text-slate-400">#{ekIdx + 2}</span>
                              <span className="truncate">
                                {ekVisible
                                  ? (ekFull || ek.api_key)
                                  : ek.api_key}
                              </span>
                              <button
                                onClick={() => handleToggleExtraKey(ek.id)}
                                disabled={extraKeyLoading === ek.id}
                                className="shrink-0 rounded-md p-0.5 text-slate-400 transition-colors hover:bg-slate-200 hover:text-slate-600 disabled:opacity-50"
                                title={ekVisible ? "隐藏" : "显示"}
                              >
                                {extraKeyLoading === ek.id ? <Loader2 size={11} className="animate-spin" /> : ekVisible ? <EyeOff size={11} /> : <Eye size={11} />}
                              </button>
                              <span className="ml-auto shrink-0 rounded-full bg-slate-200 px-1.5 py-0.5 text-[10px] text-slate-500">w:{ek.weight}</span>
                              <KeyStatusPill
                                enabled={ekEnabled}
                                loading={extraKeyToggleLoading === ek.id}
                                onToggle={() => handleSetExtraKeyStatus(ch, ek.id, !ekEnabled)}
                              />
                            </div>
                            );
                          })}
                        </div>
                        <p className="mt-1 text-[11px] text-slate-400">主 Key + 额外 Keys 按权重负载均衡，点击右侧状态可停用/启用，失效 Key 自动降级</p>
                      </div>
                    )}

                    {/* 详细统计仪表盘 */}
                    {stats && stats.total_calls > 0 ? (() => {
                      const successRate = (stats.success_calls + stats.failed_calls > 0 ? stats.success_calls / (stats.success_calls + stats.failed_calls) * 100 : 0);
                      const rateColor = successRate >= 95 ? "#10b981" : successRate >= 80 ? "#f59e0b" : "#ef4444";
                      const latColor = stats.avg_latency_ms < 500 ? "#10b981" : stats.avg_latency_ms < 2000 ? "#f59e0b" : "#ef4444";
                      const latPct = Math.min(stats.avg_latency_ms / 3000, 1) * 100;
                      return (
                        <div className="rounded-xl border border-slate-200/60 bg-gradient-to-br from-slate-50/80 to-white p-3">
                          <div className="mb-2 flex items-center justify-between">
                            <span className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wide text-slate-400">
                              <Activity size={12} /> 调用统计
                            </span>
                            {stats.last_call_at && (
                              <span className="text-[11px] text-slate-400">最后调用 {formatTime(stats.last_call_at)}</span>
                            )}
                          </div>
                          <div className="flex items-center gap-4">
                            <div className="flex flex-col items-center gap-1">
                              <div className="relative flex h-14 w-14 items-center justify-center">
                                <svg className="h-14 w-14 -rotate-90" viewBox="0 0 70 70">
                                  <circle cx="35" cy="35" r="28" fill="none" stroke="currentColor" strokeWidth="5" className="text-slate-200/60" />
                                  <circle cx="35" cy="35" r="28" fill="none" stroke={rateColor} strokeWidth="5"
                                    strokeLinecap="round" strokeDasharray={2 * Math.PI * 28}
                                    strokeDashoffset={2 * Math.PI * 28 - (successRate / 100) * 2 * Math.PI * 28}
                                    style={{ transition: "stroke-dashoffset 0.6s ease" }}
                                  />
                                </svg>
                                <span className="absolute text-xs font-bold tabular-nums" style={{ color: rateColor }}>{successRate.toFixed(0)}%</span>
                              </div>
                              <span className="text-[10px] font-medium text-slate-400">成功率</span>
                            </div>
                            <div className="h-12 w-px bg-slate-200/70" />
                            <div className="flex-1 grid grid-cols-3 gap-2">
                              <div>
                                <div className="text-[11px] text-slate-400">调用</div>
                                <div className="text-base font-bold tabular-nums text-slate-800">{formatNumber(stats.total_calls)}</div>
                                <div className="text-[10px] text-slate-400">成功 {formatNumber(stats.success_calls)} / 失败 {formatNumber(stats.failed_calls)}</div>
                              </div>
                              <div>
                                <div className="text-[11px] text-slate-400">Token</div>
                                <div className="text-base font-bold tabular-nums text-slate-800">{formatNumber(stats.total_tokens)}</div>
                                <div className="text-[10px] text-slate-400">↑{formatNumber(stats.prompt_tokens)} ↓{formatNumber(stats.completion_tokens)}</div>
                              </div>
                              <div>
                                <div className="text-[11px] text-slate-400">延迟</div>
                                <div className="mt-1 flex items-center gap-1.5">
                                  <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-slate-200/60">
                                    <div className="h-full rounded-full" style={{ width: `${latPct}%`, backgroundColor: latColor, transition: "width 0.6s ease" }} />
                                  </div>
                                  <span className="text-xs font-semibold tabular-nums" style={{ color: latColor }}>{formatDuration(stats.avg_latency_ms)}</span>
                                </div>
                              </div>
                            </div>
                          </div>
                        </div>
                      );
                    })() : (
                      <div className="rounded-xl border border-dashed border-slate-200 px-3 py-2 text-xs text-center text-slate-400">
                        暂无调用记录
                      </div>
                    )}

                    {/* 最近测试 */}
                    {ch.last_probe_ok !== null && (
                      <div className="flex items-center gap-2 text-xs text-slate-500">
                        <span className="text-slate-400">健康探测:</span>
                        <span
                          className={`inline-block h-2 w-2 rounded-full ${ch.last_probe_ok ? "bg-emerald-500" : "bg-red-500"}`}
                          title={ch.last_probe_ok ? "探测正常" : "探测失败（候选排序沉底）"}
                        />
                        <span>{ch.last_probe_ok ? "正常" : "异常"}</span>
                        {ch.probe_latency_ms !== null && <span className="text-slate-400">{ch.probe_latency_ms}ms</span>}
                        {ch.last_probe_at && <span>{formatTime(ch.last_probe_at)}</span>}
                      </div>
                    )}
                    {ch.last_test_at && (
                      <div className="flex items-center gap-2 text-xs text-slate-500">
                        <span className="text-slate-400">最近测试:</span>
                        <span>{formatTime(ch.last_test_at)}</span>
                        {ch.last_test_ok !== null && (
                          <span className={`rounded-full px-2 py-0.5 text-xs font-medium ${ch.last_test_ok ? "bg-emerald-100 text-emerald-700" : "bg-red-100 text-red-700"}`}>
                            {ch.last_test_ok ? "✓ 成功" : "✗ 失败"}
                          </span>
                        )}
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}

      {showForm && (
        <ChannelForm
          editing={editing}
          duplicate={duplicating}
          onClose={() => { setShowForm(false); setEditing(null); setDuplicating(false); }}
          onSaved={() => { setShowForm(false); setEditing(null); setDuplicating(false); load(); }}
        />
      )}

      {showImport && (
        <ImportDialog
          onClose={() => setShowImport(false)}
          onImported={() => load()}
        />
      )}

      <DeleteConfirmDialog
        target={deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={handleDelete}
        deleting={deleting}
      />
    </div>
  );
}

function DeleteConfirmDialog({
  target,
  onClose,
  onConfirm,
  deleting,
}: {
  target: Channel | null;
  onClose: () => void;
  onConfirm: () => void;
  deleting: boolean;
}) {
  if (!target) return null;
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="surface w-full max-w-sm rounded-[28px] p-6"
        onClick={e => e.stopPropagation()}
      >
        <div className="flex items-center gap-3">
          <div className="rounded-2xl border border-red-200 bg-red-50 p-2.5">
            <Trash2 className="h-5 w-5 text-red-600" />
          </div>
          <div>
            <h3 className="text-base font-semibold">删除渠道</h3>
            <p className="text-sm text-muted-foreground">此操作不可撤销</p>
          </div>
        </div>
        <div className="mt-4 rounded-2xl border border-border bg-background/50 px-4 py-3 text-sm">
          <div className="text-muted-foreground">渠道名称</div>
          <div className="mt-1 font-medium">{target.name}</div>
          <div className="mt-2 text-xs font-mono text-muted-foreground truncate">{target.native_base_url || target.base_url}</div>
        </div>
        <div className="mt-5 flex justify-end gap-2">
          <button onClick={onClose} className="action-secondary">取消</button>
          <button
            onClick={onConfirm}
            disabled={deleting}
            className="inline-flex items-center gap-2 rounded-2xl bg-red-600 px-4 py-2.5 text-sm font-medium text-white transition-colors hover:bg-red-700 disabled:opacity-50"
          >
            {deleting ? "删除中..." : "确认删除"}
          </button>
        </div>
      </div>
    </div>
  );
}
