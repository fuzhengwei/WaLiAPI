import { ChevronDown, Download, Edit3, Gauge, GripVertical, KeyRound, Loader2, Power, RefreshCw, RotateCw, Trash2 } from "lucide-react";
import { Fragment, useState } from "react";
import type { ReactNode } from "react";
import type { AuthAccount, AuthQuotaState, AuthQuotaWindow, AuthModelState } from "../../types";
import { quotaDisplayState } from "./quotaDisplay";

const WINDOW_MINUTES = {
  fiveHours: 5 * 60,
  week: 7 * 24 * 60,
  month: 30 * 24 * 60,
} as const;

type AccountActions = {
  pending: boolean;
  quotaPending: boolean;
  onEdit: () => void;
  onToggle: () => void;
  onDelete: () => void;
  onRefresh: () => void;
  onRefreshQuota: () => void;
  onSync: () => void;
  onExport: () => void;
  onRelogin: () => void;
};

type ActionButtonProps = {
  label: string;
  disabled?: boolean;
  className: string;
  onClick: () => void;
  children: ReactNode;
};

function isAvailable(account: AuthAccount) {
  return account.status !== "invalid" && !account.disabled && !account.quota?.exceeded;
}

function statusInfo(account: AuthAccount) {
  if (account.status === "invalid") return { label: "已失效", className: "bg-destructive/10 text-destructive" };
  if (account.disabled) return { label: "已停用", className: "bg-muted text-muted-foreground" };
  if (account.quota?.exceeded) return { label: "额度耗尽", className: "bg-warning/15 text-warning" };
  return { label: "正常", className: "bg-success/10 text-success" };
}

function displayTime(value: string | null) {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? null
    : date.toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

function hasWindowData(window: AuthQuotaWindow) {
  return (window.used_percent != null && window.used_percent !== 0)
    || (window.window_minutes != null && window.window_minutes !== 0)
    || window.reset_at != null;
}

function getWindow(quota: AuthQuotaState | null, target: number) {
  if (!quota) return null;
  for (const limit of quota.limits) {
    for (const window of [limit.primary, limit.secondary]) {
      if (!window || !hasWindowData(window) || !window.window_minutes) continue;
      if (window.window_minutes >= target * 0.95 && window.window_minutes <= target * 1.05) {
        return window;
      }
    }
  }
  return null;
}

function QuotaCell({ quota, target, label }: { quota: AuthQuotaState | null; target: number; label: string }) {
  const window = getWindow(quota, target);
  if (!window) {
    return <div className="text-xs text-muted-foreground">{quota?.exceeded ? "已耗尽" : "--"}</div>;
  }
  const { remaining, tone } = quotaDisplayState(window.used_percent);
  const barClass = tone === "destructive" ? "bg-destructive" : tone === "warning" ? "bg-warning" : "bg-success";
  const resetAt = displayTime(window.reset_at);
  return (
    <div className="min-w-[92px]">
      <div className={`text-xs font-semibold tabular-nums ${tone === "destructive" ? "text-destructive" : tone === "warning" ? "text-warning" : "text-foreground"}`}>
        {remaining == null ? `${label} --` : `${remaining.toFixed(0)}%`}
      </div>
      <div className="mt-1 h-1.5 overflow-hidden rounded-full bg-muted">
        <div className={`h-full rounded-full ${barClass}`} style={{ width: `${remaining ?? 0}%` }} />
      </div>
      {resetAt && <div className="mt-1 whitespace-nowrap text-[10px] text-muted-foreground">重置 {resetAt}</div>}
    </div>
  );
}

function ModelDetails({ models }: { models: AuthModelState[] }) {
  return (
    <div className="flex flex-wrap gap-1.5">
      {models.length === 0
        ? <span className="text-xs text-muted-foreground">尚无模型快照，不参与路由</span>
        : models.map((model) => (
          <span key={model.id} className={`max-w-[220px] truncate rounded-full px-2 py-1 text-[11px] ${model.unavailable ? "bg-warning/10 text-warning" : "bg-muted text-muted-foreground"}`} title={model.last_error || model.id}>
            {model.id}
          </span>
        ))}
    </div>
  );
}

function ActionButton({ label, disabled, className, onClick, children }: ActionButtonProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-label={label}
      className={className}
    >
      {children}
    </button>
  );
}

