import { useEffect, useState, useCallback, useMemo } from "react";
import { useNavigate } from "react-router-dom";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import { appConfigApi, serverApi, apiKeyApi, channelApi, authApi } from "../lib/api";
import type { AppInfo, ConfigContent } from "../lib/api";
import type { ServerStatus, Channel, ApiKey, AuthAccount } from "../types";
import { makeKeyGuards } from "../lib/keyRules";
import {
  Terminal,
  Code2,
  Sparkles,
  Boxes,
  Bot,
  Wrench,
  Check,
  Loader2,
  AlertCircle,
  RefreshCw,
  ArrowRight,
  FolderOpen,
  FileText,
  KeyRound,
  ChevronDown,
  Link2,
  Download,
  Copy,
  Lightbulb,
} from "lucide-react";

// ── 图标映射 ──

const APP_ICONS: Record<string, React.ComponentType<{ size?: number; className?: string }>> = {
  "claude-code": Terminal,
  "claude-desktop": Sparkles,
  "codex": Code2,
  "gemini-cli": Boxes,
  "opencode": Wrench,
  "openclaw": Bot,
  "hermes": Code2,
  "walicode": Code2,
};

export function getAppIcon(name: string) {
  return APP_ICONS[name] || Terminal;
}

// ── 单应用配置面板 ──

