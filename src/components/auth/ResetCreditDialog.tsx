import { AlertCircle, Check, ExternalLink, Info, Loader2, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { AuthAccount, AuthResetCredit, AuthResetCreditsSnapshot, AuthResetOperationResult } from "../../types";
import { OpenAIIcon, ResetCreditIcon } from "./ResetCreditIcon";

type Step = "select" | "success" | "error";

interface Props {
  account: AuthAccount;
  onClose: () => void;
  listCredits: (id: string) => Promise<AuthResetCreditsSnapshot>;
  consumeCredit: (id: string, creditId: string, operationId: string) => Promise<AuthResetOperationResult>;
  onCompleted: () => Promise<void> | void;
}

type OperationStatus = "idle" | "pending" | "unknown" | "completed" | "failed";

function createOperationId() {
  const cryptoApi = globalThis.crypto;
  if (typeof cryptoApi?.randomUUID === "function") return cryptoApi.randomUUID();

  // Tauri/WebView 老版本可能没有 randomUUID，但后端要求 UUID 格式。
  const bytes = new Uint8Array(16);
  if (typeof cryptoApi?.getRandomValues === "function") {
    cryptoApi.getRandomValues(bytes);
  } else {
    for (let index = 0; index < bytes.length; index += 1) bytes[index] = Math.floor(Math.random() * 256);
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function creditTime(value: string | null) {
  if (!value) return "未提供";
  const numeric = Number(value);
  const date = new Date(Number.isFinite(numeric) && /^\d+$/.test(value) ? numeric * 1000 : value);
  return Number.isNaN(date.getTime()) ? "未知" : date.toLocaleString("zh-CN", { hour12: false });
}

function availableCredit(credit: AuthResetCredit) {
  if (credit.status.toLowerCase() !== "available" || credit.resetType !== "codex_rate_limits") return false;
  if (!credit.expiresAt) return true;
  const numeric = Number(credit.expiresAt);
  const timestamp = Number.isFinite(numeric) && /^\d+$/.test(credit.expiresAt) ? numeric * 1000 : Date.parse(credit.expiresAt);
  return Number.isFinite(timestamp) && timestamp > Date.now();
}

function outcomeText(code: string | null) {
  if (code === "nothing_to_reset") return "当前账号没有可重置的额度窗口。";
  if (code === "no_credit") return "这张重置卡已不可用，请查看官方用量页。";
  if (code === "already_redeemed") return "该重置卡已经使用过，额度状态已重新读取。";
  return "重置卡消费没有完成，请打开官方用量页核对。";
}

function CreditMark() {
  return <span className="flex h-[72px] w-[72px] shrink-0 items-center justify-center rounded-2xl bg-gradient-to-br from-cyan-100 to-teal-50 text-slate-900" aria-hidden="true">
    <OpenAIIcon size={43} />
  </span>;
}

export function ResetCreditDialog({ account, onClose, listCredits, consumeCredit, onCompleted }: Props) {
  const [step, setStep] = useState<Step>("select");
  const [snapshot, setSnapshot] = useState<AuthResetCreditsSnapshot | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [result, setResult] = useState<AuthResetOperationResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [consuming, setConsuming] = useState(false);
  const [operationId, setOperationId] = useState<string | null>(null);
  const [operationStatus, setOperationStatus] = useState<OperationStatus>("idle");
  const [error, setError] = useState<string | null>(null);
  const [errorStage, setErrorStage] = useState<"list" | "consume" | null>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const submitLocked = useRef(false);

  useEffect(() => {
    let disposed = false;
    setLoading(true);
    listCredits(account.id)
      .then((value) => { if (!disposed) { setSnapshot(value); setSelectedId(value.credits.find(availableCredit)?.id ?? null); } })
      .catch((reason) => { if (!disposed) { setErrorStage("list"); setError(typeof reason === "string" ? reason : "重置卡查询失败，请打开官方用量页核对。"); setStep("error"); } })
      .finally(() => { if (!disposed) setLoading(false); });
    return () => { disposed = true; };
  }, [account.id, listCredits]);

  useEffect(() => {
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialogRef.current?.focus();
    return () => previousFocus?.focus();
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !consuming) onClose();
      if (event.key !== "Tab") return;
      const focusables = dialogRef.current?.querySelectorAll<HTMLElement>('button:not([disabled]), a[href]');
      if (!focusables?.length) return;
      const first = focusables[0]; const last = focusables[focusables.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [consuming, onClose]);

  const credits = useMemo(() => snapshot?.credits ?? [], [snapshot]);
  const availableCredits = useMemo(() => credits.filter(availableCredit), [credits]);
  const selected = availableCredits.find((credit) => credit.id === selectedId) ?? null;
  const close = () => { if (!consuming) onClose(); };

  const confirm = async () => {
    if (!selected || submitLocked.current) return;
    const currentOperationId = operationId ?? createOperationId();
    submitLocked.current = true;
    setOperationId(currentOperationId);
    setOperationStatus("pending");
    setConsuming(true);
    setError(null);
    try {
      const next = await consumeCredit(account.id, selected.id, currentOperationId);
      setResult(next);
      if (next.code === "reset" || next.code === "already_redeemed") {
        setOperationStatus("completed");
        setStep("success");
        try { await onCompleted(); } catch { /* 已确认的上游结果不因页面刷新失败而改写。 */ }
      } else {
        setOperationStatus("failed");
        setErrorStage("consume");
        setStep("error");
      }
    } catch (reason) {
      // 请求可能已经到达上游，未知结果禁止在当前任务中重新生成幂等键或重发。
      setOperationStatus("unknown");
      setErrorStage("consume");
      setError(typeof reason === "string" ? reason : "重置请求结果未知，当前任务已停止，请稍后查看官方用量页，不要重复提交。");
      setStep("error");
    } finally { setConsuming(false); }
  };

  const accountName = account.email || account.label || account.account_id;
  return <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-100/45 p-3 backdrop-blur-[2px] lg:justify-end lg:p-8" onMouseDown={(event) => { if (event.target === event.currentTarget) close(); }}>
    <div ref={dialogRef} tabIndex={-1} role="dialog" aria-modal="true" aria-labelledby="reset-credit-title" className="flex max-h-[calc(100vh-24px)] w-full max-w-[780px] flex-col overflow-hidden rounded-[26px] border border-white/80 bg-white shadow-[0_22px_70px_rgba(47,77,148,.18)] outline-none lg:max-h-[calc(100vh-64px)]">
      <div className="overflow-y-auto px-5 pb-6 pt-6 sm:px-8 sm:pb-8 sm:pt-8">
        {step === "select" ? <>
          <header className="flex items-start gap-4"><div className="flex h-[86px] w-[86px] shrink-0 items-center justify-center rounded-2xl border border-slate-200 bg-white text-slate-900"><ResetCreditIcon size={44} /></div><div className="min-w-0 flex-1"><h2 id="reset-credit-title" className="text-2xl font-semibold tracking-tight text-slate-950">选择重置卡</h2><p className="mt-1 truncate text-base font-medium text-slate-500">账号：{accountName}</p><p className="mt-3 text-sm leading-6 text-slate-500">从该账号下的可用重置卡中选择一张使用，重置 Codex 使用额度。</p></div><button type="button" onClick={close} disabled={consuming} aria-label="关闭重置卡弹窗" className="-mr-1 -mt-1 flex h-10 w-10 shrink-0 items-center justify-center rounded-xl text-slate-400 hover:bg-slate-100 hover:text-slate-700 focus-visible:outline-2 focus-visible:outline-blue-500 disabled:opacity-40"><X size={23} /></button></header>
          <section className="mt-9" aria-label="重置卡列表"><div className="mb-4 flex items-center justify-between gap-3"><h3 className="text-lg font-semibold text-slate-900">可用的重置卡</h3><span className="rounded-full bg-slate-100 px-3 py-1 text-xs font-medium text-slate-500">共 {credits.length} 张</span></div>
            {loading ? <div className="flex min-h-40 items-center justify-center gap-2 text-sm text-slate-500" role="status"><Loader2 size={18} className="animate-spin" />正在查询重置卡…</div> : availableCredits.length === 0 ? <div className="rounded-2xl border border-dashed border-slate-200 px-5 py-8 text-center text-sm text-slate-500">当前没有可用的重置卡。{snapshot?.fallbackUrl && <a className="mt-2 inline-flex items-center gap-1 text-blue-600 hover:underline" href={snapshot.fallbackUrl} target="_blank" rel="noreferrer">打开官方用量页<ExternalLink size={14} /></a>}</div> : <div className="space-y-3">{availableCredits.map((credit) => { const checked = selected?.id === credit.id; const monthly = /month|月/i.test(`${credit.title ?? ""} ${credit.description ?? ""}`); return <button key={credit.id} type="button" disabled={consuming} aria-pressed={checked} onClick={() => setSelectedId(credit.id)} className={`flex w-full items-center gap-4 rounded-[19px] border px-5 py-5 text-left transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-500 ${checked ? "border-blue-400 bg-blue-50/25 shadow-[0_3px_16px_rgba(70,111,245,.07)]" : "border-slate-200 bg-white hover:border-blue-200"}`}><CreditMark /><span className="min-w-0 flex-1"><span className="block truncate text-base font-semibold text-slate-900">{credit.title || "Codex Full Reset"}</span><span className="mt-1 block text-sm text-slate-500">{credit.description || "适用于 Codex · 重置一次使用额度"}</span><span className="mt-1.5 inline-flex rounded-md bg-blue-50 px-2 py-0.5 text-xs font-semibold text-blue-600">{monthly ? "月度" : "Codex"}</span></span><span className="hidden shrink-0 text-right text-sm leading-6 text-slate-500 sm:block"><span className="block">{credit.expiresAt ? `${creditTime(credit.expiresAt)} 过期` : "长期有效"}</span><span className="block">剩余 1 次</span></span><span className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-full border-2 ${checked ? "border-blue-500 bg-blue-500 text-white" : "border-slate-300 bg-white"}`}>{checked && <span className="h-2.5 w-2.5 rounded-full bg-white" />}</span></button>; })}</div>}</section>
          <footer className="mt-8 flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between"><div className="flex items-center gap-3 rounded-2xl bg-blue-50/70 px-4 py-3 text-xs leading-5 text-slate-500"><Info size={21} className="shrink-0 text-blue-500" /><div><p>1. 使用后将立即重置该账号的 Codex 使用额度。</p><p>2. 每张重置卡仅可使用一次，请在确认后操作。</p></div></div><div className="flex shrink-0 justify-end gap-3"><button type="button" onClick={close} disabled={consuming} className="min-h-11 rounded-xl border border-slate-200 bg-white px-5 text-sm font-medium text-slate-800 hover:bg-slate-50 focus-visible:outline-2 focus-visible:outline-blue-500 disabled:opacity-40">取消</button><button type="button" onClick={() => void confirm()} disabled={!selected || loading || consuming} className="inline-flex min-h-11 items-center justify-center gap-2 rounded-xl bg-gradient-to-r from-violet-500 to-blue-500 px-5 text-sm font-semibold text-white shadow-[0_9px_20px_rgba(95,105,255,.2)] hover:from-violet-600 hover:to-blue-600 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-500 disabled:cursor-not-allowed disabled:opacity-45">{consuming ? <Loader2 size={19} className="animate-spin" /> : <ResetCreditIcon size={19} />}确认使用</button></div></footer>
        </> : <><header className="flex justify-end"><button type="button" onClick={close} aria-label="关闭重置卡弹窗" className="flex h-10 w-10 items-center justify-center rounded-xl text-slate-400 hover:bg-slate-100 focus-visible:outline-2 focus-visible:outline-blue-500"><X size={23} /></button></header>{step === "success" && result && <div className="px-4 pb-6 text-center"><div className="mx-auto flex h-20 w-20 items-center justify-center rounded-full bg-emerald-100 text-emerald-600"><Check size={38} /></div><h2 id="reset-credit-title" className="mt-5 text-2xl font-semibold text-slate-900">额度已重置</h2><p className="mt-2 text-sm text-slate-500">{accountName} 可以继续使用 Codex。</p>{operationId && <p className="mt-2 text-xs text-slate-400">任务状态：已完成 · {operationId.slice(0, 8)}</p>}{result.quotaRefreshStatus === "failed" && <p className="mt-3 text-sm text-amber-600">额度回读暂未完成，可稍后点击“刷新额度”。</p>}<button type="button" onClick={close} className="mt-7 min-h-11 rounded-xl bg-gradient-to-r from-violet-500 to-blue-500 px-6 text-sm font-semibold text-white focus-visible:outline-2 focus-visible:outline-blue-500">我知道了</button></div>}{step === "error" && <div className="px-4 pb-6 text-center"><div className="mx-auto flex h-20 w-20 items-center justify-center rounded-full bg-amber-100 text-amber-600"><AlertCircle size={36} /></div><h2 id="reset-credit-title" className="mt-5 text-2xl font-semibold text-slate-900">{result ? outcomeText(result.code) : errorStage === "list" ? "重置卡查询失败" : "重置未完成"}</h2><p className="mx-auto mt-3 max-w-md text-sm leading-6 text-slate-500">{error || outcomeText(result?.code ?? null)}</p>{operationId && errorStage === "consume" && <p className="mt-2 text-xs text-slate-400">任务状态：{operationStatus === "unknown" ? "结果未知，已停止重复提交" : "未完成"} · {operationId.slice(0, 8)}</p>}{(result?.fallbackUrl || snapshot?.fallbackUrl) && <a className="mt-4 inline-flex items-center gap-1 text-sm text-blue-600 hover:underline" href={result?.fallbackUrl || snapshot?.fallbackUrl} target="_blank" rel="noreferrer">打开官方用量页<ExternalLink size={15} /></a>}<div className="mt-7"><button type="button" onClick={close} className="min-h-11 rounded-xl border border-slate-200 px-6 text-sm font-medium text-slate-800 focus-visible:outline-2 focus-visible:outline-blue-500">关闭</button></div></div>}</>}
      </div>
    </div>
  </div>;
}
