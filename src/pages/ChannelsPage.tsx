import { useEffect, useState, useRef, useCallback } from "react";
import { channelApi, importExportApi } from "../lib/api";
import type { ChannelStats } from "../lib/api";
import type { Channel } from "../types";
import { CHANNEL_TYPES, formatTime, formatNumber, formatDuration } from "../lib/constants";
import { Plus, Radio, Trash2, Zap, Power, Edit, Download, ChevronDown, Upload, Loader2, X, Activity, Clock, GripVertical, Eye, EyeOff, Copy, Check } from "lucide-react";
import { ChannelForm } from "../components/ChannelForm";
import { ImportDialog } from "../components/ImportDialog";

export function ChannelsPage() {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [channelStats, setChannelStats] = useState<Record<string, ChannelStats>>({});
  const [showForm, setShowForm] = useState(false);
  const [editing, setEditing] = useState<Channel | null>(null);
  const [testing, setTesting] = useState<string | null>(null);
  const [testResult, setTestResult] = useState<Record<string, { success: boolean; message: string; latency_ms: number }>>({});
  const [showImport, setShowImport] = useState(false);
  const [showImportMenu, setShowImportMenu] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<Channel | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [draggedId, setDraggedId] = useState<string | null>(null);
  const [dragOverId, setDragOverId] = useState<string | null>(null);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [showKeyMap, setShowKeyMap] = useState<Record<string, boolean>>({});
  const [fullKeyMap, setFullKeyMap] = useState<Record<string, string>>({});
  const [keyLoading, setKeyLoading] = useState<string | null>(null);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const importMenuRef = useRef<HTMLDivElement>(null);
  const dragCounter = useRef(0);

  const load = useCallback(() => {
    channelApi.getAll().then(setChannels).catch(() => {});
    channelApi.getStats().then(stats => {
      const map: Record<string, ChannelStats> = {};
      stats.forEach(s => { map[s.channel_id] = s; });
      setChannelStats(map);
    }).catch(() => {});
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
    setExporting(true);
    try {
      const content = await importExportApi.exportChannels();
      const timestamp = new Date().toISOString().slice(0, 10);
      await importExportApi.saveExportFile(content, `waliapi-export-${timestamp}.json`);
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

  // API Key 显示/隐藏切换
  const handleToggleKey = async (ch: Channel) => {
    const next = !showKeyMap[ch.id];
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

  // 复制 API Key
  const handleCopyKey = async (ch: Channel) => {
    const keyToCopy = fullKeyMap[ch.id] || ch.api_key;
    try {
      await navigator.clipboard.writeText(keyToCopy);
      setCopiedKey(ch.id);
      setTimeout(() => setCopiedKey(null), 2000);
    } catch (e) {
      console.error("Failed to copy:", e);
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
    } catch {
      load(); // revert on failure
    }
  };

  return (
    <div className="page-shell space-y-6">
      <div className="page-header sticky top-0 z-30 -mx-7 -mt-7 mb-2 bg-white/90 px-7 py-5 backdrop-blur-md border-b border-slate-100">
        <div>
          <h1 className="page-title">渠道管理</h1>
          <p className="page-subtitle">拖拽排序 · 配置上游供应商与调度优先级</p>
        </div>
        <div className="flex items-center gap-2">
          <button onClick={handleExport} disabled={exporting} className="action-secondary flex items-center gap-1.5">
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
          </div>
          <button onClick={() => { setEditing(null); setShowForm(true); }} className="action-primary">
            <Plus size={16} /> 新建渠道
          </button>
        </div>
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
          <Radio className="h-12 w-12 text-muted-foreground/70" />
          <p className="text-base font-medium">还没有配置任何渠道</p>
          <p className="text-sm text-muted-foreground">先添加一个上游服务商，即可开始分发请求</p>
        </div>
      ) : (
        <div className="space-y-3">
          {channels.map((ch, idx) => {
            const typeInfo = CHANNEL_TYPES.find(t => t.value === ch.type);
            const result = testResult[ch.id];
            const stats = channelStats[ch.id];
            const isDragging = draggedId === ch.id;
            const isDragOver = dragOverId === ch.id;
            const isExpanded = expandedId === ch.id;
            const keyVisible = showKeyMap[ch.id];
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
                <div className="flex items-center gap-3">
                  {/* 拖拽手柄 */}
                  <div className="flex cursor-grab items-center text-slate-300 transition-colors hover:text-slate-400 active:cursor-grabbing">
                    <GripVertical size={18} />
                  </div>

                  {/* 排序序号 */}
                  <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-lg bg-slate-100 text-xs font-bold text-slate-500">
                    {idx + 1}
                  </div>

                  {/* 状态点 */}
                  <span className={`h-2 w-2 shrink-0 rounded-full ${ch.status === 1 ? "bg-emerald-400 shadow-[0_0_8px_rgba(52,211,153,0.6)]" : "bg-zinc-400"}`} />

                  {/* 名称 + 类型 */}
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <h3 className="truncate text-sm font-semibold tracking-tight">{ch.name}</h3>
                      <span className="shrink-0 rounded-full border border-slate-200 bg-slate-50 px-2 py-0.5 text-[10px] font-medium text-slate-500">
                        {typeInfo?.label || ch.type}
                      </span>
                    </div>
                    <div className="mt-0.5 truncate text-xs font-mono text-slate-400" title={ch.base_url}>
                      {ch.base_url}
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
                        <span className="font-semibold tabular-nums" style={{ color: (stats.success_calls / stats.total_calls * 100) >= 95 ? "#10b981" : (stats.success_calls / stats.total_calls * 100) >= 80 ? "#f59e0b" : "#ef4444" }}>
                          {(stats.success_calls / stats.total_calls * 100).toFixed(0)}%
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
                  <div className="flex items-center gap-1">
                    <button onClick={() => handleTest(ch.id)} disabled={testing === ch.id} className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-blue-50 hover:text-blue-600 disabled:opacity-50" title="测试连接">
                      {testing === ch.id ? <Loader2 size={15} className="animate-spin" /> : <Zap size={15} />}
                    </button>
                    <button onClick={() => { setEditing(ch); setShowForm(true); }} className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-slate-100 hover:text-slate-600" title="编辑">
                      <Edit size={15} />
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
                    <span className="text-slate-400">({result.latency_ms}ms)</span>
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
                          <span key={m} className="rounded-full bg-slate-100 px-2 py-0.5 text-xs font-medium text-slate-700">{m}</span>
                        ))}
                      </div>
                    </div>

                    {/* 映射模型 */}
                    {ch.model_mapping && Object.keys(ch.model_mapping).length > 0 && (
                      <div>
                        <div className="mb-1.5 text-xs font-semibold text-slate-500">映射模型</div>
                        <div className="flex flex-wrap gap-1.5">
                          {Object.entries(ch.model_mapping).map(([name, target]) => (
                            <span key={name} className="rounded-full bg-violet-50 px-2 py-0.5 text-xs font-medium text-violet-700">
                              {name} → {target}
                            </span>
                          ))}
                        </div>
                      </div>
                    )}

                    {/* API Key */}
                    <div>
                      <div className="mb-1.5 flex items-center justify-between">
                        <span className="text-xs font-semibold text-slate-500">API Key</span>
                        <div className="flex items-center gap-1">
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
                      <div className="flex items-center gap-2 rounded-lg bg-slate-50 px-3 py-2 text-xs font-mono text-slate-600">
                        <span className="truncate">
                          {keyVisible
                            ? (fullKeyMap[ch.id] || ch.api_key)
                            : `${ch.api_key.slice(0, 8)}${"•".repeat(12)}`}
                        </span>
                        {keyVisible && fullKeyMap[ch.id] && (
                          <span className="shrink-0 rounded-full bg-slate-200 px-1.5 py-0.5 text-[10px] text-slate-500">
                            {fullKeyMap[ch.id].length} chars
                          </span>
                        )}
                      </div>
                      {copiedKey === ch.id && (
                        <div className="mt-1 text-[11px] text-emerald-600">✓ 已复制到剪贴板</div>
                      )}
                    </div>

                    {/* 详细统计仪表盘 */}
                    {stats && stats.total_calls > 0 ? (() => {
                      const successRate = (stats.success_calls / stats.total_calls * 100);
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
          onClose={() => { setShowForm(false); setEditing(null); }}
          onSaved={() => { setShowForm(false); setEditing(null); load(); }}
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
          <div className="mt-2 text-xs font-mono text-muted-foreground truncate">{target.base_url}</div>
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
