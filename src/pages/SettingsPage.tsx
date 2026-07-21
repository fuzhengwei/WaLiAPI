import { useEffect, useState } from "react";
import { settingsApi, serverApi, securityApi } from "../lib/api";
import type { Settings, BuiltinRule, CustomRule } from "../types";
import { Save, RotateCcw, Check, Server, SlidersHorizontal, Palette, RefreshCw, ShieldAlert, Plus, Trash2, ListChecks, Pencil, X } from "lucide-react";

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
  const [showAddRule, setShowAddRule] = useState(false);
  const [newRule, setNewRule] = useState({ rule_type: "blacklist", category: "domain", pattern: "", severity: "medium", action: "warn", description: "" });
  const [editingBuiltin, setEditingBuiltin] = useState<string | null>(null);
  const [editBuiltinData, setEditBuiltinData] = useState({ severity: "", title: "", description: "" });
  const [activeTab, setActiveTab] = useState<string>("security");

  useEffect(() => {
    settingsApi.get().then(setSettings).catch(() => {});
    securityApi.getBuiltinRules().then(setBuiltinRules).catch(() => {});
    securityApi.getCustomRules().then(setCustomRules).catch(() => {});
  }, []);

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

  if (!settings) return <div className="page-shell text-sm text-muted-foreground">加载中...</div>;

  const handleSave = async () => {
    await settingsApi.save(settings);
    await settingsApi.applyTheme(settings.ui_theme);
    await settingsApi.setAutoStart(settings.auto_start);
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

  // 统一 select 样式
  const selectCls = "w-full appearance-none rounded-2xl border border-border bg-background/70 px-4 py-3 pr-10 text-sm focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary bg-[url('data:image/svg+xml;utf8,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 20 20%22 fill=%22%2366758a%22><path d=%22M5.23 7.21a.75.75 0 011.06.02L10 11.06l3.71-3.83a.75.75 0 111.08 1.04l-4.25 4.39a.75.75 0 01-1.08 0L5.21 8.27a.75.75 0 01.02-1.06z%22/></svg>')] bg-[length:20px_20px] bg-[right_0.75rem_center] bg-no-repeat";
  const inputCls = "w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary";

  // Tab 配置
  const TABS = [
    { id: "security", label: "安全审计", icon: ShieldAlert },
    { id: "server", label: "服务配置", icon: Server },
    { id: "general", label: "通用设置", icon: SlidersHorizontal },
    { id: "appearance", label: "界面设置", icon: Palette },
    { id: "retry", label: "重试策略", icon: RefreshCw },
  ] as const;

  return (
    <div className="page-shell space-y-5">
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
            「响应侧安全扫描」开启后，上游返回内容也会被扫描并记录风险。
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
          <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
            {([
              ["最小化到托盘", "minimize_to_tray"],
              ["关闭到托盘", "close_to_tray"],
              ["开机自启", "auto_start"],
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
    </div>
  );
}
