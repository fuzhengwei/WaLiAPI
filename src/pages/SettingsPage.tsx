import { useEffect, useState } from "react";
import { settingsApi, serverApi } from "../lib/api";
import type { Settings } from "../types";
import { Save, RotateCcw, Check, Server, SlidersHorizontal, Palette, RefreshCw } from "lucide-react";

export function SettingsPage() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    settingsApi.get().then(setSettings).catch(() => {});
  }, []);

  if (!settings) return <div className="page-shell text-sm text-muted-foreground">加载中...</div>;

  const handleSave = async () => {
    await settingsApi.save(settings);
    setSaved(true);
    setTimeout(() => setSaved(false), 2000);
  };

  const handleRestart = async () => {
    await serverApi.restart();
  };

  return (
    <div className="page-shell space-y-6 max-w-5xl">
      <div className="page-header">
        <div>
          <h1 className="page-title">设置</h1>
          <p className="page-subtitle">服务、界面与重试策略统一配置</p>
        </div>
        <button onClick={handleSave} className="action-primary">
          {saved ? <Check size={16} /> : <Save size={16} />}
          {saved ? "已保存" : "保存设置"}
        </button>
      </div>

      <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
        <div className="surface rounded-[24px] p-6 space-y-5">
          <div className="flex items-center gap-3">
            <div className="rounded-2xl border border-white/8 bg-white/6 p-3"><Server size={18} className="text-primary" /></div>
            <div>
              <h2 className="text-lg font-semibold">服务配置</h2>
              <p className="text-sm text-muted-foreground">控制本地网关监听地址与服务重启</p>
            </div>
          </div>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <div>
              <label className="mb-2 block text-sm font-medium">监听地址</label>
              <input
                value={settings.server_host}
                onChange={e => setSettings({ ...settings, server_host: e.target.value })}
                className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm font-mono"
              />
            </div>
            <div>
              <label className="mb-2 block text-sm font-medium">端口 (0=随机)</label>
              <input
                type="number"
                value={settings.server_port}
                onChange={e => setSettings({ ...settings, server_port: parseInt(e.target.value) || 0 })}
                className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm"
              />
            </div>
          </div>
          <button onClick={handleRestart} className="action-secondary">
            <RotateCcw size={16} /> 重启服务
          </button>
        </div>

        <div className="surface rounded-[24px] p-6 space-y-5">
          <div className="flex items-center gap-3">
            <div className="rounded-2xl border border-white/8 bg-white/6 p-3"><SlidersHorizontal size={18} className="text-primary" /></div>
            <div>
              <h2 className="text-lg font-semibold">通用设置</h2>
              <p className="text-sm text-muted-foreground">桌面端交互习惯与启动行为</p>
            </div>
          </div>
          <div className="space-y-3">
            {[
              ["最小化到托盘", "minimize_to_tray"],
              ["关闭到托盘", "close_to_tray"],
              ["开机自启", "auto_start"],
            ].map(([label, key]) => (
              <label key={key} className="surface-soft flex items-center justify-between rounded-2xl px-4 py-4">
                <span className="text-sm">{label}</span>
                <input
                  type="checkbox"
                  checked={settings[key as keyof Settings] as boolean}
                  onChange={e => setSettings({ ...settings, [key]: e.target.checked })}
                  className="h-5 w-5"
                />
              </label>
            ))}
          </div>
        </div>

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
                className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm"
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
                className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm"
              >
                <option value="zh-CN">简体中文</option>
                <option value="en">English</option>
              </select>
            </div>
          </div>
        </div>

        <div className="surface rounded-[24px] p-6 space-y-5">
          <div className="flex items-center gap-3">
            <div className="rounded-2xl border border-white/8 bg-white/6 p-3"><RefreshCw size={18} className="text-primary" /></div>
            <div>
              <h2 className="text-lg font-semibold">重试策略</h2>
              <p className="text-sm text-muted-foreground">请求失败后的自动恢复行为</p>
            </div>
          </div>
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
                value={settings.retry_times}
                onChange={e => setSettings({ ...settings, retry_times: parseInt(e.target.value) || 0 })}
                className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm"
              />
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
