import { useEffect, useState } from "react";
import {
  Check,
  CircleAlert,
  Copy,
  ExternalLink,
  Loader2,
  RefreshCw,
  X,
} from "lucide-react";
import { authApi } from "../../lib/api";
import { isTauriRuntime } from "../../lib/runtime";
import type {
  AuthLoginMethod,
  AuthLoginSessionStatus,
  AuthMutationResult,
  AuthProviderInfo,
  DeviceVerification,
} from "../../types";

const codexSteps = [
  "准备本地回调",
  "打开浏览器授权",
  "等待授权回调",
  "交换令牌",
  "保存账号",
  "同步模型",
];

const kimiSteps = [
  "申请设备授权",
  "打开 Kimi 授权页",
  "等待确认",
  "交换令牌",
  "保存账号",
  "同步模型",
];

const geminiSteps = [
  "准备本地回调",
  "打开 Antigravity 授权",
  "等待授权回调",
  "交换令牌",
  "保存账号",
  "同步模型",
];


const codexDeviceSteps = [
  "申请设备授权",
  "打开 OpenAI 授权页",
  "等待确认",
  "交换令牌",
  "保存账号",
  "同步模型",
];

function stepIndex(status: AuthLoginSessionStatus): number {
  switch (status.step) {
    case "preparing":
      return 0;
    case "authorizing":
      return 1;
    case "waiting":
      return 2;
    case "exchanging":
      return 3;
    case "saving":
      return 4;
    case "syncing":
      return 5;
    default:
      return 0;
  }
}

