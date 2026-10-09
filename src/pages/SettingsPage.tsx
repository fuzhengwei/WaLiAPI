import { useEffect, useState } from "react";
import { settingsApi, serverApi, securityApi, ocrApi, semanticCacheApi, networkApi, type ProxyCandidate, type OcrCacheInfo } from "../lib/api";
import { isWebRuntime } from "../lib/web";
import { SELECT_CLS } from "../lib/constants";
import { PanelSettingsSection } from "../components/PanelSettingsSection";
import type { Settings, BuiltinRule, CustomRule } from "../types";
import { Save, RotateCcw, Check, Server, SlidersHorizontal, Palette, RefreshCw, ShieldAlert, Plus, Trash2, ListChecks, Pencil, X, AlertCircle, HelpCircle, UserRound, ScanText, type LucideIcon } from "lucide-react";

const SEVERITY_BADGE: Record<string, string> = {
  critical: "bg-red-50 text-red-700 border-red-200",
  high: "bg-orange-50 text-orange-700 border-orange-200",
  medium: "bg-amber-50 text-amber-700 border-amber-200",
  low: "bg-blue-50 text-blue-700 border-blue-200",
  info: "bg-slate-50 text-slate-600 border-slate-200",
};

export function SettingsPage() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [saved, setSaved] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [builtinRules, setBuiltinRules] = useState<BuiltinRule[]>([]);
  const [customRules, setCustomRules] = useState<CustomRule[]>([]);
  const [loadError, setLoadError] = useState(false);
  const [showAddRule, setShowAddRule] = useState(false);
  const [newRule, setNewRule] = useState({ rule_type: "blacklist", category: "domain", pattern: "", severity: "medium", action: "warn", description: "" });
  const [editingBuiltin, setEditingBuiltin] = useState<string | null>(null);
  const [editBuiltinData, setEditBuiltinData] = useState({ severity: "", title: "", description: "" });
  const [ocrCacheInfo, setOcrCacheInfo] = useState<OcrCacheInfo | null>(null);
  // 出站代理（VPN 固定端口）自动探测
  const [proxyDetecting, setProxyDetecting] = useState(false);
  const [proxyCandidates, setProxyCandidates] = useState<ProxyCandidate[] | null>(null);
  const [activeTab, setActiveTab] = useState<string>(() => {
    const hash = window.location.hash.replace("#", "");
    return hash || "security";
  });

  // 监听 URL hash 变化（如从日志页「前往设置」跳转），同步切换 Tab
  useEffect(() => {
    const onHashChange = () => {
      const hash = window.location.hash.replace("#", "");
      if (hash) setActiveTab(hash);
    };
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  useEffect(() => {
    settingsApi.get().then(setSettings).catch(() => {});
    securityApi.getBuiltinRules().then(setBuiltinRules).catch(() => {});
    securityApi.getCustomRules().then(setCustomRules).catch(() => {});
    Promise.all([settingsApi.get(), securityApi.getBuiltinRules(), securityApi.getCustomRules()])
      .then(() => setLoadError(false))
      .catch(() => setLoadError(true));
  }, []);

  // 切到 OCR 分组时加载缓存占用信息
  useEffect(() => {
    if (activeTab !== "ocr") return;
    ocrApi.getCacheInfo().then(setOcrCacheInfo).catch(() => setOcrCacheInfo(null));
  }, [activeTab]);

  const handleDetectProxies = async () => {
    setProxyDetecting(true);
    try {
      const list = await networkApi.detectLocalProxies();
      setProxyCandidates(list);
      setMessage(list.length > 0 ? `探测到 ${list.length} 个可用代理端口。` : "未探测到可用的本地代理端口。");
    } catch (e) {
      setMessage(`探测失败: ${e}`);
      setProxyCandidates([]);
    } finally {
      setProxyDetecting(false);
      setTimeout(() => setMessage(null), 3000);
    }
  };

  const handleClearOcrCache = async () => {    try {
      await ocrApi.clearCache();
      const info = await ocrApi.getCacheInfo().catch(() => null);
      setOcrCacheInfo(info);
      setMessage("OCR 缓存已清空。");
      setTimeout(() => setMessage(null), 2000);
    } catch (e) {
      setMessage(`清空 OCR 缓存失败: ${e}`);
      setTimeout(() => setMessage(null), 3000);
    }
  };

  const handleAddRule = async () => {
    if (!newRule.pattern.trim()) return;
    try {
      await securityApi.createCustomRule({
        rule_type: newRule.rule_type,
        category: newRule.category,
        pattern: newRule.pattern,
        severity: newRule.severity,
        action: newRule.action,
        description: newRule.description || undefined,
      });
      const refreshed = await securityApi.getCustomRules();
      setCustomRules(refreshed);
      setNewRule({ rule_type: "blacklist", category: "domain", pattern: "", severity: "medium", action: "warn", description: "" });
      setShowAddRule(false);
      setMessage("自定义规则已添加。");
      setTimeout(() => setMessage(null), 2000);
    } catch (e) {
      setMessage(`添加失败: ${e}`);
      setTimeout(() => setMessage(null), 3000);
    }
  };

  const handleToggleCustomRule = async (id: string, enabled: boolean) => {
    try {
      await securityApi.toggleCustomRule(id, enabled);
      setCustomRules(prev => prev.map(r => r.id === id ? { ...r, enabled } : r));
    } catch (e) {
      setMessage(`操作失败: ${e}`);
    }
  };

  const handleDeleteCustomRule = async (id: string) => {
    try {
      await securityApi.deleteCustomRule(id);
      setCustomRules(prev => prev.filter(r => r.id !== id));
    } catch (e) {
      setMessage(`删除失败: ${e}`);
    }
  };

  const handleToggleBuiltin = async (rule: BuiltinRule) => {
    try {
      await securityApi.updateBuiltinRule(rule.id, { enabled: !rule.enabled });
      setBuiltinRules(prev => prev.map(r => r.id === rule.id ? { ...r, enabled: !r.enabled } : r));
    } catch (e) {
      setMessage(`操作失败: ${e}`);
    }
  };

  const handleStartEditBuiltin = (rule: BuiltinRule) => {
    setEditingBuiltin(rule.id);
    setEditBuiltinData({ severity: rule.severity, title: rule.title, description: rule.description || "" });
  };

  const handleSaveEditBuiltin = async (id: string) => {
    try {
      await securityApi.updateBuiltinRule(id, {
        severity: editBuiltinData.severity,
        title: editBuiltinData.title,
        description: editBuiltinData.description,
      });
      setBuiltinRules(prev => prev.map(r => r.id === id ? {
        ...r,
        severity: editBuiltinData.severity,
        title: editBuiltinData.title,
        description: editBuiltinData.description,
      } : r));
      setEditingBuiltin(null);
      setMessage("内置规则已更新。");
      setTimeout(() => setMessage(null), 2000);
    } catch (e) {
      setMessage(`更新失败: ${e}`);
    }
  };

  const handleDeleteBuiltin = async (id: string) => {
    try {
      await securityApi.deleteBuiltinRule(id);
      setBuiltinRules(prev => prev.filter(r => r.id !== id));
      setMessage("内置规则已删除。");
      setTimeout(() => setMessage(null), 2000);
    } catch (e) {
      setMessage(`删除失败: ${e}`);
    }
  };

  const handleResetBuiltin = async () => {
    try {
      const reset = await securityApi.resetBuiltinRules();
      setBuiltinRules(reset);
      setMessage("内置规则已恢复默认。");
      setTimeout(() => setMessage(null), 2000);
    } catch (e) {
      setMessage(`重置失败: ${e}`);
    }
  };

  if (!settings) {
    if (loadError) {
      return (
        <div className="page-shell flex flex-col items-center justify-center gap-3 text-sm text-slate-500">
          <AlertCircle className="h-10 w-10 text-red-400/70" />
          <p>设置加载失败，请检查服务是否已启动。</p>
          <button onClick={() => window.location.reload()} className="rounded-lg bg-blue-600 px-4 py-2 text-xs font-medium text-white hover:bg-blue-700">重新加载</button>
        </div>
      );
    }
    return <div className="page-shell text-sm text-muted-foreground">加载中...</div>;
  }

  const handleSave = async () => {
    await settingsApi.save(settings);
    await settingsApi.applyTheme(settings.ui_theme);
    // Web 版无系统自启能力，跳过（开关在 Web 下已隐藏，保持默认值）
    if (!isWebRuntime()) {
      await settingsApi.setAutoStart(settings.auto_start);
    }
    document.documentElement.setAttribute("data-theme", settings.ui_theme || "dark");
    document.documentElement.lang = settings.ui_language || "zh-CN";
    setSaved(true);
    setMessage("设置已保存，主题与桌面行为已应用。");
    setTimeout(() => setSaved(false), 2000);
  };

  const handleRestart = async () => {
    await serverApi.restart();
    setMessage("服务已触发重启，请稍候查看状态。");
  };

  // 统一 select 样式（共享常量，见 lib/constants.ts）
  const selectCls = SELECT_CLS;
  const inputCls = "w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary";
  const helpTooltipCls = "pointer-events-none absolute left-1/2 top-full z-50 mt-2 w-64 max-w-[calc(100vw-3rem)] -translate-x-1/2 rounded-xl border border-border bg-background px-3 py-2 text-xs leading-relaxed text-foreground opacity-0 shadow-lg transition-opacity group-hover/help:opacity-100";

  // Tab 配置（面板设置为 Web 版专属，桌面端不展示）
  const TABS: { id: string; label: string; icon: LucideIcon }[] = [
    { id: "security", label: "安全审计", icon: ShieldAlert },
    { id: "server", label: "服务配置", icon: Server },
    { id: "general", label: "通用设置", icon: SlidersHorizontal },
    { id: "appearance", label: "界面设置", icon: Palette },
    { id: "retry", label: "重试策略", icon: RefreshCw },
    { id: "ocr", label: "OCR", icon: ScanText },
    ...(isWebRuntime() ? [{ id: "panel", label: "面板设置", icon: UserRound }] : []),
  ];

  const formatBytes = (bytes: number) => {
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  };

  return (
    <div className="page-shell space-y-5">
      <div className="sticky top-0 z-30 -mx-7 -mt-7 mb-2 space-y-5 bg-[#f5f7fa]/95 px-7 pt-7 pb-2 backdrop-blur-md max-lg:-mx-5 max-lg:px-5 max-lg:pt-5">
        <div className="page-header">
          <div>
            <h1 className="page-title">设置</h1>
            <p className="page-subtitle">分类管理服务、安全、界面与重试策略</p>
          </div>
          <button onClick={handleSave} className="action-primary">
            {saved ? <Check size={16} /> : <Save size={16} />}
            {saved ? "已保存" : "保存设置"}
          </button>
        </div>

        {message && <div className="surface-soft rounded-2xl px-4 py-3 text-sm text-primary">{message}</div>}

        {/* Tab 标签页 */}
        <div className="flex items-center gap-1 border-b border-border overflow-x-auto">
          {TABS.map(tab => {
          const Icon = tab.icon;
          const active = activeTab === tab.id;
          return (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`inline-flex items-center gap-1.5 px-4 py-2.5 text-sm font-medium border-b-2 -mb-px transition-colors whitespace-nowrap ${
                active
                  ? "border-primary text-primary"
                  : "border-transparent text-muted-foreground hover:text-foreground hover:border-border"
              }`}
            >
              <Icon size={15} />
              {tab.label}
            </button>
          );
          })}
        </div>
      </div>

      {/* Tab 内容 */}
      {activeTab === "security" && (
        <div className="surface rounded-[24px] p-6 space-y-5">
          <div className="flex items-center gap-3">
            <div className="rounded-2xl border border-white/8 bg-white/6 p-3"><ShieldAlert size={18} className="text-primary" /></div>
            <div>
              <h2 className="text-lg font-semibold">安全审计中心</h2>
              <p className="text-sm text-muted-foreground">检测请求中的凭证泄露、敏感路径、工具外联、Unicode 隐写与追踪风险</p>
            </div>
          </div>

          {/* 启用 + 模式 + 子开关：横向铺开 */}
          <div className="grid grid-cols-2 gap-4 lg:grid-cols-4 xl:grid-cols-5">
            <label className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
              <span className="text-sm">启用安全审计</span>
              <input
                type="checkbox"
                checked={settings.security_enabled}
                onChange={e => setSettings({ ...settings, security_enabled: e.target.checked })}
                className="h-5 w-5"
              />
            </label>
            <div className="col-span-1 lg:col-span-3 xl:col-span-4">
              <label className="mb-2 block text-sm font-medium">安全模式</label>
              <select
                value={settings.security_mode}
                onChange={e => setSettings({ ...settings, security_mode: e.target.value })}
                className={selectCls}
              >
                <option value="audit">只审计 — 记录风险，不影响请求</option>
                <option value="warn">警告 — 中高风险标记告警</option>
                <option value="redact">脱敏 — 高风险自动脱敏后转发</option>
                <option value="block">阻断 — 高风险直接阻断</option>
              </select>
            </div>
          </div>

          {/* 子开关：横向铺开 */}
          <div className="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-6">
            {([
              ["Unicode 隐写检测", "security_scan_unicode"],
              ["工具/命令风险检测", "security_scan_tools"],
              ["外联/追踪风险检测", "security_scan_network"],
              ["响应侧安全扫描", "security_scan_response"],
              ["请求脱敏转发", "security_redact_secrets"],
              ["严重风险强制阻断", "security_block_on_critical"],
            ] as const).map(([label, key]) => (
              <label key={key} className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
                <span className="text-sm">{label}</span>
                <input
                  type="checkbox"
                  checked={Boolean(settings[key as keyof Settings])}
                  onChange={e => setSettings({ ...settings, [key]: e.target.checked })}
                  className="h-5 w-5"
                />
              </label>
            ))}
          </div>
          <p className="text-xs text-muted-foreground">
            「请求脱敏转发」开启后，请求体中的 API Key、Token、私钥等敏感信息会在转发上游前被替换为脱敏值。
            「响应侧安全扫描」开启后，上游返回内容（非流式、流式与原生 Anthropic 路径均覆盖）也会被扫描并记录风险；扫描为尽力而为，不影响响应转发。
          </p>

          {/* 内置规则 + 自定义规则：左右分栏 */}
          <div className="grid grid-cols-1 gap-5 xl:grid-cols-2">
          {/* ── 内置规则 ── */}
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2 text-sm font-medium">
                <ListChecks size={15} />
                内置规则 ({builtinRules.length} 条)
              </div>
              <button onClick={handleResetBuiltin} className="action-secondary" style={{ padding: "4px 12px", fontSize: "12px" }}>
                <RotateCcw size={12} /> 恢复默认
              </button>
            </div>
            <div className="space-y-1.5">
              {builtinRules.map(rule => (
                <div key={rule.id} className={`rounded-xl border px-3 py-2.5 text-xs transition-colors ${rule.enabled ? "border-white/8 bg-white/4" : "border-white/4 bg-white/2 opacity-60"}`}>
                  {editingBuiltin === rule.id ? (
                    /* 编辑模式 */
                    <div className="space-y-2">
                      <div className="flex items-center gap-2">
                        <select
                          value={editBuiltinData.severity}
                          onChange={e => setEditBuiltinData({ ...editBuiltinData, severity: e.target.value })}
                          className="rounded-lg border border-border bg-background/70 px-2 py-1 text-xs"
                        >
                          <option value="info">提示</option>
                          <option value="low">低</option>
                          <option value="medium">中</option>
                          <option value="high">高</option>
                          <option value="critical">严重</option>
                        </select>
                        <input
                          value={editBuiltinData.title}
                          onChange={e => setEditBuiltinData({ ...editBuiltinData, title: e.target.value })}
                          className="flex-1 rounded-lg border border-border bg-background/70 px-2 py-1 text-xs font-medium"
                        />
                      </div>
                      <input
                        value={editBuiltinData.description}
                        onChange={e => setEditBuiltinData({ ...editBuiltinData, description: e.target.value })}
                        className="w-full rounded-lg border border-border bg-background/70 px-2 py-1 text-xs"
                        placeholder="规则描述"
                      />
                      <div className="flex items-center gap-2">
                        <button onClick={() => handleSaveEditBuiltin(rule.id)} className="action-primary" style={{ padding: "3px 10px", fontSize: "11px" }}>
                          <Check size={11} /> 保存
                        </button>
                        <button onClick={() => setEditingBuiltin(null)} className="action-secondary" style={{ padding: "3px 10px", fontSize: "11px" }}>
                          <X size={11} /> 取消
                        </button>
                        <span className="ml-auto font-mono text-[10px] text-slate-400">{rule.rule_id}</span>
                      </div>
                    </div>
                  ) : (
                    /* 展示模式 */
                    <div className="flex items-center gap-2">
                      <input
                        type="checkbox"
                        checked={rule.enabled}
                        onChange={() => handleToggleBuiltin(rule)}
                        className="h-4 w-4 shrink-0"
                      />
                      <span className={`shrink-0 rounded-full border px-1.5 py-0.5 text-[10px] font-medium ${SEVERITY_BADGE[rule.severity] || SEVERITY_BADGE.info}`}>
                        {rule.severity}
                      </span>
                      <span className="shrink-0 text-slate-500 min-w-[56px]">{rule.category}</span>
                      <div className="min-w-0 flex-1">
                        <span className="font-medium text-slate-800">{rule.title}</span>
                        {rule.description && <span className="ml-2 text-slate-500">{rule.description}</span>}
                      </div>
                      <button onClick={() => handleStartEditBuiltin(rule)} className="shrink-0 text-slate-400 hover:text-blue-500 transition-colors" title="编辑">
                        <Pencil size={12} />
                      </button>
                      <button onClick={() => handleDeleteBuiltin(rule.id)} className="shrink-0 text-slate-400 hover:text-red-400 transition-colors" title="删除">
                        <Trash2 size={12} />
                      </button>
                    </div>
                  )}
                </div>
              ))}
            </div>
          </div>

          {/* ── 自定义规则 ── */}
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2 text-sm font-medium">
                <Plus size={15} />
                自定义规则 ({customRules.length} 条)
              </div>
              <button onClick={() => setShowAddRule(!showAddRule)} className="action-secondary" style={{ padding: "4px 12px", fontSize: "12px" }}>
                <Plus size={14} /> 添加规则
              </button>
            </div>

            {showAddRule && (
              <div className="surface-soft rounded-2xl p-4 space-y-3">
                <div className="grid grid-cols-2 gap-3 md:grid-cols-3">
                  <div>
                    <label className="mb-1 block text-[11px] text-muted-foreground">类型</label>
                    <select value={newRule.rule_type} onChange={e => setNewRule({ ...newRule, rule_type: e.target.value })} className={selectCls} style={{ padding: "8px 12px", fontSize: "12px" }}>
                      <option value="blacklist">黑名单</option>
                      <option value="whitelist">白名单</option>
                    </select>
                  </div>
                  <div>
                    <label className="mb-1 block text-[11px] text-muted-foreground">分类</label>
                    <select value={newRule.category} onChange={e => setNewRule({ ...newRule, category: e.target.value })} className={selectCls} style={{ padding: "8px 12px", fontSize: "12px" }}>
                      <option value="domain">域名</option>
                      <option value="tool">工具名</option>
                      <option value="path">文件路径</option>
                      <option value="keyword">关键词</option>
                    </select>
                  </div>
                  <div>
                    <label className="mb-1 block text-[11px] text-muted-foreground">严重程度</label>
                    <select value={newRule.severity} onChange={e => setNewRule({ ...newRule, severity: e.target.value })} className={selectCls} style={{ padding: "8px 12px", fontSize: "12px" }}>
                      <option value="info">提示</option>
                      <option value="low">低</option>
                      <option value="medium">中</option>
                      <option value="high">高</option>
                      <option value="critical">严重</option>
                    </select>
                  </div>
                </div>
                <div>
                  <label className="mb-1 block text-[11px] text-muted-foreground">匹配模式</label>
                  <input
                    type="text"
                    placeholder="如 example.com / curl / ~/.ssh"
                    value={newRule.pattern}
                    onChange={e => setNewRule({ ...newRule, pattern: e.target.value })}
                    className={inputCls}
                    style={{ padding: "8px 12px", fontSize: "12px", fontFamily: "monospace" }}
                  />
                </div>
                <div>
                  <label className="mb-1 block text-[11px] text-muted-foreground">描述（可选）</label>
                  <input
                    type="text"
                    placeholder="规则说明"
                    value={newRule.description}
                    onChange={e => setNewRule({ ...newRule, description: e.target.value })}
                    className={inputCls}
                    style={{ padding: "8px 12px", fontSize: "12px" }}
                  />
                </div>
                <div className="flex gap-2">
                  <button onClick={handleAddRule} className="action-primary" style={{ padding: "6px 16px", fontSize: "12px" }}>
                    <Check size={13} /> 确认添加
                  </button>
                  <button onClick={() => setShowAddRule(false)} className="action-secondary" style={{ padding: "6px 16px", fontSize: "12px" }}>
                    取消
                  </button>
                </div>
              </div>
            )}

            {customRules.length > 0 ? (
              <div className="space-y-1.5">
                {customRules.map(rule => (
                  <div key={rule.id} className={`flex items-center gap-2 rounded-xl border px-3 py-2.5 text-xs transition-colors ${rule.enabled ? "border-white/8 bg-white/4" : "border-white/4 bg-white/2 opacity-60"}`}>
                    <input
                      type="checkbox"
                      checked={rule.enabled}
                      onChange={e => handleToggleCustomRule(rule.id, e.target.checked)}
                      className="h-4 w-4 shrink-0"
                    />
                    <span className={`shrink-0 rounded-full px-2 py-0.5 text-[10px] font-medium ${rule.rule_type === "blacklist" ? "bg-red-50 text-red-700" : "bg-emerald-50 text-emerald-700"}`}>
                      {rule.rule_type === "blacklist" ? "黑名单" : "白名单"}
                    </span>
                    <span className="shrink-0 text-slate-500 min-w-[48px]">{rule.category}</span>
                    <span className="font-mono text-slate-800 flex-1 truncate">{rule.pattern}</span>
                    {rule.description && <span className="text-slate-400 truncate hidden md:inline">{rule.description}</span>}
                    <span className={`shrink-0 rounded-full px-2 py-0.5 text-[10px] ${SEVERITY_BADGE[rule.severity] || SEVERITY_BADGE.info}`}>
                      {rule.severity}
                    </span>
                    <button onClick={() => handleDeleteCustomRule(rule.id)} className="shrink-0 text-slate-400 hover:text-red-400 transition-colors">
                      <Trash2 size={13} />
                    </button>
                  </div>
                ))}
              </div>
            ) : (
              <p className="text-xs text-muted-foreground">暂无自定义规则。可添加域名黑名单、工具白名单等。</p>
            )}
          </div>
          </div>{/* end rules grid */}

          <p className="text-xs text-muted-foreground">默认建议使用「只审计」模式：先在审计日志中展示风险证据；需要强防护时再切换到「脱敏」或「阻断」。</p>
        </div>
      )}

      {activeTab === "server" && (
        <div className="surface rounded-[24px] p-6 space-y-5">
          <div className="flex items-center gap-3">
            <div className="rounded-2xl border border-white/8 bg-white/6 p-3"><Server size={18} className="text-primary" /></div>
            <div>
              <h2 className="text-lg font-semibold">服务配置</h2>
              <p className="text-sm text-muted-foreground">控制本地网关监听地址与服务重启</p>
            </div>
          </div>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
            <div>
              <label className="mb-2 block text-sm font-medium">监听地址</label>
              <input
                value={settings.server_host}
                onChange={e => setSettings({ ...settings, server_host: e.target.value })}
                className={`${inputCls} font-mono`}
              />
            </div>
            <div>
              <label className="mb-2 block text-sm font-medium">端口 (0=随机)</label>
              <input
                type="number"
                value={settings.server_port}
                onChange={e => setSettings({ ...settings, server_port: parseInt(e.target.value) || 0 })}
                className={inputCls}
              />
            </div>
            <div className="flex items-end">
              <button onClick={handleRestart} className="action-secondary w-full">
                <RotateCcw size={16} /> 重启服务
              </button>
            </div>
          </div>

          <div className="border-t border-white/8 pt-5">
            <h3 className="mb-1 text-sm font-medium">出站代理（VPN 固定转发端口）</h3>
            <p className="mb-4 text-xs text-muted-foreground">
              开启后，所有「跟随全局」的上游请求经该代理转发；保存即刻生效，无需重启。渠道可单独改为直连或自定义代理。
            </p>
            <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
              <label className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
                <span className="text-sm">启用出站代理</span>
                <input
                  type="checkbox"
                  checked={settings.proxy_enabled}
                  onChange={e => setSettings({ ...settings, proxy_enabled: e.target.checked })}
                  className="h-5 w-5"
                />
              </label>
              <div>
                <label className="mb-2 block text-sm font-medium">代理地址（如 http://127.0.0.1:7890）</label>
                <input
                  value={settings.proxy_url}
                  onChange={e => setSettings({ ...settings, proxy_url: e.target.value })}
                  placeholder="http://127.0.0.1:7890"
                  disabled={!settings.proxy_enabled}
                  className={`${inputCls} font-mono disabled:opacity-50`}
                />
              </div>
              <div className="flex items-end">
                <button
                  onClick={handleDetectProxies}
                  disabled={proxyDetecting || !settings.proxy_enabled}
                  className="action-secondary w-full disabled:opacity-50"
                >
                  <RefreshCw size={16} className={proxyDetecting ? "animate-spin" : ""} />
                  {proxyDetecting ? "探测中…" : "自动探测端口"}
                </button>
              </div>
            </div>
            {settings.proxy_enabled && proxyCandidates !== null && (
              <div className="mt-3 space-y-2">
                {proxyCandidates.length === 0 ? (
                  <p className="text-xs text-muted-foreground">未探测到可用端口，请确认 VPN/代理客户端已开启「允许局域网连接」。</p>
                ) : (
                  proxyCandidates.map(c => (
                    <button
                      key={c.url}
                      onClick={() => setSettings({ ...settings, proxy_url: c.url })}
                      className="surface-soft flex w-full items-center justify-between rounded-xl px-4 py-2.5 text-left text-sm hover:border-primary/40"
                    >
                      <span className="font-mono text-xs">{c.url}</span>
                      <span className="flex items-center gap-3 text-xs text-muted-foreground">
                        <span>{c.label}</span>
                        <span className={c.latency_ms !== null && c.latency_ms < 500 ? "text-green-600" : ""}>
                          {c.latency_ms !== null ? `${c.latency_ms} ms` : "超时"}
                        </span>
                        {settings.proxy_url === c.url && <Check size={14} className="text-primary" />}
                      </span>
                    </button>
                  ))
                )}
              </div>
            )}
          </div>
        </div>
      )}

      {activeTab === "general" && (
        <div className="surface rounded-[24px] p-6 space-y-5">
          <div className="flex items-center gap-3">
            <div className="rounded-2xl border border-white/8 bg-white/6 p-3"><SlidersHorizontal size={18} className="text-primary" /></div>
            <div>
              <h2 className="text-lg font-semibold">通用设置</h2>
              <p className="text-sm text-muted-foreground">桌面端交互习惯与启动行为</p>
            </div>
          </div>
          <div>
            <h3 className="mb-3 text-sm font-medium text-muted-foreground">桌面行为</h3>
          <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
            {([
              ["最小化到托盘", "minimize_to_tray"],
              ["关闭到托盘", "close_to_tray"],
              ["开机自启", "auto_start"],
            ] as const).filter(() => !isWebRuntime()).map(([label, key]) => (
              <label key={key} className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
                <span className="text-sm">{label}</span>
                <input
                  type="checkbox"
                  checked={Boolean(settings[key as keyof Settings])}
                  onChange={e => setSettings({ ...settings, [key]: e.target.checked })}
                  className="h-5 w-5"
                />
              </label>
            ))}
          </div>
          </div>
          <div>
            <h3 className="mb-3 text-sm font-medium text-muted-foreground">审计日志</h3>
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <div>
                <label className="mb-2 block text-sm font-medium">日志级别</label>
                <select
                  value={settings.log_detail_level}
                  onChange={e => setSettings({ ...settings, log_detail_level: e.target.value })}
                  className={selectCls}
                >
                  <option value="basic">基本</option>
                  <option value="brief">简要</option>
                  <option value="detailed">详情</option>
                </select>
                <p className="mt-1 text-xs text-muted-foreground">基本模式只保存请求状态与用量摘要；简要模式保存完整响应正文，但请求的消息列表只保留最新 3 条（Agent 长对话下可省下绝大部分空间）；详情模式保存完整请求与响应正文，可能显著增加数据库大小。三种模式的调用次数、成功率与 Token 用量统计完全一致。</p>
              </div>
              <div>
                <label className="mb-2 block text-sm font-medium">日志保留期</label>
                <select
                  value={settings.log_retention_days}
                  onChange={e => setSettings({ ...settings, log_retention_days: Number(e.target.value) })}
                  className={selectCls}
                >
                  <option value={1}>1 天</option>
                  <option value={7}>7 天</option>
                  <option value={30}>30 天</option>
                  <option value={90}>90 天</option>
                  <option value={0}>永久保留</option>
                </select>
                <p className="mt-1 text-xs text-muted-foreground">过期审计日志会由服务自动清理；清理日志不影响已统计的调用量与 Token 用量（统计数据独立持久化）。如需重置统计数据，请在日志页清理时勾选「同时清除统计数据」（会二次确认并要求先备份）。删除后数据库文件需单独压缩才会缩小。</p>
              </div>
            </div>
          </div>
          <div>
            <h3 className="mb-3 text-sm font-medium text-muted-foreground">OTLP 导出</h3>
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <label className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
                <div>
                  <div className="text-sm font-medium">启用 OTLP 导出</div>
                  <p className="text-xs text-muted-foreground">把请求日志增量导出为 OTLP span（Langfuse 等 OTLP 兼容端可直接接入）；默认关闭，关闭时零后台流量。</p>
                </div>
                <input
                  type="checkbox"
                  checked={settings.otlp_enabled}
                  onChange={e => setSettings({ ...settings, otlp_enabled: e.target.checked })}
                  className="h-5 w-5"
                />
              </label>
              <div>
                <label className="mb-2 block text-sm font-medium">OTLP/HTTP 端点</label>
                <input
                  type="text"
                  value={settings.otlp_endpoint}
                  onChange={e => setSettings({ ...settings, otlp_endpoint: e.target.value })}
                  placeholder="https://langfuse.example.com/api/public/otel/v1/traces"
                  className={inputCls}
                />
                <p className="mt-1 text-xs text-muted-foreground">OTLP/HTTP JSON 端点地址；导出失败自动退避重试，不影响请求主链路。</p>
              </div>
              <div>
                <label className="mb-2 block text-sm font-medium">附加请求头（JSON）</label>
                <input
                  type="text"
                  value={settings.otlp_headers}
                  onChange={e => setSettings({ ...settings, otlp_headers: e.target.value })}
                  placeholder='{"Authorization": "Basic …"}'
                  className={inputCls}
                />
                <p className="mt-1 text-xs text-muted-foreground">JSON 对象字符串，鉴权头仅保存在本地设置存储中。</p>
              </div>
              <div className="grid grid-cols-2 gap-4">
                <div>
                  <label className="mb-2 block text-sm font-medium">导出间隔（秒）</label>
                  <input
                    type="number"
                    min={5}
                    value={settings.otlp_interval_secs}
                    onChange={e => setSettings({ ...settings, otlp_interval_secs: Number(e.target.value) })}
                    className={inputCls}
                  />
                </div>
                <div>
                  <label className="mb-2 block text-sm font-medium">每批条数</label>
                  <input
                    type="number"
                    min={1}
                    value={settings.otlp_batch_size}
                    onChange={e => setSettings({ ...settings, otlp_batch_size: Number(e.target.value) })}
                    className={inputCls}
                  />
                </div>
              </div>
            </div>
          </div>
          <div>
            <h3 className="mb-3 text-sm font-medium text-muted-foreground">渠道健康探测</h3>
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <label className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
                <div>
                  <div className="text-sm font-medium">启用主动探测</div>
                  <p className="text-xs text-muted-foreground">后台定期对启用渠道发 GET /models 廉价探测；失败渠道候选排序沉底（不剔除）；关闭时零后台流量。Auth 账号渠道永不探测。</p>
                </div>
                <input
                  type="checkbox"
                  checked={settings.probe_enabled}
                  onChange={e => setSettings({ ...settings, probe_enabled: e.target.checked })}
                  className="h-5 w-5"
                />
              </label>
              <div>
                <label className="mb-2 block text-sm font-medium">探测间隔（秒）</label>
                <input
                  type="number"
                  min={30}
                  value={settings.probe_interval_secs}
                  onChange={e => setSettings({ ...settings, probe_interval_secs: Number(e.target.value) })}
                  className={inputCls}
                />
                <p className="mt-1 text-xs text-muted-foreground">默认 300 秒；最短 30 秒。探测请求记入请求日志（is_probe 标记），不计入用量统计。</p>
              </div>
            </div>
          </div>
          <div>
            <h3 className="mb-3 text-sm font-medium text-muted-foreground">语义缓存</h3>
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <label className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
                <div>
                  <div className="text-sm font-medium">启用语义缓存</div>
                  <p className="text-xs text-muted-foreground">相同/同义请求直接返回缓存答案（响应头 X-Cache: hit）。带工具调用、高温采样、安全审计命中的请求永不入缓存。默认关闭。</p>
                </div>
                <input
                  type="checkbox"
                  checked={settings.cache_enabled}
                  onChange={e => setSettings({ ...settings, cache_enabled: e.target.checked })}
                  className="h-5 w-5"
                />
              </label>
              <div>
                <label className="mb-2 block text-sm font-medium">嵌入模型（语义层）</label>
                <input
                  type="text"
                  value={settings.cache_embedding_model}
                  onChange={e => setSettings({ ...settings, cache_embedding_model: e.target.value })}
                  placeholder="留空 = 仅精确匹配层"
                  className={inputCls}
                />
                <p className="mt-1 text-xs text-muted-foreground">填写渠道中的 Embedding 模型名以启用语义相似命中（阈值默认 0.95，保守）。</p>
              </div>
              <div>
                <label className="mb-2 block text-sm font-medium">缓存 TTL（秒）</label>
                <input
                  type="number"
                  min={60}
                  value={settings.cache_ttl_secs}
                  onChange={e => setSettings({ ...settings, cache_ttl_secs: Number(e.target.value) })}
                  className={inputCls}
                />
                <p className="mt-1 text-xs text-muted-foreground">默认 86400（24 小时），过期自动不命中并由后台清理。</p>
              </div>
              <div>
                <label className="mb-2 block text-sm font-medium">手动清空缓存</label>
                <button
                  onClick={async () => {
                    try {
                      const n = await semanticCacheApi.clear();
                      window.alert(`已清空 ${n} 条缓存`);
                    } catch (e) {
                      window.alert(`清空失败：${e}`);
                    }
                  }}
                  className="rounded-full border border-border px-4 py-2 text-sm font-medium hover:bg-white/5"
                >
                  清空全部缓存条目
                </button>
              </div>
            </div>
          </div>
          <div>
            <h3 className="mb-3 text-sm font-medium text-muted-foreground">路由设置</h3>
            <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
              <label className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
                <span className="flex items-center gap-1.5 text-sm">
                  Auth 账号优先
                  <span className="group/help relative inline-flex">
                    <HelpCircle size={14} className="shrink-0 text-muted-foreground" aria-label="Auth 账号优先说明" />
                    <span className={helpTooltipCls}>
                      开启后，支持该模型的 Auth 账号会优先于普通渠道；若同时开启同协议优先，则同协议 Auth 账号最优先。
                    </span>
                  </span>
                </span>
                <input
                  type="checkbox"
                  checked={settings.routing_prefer_auth_accounts}
                  onChange={e => setSettings({ ...settings, routing_prefer_auth_accounts: e.target.checked })}
                  className="h-5 w-5"
                />
              </label>
              <label className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
                <span className="flex items-center gap-1.5 text-sm">
                  同协议优先
                  <span className="group/help relative inline-flex">
                    <HelpCircle size={14} className="shrink-0 text-muted-foreground" aria-label="同协议优先说明" />
                    <span className={helpTooltipCls}>
                      开启后，优先选择无需协议转换的候选；没有同协议候选时再使用跨协议转换候选。
                    </span>
                  </span>
                </span>
                <input
                  type="checkbox"
                  checked={settings.routing_prefer_same_protocol}
                  onChange={e => setSettings({ ...settings, routing_prefer_same_protocol: e.target.checked })}
                  className="h-5 w-5"
                />
              </label>
            </div>
          </div>
        </div>
      )}

      {activeTab === "appearance" && (
        <div className="surface rounded-[24px] p-6 space-y-5">
          <div className="flex items-center gap-3">
            <div className="rounded-2xl border border-white/8 bg-white/6 p-3"><Palette size={18} className="text-primary" /></div>
            <div>
              <h2 className="text-lg font-semibold">界面设置</h2>
              <p className="text-sm text-muted-foreground">外观与语言偏好</p>
            </div>
          </div>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <div>
              <label className="mb-2 block text-sm font-medium">主题</label>
              <select
                value={settings.ui_theme}
                onChange={e => setSettings({ ...settings, ui_theme: e.target.value })}
                className={selectCls}
              >
                <option value="dark">深色</option>
                <option value="light">浅色</option>
                <option value="system">跟随系统</option>
              </select>
            </div>
            <div>
              <label className="mb-2 block text-sm font-medium">语言</label>
              <select
                value={settings.ui_language}
                onChange={e => setSettings({ ...settings, ui_language: e.target.value })}
                className={selectCls}
              >
                <option value="zh-CN">简体中文</option>
                <option value="en">English</option>
              </select>
            </div>
          </div>
        </div>
      )}

      {activeTab === "retry" && (
        <div className="surface rounded-[24px] p-6 space-y-5">
          <div className="flex items-center gap-3">
            <div className="rounded-2xl border border-white/8 bg-white/6 p-3"><RefreshCw size={18} className="text-primary" /></div>
            <div>
              <h2 className="text-lg font-semibold">重试策略</h2>
              <p className="text-sm text-muted-foreground">请求失败后的自动恢复行为</p>
            </div>
          </div>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2 md:items-end">
            <label className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
              <span className="text-sm">启用自动重试</span>
              <input
                type="checkbox"
                checked={settings.retry_enabled}
                onChange={e => setSettings({ ...settings, retry_enabled: e.target.checked })}
                className="h-5 w-5"
              />
            </label>
            {settings.retry_enabled && (
              <div>
                <label className="mb-2 block text-sm font-medium">重试次数</label>
                <input
                  type="number"
                  min={0}
                  value={settings.retry_times}
                  onChange={e => setSettings({ ...settings, retry_times: Math.max(0, parseInt(e.target.value) || 0) })}
                  className={inputCls}
                />
              </div>
            )}
          </div>
        </div>
      )}

      {activeTab === "ocr" && (
        <div className="surface rounded-[24px] p-6 space-y-5">
          <div className="flex items-center gap-3">
            <div className="rounded-2xl border border-white/8 bg-white/6 p-3"><ScanText size={18} className="text-primary" /></div>
            <div>
              <h2 className="text-lg font-semibold">LLM OCR</h2>
              <p className="text-sm text-muted-foreground">识别知识库中的扫描版 PDF（无文字层），逐页渲染后调用视觉模型转录为文本</p>
            </div>
          </div>

          <label className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
            <span className="text-sm">
              启用 LLM OCR
              <span className="mt-0.5 block text-xs text-muted-foreground">识别扫描版 PDF 会调用配置的视觉模型，产生 API 费用</span>
            </span>
            <input
              type="checkbox"
              checked={settings.ocr_enabled}
              onChange={e => setSettings({ ...settings, ocr_enabled: e.target.checked })}
              className="h-5 w-5 shrink-0"
            />
          </label>

          {/* 关闭总开关时，以下配置项整体置灰 */}
          <div className={settings.ocr_enabled ? "space-y-5" : "space-y-5 opacity-50 pointer-events-none"}>
            <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
              <div>
                <label className="mb-2 block text-sm font-medium">页数上限</label>
                <input
                  type="number"
                  min={1}
                  value={settings.ocr_max_pages}
                  disabled={!settings.ocr_enabled}
                  onChange={e => setSettings({ ...settings, ocr_max_pages: parseInt(e.target.value) || 200 })}
                  className={inputCls}
                />
                <p className="mt-1 text-xs text-muted-foreground">单文档超过该页数将跳过 OCR，默认 200</p>
              </div>
              <div>
                <label className="mb-2 block text-sm font-medium">并发数</label>
                <input
                  type="number"
                  min={1}
                  max={4}
                  value={settings.ocr_concurrency}
                  disabled={!settings.ocr_enabled}
                  onChange={e => setSettings({ ...settings, ocr_concurrency: parseInt(e.target.value) || 2 })}
                  className={inputCls}
                />
                <p className="mt-1 text-xs text-muted-foreground">同时识别的页数，范围 1–4，默认 2</p>
              </div>
              <div>
                <label className="mb-2 block text-sm font-medium">渲染 DPI</label>
                <input
                  type="number"
                  min={150}
                  max={300}
                  step={10}
                  value={settings.ocr_dpi}
                  disabled={!settings.ocr_enabled}
                  onChange={e => setSettings({ ...settings, ocr_dpi: parseInt(e.target.value) || 200 })}
                  className={inputCls}
                />
                <p className="mt-1 text-xs text-muted-foreground">页面渲染精度，范围 150–300，默认 200</p>
              </div>
            </div>

            <div className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
              <span className="text-sm">
                OCR 缓存
                <span className="ml-2 text-xs text-muted-foreground">
                  {ocrCacheInfo
                    ? `占用 ${formatBytes(ocrCacheInfo.total_bytes)} · ${ocrCacheInfo.doc_count} 个文档`
                    : "占用信息加载中..."}
                </span>
              </span>
              <button
                onClick={handleClearOcrCache}
                disabled={!settings.ocr_enabled}
                className="action-secondary"
                style={{ padding: "6px 14px", fontSize: "12px" }}
              >
                <Trash2 size={13} /> 清空缓存
              </button>
            </div>
          </div>

          <p className="text-xs text-muted-foreground">
            关闭总开关后，所有 PDF（含扫描件）按原有逻辑解析入库，不做扫描判定、不产生 LLM 调用；已生成的 OCR 缓存保留，重新开启后仍可命中。OCR 视觉模型在各知识库的设置中单独配置。
          </p>
        </div>
      )}

      {/* Web 管理面板专属：登录账号/密码（桌面端不渲染） */}
      {activeTab === "panel" && isWebRuntime() && <PanelSettingsSection />}
    </div>
  );
}
