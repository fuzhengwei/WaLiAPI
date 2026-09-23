import { useEffect, useState } from "react";
import { Check, Download, Edit3, Gauge, KeyRound, Loader2, Power, RefreshCw, RotateCw, Trash2, X } from "lucide-react";
import type { AuthAccount, AuthModelState } from "../../types";
import { QuotaBlock } from "./QuotaBlock";
import { MappingChips } from "../MappingChips";

// 统一模型 chip 与底部元信息 pill 的字号、行高和内边距。
const metaPillBase =
  "inline-flex items-center justify-center rounded-full bg-muted px-2 py-1 !text-[11px] font-normal !leading-[14px] text-muted-foreground";
const chipBase =
  `${metaPillBase} appearance-none gap-1`;

function formatTime(value: string | null) {
  if (!value) return "未同步";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "未知" : date.toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

function AccountStatus({ account }: { account: AuthAccount }) {
  if (account.status === "invalid") return <span className="inline-flex items-center gap-1 rounded-full bg-destructive/10 px-2 py-0.5 text-[11px] font-semibold text-destructive"><span className="h-1.5 w-1.5 rounded-full bg-destructive" />已失效</span>;
  if (account.disabled) return <span className="inline-flex items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-[11px] font-semibold text-muted-foreground"><span className="h-1.5 w-1.5 rounded-full bg-muted-foreground" />已停用</span>;
  if (account.quota?.exceeded) return <span className="inline-flex items-center gap-1 rounded-full bg-warning/15 px-2 py-0.5 text-[11px] font-semibold text-warning"><span className="h-1.5 w-1.5 rounded-full bg-warning" />已踢出路由</span>;
  return <span className="inline-flex items-center gap-1 rounded-full bg-success/10 px-2 py-0.5 text-[11px] font-semibold text-success"><span className="h-1.5 w-1.5 rounded-full bg-success" />正常</span>;
}

// Stable, non-secret invalidation reasons → user-facing explanation.
const INVALIDATION_MESSAGES: Record<string, string> = {
  payment_required: "订阅未激活或权益无法验证（Kimi 侧返回 402）。请先在 Kimi 确认订阅生效，再重新登录或同步模型。",
  permission_denied: "账号没有调用权限（服务商返回 403）。请确认已开通对应服务，或重新登录。",
  quota_exhausted: "额度已用尽或触发限流（服务商返回 429）。请稍后重试，或检查账号配额。",
  validation_required: "账号需要先完成服务商验证才能使用。请按登录提示打开验证链接后，再重新登录。",
};

function invalidationText(reason: string | null): string {
  if (reason && INVALIDATION_MESSAGES[reason]) return INVALIDATION_MESSAGES[reason];
  return "自动刷新未成功；重新登录后才能恢复为路由候选。";
}

export function AccountCard({ account, pending, quotaPending, onEdit, onToggle, onDelete, onRefresh, onRefreshQuota, onSync, onExport, onRelogin, onToggleMapping }: { account: AuthAccount; pending: boolean; quotaPending: boolean; onEdit: () => void; onToggle: () => void; onDelete: () => void; onRefresh: () => void; onRefreshQuota: () => void; onSync: () => void; onExport: () => void; onRelogin: () => void; onToggleMapping: (from: string, to: string, currentlyOff: boolean) => void }) {
  const invalid = account.status === "invalid";
  const disabled = account.disabled;
  const isKimi = account.provider === "kimi";
  const isGrok = account.provider === "grok";
  const isCodex = account.provider === "codex";
  const isGemini = account.provider === "gemini";
  const canExport = isCodex;
  const canQuota = isCodex || isGemini;
  const [showModels, setShowModels] = useState(false);
  const cardClass = `surface rounded-[24px] p-5 transition-all hover:shadow-lg ${disabled ? "opacity-65 saturate-50 shadow-none" : ""}`;
  const markClass = `flex h-11 w-11 shrink-0 items-center justify-center rounded-2xl text-lg font-bold text-white shadow-sm ${disabled ? "bg-muted-foreground/45" : "bg-success"}`;
  const markIcon = isKimi ? "☾" : isGemini ? "G" : isGrok ? "✦" : "⌘";
  const toggleClass = disabled
    ? "rounded-lg border border-success/20 bg-success/10 p-1.5 text-success hover:bg-success/15 hover:text-success"
    : "rounded-lg border border-warning/25 bg-warning/10 p-1.5 text-warning hover:bg-warning/15 hover:text-warning";
  return <article className={cardClass} aria-label={`${account.label} 认证账号`}>
    <header className="flex items-start gap-3"><div className={markClass}>{markIcon}</div><div className="min-w-0 flex-1"><h2 className="truncate font-semibold">{account.label}</h2><p className="mt-1 truncate text-xs text-muted-foreground">{account.email || account.account_id}</p><div className="mt-2 flex flex-wrap items-center gap-2 text-[11px] text-muted-foreground"><span className="inline-flex items-center gap-1">状态 <AccountStatus account={account} /></span>{account.plan_type && <span className="inline-flex items-center gap-1">账号类型 <span className="rounded-full border border-border bg-muted px-2 py-0.5 text-[11px] text-muted-foreground">{account.plan_type}</span></span>}</div></div><div className="flex shrink-0 items-center gap-1"><button onClick={onEdit} disabled={pending} aria-label="编辑账号" className="rounded-lg p-1.5 text-muted-foreground hover:bg-muted hover:text-foreground"><Edit3 size={16} /></button><button onClick={onToggle} disabled={pending} aria-label={disabled ? "启用账号" : "停用账号"} className={toggleClass}><Power size={16} /></button><button onClick={onDelete} disabled={pending} aria-label="删除账号" className="rounded-lg p-1.5 text-destructive hover:bg-destructive/10"><Trash2 size={16} /></button></div></header>
    <div className="mt-4 space-y-3 border-y border-border py-4">{invalid ? <div className="rounded-xl border border-destructive/25 bg-destructive/10 p-3 text-sm"><p className="font-semibold text-destructive">令牌已失效 · 需重新登录</p><p className="mt-1 text-xs leading-5 text-muted-foreground">{invalidationText(account.invalidation_reason)}</p></div> : account.quota && canQuota && <QuotaBlock quota={account.quota} modelQuotas={isGemini} />}{!invalid && isGemini && account.project_id && <p className="text-xs text-muted-foreground">GCP 项目 {account.project_id}</p>}</div>
    <section className="mt-4"><div className="flex items-center justify-between gap-3"><p className="text-xs font-medium">◎ 可用模型</p><span className="text-[11px] text-muted-foreground">登录/12h 自动同步 · 全量支持</span></div><div className="mt-2 flex flex-wrap items-center gap-1.5">{account.models.slice(0, 4).map(model => <ModelChip key={model.id} id={model.id} />)}{account.models.length > 4 && <button onClick={() => setShowModels(true)} className={`${chipBase} transition-colors hover:bg-muted/60 hover:text-foreground`} aria-label="查看全部模型">+{account.models.length - 4}</button>}{account.models.length === 0 && <span className="text-xs text-muted-foreground">尚无模型快照，不参与路由</span>}</div>{account.model_mapping && Object.keys(account.model_mapping).length > 0 && <div className="mt-2"><p className="mb-1 text-[11px] font-semibold text-muted-foreground">映射模型</p><MappingChips mapping={account.model_mapping} disabledPairs={account.model_mapping_disabled} onToggle={onToggleMapping} /></div>}</section>
    <div className="mt-4 flex flex-wrap gap-2"><span className={metaPillBase}>优先级 {account.priority} · 权重 {account.weight}</span><span className={metaPillBase}>同步于 {formatTime(account.last_models_sync_at)}</span><span className={metaPillBase}>刷新 {formatTime(account.last_refreshed_at)}</span></div>
    <div className={`mt-4 grid ${canExport || canQuota ? "grid-cols-2 sm:grid-cols-4" : "grid-cols-2"} gap-2 border-t border-border pt-4`}>{invalid ? <button onClick={onRelogin} disabled={pending} className="col-span-full action-primary justify-center"><KeyRound size={15} />重新登录</button> : null}{canQuota && <button onClick={onRefreshQuota} disabled={pending} className="action-secondary justify-center px-1.5 py-2 text-xs">{quotaPending ? <Loader2 size={14} className="animate-spin shrink-0" /> : <Gauge size={14} className="shrink-0" />}刷新额度</button>}<button onClick={onRefresh} disabled={pending} className="action-secondary justify-center px-1.5 py-2 text-xs">{pending && !quotaPending ? <Loader2 size={14} className="animate-spin shrink-0" /> : <RefreshCw size={14} className="shrink-0" />}刷新令牌</button><button onClick={onSync} disabled={pending} className="action-secondary justify-center px-1.5 py-2 text-xs"><RotateCw size={14} className="shrink-0" />同步模型</button>{canExport && <button onClick={onExport} disabled={pending} className="action-secondary justify-center px-1.5 py-2 text-xs"><Download size={14} className="shrink-0" />导出 JSON</button>}</div>
    {showModels && <ModelsPopup models={account.models} onClose={() => setShowModels(false)} />}
  </article>;
}

function ModelChip({ id, maxWidth = "max-w-[10rem]" }: { id: string; maxWidth?: string }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    navigator.clipboard
      .writeText(id)
      .then(() => { setCopied(true); window.setTimeout(() => setCopied(false), 1200); })
      .catch(() => {});
  };
  return (
    <button type="button" onClick={() => void copy()} title={id} aria-label={`复制模型 id ${id}`}
      className={`${chipBase} ${maxWidth} ${copied ? "bg-success/10 text-success" : "hover:bg-muted/60 hover:text-foreground"}`}>
      <span className="min-w-0 truncate">{id}</span>
      {copied && <Check size={11} className="shrink-0" />}
    </button>
  );
}

function ModelsPopup({ models, onClose }: { models: AuthModelState[]; onClose: () => void }) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-foreground/35 p-4" role="dialog" aria-modal="true" aria-labelledby="models-popup-title" onClick={onClose}>
      <div className="surface w-full max-w-md rounded-[24px] p-6 shadow-2xl" onClick={(event) => event.stopPropagation()}>
        <div className="flex items-start justify-between gap-3">
          <h2 id="models-popup-title" className="text-lg font-semibold">全部模型 ({models.length})</h2>
          <button onClick={onClose} aria-label="关闭全部模型弹窗" className="rounded-lg p-1 text-muted-foreground hover:bg-muted"><X size={18} /></button>
        </div>
        <div className="mt-4 flex max-h-[60vh] flex-wrap gap-1.5 overflow-y-auto">
          {models.map((model) => <ModelChip key={model.id} id={model.id} maxWidth="max-w-full" />)}
        </div>
      </div>
    </div>
  );
}