export function AppConfigPanel({ appName }: { appName: string }) {
  const navigate = useNavigate();
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const [configContent, setConfigContent] = useState<ConfigContent | null>(null);
  const [ss, setSs] = useState<ServerStatus | null>(null);
  const [keys, setKeys] = useState<ApiKey[]>([]);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [authAccounts, setAuthAccounts] = useState<AuthAccount[]>([]);
  const [selKey, setSelKey] = useState("");
  const [selModel, setSelModel] = useState("");
  const [loading, setLoading] = useState(true);

  // Storage key for persisting model selection per app
  const modelStorageKey = `waliapi:codex-model:${appName}`;
  const keyStorageKey = `waliapi:codex-key:${appName}`;
  const [applying, setApplying] = useState(false);
  const [appliedResult, setAppliedResult] = useState<{ success: boolean; message: string; authWarning?: string | null } | null>(null);
  const [resultKind, setResultKind] = useState<"apply" | "clear" | "reset">("apply");
  const [copied, setCopied] = useState(false);

  const load = useCallback(async () => {
    try {
      const [appList, content, status, ks, chs, accts] = await Promise.all([
        appConfigApi.getApps(),
        appConfigApi.getContent(appName),
        serverApi.getStatus().catch(() => null),
        apiKeyApi.getAll().catch(() => []),
        channelApi.getAll().catch(() => []),
        authApi.accountsList().catch(() => []),
      ]);
      const info = appList.find(a => a.name === appName);
      setAppInfo(info || null);
      setConfigContent(content);
      setSs(status as ServerStatus | null);
      const keyList = ks as ApiKey[];
      const chList = chs as Channel[];
      const acctList = accts as AuthAccount[];
      setKeys(keyList);
      setChannels(chList);
      setAuthAccounts(acctList);

      // Restore persisted key selection, fallback to first key
      // （FIX-19：密钥改存会话级；FIX-13：选择标识改用 key id）
      const savedKey = sessionStorage.getItem(keyStorageKey);
      const savedKeyValid = savedKey && keyList.some(k => k.id === savedKey);
      if (keyList.length > 0 && !selKey) {
        setSelKey(savedKeyValid ? savedKey! : keyList[0].id);
      }

      // Restore persisted model selection, fallback to first model
      // (filtered by selected key's allowed/denied lists)
      const selectedKeyObj = keyList.find(k => k.id === (savedKeyValid ? savedKey! : keyList[0]?.id));
      const { channelAllowed, modelAllowed } = makeKeyGuards(selectedKeyObj);
      const ms: string[] = [];
      chList.forEach(c => {
        if (!channelAllowed(c.id)) return;
        c.models.forEach(m => { if (modelAllowed(m) && !ms.includes(m)) ms.push(m); });
        if (c.model_mapping) {
          Object.keys(c.model_mapping).forEach(from => { if (modelAllowed(from) && !ms.includes(from)) ms.push(from); });
        }
      });
      acctList.forEach(a => {
        if (a.disabled) return;
        a.models.forEach(m => { if (!m.unavailable && modelAllowed(m.id) && !ms.includes(m.id)) ms.push(m.id); });
        if (a.model_mapping) {
          Object.keys(a.model_mapping).forEach(from => { if (modelAllowed(from) && !ms.includes(from)) ms.push(from); });
        }
      });
      if (ms.length > 0 && !selModel) {
        const savedModel = localStorage.getItem(modelStorageKey);
        setSelModel(savedModel && ms.includes(savedModel) ? savedModel : ms[0]);
      }
    } catch (e) {
      console.error("Failed to load app config:", e);
    } finally {
      setLoading(false);
    }
  }, [appName]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => { load(); }, [load]);

  // Find the currently selected API key object.
  const selectedApiKey = useMemo(
    () => keys.find(k => k.id === selKey),
    [keys, selKey],
  );

  // 模型列表 — filtered by selected API key's allowed/denied lists
  const modelList = useMemo(() => {
    const { channelAllowed, modelAllowed } = makeKeyGuards(selectedApiKey);
    const realSeen = new Set<string>();
    const mappedSeen = new Set<string>();
    const real: string[] = [];
    const mapped: string[] = [];
    channels.forEach(c => {
      if (!channelAllowed(c.id)) return;
      c.models.forEach(m => {
        if (modelAllowed(m) && !realSeen.has(m)) { realSeen.add(m); real.push(m); }
      });
      if (c.model_mapping) {
        Object.keys(c.model_mapping).forEach(from => {
          if (modelAllowed(from) && !mappedSeen.has(from)) { mappedSeen.add(from); mapped.push(from); }
        });
      }
    });
    // Auth accounts: exempt from channel-level restrictions
    authAccounts.forEach(a => {
      if (a.disabled) return;
      a.models.forEach(m => {
        if (!m.unavailable && modelAllowed(m.id) && !realSeen.has(m.id)) { realSeen.add(m.id); real.push(m.id); }
      });
      if (a.model_mapping) {
        Object.keys(a.model_mapping).forEach(from => {
          if (modelAllowed(from) && !mappedSeen.has(from)) { mappedSeen.add(from); mapped.push(from); }
        });
      }
    });
    return { real, mapped };
  }, [channels, authAccounts, selectedApiKey]);

  const allModels = useMemo(() => [...modelList.real, ...modelList.mapped], [modelList]);

  // Reset selModel if it's no longer in the filtered list after switching API Key.
  useEffect(() => {
    if (allModels.length > 0 && !allModels.includes(selModel)) {
      const fallback = allModels[0];
      setSelModel(fallback);
      localStorage.setItem(modelStorageKey, fallback);
    }
  }, [allModels, selModel]);

  const handleApply = async () => {
    if (!selKey || !selModel) return;
    setApplying(true);
    setAppliedResult(null);
    setResultKind("apply");
    try {
      // FIX-13：列表只回掩码，写配置前按需取完整密钥。
      const fullKey = await apiKeyApi.getFull(selKey);
      const result = await appConfigApi.apply(appName, fullKey, selModel);
      setAppliedResult(result);
      const [appList, content] = await Promise.all([
        appConfigApi.getApps(),
        appConfigApi.getContent(appName),
      ]);
      setAppInfo(appList.find(a => a.name === appName) || null);
      setConfigContent(content);
    } catch (e: any) {
      setAppliedResult({ success: false, message: String(e) });
    } finally {
      setApplying(false);
    }
  };

  const handleClear = async () => {
    setApplying(true);
    setAppliedResult(null);
    setResultKind("clear");
    try {
      const result = await appConfigApi.clear(appName);
      setAppliedResult(result);
      const [appList, content] = await Promise.all([
        appConfigApi.getApps(),
        appConfigApi.getContent(appName),
      ]);
      setAppInfo(appList.find(a => a.name === appName) || null);
      setConfigContent(content);
    } catch (e: any) {
      setAppliedResult({ success: false, message: String(e) });
    } finally {
      setApplying(false);
    }
  };

  const handleOpenFolder = async () => {
    try {
      await appConfigApi.openFolder(appName);
    } catch (e) {
      console.error("Failed to open folder:", e);
    }
  };

  // Codex 专用：auth.json 卡在 API Key 模式时，重置为 ChatGPT 登录模式
  const handleResetAuth = async () => {
    setApplying(true);
    setResultKind("reset");
    try {
      const result = await appConfigApi.resetCodexAuth();
      setAppliedResult(result);
    } catch (e: any) {
      setAppliedResult({ success: false, message: String(e) });
    } finally {
      setApplying(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[300px]">
        <Loader2 className="h-6 w-6 animate-spin text-slate-400" />
      </div>
    );
  }

  if (!appInfo) {
    return (
      <div className="surface flex min-h-[300px] items-center justify-center rounded-2xl text-sm text-slate-400">
        应用信息加载失败
      </div>
    );
  }

  const Icon = getAppIcon(appName);
  const gatewayUrl = ss?.running ? ss.url : "http://127.0.0.1:8777";

  return (
    <div className="space-y-4">
      {/* 应用信息卡片 */}
      <div className="surface rounded-2xl p-5">
        <div className="mb-4 flex items-center gap-4 border-b border-slate-100 pb-4">
          <div className="flex h-11 w-11 items-center justify-center rounded-2xl bg-gradient-to-br from-blue-50 to-indigo-50 text-blue-600">
            <Icon size={20} />
          </div>
          <div className="flex-1">
            <div className="flex items-center gap-2">
              <h2 className="text-base font-bold tracking-tight text-slate-900">{appInfo.label}</h2>
              {appInfo.applied && (
                <span className="inline-flex items-center gap-1 rounded-full bg-emerald-50 px-2 py-0.5 text-[11px] font-medium text-emerald-600">
                  <Check size={11} /> {appName === "codex" ? "已切换到网关" : "已配置"}
                </span>
              )}
              {appInfo.available ? (
                <span className="inline-flex items-center gap-1 rounded-full bg-emerald-50 px-2 py-0.5 text-[11px] font-medium text-emerald-600">
                  已安装
                </span>
              ) : (
                <span className="inline-flex items-center gap-1 rounded-full bg-amber-50 px-2 py-0.5 text-[11px] font-medium text-amber-600">
                  未检测到
                </span>
              )}
              {/* 下载链接协议白名单（FIX-24）：配置来源不可信，只渲染 http(s)，防 javascript: 等协议注入 */}
              {appInfo.download_url && /^https?:\/\//i.test(appInfo.download_url) && (
                <a
                  href={appInfo.download_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-1 rounded-full bg-blue-50 px-2 py-0.5 text-[11px] font-medium text-blue-600 transition-colors hover:bg-blue-100"
                  title={`下载 ${appInfo.label}`}
                >
                  <Download size={11} />
                  下载
                </a>
              )}
            </div>
            <p className="mt-0.5 text-xs text-slate-500">{appInfo.description}</p>
          </div>
          <button
            onClick={load}
            className="rounded-lg p-2 text-slate-400 transition-colors hover:bg-slate-100 hover:text-slate-600"
            title="刷新"
          >
            <RefreshCw size={14} />
          </button>
        </div>

        {/* 网关地址 + 配置文件路径 */}
        <div className="mb-4 grid grid-cols-1 gap-3 md:grid-cols-2">
          <div className="rounded-xl border border-slate-100 bg-slate-50/80 px-4 py-3">
            <div className="mb-1 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wider text-slate-400">
              <Link2 size={11} /> 网关地址
            </div>
            <div className="font-mono text-xs text-slate-600 break-all">{gatewayUrl}</div>
          </div>
          <div className="rounded-xl border border-slate-100 bg-slate-50/80 px-4 py-3">
            <div className="mb-1 text-[11px] font-semibold uppercase tracking-wider text-slate-400">
              配置文件
            </div>
            <div className="font-mono text-xs text-slate-600 break-all">{appInfo.config_path}</div>
          </div>
        </div>

        {/* API Key & Model 选择 — 和 API 接口页一致 */}
        <div className="mb-4 grid grid-cols-1 gap-3 md:grid-cols-2">
          <div className="rounded-xl border border-slate-100 bg-slate-50/80 px-4 py-3">
            <div className="mb-2 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wider text-slate-400">
              <KeyRound size={11} /> API Key
            </div>
            <div className="relative">
              <select
                value={selKey}
                onChange={e => {
                  setSelKey(e.target.value);
                  sessionStorage.setItem(keyStorageKey, e.target.value);
                }}
                className="w-full appearance-none rounded-xl border border-slate-200 bg-white px-3 py-2.5 pr-8 text-sm font-mono text-slate-900 shadow-sm cursor-pointer"
              >
                {keys.length === 0 && <option value="">请先创建密钥</option>}
                {keys.map(k => <option key={k.id} value={k.id}>{k.name} ({k.key})</option>)}
              </select>
              <ChevronDown size={14} className="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 text-slate-400" />
            </div>
            {keys.length === 0 && (
              <div className="mt-2 flex items-center gap-1.5 text-xs text-rose-500">
                <AlertCircle size={12} />
                <span>尚未创建密钥，</span>
                <button onClick={() => navigate("/api-keys")} className="font-medium text-rose-600 underline hover:text-rose-700">
                  去配置 →
                </button>
              </div>
            )}
          </div>

          <div className="rounded-xl border border-slate-100 bg-slate-50/80 px-4 py-3">
            <div className="mb-2 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wider text-slate-400">
              <Bot size={11} /> Model
            </div>
            <div className="relative">
              <select
                value={selModel}
                onChange={e => {
                  setSelModel(e.target.value);
                  localStorage.setItem(modelStorageKey, e.target.value);
                }}
                className="w-full appearance-none rounded-xl border border-slate-200 bg-white px-3 py-2.5 pr-8 text-sm font-mono text-slate-900 shadow-sm cursor-pointer"
              >
                {allModels.length === 0 && <option value="">请先配置渠道</option>}
                {modelList.real.length > 0 && (
                  <optgroup label="实际模型">
                    {modelList.real.map(m => <option key={m} value={m}>{m}</option>)}
                  </optgroup>
                )}
                {modelList.mapped.length > 0 && (
                  <optgroup label="映射模型">
                    {modelList.mapped.map(m => <option key={m} value={m}>{m}</option>)}
                  </optgroup>
                )}
              </select>
              <ChevronDown size={14} className="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 text-slate-400" />
            </div>
            {allModels.length === 0 && (
              <div className="mt-2 flex items-center gap-1.5 text-xs text-rose-500">
                <AlertCircle size={12} />
                <span>尚未配置渠道，</span>
                <button onClick={() => navigate("/channels")} className="font-medium text-rose-600 underline hover:text-rose-700">
                  去配置 →
                </button>
              </div>
            )}
          </div>
        </div>

        {/* 操作按钮 */}
        <div className="flex items-center gap-3">
          <button
            onClick={handleApply}
            disabled={applying || !selKey || !selModel}
            className="action-primary"
          >
            {applying ? <Loader2 size={15} className="animate-spin" /> : <ArrowRight size={15} />}
            写入网关配置
          </button>
          {appInfo.applied && (
            <button
              onClick={handleClear}
              disabled={applying}
              className="action-secondary"
              title={appName === "codex" ? "恢复 ~/.codex/config.toml 原始配置，auth.json 中的账号不会被改动" : undefined}
            >
              {appName === "codex" ? "切回原账号" : "恢复原始配置"}
            </button>
          )}
        </div>

        {/* 操作结果 */}
        {appliedResult && (
          <div
            className={`mt-3 flex items-start gap-2.5 rounded-xl border px-4 py-3 text-sm ${
              appliedResult.success
                ? "border-emerald-200 bg-emerald-50 text-emerald-700"
                : "border-red-200 bg-red-50 text-red-700"
            }`}
          >
            {appliedResult.success ? (
              <Check size={16} className="mt-0.5 shrink-0" />
            ) : (
              <AlertCircle size={16} className="mt-0.5 shrink-0" />
            )}
            <div className="flex-1">
              <div className="font-medium">
                {appliedResult.success
                  ? resultKind === "apply"
                    ? "配置已写入"
                    : resultKind === "clear"
                      ? "已恢复原始配置"
                      : "已重置登录方式"
                  : resultKind === "apply"
                    ? "写入失败"
                    : resultKind === "clear"
                      ? "恢复失败"
                      : "重置失败"}
              </div>
              <div className="mt-0.5 text-xs opacity-80">{appliedResult.message}</div>
              {resultKind === "apply" && appliedResult.success && (
                <div className="mt-2 flex items-start gap-1.5 rounded-lg bg-blue-50 px-2.5 py-1.5 text-xs text-blue-600">
                  <Lightbulb size={13} className="mt-0.5 shrink-0" />
                  <span>{appName === "claude-code" ? "配置已写入，尚未代表客户端对话验证成功。请重启 Claude Code 后发送一条短消息；若出现 Not logged in 或发送失败，请按网关日志中的认证/上游错误排查。" : `建议重启 ${appInfo.label} 以避免缓存导致仍使用旧模型。重启后可在「日志」页面查看实际调用的模型。`}</span>
                </div>
              )}
              {appliedResult.success && appliedResult.authWarning && appName === "codex" && (
                <div className="mt-2 flex items-start gap-2 rounded-lg border border-amber-200 bg-amber-50 px-2.5 py-2 text-xs text-amber-700">
                  <AlertCircle size={13} className="mt-0.5 shrink-0" />
                  <div className="flex-1">
                    <div className="font-medium">auth.json 仍处于 API Key 模式，无法回到 ChatGPT 账号</div>
                    <div className="mt-0.5 opacity-80">
                      {appliedResult.authWarning}（通常由其他切换工具写入）。可重置登录方式后重新 codex login，或手动删除 ~/.codex/auth.json。
                    </div>
                    <button
                      onClick={handleResetAuth}
                      disabled={applying}
                      className="mt-1.5 rounded-md border border-amber-300 bg-white px-2 py-1 font-medium text-amber-700 transition-colors hover:bg-amber-100 disabled:opacity-50"
                    >
                      {applying ? <Loader2 size={12} className="inline animate-spin" /> : null}
                      {applying ? " 重置中..." : "重置为 ChatGPT 登录"}
                    </button>
                  </div>
                </div>
              )}
            </div>
          </div>
        )}
      </div>

      {/* 配置文件内容预览 */}
      <div className="surface rounded-2xl p-5">
        <div className="mb-3 flex items-center justify-between gap-3">
          <div className="flex items-center gap-2 min-w-0">
            <FileText size={15} className="text-slate-400 shrink-0" />
            <h3 className="text-sm font-semibold text-slate-700 shrink-0">配置文件</h3>
            <code className="text-[11px] font-mono text-slate-400 truncate">{appInfo.config_path}</code>
          </div>
          <div className="flex items-center gap-2 shrink-0">
            <button
              onClick={handleOpenFolder}
              className="action-secondary flex items-center gap-1.5 px-2.5 py-1.5"
              title="在文件管理器中打开"
            >
              <FolderOpen size={13} />
              打开
            </button>
            <button
              onClick={() => {
                if (configContent?.content) {
                  navigator.clipboard.writeText(configContent.content);
                  setCopied(true);
                  setTimeout(() => setCopied(false), 2000);
                }
              }}
              disabled={!configContent?.content}
              className="action-secondary flex items-center gap-1.5 px-2.5 py-1.5 disabled:opacity-40 disabled:cursor-not-allowed"
              title="复制配置内容"
            >
              {copied ? <Check size={13} /> : <Copy size={13} />}
              {copied ? "已复制" : "复制"}
            </button>
            <button
              onClick={load}
              className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-slate-100 hover:text-slate-600"
              title="刷新"
            >
              <RefreshCw size={13} />
            </button>
          </div>
        </div>
        {configContent?.exists ? (
          <SyntaxHighlighter
            language={appInfo.name === "codex" ? "toml" : appInfo.name === "gemini-cli" ? "bash" : "json"}
            style={oneDark}
            customStyle={{
              maxHeight: "20rem",
              margin: 0,
              borderRadius: "0.75rem",
              fontSize: "0.75rem",
              lineHeight: "1.6rem",
              overflow: "auto",
            }}
            wrapLongLines={false}
          >
            {configContent.content || "(空文件)"}
          </SyntaxHighlighter>
        ) : configContent?.error ? (
          <div className="rounded-xl border border-red-200 bg-red-50 px-4 py-3 text-xs text-red-600">
            {configContent.error}
          </div>
        ) : (
          <div className="rounded-xl border border-dashed border-slate-200 px-4 py-8 text-center text-xs text-slate-400">
            配置文件尚未创建，选择 API Key 和 Model 后点击"写入网关配置"即可自动生成
          </div>
        )}
      </div>
    </div>
  );
}
