import { useEffect, useState, useRef } from "react";
import { channelApi, importExportApi } from "../lib/api";
import type { Channel } from "../types";
import { CHANNEL_TYPES, formatTime } from "../lib/constants";
import { Plus, Radio, Trash2, Zap, Power, Edit, Gauge, Boxes, Download, ChevronDown, Upload, Loader2, X } from "lucide-react";
import { ChannelForm } from "../components/ChannelForm";
import { ImportDialog } from "../components/ImportDialog";

export function ChannelsPage() {
  const [channels, setChannels] = useState<Channel[]>([]);
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
  const importMenuRef = useRef<HTMLDivElement>(null);

  const load = () => channelApi.getAll().then(setChannels).catch(() => {});

  useEffect(() => { load(); }, []);

  // Close import menu on outside click
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

  // 注意：Tauri 2 的 webview 未实现原生 confirm()/alert()，
  // confirm() 会静默返回 false，因此这里使用自定义确认弹窗（与密钥管理页一致）
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

  return (
    <div className="page-shell space-y-6">
      <div className="page-header">
        <div>
          <h1 className="page-title">渠道管理</h1>
          <p className="page-subtitle">配置上游供应商、模型能力与调度优先级</p>
        </div>
        <div className="flex items-center gap-2">
          {/* Export button */}
          <button
            onClick={handleExport}
            disabled={exporting}
            className="action-secondary flex items-center gap-1.5"
          >
            {exporting ? <Loader2 size={16} className="animate-spin" /> : <Download size={16} />}
            导出
          </button>
          {/* Import dropdown */}
          <div className="relative" ref={importMenuRef}>
            <button
              onClick={() => setShowImportMenu(!showImportMenu)}
              className="action-secondary flex items-center gap-1.5"
            >
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
          <button onClick={() => setActionError(null)} className="ml-3 shrink-0 text-red-400 transition-colors hover:text-red-600" title="关闭">
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
        <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
          {channels.map(ch => {
            const typeInfo = CHANNEL_TYPES.find(t => t.value === ch.type);
            const result = testResult[ch.id];
            return (
              <div key={ch.id} className="surface rounded-[24px] p-5">
                <div className="flex items-start justify-between gap-4">
                  <div className="min-w-0 flex-1">
                    <div className="mb-3 flex flex-wrap items-center gap-2">
                      <span className={`h-2.5 w-2.5 rounded-full ${ch.status === 1 ? "bg-emerald-400 shadow-[0_0_16px_rgba(52,211,153,0.8)]" : "bg-zinc-500"}`} />
                      <h3 className="text-lg font-semibold tracking-tight">{ch.name}</h3>
                      <span className="rounded-full border border-white/8 bg-white/5 px-2.5 py-1 text-xs text-muted-foreground">
                        {typeInfo?.label || ch.type}
                      </span>
                    </div>

                    <div className="surface-soft rounded-2xl px-3 py-3 text-xs font-mono text-foreground/80 break-all">
                      {ch.base_url}
                    </div>

                    {/* 可用模型 + 映射模型 */}
                    <div className="mt-4">
                      <div className="mb-2 text-xs font-semibold text-foreground/70">可用模型</div>
                      <div className="flex flex-wrap gap-1.5">
                        {ch.models.map(m => (
                          <span key={m} className="rounded-full bg-slate-100 px-2.5 py-1 text-xs font-semibold text-slate-900">
                            {m}
                          </span>
                        ))}
                      </div>
                      {/* 映射模型名（客户端请求时使用的名字）*/}
                      {ch.model_mapping && Object.keys(ch.model_mapping).length > 0 && (
                        <>
                          <div className="mb-2 mt-3 text-xs font-semibold text-foreground/70">映射模型</div>
                          <div className="flex flex-wrap gap-1.5">
                            {Object.keys(ch.model_mapping).map(name => (
                              <span key={name} className="rounded-full bg-slate-100 px-2.5 py-1 text-xs font-semibold text-slate-900">{name}</span>
                            ))}
                          </div>
                        </>
                      )}
                    </div>

                    <div className="mt-4 grid grid-cols-2 gap-3 text-sm">
                      <div className="surface-soft rounded-2xl px-3 py-3">
                        <div className="mb-1 flex items-center gap-2 text-muted-foreground"><Gauge size={14} /> 调度</div>
                        <div className="font-medium">优先级 {ch.priority} · 权重 {ch.weight}</div>
                      </div>
                      <div className="surface-soft rounded-2xl px-3 py-3">
                        <div className="mb-1 flex items-center gap-2 text-muted-foreground"><Boxes size={14} /> 模型数</div>
                        <div className="font-medium">{ch.models.length} 个</div>
                      </div>
                    </div>

                    {(ch.last_test_at || result) && (
                      <div className="mt-4 rounded-2xl border border-slate-200 bg-slate-50 px-3 py-3 text-xs">
                        {ch.last_test_at && (
                          <div className="text-slate-500">
                            <span className="text-slate-400">最近测试:</span>{" "}
                            <span className="font-medium text-slate-600">{formatTime(ch.last_test_at)}</span>
                            {ch.last_test_ok !== null && (
                              <span className={`ml-2 inline-flex items-center gap-1 rounded-full px-2 py-0.5 font-medium ${
                                ch.last_test_ok
                                  ? "bg-emerald-100 text-emerald-700"
                                  : "bg-red-100 text-red-700"
                              }`}>
                                {ch.last_test_ok ? (
                                  <><span className="text-emerald-500">✓</span> 成功</>
                                ) : (
                                  <><span className="text-red-500">✗</span> 失败</>
                                )}
                              </span>
                            )}
                          </div>
                        )}
                        {result && (
                          <div className={`mt-1.5 flex items-center gap-1.5 font-medium ${
                            result.success ? "text-emerald-700" : "text-red-700"
                          }`}>
                            {result.success ? (
                              <><span className="text-emerald-500">✓</span> 连接成功</>
                            ) : (
                              <><span className="text-red-500">✗</span> {result.message}</>
                            )}
                            <span className="text-slate-400">({result.latency_ms}ms)</span>
                          </div>
                        )}
                      </div>
                    )}
                  </div>

                  <div className="flex flex-col gap-2">
                    <button onClick={() => handleTest(ch.id)} disabled={testing === ch.id} className="action-secondary px-3 py-2 text-blue-300" title="测试连接">
                      <Zap size={16} />
                    </button>
                    <button onClick={() => { setEditing(ch); setShowForm(true); }} className="action-secondary px-3 py-2" title="编辑">
                      <Edit size={16} />
                    </button>
                    <button onClick={() => handleToggle(ch)} className="action-secondary px-3 py-2" title={ch.status === 1 ? "禁用" : "启用"}>
                      <Power size={16} className={ch.status === 1 ? "text-emerald-300" : "text-zinc-400"} />
                    </button>
                    <button onClick={() => setDeleteTarget(ch)} className="action-secondary px-3 py-2 text-red-300" title="删除">
                      <Trash2 size={16} />
                    </button>
                  </div>
                </div>
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