function RowActions({ account, actions }: { account: AuthAccount; actions: AccountActions }) {
  const invalid = account.status === "invalid";
  const disabled = account.disabled;
  return (
    <div className="flex items-center justify-end gap-0.5 overflow-visible">
      {invalid ? (
        <ActionButton label="重新登录" onClick={actions.onRelogin} disabled={actions.pending} className="rounded-lg p-1.5 text-primary hover:bg-primary/10 disabled:opacity-50"><KeyRound size={15} /></ActionButton>
      ) : (
        <>
          {account.provider === "codex" && <ActionButton label="刷新额度" onClick={actions.onRefreshQuota} disabled={actions.pending} className="rounded-lg p-1.5 text-muted-foreground hover:bg-primary/10 hover:text-primary disabled:opacity-50">{actions.quotaPending ? <Loader2 size={15} className="animate-spin" /> : <Gauge size={15} />}</ActionButton>}
          <ActionButton label="刷新令牌" onClick={actions.onRefresh} disabled={actions.pending} className="rounded-lg p-1.5 text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50">{actions.pending && !actions.quotaPending ? <Loader2 size={15} className="animate-spin" /> : <RefreshCw size={15} />}</ActionButton>
          <ActionButton label="同步模型" onClick={actions.onSync} disabled={actions.pending} className="rounded-lg p-1.5 text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"><RotateCw size={15} /></ActionButton>
          {account.provider === "codex" && <ActionButton label="导出 JSON" onClick={actions.onExport} disabled={actions.pending} className="rounded-lg p-1.5 text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"><Download size={15} /></ActionButton>}
        </>
      )}
      <ActionButton label="编辑账号" onClick={actions.onEdit} disabled={actions.pending} className="rounded-lg p-1.5 text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"><Edit3 size={15} /></ActionButton>
      <ActionButton label={disabled ? "启用账号" : "停用账号"} onClick={actions.onToggle} disabled={actions.pending} className={`rounded-lg p-1.5 disabled:opacity-50 ${disabled ? "text-success hover:bg-success/10" : "text-warning hover:bg-warning/10"}`}><Power size={15} /></ActionButton>
      <ActionButton label="删除账号" onClick={actions.onDelete} disabled={actions.pending} className="rounded-lg p-1.5 text-destructive hover:bg-destructive/10 disabled:opacity-50"><Trash2 size={15} /></ActionButton>
    </div>
  );
}

