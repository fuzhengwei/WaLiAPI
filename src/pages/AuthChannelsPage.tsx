import { useCallback, useEffect, useRef, useState } from "react";
import { ChevronDown, CircleAlert, FileJson, KeyRound, LayoutGrid, List, Loader2, RefreshCw, Upload, X } from "lucide-react";
import { useMemo } from "react";
import { authApi } from "../lib/api";
import { downloadTextFile, isWebRuntime, pickFileAsText } from "../lib/web";
import type { AuthAccount, AuthMutationResult, AuthProviderInfo } from "../types";
import { AccountCard } from "../components/auth/AccountCard";
import { AccountList } from "../components/auth/AccountList";
import { EditModal } from "../components/auth/EditModal";
import { LoginModal } from "../components/auth/LoginModal";
import { ModelSyncModal } from "../components/auth/ModelSyncModal";
import { ProviderPills } from "../components/auth/ProviderPills";
import { ChannelTabs } from "../components/layout/ChannelTabs";

type Confirmation = { kind: "delete"; account: AuthAccount };

/// 与后端 `AuthFileFormat` 对齐的导入格式。sub2api 为多账号批量导入。
type ImportFormat = "codex" | "sub2api" | "cpa";

const IMPORT_OPTIONS: { format: ImportFormat; label: string; description: string }[] = [
  { format: "codex", label: "Codex auth.json", description: "~/.codex/auth.json（单账号）" },
  { format: "sub2api", label: "sub2api.json", description: "批量导入其中 openai 平台账号" },
  { format: "cpa", label: "cpa.json", description: "CLIProxyAPI 认证文件（type: codex）" },
];

function optionLabel(format: ImportFormat) {
  return IMPORT_OPTIONS.find((option) => option.format === format)?.label ?? "Codex auth.json";
}

function importAccountLabel(account: AuthAccount) {
  return account.email || account.label || account.account_id;
}

function importSuccessMessage(result: AuthMutationResult, fallback: string) {
  const imported = result.imported_accounts ?? [result.account];
  if (imported.length <= 1) return result.notice || fallback;
  const labels = imported.map(importAccountLabel).join("、");
  return `${result.notice || `成功导入 ${imported.length} 个账号。`} 导入账号：${labels}`;
}

/// 「导入」触发按钮 + 三格式下拉菜单。头部与空状态卡片共用，
/// 各自持有打开状态并在点击外部时收起。
function ImportDropdown({ busy, onSelect }: { busy: boolean; onSelect: (format: ImportFormat) => void }) {
  const [open, setOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: MouseEvent | TouchEvent) => {
      if (menuRef.current && !menuRef.current.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    document.addEventListener("touchstart", onPointerDown);
    return () => {
      document.removeEventListener("mousedown", onPointerDown);
      document.removeEventListener("touchstart", onPointerDown);
    };
  }, [open]);
  return <div className="relative" ref={menuRef}><button onClick={() => setOpen((value) => !value)} disabled={busy} aria-haspopup="menu" aria-expanded={open} className="action-secondary">{busy ? <Loader2 size={16} className="animate-spin" /> : <Upload size={16} />}导入<ChevronDown size={14} className="opacity-60" /></button>{open && <div role="menu" className="absolute right-0 top-full z-40 mt-2 w-72 overflow-hidden rounded-2xl border border-border bg-card shadow-xl">{IMPORT_OPTIONS.map((option) => <button key={option.format} role="menuitem" onClick={() => { setOpen(false); onSelect(option.format); }} className="flex w-full flex-col items-start gap-0.5 px-4 py-3 text-left hover:bg-muted disabled:opacity-50"><span className="flex items-center gap-2 text-sm font-medium"><FileJson size={15} className="text-muted-foreground" />{option.label}</span><span className="text-xs text-muted-foreground">{option.description}</span></button>)}</div>}</div>;
}

