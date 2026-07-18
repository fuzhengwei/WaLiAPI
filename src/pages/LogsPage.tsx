import { useEffect, useState, useCallback } from "react";
import { logApi } from "../lib/api";
import type { RequestLog } from "../types";
import { formatTime, formatDuration, formatNumber } from "../lib/constants";
import { ScrollText, RefreshCw, Trash2, ChevronDown, ChevronRight, AlertCircle } from "lucide-react";

const PAGE_SIZE = 20;

export function LogsPage() {
  const [logs, setLogs] = useState<RequestLog[]>([]);
  const [loading, setLoading] = useState(false);
  const [page, setPage] = useState(0);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [showCleanModal, setShowCleanModal] = useState(false);

  const load = useCallback((p: number = 0) => {
    setLoading(true);
    logApi.getAll(PAGE_SIZE, p * PAGE_SIZE)
      .then(setLogs)
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => { load(0); }, [load]);

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
          <h1 className="text-[28px] font-bold leading-tight tracking-tight">请求日志</h1>
          <p className="mt-1.5 text-sm text-muted-foreground">查看请求结果、Token 消耗与请求详情</p>
        </div>
        <div className="flex items-center gap-2">
          <button onClick={() => setShowCleanModal(true)} className="action-secondary text-red-500">
            <Trash2 size={16} /> 清理
          </button>
          <button onClick={() => load(page)} disabled={loading} className="action-secondary">
            <RefreshCw size={16} className={loading ? "animate-spin" : ""} /> 刷新
          </button>
        </div>
      </div>

      {/* Table area — fills remaining height, scrolls internally */}
      <div className="flex-1 overflow-hidden px-7 pb-7 min-h-0">
        <div className="surface h-full overflow-hidden rounded-[24px] flex flex-col">
          {logs.length === 0 ? (
            <div className="flex flex-1 flex-col items-center justify-center gap-2 text-center">
              <ScrollText className="h-12 w-12 text-muted-foreground/70" />
              <p className="text-base font-medium">暂无请求日志</p>
              <p className="text-sm text-muted-foreground">当有模型请求经过网关后，这里会显示调用记录</p>
            </div>
          ) : (
            <>
              {/* Table header + body share the scroll area */}
              <div className="flex-1 overflow-auto">
                <table className="w-full text-sm">
                  <thead className="sticky top-0 z-10 border-b border-border bg-white/90 backdrop-blur text-muted-foreground">
                    <tr>
                      <th className="w-8 px-3 py-3"></th>
                      <th className="px-3 py-3 text-left font-medium">时间</th>
                      <th className="px-3 py-3 text-left font-medium">密钥</th>
                      <th className="px-3 py-3 text-left font-medium">渠道</th>
                      <th className="px-3 py-3 text-left font-medium">模型</th>
                      <th className="px-3 py-3 text-left font-medium">状态</th>
                      <th className="px-3 py-3 text-right font-medium">Token</th>
                      <th className="px-3 py-3 text-right font-medium">耗时</th>
                      <th className="w-10 px-3 py-3"></th>
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

function LogRow({
  log,
  expanded,
  onToggle,
  onDelete,
}: {
  log: RequestLog;
  expanded: boolean;
  onToggle: () => void;
  onDelete: () => void;
}) {
  const tokenDetail = log.prompt_tokens > 0 || log.completion_tokens > 0
    ? `${log.prompt_tokens} + ${log.completion_tokens} = ${log.total_tokens}`
    : `${log.total_tokens}`;

  return (
    <>
      <tr className="border-b border-white/6 transition-colors hover:bg-white/4">
        <td className="px-3 py-2.5">
          <button onClick={onToggle} className="text-muted-foreground hover:text-foreground transition-colors">
            {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          </button>
        </td>
        <td className="px-3 py-2.5 text-xs text-muted-foreground whitespace-nowrap">{formatTime(log.created_at)}</td>
        <td className="px-3 py-2.5 text-xs">{log.api_key_name || "-"}</td>
        <td className="px-3 py-2.5 text-xs">{log.channel_name || "-"}</td>
        <td className="px-3 py-2.5 text-xs font-mono">{log.model}</td>
        <td className="px-3 py-2.5 text-xs">
          <div className="flex items-center gap-1.5">
            <span className={`rounded-full px-2 py-0.5 ${log.status_code === 200 ? "bg-emerald-500/12 text-emerald-300" : "bg-red-500/12 text-red-300"}`}>
              {log.status_code}
            </span>
            {log.is_stream && <span className="text-blue-400 text-[10px]">stream</span>}
            {log.is_retry && <span className="text-amber-400 text-[10px]">retry</span>}
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
          <td colSpan={9} className="px-3 py-3 bg-white/3 border-b border-white/6">
            <div className="space-y-3">
              {/* Token detail */}
              <div className="flex flex-wrap gap-4 text-xs">
                <div>
                  <span className="text-muted-foreground">Prompt Tokens: </span>
                  <span className="font-mono">{log.prompt_tokens}</span>
                </div>
                <div>
                  <span className="text-muted-foreground">Completion Tokens: </span>
                  <span className="font-mono">{log.completion_tokens}</span>
                </div>
                <div>
                  <span className="text-muted-foreground">Total Tokens: </span>
                  <span className="font-mono font-semibold">{log.total_tokens}</span>
                </div>
                <div>
                  <span className="text-muted-foreground">Mode: </span>
                  <span>{log.mode}</span>
                </div>
                <div>
                  <span className="text-muted-foreground">Retry: </span>
                  <span>{log.is_retry ? "Yes" : "No"}</span>
                </div>
              </div>

              {/* Error */}
              {log.error_message && (
                <div className="flex items-start gap-2 rounded-lg bg-red-500/8 p-3 text-xs text-red-400">
                  <AlertCircle size={14} className="mt-0.5 shrink-0" />
                  <span className="break-all">{log.error_message}</span>
                </div>
              )}

              {/* Request body */}
              {log.request_body ? (
                <div>
                  <div className="text-xs text-muted-foreground mb-1">请求内容:</div>
                  <pre className="max-h-60 overflow-auto rounded-lg bg-black/5 p-3 text-xs font-mono whitespace-pre-wrap break-all">
                    {log.request_body}
                  </pre>
                </div>
              ) : (
                <div className="text-xs text-muted-foreground/50">无请求内容记录</div>
              )}
            </div>
          </td>
        </tr>
      )}
    </>
  );
}

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
