import { useState } from "react";
import { Clock3 } from "lucide-react";
import type { AuthQuotaLimit, AuthQuotaWindow } from "../../types";
import { quotaDisplayState } from "./quotaDisplay";

// `window_minutes` is in minutes: 5h = 300, 7d = 10080, 30d = 43200.
const MINUTES_5H = 5 * 60;
const MINUTES_WEEK = 7 * 24 * 60;
const MINUTES_MONTH = 30 * 24 * 60;

function modelOrder(id: string) {
  if (id.startsWith("claude-")) return 0;
  if (id.startsWith("gemini-")) return 1;
  return 2;
}

function resetLabel(resetAt: string | null) {
  if (!resetAt) return null;
  const date = new Date(resetAt);
  if (!Number.isFinite(date.getTime())) return null;
  if (date.getTime() <= Date.now()) return "即将重置";
  return date.toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

/// Whether a window carries real data (mirrors upstream codex `has_data`):
/// a window whose used_percent / window_minutes / reset_at are all empty or
/// zero carries nothing to display and must not render an empty bar.
function hasWindowData(window: AuthQuotaWindow) {
  return (window.used_percent != null && window.used_percent !== 0)
    || (window.window_minutes != null && window.window_minutes !== 0)
    || window.reset_at != null;
}

/// Only three window types are labeled (5H / weekly / monthly), matching the
/// durations the backend actually reports. Anything else shows bare "限额"
/// rather than a guessed duration (e.g. a 12h mislabel).
function windowLabel(window: AuthQuotaWindow) {
  const minutes = window.window_minutes;
  if (minutes) {
    const known = [
      { expected: MINUTES_5H, label: "5H限额" },
      { expected: MINUTES_WEEK, label: "周限额" },
      { expected: MINUTES_MONTH, label: "月限额" },
    ];
    // Approximate match (±5%) mirrors upstream codex `get_limits_duration`.
    const hit = known.find(({ expected }) => minutes >= expected * 0.95 && minutes <= expected * 1.05);
    if (hit) return hit.label;
  }
  return "限额";
}

function QuotaWindow({ limit, window, modelQuotas }: { limit: AuthQuotaLimit; window: AuthQuotaWindow; modelQuotas: boolean }) {
  const { remaining, tone } = quotaDisplayState(window.used_percent);
  const exhausted = tone === "destructive";
  const barColor = tone === "destructive" ? "bg-destructive" : tone === "warning" ? "bg-warning" : "bg-success";
  const reset = resetLabel(window.reset_at);
  return (
    <div className="rounded-xl border border-border bg-muted/45 p-3">
      <div className="flex items-center justify-between gap-3 text-xs">
        <span className="flex items-center gap-1.5 font-medium"><Clock3 size={13} className="text-muted-foreground" />限额 · {limit.limit_name || windowLabel(window)}</span>
        <span className={exhausted ? "font-semibold text-destructive" : "font-semibold text-muted-foreground"}>{remaining == null ? "剩余 --" : modelQuotas ? `已用 ${(100 - remaining).toFixed(0)}% · 剩余 ${remaining.toFixed(0)}%` : `剩余 ${remaining.toFixed(0)}%`}</span>
      </div>
      <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-card">
        <div className={`h-full rounded-full ${barColor}`} style={{ width: `${remaining ?? 0}%` }} />
      </div>
      {(reset || modelQuotas) && <p className="mt-2 text-[11px] text-muted-foreground">{reset && `重置 ${reset} · `}{limit.limit_id}</p>}
    </div>
  );
}

export function QuotaBlock({ quota, modelQuotas = false }: { quota: NonNullable<import("../../types").AuthAccount["quota"]>; modelQuotas?: boolean }) {
  const [expanded, setExpanded] = useState(false);
  const windows = quota.limits.flatMap(limit => [
    limit.primary && { limit, window: limit.primary },
    limit.secondary && { limit, window: limit.secondary },
  ].filter((entry): entry is { limit: AuthQuotaLimit; window: AuthQuotaWindow } => {
    if (!entry) return false;
    return hasWindowData(entry.window) || (modelQuotas && entry.window.used_percent != null);
  }))
    // Shorter windows first (5H before weekly/monthly) so the card always
    // reads in the same order regardless of which slot upstream fills.
    .sort((a, b) => modelQuotas
      ? (b.window.used_percent ?? 0) - (a.window.used_percent ?? 0)
        || modelOrder(a.limit.limit_id) - modelOrder(b.limit.limit_id)
        || a.limit.limit_id.localeCompare(b.limit.limit_id)
      : (a.window.window_minutes ?? 0) - (b.window.window_minutes ?? 0));

  if (quota.exceeded && windows.length === 0) {
    return <div className="rounded-xl border border-destructive/25 bg-destructive/10 px-3 py-2.5 text-xs font-medium text-destructive">已踢出路由 · {quota.reason || "订阅限额已耗尽"}</div>;
  }

  const visible = modelQuotas && !expanded ? windows.slice(0, 4) : windows;
  return <div className="space-y-2">
    {modelQuotas && <p className="text-xs font-semibold text-muted-foreground">模型额度 · {windows.length} 项</p>}
    {visible.map(({ limit, window }, index) => <QuotaWindow key={`${limit.limit_id}-${index}`} limit={limit} window={window} modelQuotas={modelQuotas} />)}
    {modelQuotas && windows.length > 4 && <button type="button" onClick={() => setExpanded(!expanded)} className="text-xs font-medium text-primary hover:underline">{expanded ? "收起额度" : `查看全部 ${windows.length} 项额度`}</button>}
  </div>;
}