export function LoginModal({
  provider,
  replaceAccountId,
  onClose,
  onCompleted,
}: {
  provider: AuthProviderInfo;
  replaceAccountId?: string;
  onClose: () => void;
  onCompleted: (result: AuthMutationResult) => void;
}) {
  const [state, setState] = useState<"idle" | "running" | "done" | "error">("idle");
  const [currentStep, setCurrentStep] = useState(-1);
  const [error, setError] = useState<string | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [verification, setVerification] = useState<DeviceVerification | null>(null);
  const [copied, setCopied] = useState(false);
  const needsChoice = provider.loginMethods.length > 1;
  const [loginMethod, setLoginMethod] = useState<AuthLoginMethod | null>(
    needsChoice ? null : (provider.loginMethods[0] ?? "device_code"),
  );
  const isDevice = loginMethod === "device_code";
  const steps = isDevice
    ? provider.id === "kimi" ? kimiSteps : codexDeviceSteps
    : provider.id === "gemini" ? geminiSteps : codexSteps;
  const isDesktop = isTauriRuntime();

  useEffect(() => {
    if (!sessionId || state !== "running") return;
    let disposed = false;
    const apply = (status: AuthLoginSessionStatus) => {
      if (disposed) return;
      setCurrentStep(stepIndex(status));
      setVerification(status.verification);
      if (status.state === "succeeded" && status.result) {
        setState("done");
        setVerification(null);
        onCompleted(status.result);
      } else if (status.state === "cancelled") {
        setState("idle"); setSessionId(null); setCurrentStep(-1); setVerification(null);
        setError(status.error ?? "登录已取消，可以重新开始。");
      } else if (status.state === "failed") {
        setState("error"); setSessionId(null); setVerification(null);
        setError(status.error ?? "登录未完成，请重试。");
      }
    };
    const poll = async () => {
      try { apply(await authApi.loginStatus(sessionId)); }
      catch (_) { if (!disposed) { setState("error"); setSessionId(null); setVerification(null); setError("无法查询登录状态，请重新开始登录。"); } }
    };
    void poll();
    const interval = window.setInterval(() => void poll(), 350);
    return () => { disposed = true; window.clearInterval(interval); };
  }, [sessionId, state, onCompleted]);

  const login = async () => {
    setState("running"); setCurrentStep(0); setError(null);
    try {
      if (!loginMethod) return;
      const session = await authApi.loginStart(provider.id, replaceAccountId, loginMethod);
      setSessionId(session.sessionId);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : "无法启动登录，请重试。";
      setState("error"); setError(message);
    }
  };

  const cancel = async () => {
    const activeSession = sessionId;
    // Return to a retryable state immediately; the server tombstone prevents a
    // late callback from persisting credentials after this request.
    setState("idle"); setSessionId(null); setCurrentStep(-1); setVerification(null); setError("登录已取消，可以重新开始。");
    if (activeSession) {
      try {
        const status = await authApi.loginCancel(activeSession);
        // Once the commit gate has opened, cancellation cannot honestly claim
        // that no account will be written. Resume status tracking instead.
        if (status.state === "saving" || status.state === "syncing") {
          setSessionId(activeSession); setState("running");
          setError("账号正在保存，无法安全取消；请等待当前操作完成。");
        }
      }
      catch (_) { setError("取消请求未确认，请稍后重新打开登录窗口。"); }
    }
  };

  const copyUserCode = async () => {
    if (!verification) return;
    try {
      await navigator.clipboard.writeText(verification.userCode);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch (_) { /* clipboard unavailable */ }
  };

  const reopenBrowser = () => {
    if (verification) {
      window.open(verification.url, "_blank", "noopener,noreferrer");
    }
  };


  return <div className="fixed inset-0 z-50 flex items-center justify-center bg-foreground/35 p-4" role="dialog" aria-modal="true" aria-labelledby="login-auth-title">
    <div className="surface w-full max-w-lg rounded-[24px] p-6 shadow-2xl"><div className="flex items-start justify-between"><div><h2 id="login-auth-title" className="text-lg font-semibold">登录 {provider.displayName} 账号</h2><p className="mt-1 text-sm text-muted-foreground">{isDevice ? "设备码授权 · 浏览器确认 · 消耗订阅额度" : provider.id === "gemini" ? "Antigravity OAuth · 本机回调 · 消耗订阅额度" : "浏览器 OAuth 授权 · PKCE · 消耗订阅额度"}</p></div><button onClick={() => state === "running" ? void cancel() : onClose()} disabled={state === "running" && !isDevice} aria-label="关闭登录弹窗" className="rounded-lg p-1 text-muted-foreground hover:bg-muted"><X size={18} /></button></div>
      {needsChoice && !loginMethod && <div className="mt-5 space-y-3">
        <p className="text-sm font-medium">选择登录方式</p>
        <button type="button" disabled={!isDesktop} onClick={() => setLoginMethod("browser_callback")} className="w-full rounded-xl border border-border p-4 text-left hover:bg-muted disabled:cursor-not-allowed disabled:opacity-55"><span className="block text-sm font-semibold">浏览器 OAuth</span><span className="mt-1 block text-xs text-muted-foreground">{isDesktop ? "通过本机 localhost 回调完成授权" : "仅桌面端支持 localhost 回调"}</span></button>
        <button type="button" onClick={() => setLoginMethod("device_code")} className="w-full rounded-xl border border-border p-4 text-left hover:bg-muted"><span className="block text-sm font-semibold">设备代码授权（Device Code）</span><span className="mt-1 block text-xs text-muted-foreground">在任意设备打开授权页并输入一次性代码，适用于 Web / Docker</span></button>
        {provider.id === "codex" && <p className="rounded-xl bg-primary/10 px-3 py-2.5 text-xs leading-5 text-primary">首次使用前，请登录 ChatGPT Web，依次进入“设置 → 账户 → 安全与登录”，打开“为 Codex 启用设备代码授权”。</p>}
      </div>}
      {loginMethod && <>
      <ol className="mt-5 space-y-3" aria-label="登录步骤">{steps.map((step, index) => { const complete = state === "done" || index < currentStep; const active = state === "running" && index === currentStep; return <li key={step} className="flex items-center gap-3 rounded-xl border border-border bg-muted/35 px-3 py-2.5 text-sm"><span className={`flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-xs font-semibold ${complete ? "bg-success text-white" : active ? "bg-primary text-primary-foreground" : "bg-card text-muted-foreground"}`}>{complete ? <Check size={14} /> : active ? <Loader2 size={14} className="animate-spin" /> : index + 1}</span><span className={active ? "font-medium" : "text-muted-foreground"}>{step}</span></li>; })}</ol>
      {isDevice && verification && state === "running" && <div className="mt-4 space-y-3 rounded-xl border border-border bg-muted/35 p-4">
        <div className="flex items-center justify-between gap-3"><div><p className="text-xs font-medium text-muted-foreground">授权码（请勿发送给他人）</p><p className="mt-1 font-mono text-2xl font-bold tracking-widest">{verification.userCode}</p></div><button onClick={() => void copyUserCode()} className="action-secondary px-2 py-2 text-xs">{copied ? <Check size={14} /> : <Copy size={14} />}{copied ? "已复制" : "复制授权码"}</button></div>
        <div className="space-y-1.5"><p className="text-xs font-medium text-muted-foreground">授权地址</p><p className="break-all font-mono text-xs text-primary">{verification.url}</p></div>
        <button onClick={reopenBrowser} className="action-secondary w-full justify-center px-2 py-2 text-xs"><RefreshCw size={14} />重新打开授权页</button>
        {verification.expiresAt && <p className="text-[11px] text-muted-foreground">授权码有效期至 {new Date(verification.expiresAt).toLocaleString("zh-CN")}</p>}
      </div>}

      {state === "running" && !isDevice && <p className="mt-4 flex items-center gap-2 rounded-xl bg-primary/10 px-3 py-2.5 text-sm text-primary"><ExternalLink size={15} />已在浏览器打开授权页，请完成授权；回调后应用会继续交换令牌和保存账号</p>}
      {error && <p role="alert" className="mt-4 flex items-center gap-2 rounded-xl bg-destructive/10 px-3 py-2.5 text-sm text-destructive"><CircleAlert size={15} />{error}</p>}
      {state === "done" && <p className="mt-4 rounded-xl bg-success/10 px-3 py-2.5 text-sm text-success">账号已保存。{currentStep === 5 ? "模型同步已完成。" : ""}</p>}
      </>}
      {error && isDevice && provider.id === "codex" && <p className="mt-2 text-xs leading-5 text-muted-foreground">请登录 ChatGPT Web，进入“设置 → 账户 → 安全与登录”，确认已打开“为 Codex 启用设备代码授权”。Workspace 用户可能还需联系管理员开启权限；也可改用 auth.json 导入。</p>}
      <div className="mt-6 flex justify-end gap-2">{state === "running" ? <button onClick={() => void cancel()} className="action-secondary">取消登录</button> : state !== "done" && <button onClick={onClose} className="action-secondary">取消</button>}{state === "done" ? <button onClick={onClose} className="action-primary">完成</button> : <button onClick={() => void login()} disabled={state === "running" || !loginMethod} className="action-primary">{state === "running" ? "登录中…" : "开始登录"}</button>}</div>
    </div>
  </div>;
}
