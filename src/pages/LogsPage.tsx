import { useEffect, useState } from "react";
import { logApi } from "../lib/api";
import type { RequestLog } from "../types";
import { formatTime, formatDuration, formatNumber } from "../lib/constants";
import { ScrollText, RefreshCw } from "lucide-react";

export function LogsPage() {
  const [logs, setLogs] = useState<RequestLog[]>([]);
  const [loading, setLoading] = useState(false);
  const [page, setPage] = useState(0);
  const pageSize = 50;

  const load = (p: number = 0) => {
    setLoading(true);
    logApi.getAll(pageSize, p * pageSize)
      .then(setLogs)
      .catch(() => {})
      .finally(() => setLoading(false));
  };

  useEffect(() => { load(0); }, []);

  return (
    <div className="page-shell space-y-6">
      <div className="page-header">
        <div>
          <h1 className="page-title">请求日志</h1>
          <p className="page-subtitle">查看请求结果、耗时、Token 消耗与分页记录</p>
        </div>
        <button onClick={() => load(page)} disabled={loading} className="action-secondary">
          <RefreshCw size={16} className={loading ? "animate-spin" : ""} /> 刷新
        </button>
      </div>

      {logs.length === 0 ? (
        <div className="surface empty-state">
          <ScrollText className="h-12 w-12 text-muted-foreground/70" />
          <p className="text-base font-medium">暂无请求日志</p>
          <p className="text-sm text-muted-foreground">当有模型请求经过网关后，这里会显示完整调用记录</p>
        </div>
      ) : (
        <>
          <div className="surface overflow-hidden rounded-[24px]">
            <div className="overflow-x-auto">
              <table className="min-w-full text-sm">
                <thead className="border-b border-border bg-white/4 text-muted-foreground">
                  <tr>
                    <th className="px-4 py-3 text-left font-medium">时间</th>
                    <th className="px-4 py-3 text-left font-medium">密钥</th>
                    <th className="px-4 py-3 text-left font-medium">渠道</th>
                    <th className="px-4 py-3 text-left font-medium">模型</th>
                    <th className="px-4 py-3 text-left font-medium">状态</th>
                    <th className="px-4 py-3 text-right font-medium">Token</th>
                    <th className="px-4 py-3 text-right font-medium">耗时</th>
                  </tr>
                </thead>
                <tbody>
                  {logs.map(log => (
                    <tr key={log.id} className="border-b border-white/6 transition-colors hover:bg-white/4">
                      <td className="px-4 py-3 text-xs text-muted-foreground whitespace-nowrap">{formatTime(log.created_at)}</td>
                      <td className="px-4 py-3 text-xs">{log.api_key_name || "-"}</td>
                      <td className="px-4 py-3 text-xs">{log.channel_name || "-"}</td>
                      <td className="px-4 py-3 text-xs font-mono">{log.model}</td>
                      <td className="px-4 py-3 text-xs">
                        <span className={`rounded-full px-2 py-1 ${log.status_code === 200 ? "bg-emerald-500/12 text-emerald-300" : "bg-red-500/12 text-red-300"}`}>
                          {log.status_code}
                        </span>
                        {log.is_stream && <span className="ml-2 text-blue-300">stream</span>}
                      </td>
                      <td className="px-4 py-3 text-right text-xs">{formatNumber(log.total_tokens)}</td>
                      <td className="px-4 py-3 text-right text-xs text-muted-foreground">{formatDuration(log.duration_ms)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>

          <div className="flex items-center justify-between">
            <button
              onClick={() => { const p = Math.max(0, page - 1); setPage(p); load(p); }}
              disabled={page === 0 || loading}
              className="action-secondary disabled:opacity-50"
            >
              上一页
            </button>
            <span className="text-sm text-muted-foreground">第 {page + 1} 页</span>
            <button
              onClick={() => { const p = page + 1; setPage(p); load(p); }}
              disabled={logs.length < pageSize || loading}
              className="action-secondary disabled:opacity-50"
            >
              下一页
            </button>
          </div>
        </>
      )}
    </div>
  );
}