function exportFileName(account: AuthAccount) {
  const base = (account.label || account.email || account.account_id || "codex-auth")
    .replace(/[\\/:*?"<>|]/g, "-")
    .trim();
  return `${base || "codex-auth"}.json`;
}

function providerMark(provider: AuthProviderInfo) {
  if (provider.id === "kimi" || provider.iconKey === "moonshot") return "☾";
  if (provider.id === "grok" || provider.iconKey === "grok") return "✦";
  return "⌘";
}

function EmptyAccountSlot({ provider, onLogin, onSelectImportFormat, busy }: { provider: AuthProviderInfo; onLogin: () => void; onSelectImportFormat: (format: ImportFormat) => void; busy: boolean }) {
  const isDevice = provider.loginMode === "device_code";
  const displayName = provider.displayName;
  return <section className="flex min-h-80 flex-col items-center justify-center rounded-[24px] border border-dashed border-border bg-card/50 p-6 text-center"><div className="flex h-12 w-12 items-center justify-center rounded-2xl bg-success/10 text-xl font-bold text-success">{providerMark(provider)}</div><h2 className="mt-4 font-semibold">＋ 登录 {displayName} 账号</h2><p className="mt-2 max-w-xs text-sm leading-6 text-muted-foreground">{isDevice ? "设备码授权：在浏览器确认后返回。" : "浏览器 OAuth 登录（PKCE）或从本机 ~/.codex/auth.json 导入"}</p><div className="mt-5 flex flex-wrap justify-center gap-2"><button onClick={onLogin} disabled={busy} className="action-primary"><KeyRound size={16} />登录</button>{provider.supportsImport && <ImportDropdown busy={busy} onSelect={onSelectImportFormat} />}</div></section>;
}

function ConfirmationDialog({ confirmation, pending, onCancel, onConfirm }: { confirmation: Confirmation; pending: boolean; onCancel: () => void; onConfirm: () => void }) {
  return <div className="fixed inset-0 z-50 flex items-center justify-center bg-foreground/35 p-4" role="dialog" aria-modal="true" aria-labelledby="auth-confirm-title"><div className="surface w-full max-w-md rounded-[24px] p-6 shadow-2xl"><div className="flex items-start justify-between gap-3"><div><h2 id="auth-confirm-title" className="text-lg font-semibold">删除 Auth 账号</h2><p className="mt-1 text-sm text-muted-foreground">{confirmation.account.label}</p></div><button onClick={onCancel} aria-label="关闭确认弹窗" className="rounded-lg p-1 text-muted-foreground hover:bg-muted"><X size={18} /></button></div><p className="mt-5 text-sm leading-6 text-muted-foreground">是否删除该账号？删除后此账号不再参与路由。仅从本应用移除，不影响对应 CLI 的登录态。</p><div className="mt-6 flex flex-wrap justify-end gap-2"><button onClick={onCancel} className="action-secondary">取消</button><button disabled={pending} onClick={onConfirm} className="inline-flex items-center gap-2 rounded-xl bg-destructive px-4 py-2.5 text-sm font-semibold text-destructive-foreground">{pending ? <Loader2 size={16} className="animate-spin" /> : null}确认删除</button></div></div></div>;
}

export function AuthChannelsPage() {
  const [accounts, setAccounts] = useState<AuthAccount[]>([]);
  const [providers, setProviders] = useState<AuthProviderInfo[]>([]);
  const [selectedProvider, setSelectedProvider] = useState<string>("codex");
  const [loading, setLoading] = useState(true);
  const [pendingId, setPendingId] = useState<string | null>(null);
  const [quotaPendingIds, setQuotaPendingIds] = useState<Set<string>>(new Set());
  const [batchQuotaRefreshing, setBatchQuotaRefreshing] = useState(false);
  const [viewMode, setViewMode] = useState<"list" | "card">(() => {
    try { return localStorage.getItem("waliapi:auth-channel-view") === "card" ? "card" : "list"; } catch { return "list"; }
  });
  const [showLogin, setShowLogin] = useState(false);
  const [reloginAccount, setReloginAccount] = useState<AuthAccount | null>(null);
  const [editAccount, setEditAccount] = useState<AuthAccount | null>(null);
  const [syncAccount, setSyncAccount] = useState<AuthAccount | null>(null);
  const [confirmation, setConfirmation] = useState<Confirmation | null>(null);
  const [notice, setNotice] = useState<{ kind: "success" | "error" | "warning"; message: string } | null>(null);
  const [channelTabRefreshKey, setChannelTabRefreshKey] = useState(0);

  useEffect(() => {
    let disposed = false;
    authApi
      .providersList()
      .then((list) => { if (!disposed) setProviders(list); })
      .catch(() => {});
    return () => { disposed = true; };
  }, []);

  const activeProvider = providers.find((p) => p.id === selectedProvider) ?? {
    id: selectedProvider,
    displayName: selectedProvider === "kimi" ? "Kimi Code" : selectedProvider === "grok" ? "Grok" : "Codex",
    iconKey: selectedProvider === "kimi" ? "moonshot" : selectedProvider === "grok" ? "grok" : "codex",
    loginMode: selectedProvider === "kimi" || selectedProvider === "grok" ? "device_code" : "browser_callback",
    loginMethods: selectedProvider === "kimi" || selectedProvider === "grok"
      ? ["device_code" as const]
      : ["browser_callback" as const, "device_code" as const],
    supportsImport: selectedProvider === "codex",
    supportsExport: selectedProvider === "codex",
    supportsQuota: selectedProvider === "codex",
  };

  // The pill selects which provider's accounts are shown.  `load()` re-fetches
  // the full list after every mutation, so a newly added account of the
  // currently selected provider appears here (kimi-auth.md §Verification:
  // "Provider filter 不隐藏新增账号后的刷新结果").
  const visibleAccounts = useMemo(() => accounts
    .filter((a) => a.provider === selectedProvider)
    .map((account, index) => ({ account, index }))
    .sort((a, b) => {
      const availability = (account: AuthAccount) => account.status !== "invalid" && !account.disabled && !account.quota?.exceeded ? 1 : 0;
      return availability(b.account) - availability(a.account)
        || b.account.priority - a.account.priority
        || b.account.weight - a.account.weight
        || a.index - b.index;
    })
    .map(({ account }) => account), [accounts, selectedProvider]);

  const load = useCallback(async (showLoading = true) => {
    if (showLoading) setLoading(true);
    try { setAccounts(await authApi.accountsList()); }
    catch (_) { setNotice({ kind: "error", message: "Auth 账号加载失败，请检查本地服务后重试。" }); }
    finally { if (showLoading) setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const runFor = async (id: string, success: string, work: () => Promise<void>) => {
    setPendingId(id);
    try { await work(); setNotice({ kind: "success", message: success }); await load(false); }
    catch (_) { setNotice({ kind: "error", message: "操作失败，请稍后重试。" }); }
    finally { setPendingId(null); }
  };

  const refreshQuota = async (id: string) => {
    setQuotaPendingIds((ids) => new Set(ids).add(id));
    try {
      await authApi.refreshQuota(id);
      setNotice({ kind: "success", message: "额度刷新完成。" });
      await load(false);
    } catch (_) {
      setNotice({ kind: "error", message: "额度刷新失败，已保留上次额度数据。" });
    } finally {
      setQuotaPendingIds((ids) => {
        const next = new Set(ids);
        next.delete(id);
        return next;
      });
    }
  };

  const refreshVisibleQuotas = async () => {
    const eligible = visibleAccounts.filter((account) => activeProvider.supportsQuota && account.status !== "invalid" && !account.disabled);
    if (eligible.length === 0 || batchQuotaRefreshing) return;
    setBatchQuotaRefreshing(true);
    setQuotaPendingIds(new Set(eligible.map((account) => account.id)));
    try {
      const results = await Promise.allSettled(eligible.map((account) => authApi.refreshQuota(account.id)));
      await load(false);
      const succeeded = results.filter((result) => result.status === "fulfilled").length;
      const failed = results.length - succeeded;
      setNotice({
        kind: failed === 0 ? "success" : "warning",
        message: failed === 0 ? `额度刷新完成，${succeeded} 个账号已更新。` : `额度刷新完成，${succeeded} 个成功，${failed} 个失败。`,
      });
    } catch (_) {
      setNotice({ kind: "error", message: "额度刷新失败，已保留上次额度数据。" });
    } finally {
      setQuotaPendingIds(new Set());
      setBatchQuotaRefreshing(false);
    }
  };

  const setAuthViewMode = (mode: "list" | "card") => {
    setViewMode(mode);
    try { localStorage.setItem("waliapi:auth-channel-view", mode); } catch {}
  };

  const actionFor = (account: AuthAccount) => ({
    pending: pendingId === account.id || quotaPendingIds.has(account.id),
    quotaPending: quotaPendingIds.has(account.id),
    onEdit: () => setEditAccount(account),
    onToggle: () => void runFor(account.id, account.disabled ? "账号已启用。" : "账号已停用。", () => authApi.toggle(account.id, !account.disabled).then(() => undefined)),
    onDelete: () => setConfirmation({ kind: "delete", account }),
    onRefresh: () => void runFor(account.id, "令牌刷新完成。", () => authApi.refreshToken(account.id).then(() => undefined)),
    onRefreshQuota: () => void refreshQuota(account.id),
    onSync: () => setSyncAccount(account),
    onExport: () => void exportAuth(account),
    onRelogin: () => { setReloginAccount(account); setSelectedProvider(account.provider); setShowLogin(true); },
  });

  const handleReorder = async (orderedIds: string[]) => {
    // 乐观更新本地顺序
    const next = orderedIds
      .map((id) => accounts.find((a) => a.id === id))
      .filter((a): a is AuthAccount => !!a);
    setAccounts(next);
    try {
      await authApi.reorder(orderedIds);
    } catch (_) {
      setNotice({ kind: "error", message: "排序保存失败，已恢复原有顺序。" });
      await load(false);
    }
  };

  const completeLogin = (result: AuthMutationResult) => {
    setShowLogin(false);
    setReloginAccount(null);
    const displayName = activeProvider.displayName;
    setNotice(result.warning ? { kind: "warning", message: "账号已保存但暂不参与路由：模型同步失败。" } : { kind: "success", message: `${displayName} 账号登录完成。` });
    void load(false);
  };
  const importAuth = async (format: ImportFormat) => {
    // Web 版：浏览器文件选择器读取内容后按内容导入
    if (isWebRuntime()) {
      const content = await pickFileAsText(".json,application/json");
      if (content === null) return; // 用户取消 → 静默 no-op
      setPendingId("import"); setNotice({ kind: "success", message: "正在导入 …" });
      try {
        const result = await authApi.loginImportContent("codex", content, format);
        setNotice(result.warning ? { kind: "warning", message: "账号已保存但暂不参与路由：模型同步失败。" } : { kind: "success", message: importSuccessMessage(result, "已导入账号。") });
        await load(false);
        setChannelTabRefreshKey((key) => key + 1);
      } catch (_) {
        setNotice({ kind: "error", message: `导入失败，请确认所选文件是有效的${optionLabel(format)}。` });
      } finally { setPendingId(null); }
      return;
    }
    let path: string | null = null;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      let defaultPath: string | undefined;
      if (format === "codex") {
        try {
          defaultPath = await authApi.defaultImportPath();
        } catch {
          // 默认路径解析失败(无 home)时回退为不带 defaultPath 弹框,仍可手动选文件
        }
      }
      const fileLabel = optionLabel(format);
      path = await open({
        title: format === "codex" ? "选择 Codex auth.json 文件" : `选择 ${fileLabel} 文件`,
        filters: [{ name: fileLabel, extensions: ["json"] }],
        multiple: false,
        defaultPath,
      });
    } catch {
      // 对话框不可用,忽略(保持旧行为直接读默认路径)
    }
    if (path === null) return; // 用户取消 → 静默 no-op
    const label = path; // 实际选中路径
    setPendingId("import"); setNotice({ kind: "success", message: `正在读取 ${label} …` });
    try {
      const result = await authApi.loginImport("codex", path, format);
      setNotice(result.warning ? { kind: "warning", message: "账号已保存但暂不参与路由：模型同步失败。" } : { kind: "success", message: importSuccessMessage(result, `已从 ${label} 导入账号。`) });
      await load(false);
      setChannelTabRefreshKey((key) => key + 1);
    } catch (_) {
      setNotice({ kind: "error", message: `导入失败，请确认所选文件是有效的${optionLabel(format)}。` });
    } finally { setPendingId(null); }
  };
  const confirmAction = async () => {
    if (!confirmation) return;
    const { account } = confirmation;
    setPendingId(account.id);
    try {
      await authApi.logout(account.id);
      setNotice({ kind: "success", message: "账号已删除。" });
      setConfirmation(null);
      await load(false);
    } catch (_) {
      setNotice({ kind: "error", message: "操作失败，请稍后重试。" });
    } finally {
      setPendingId(null);
    }
  };
  const exportAuth = async (account: AuthAccount) => {
    // Web 版：后端返回文件内容，浏览器直接下载
    if (isWebRuntime()) {
      setPendingId(account.id);
      try {
        const content = await authApi.exportJsonContent(account.id);
        downloadTextFile(content, exportFileName(account));
        setNotice({ kind: "success", message: "已导出 auth.json。" });
      } catch (_) {
        setNotice({ kind: "error", message: "导出失败，请稍后重试。" });
      } finally {
        setPendingId(null);
      }
      return;
    }
    let path: string | null = null;
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      path = await save({
        title: "导出 Codex auth JSON",
        filters: [{ name: "Codex auth JSON", extensions: ["json"] }],
        defaultPath: exportFileName(account),
      });
    } catch {
      setNotice({ kind: "error", message: "无法打开保存位置选择器，请稍后重试。" });
      return;
    }
    if (path === null) return;
    setPendingId(account.id);
    try {
      const result = await authApi.exportJson(account.id, path);
      const backup = result.backup_path ? `；已备份原文件：${result.backup_path}` : "";
      setNotice({ kind: "success", message: `已导出到 ${result.path}${backup}` });
      await load(false);
    } catch (_) {
      setNotice({ kind: "error", message: "导出失败，请稍后重试。" });
    } finally {
      setPendingId(null);
    }
  };

  return <div className="page-shell space-y-3"><div className="page-header sticky top-0 z-30 -mx-7 -mt-7 mb-2 flex-col bg-card/90 px-7 pt-3"><div className="flex w-full items-start justify-between gap-4 pb-1.5"><div><h1 className="page-title">渠道管理</h1><p className="page-subtitle mt-0.5">登录各厂商订阅账号，作为上游路由候选</p></div><div className="flex items-center gap-2"><button onClick={() => { setReloginAccount(null); setShowLogin(true); }} disabled={pendingId === "import"} className="action-primary"><KeyRound size={16} />登录账号</button>{activeProvider.supportsImport && <ImportDropdown busy={pendingId === "import"} onSelect={(format) => void importAuth(format)} />}</div></div><ChannelTabs refreshKey={channelTabRefreshKey} /></div>
    {notice && <div role="status" className={`flex items-center justify-between gap-3 rounded-2xl border px-4 py-3 text-sm ${notice.kind === "error" ? "border-destructive/25 bg-destructive/10 text-destructive" : notice.kind === "warning" ? "border-warning/25 bg-warning/10 text-warning" : "border-success/25 bg-success/10 text-success"}`}><span>{notice.message}</span><button onClick={() => setNotice(null)} aria-label="关闭提示"><X size={16} /></button></div>}
    <ProviderPills selected={selectedProvider} onSelect={setSelectedProvider} />
    <div className="flex flex-wrap items-center justify-between gap-3">
      <p className="text-sm text-muted-foreground">登录后作为路由候选并消耗订阅额度；开启 Auth 账号优先后，将优先使用。</p>
      <div className="flex items-center gap-2">
        {activeProvider.supportsQuota && <button onClick={() => void refreshVisibleQuotas()} disabled={batchQuotaRefreshing || visibleAccounts.every((account) => account.status === "invalid" || account.disabled)} className="action-secondary">
          {batchQuotaRefreshing ? <Loader2 size={16} className="animate-spin" /> : <RefreshCw size={16} />}
          {batchQuotaRefreshing ? `刷新额度 ${quotaPendingIds.size}/${visibleAccounts.length}` : "刷新全部额度"}
        </button>}
        <div className="flex items-center rounded-xl border border-border bg-card p-1" aria-label="账号视图切换">
          <button onClick={() => setAuthViewMode("list")} className={`rounded-lg p-1.5 ${viewMode === "list" ? "bg-primary/10 text-primary" : "text-muted-foreground hover:bg-muted"}`} title="列表视图" aria-label="列表视图" aria-pressed={viewMode === "list"}><List size={16} /></button>
          <button onClick={() => setAuthViewMode("card")} className={`rounded-lg p-1.5 ${viewMode === "card" ? "bg-primary/10 text-primary" : "text-muted-foreground hover:bg-muted"}`} title="卡片视图" aria-label="卡片视图" aria-pressed={viewMode === "card"}><LayoutGrid size={16} /></button>
        </div>
      </div>
    </div>
    <div className="flex gap-2 rounded-2xl border border-destructive/25 bg-destructive/10 px-4 py-3 text-xs leading-5 text-destructive"><CircleAlert className="mt-0.5 shrink-0" size={16} /><p>⚠️ 风险提示：此提供商使用的订阅 / OAuth 会话未获官方授权用于代理 / 路由器使用。账户可能被限制或封禁。使用风险自负。</p></div>
    {loading ? <div className="flex min-h-64 items-center justify-center gap-2 text-sm text-muted-foreground"><Loader2 size={18} className="animate-spin" />加载 Auth 账号…</div> : visibleAccounts.length === 0 ? <EmptyAccountSlot provider={activeProvider} onLogin={() => { setReloginAccount(null); setShowLogin(true); }} onSelectImportFormat={(format) => void importAuth(format)} busy={pendingId === "import"} /> : viewMode === "list" ? <AccountList accounts={visibleAccounts} actionFor={actionFor} onReorder={handleReorder} /> : <div className="grid grid-cols-1 gap-5 xl:grid-cols-2">{visibleAccounts.map(account => { const actions = actionFor(account); return <AccountCard key={account.id} account={account} pending={actions.pending} quotaPending={actions.quotaPending} onEdit={actions.onEdit} onToggle={actions.onToggle} onDelete={actions.onDelete} onRefresh={actions.onRefresh} onRefreshQuota={actions.onRefreshQuota} onSync={actions.onSync} onExport={actions.onExport} onRelogin={actions.onRelogin} />; })}</div>}
    {showLogin && <LoginModal provider={activeProvider} replaceAccountId={reloginAccount?.id} onClose={() => { setShowLogin(false); setReloginAccount(null); }} onCompleted={completeLogin} />}
    {editAccount && <EditModal account={editAccount} pending={pendingId === editAccount.id} onClose={() => setEditAccount(null)} onSave={async input => { await runFor(input.id, "账号配置已保存。", () => authApi.update(input).then(() => undefined)); setEditAccount(null); }} />}
    {syncAccount && <ModelSyncModal account={syncAccount} onClose={() => setSyncAccount(null)} onSynced={() => { void load(false); setNotice({ kind: "success", message: "模型同步完成。" }); }} />}
    {confirmation && <ConfirmationDialog confirmation={confirmation} pending={pendingId === confirmation.account.id} onCancel={() => setConfirmation(null)} onConfirm={() => void confirmAction()} />}
  </div>;
}