export function AccountList({ accounts, actionFor, onReorder }: { accounts: AuthAccount[]; actionFor: (account: AuthAccount) => AccountActions; onReorder?: (orderedIds: string[]) => void }) {
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [draggedId, setDraggedId] = useState<string | null>(null);
  const [dragOverId, setDragOverId] = useState<string | null>(null);

  const handleDragStart = (e: React.DragEvent, id: string) => {
    setDraggedId(id);
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", id);
  };

  const handleDragEnd = () => {
    setDraggedId(null);
    setDragOverId(null);
  };

  const handleDragOver = (e: React.DragEvent, id: string) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    if (id !== draggedId) setDragOverId(id);
  };

  const handleDrop = (e: React.DragEvent, targetId: string) => {
    e.preventDefault();
    if (!draggedId || draggedId === targetId || !onReorder) return;
    const fromIdx = accounts.findIndex(a => a.id === draggedId);
    const toIdx = accounts.findIndex(a => a.id === targetId);
    if (fromIdx === -1 || toIdx === -1) return;
    const next = [...accounts];
    const [moved] = next.splice(fromIdx, 1);
    next.splice(toIdx, 0, moved);
    onReorder(next.map(a => a.id));
    setDraggedId(null);
    setDragOverId(null);
  };

  return (
    <div className="space-y-3">
      {accounts.map((account, index) => {
        const expanded = expandedId === account.id;
        const status = statusInfo(account);
        const actions = actionFor(account);
        const isDragging = draggedId === account.id;
        const isDragOver = dragOverId === account.id;
        return (
          <Fragment key={account.id}>
            <div
              draggable={!!onReorder}
              onDragStart={(e) => onReorder && handleDragStart(e, account.id)}
              onDragEnd={handleDragEnd}
              onDragOver={(e) => handleDragOver(e, account.id)}
              onDrop={(e) => handleDrop(e, account.id)}
              onClick={() => setExpandedId(expanded ? null : account.id)}
              className={`group surface cursor-pointer rounded-2xl p-4 transition-all ${
                isDragging ? "opacity-40 scale-[0.98]" : ""
              } ${
                isDragOver ? "ring-2 ring-blue-400 ring-offset-1" : ""
              }`}
            >
              <div className="flex items-center gap-3">
                {/* 拖拽手柄 */}
                {onReorder && (
                  <div className="flex cursor-grab items-center text-slate-300 transition-colors hover:text-slate-400 active:cursor-grabbing" onClick={e => e.stopPropagation()}>
                    <GripVertical size={18} />
                  </div>
                )}

                {/* 排序序号 */}
                <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-lg bg-slate-100 text-xs font-bold text-slate-500">
                  {index + 1}
                </div>

                {/* 状态点 */}
                <span className={`h-2 w-2 shrink-0 rounded-full ${isAvailable(account) ? "bg-emerald-400 shadow-[0_0_8px_rgba(52,211,153,0.6)]" : "bg-zinc-400"}`} />

                {/* 名称 + 邮箱 + 类型 */}
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <h3 className="truncate text-sm font-semibold tracking-tight">{account.label || account.account_id}</h3>
                    <span className={`shrink-0 rounded-full px-2 py-0.5 text-[10px] font-semibold ${status.className}`}>
                      {status.label}
                    </span>
                    {account.plan_type && (
                      <span className="shrink-0 rounded-full border border-purple-200 bg-purple-50 px-2 py-0.5 text-[10px] font-semibold text-purple-600">
                        {account.plan_type}
                      </span>
                    )}
                  </div>
                  <div className="mt-0.5 truncate text-xs font-mono text-slate-400" title={account.email || account.account_id}>
                    {account.email || account.account_id}
                  </div>
                </div>

                {/* 额度信息 */}
                <div className="hidden items-center gap-4 lg:flex">
                  <QuotaCell quota={account.quota} target={WINDOW_MINUTES.fiveHours} label="5H" />
                  <QuotaCell quota={account.quota} target={WINDOW_MINUTES.week} label="周" />
                  <QuotaCell quota={account.quota} target={WINDOW_MINUTES.month} label="月" />
                </div>

                {/* 调度信息 */}
                <div className="hidden items-center gap-2 text-xs text-slate-400 md:flex">
                  <span className="rounded-md bg-slate-100 px-1.5 py-0.5">P{account.priority}</span>
                  <span className="rounded-md bg-slate-100 px-1.5 py-0.5">W{account.weight}</span>
                </div>

                {/* 操作按钮 */}
                <div className="flex items-center gap-1" onClick={e => e.stopPropagation()}>
                  <RowActions account={account} actions={actions} />
                  <button onClick={(e) => { e.stopPropagation(); setExpandedId(expanded ? null : account.id); }} className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-slate-100 hover:text-slate-600" title={expanded ? "收起" : "展开"}>
                    <ChevronDown size={15} className={`transition-transform ${expanded ? "rotate-180" : ""}`} />
                  </button>
                </div>
              </div>

              {/* 展开区域 */}
              {expanded && (
                <div className="mt-3 space-y-3 border-t border-slate-100 pt-3">
                  <div>
                    <div className="mb-1.5 text-xs font-semibold text-slate-500">可用模型 ({account.models.length})</div>
                    <ModelDetails models={account.models} />
                  </div>
                  <div className="flex items-center gap-4 text-xs text-slate-500">
                    <span className="text-slate-400">最近刷新：</span>
                    <span>{displayTime(account.last_refreshed_at) || "未刷新"}</span>
                    <span className="text-slate-400">模型同步：</span>
                    <span>{displayTime(account.last_models_sync_at) || "未同步"}</span>
                  </div>
                </div>
              )}
            </div>
          </Fragment>
        );
      })}
    </div>
  );
}
