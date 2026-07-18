import { useEffect, useState } from "react";
import { NavLink, useLocation } from "react-router-dom";
import {
  LayoutDashboard,
  BookOpen,
  Radio,
  Key,
  ScrollText,
  Settings,
  Server,
  Sparkles,
} from "lucide-react";
import { serverApi } from "../../lib/api";
import type { ServerStatus } from "../../types";

const navItems = [
  { to: "/", icon: LayoutDashboard, label: "仪表盘" },
  { to: "/usage", icon: BookOpen, label: "使用" },
  { to: "/channels", icon: Radio, label: "渠道" },
  { to: "/api-keys", icon: Key, label: "密钥" },
  { to: "/logs", icon: ScrollText, label: "日志" },
  { to: "/settings", icon: Settings, label: "设置" },
];

export function Sidebar() {
  const [serverStatus, setServerStatus] = useState<ServerStatus | null>(null);
  const location = useLocation();

  useEffect(() => {
    serverApi.getStatus().then(setServerStatus).catch(() => {});
    const interval = setInterval(() => {
      serverApi.getStatus().then(setServerStatus).catch(() => {});
    }, 5000);
    return () => clearInterval(interval);
  }, []);

  return (
    <aside className="w-68 h-screen flex-col border-r border-white/8 bg-[linear-gradient(180deg,rgba(12,13,17,0.92),rgba(10,10,12,0.98))] px-3 py-3 hidden md:flex">
      <div className="surface rounded-[24px] p-4">
        <div className="flex items-center gap-3">
          <div className="flex h-11 w-11 items-center justify-center rounded-2xl bg-[linear-gradient(135deg,#7c82ff,#9b7bff)] shadow-[0_12px_30px_rgba(124,130,255,0.35)]">
            <Sparkles size={18} className="text-white" />
          </div>
          <div>
            <div className="text-[11px] uppercase tracking-[0.24em] text-muted-foreground">Local LLM Gateway</div>
            <div className="text-xl font-semibold tracking-tight">xapi</div>
          </div>
        </div>
      </div>

      <nav className="mt-4 flex-1 space-y-1.5">
        {navItems.map(({ to, icon: Icon, label }) => (
          <NavLink
            key={to}
            to={to}
            className={({ isActive }) =>
              `group flex items-center gap-3 rounded-2xl px-4 py-3 text-sm transition-all ${
                isActive || (to === "/" && location.pathname === "/")
                  ? "bg-[linear-gradient(135deg,rgba(124,130,255,0.2),rgba(155,123,255,0.16))] text-white shadow-[0_10px_30px_rgba(0,0,0,0.2)]"
                  : "text-muted-foreground hover:bg-white/5 hover:text-foreground"
              }`
            }
          >
            <span className="flex h-9 w-9 items-center justify-center rounded-xl border border-white/8 bg-white/4 group-hover:bg-white/8">
              <Icon size={17} />
            </span>
            <span className="font-medium">{label}</span>
          </NavLink>
        ))}
      </nav>

      <div className="surface-soft rounded-[22px] p-4">
        <div className="mb-3 flex items-center justify-between">
          <div>
            <div className="text-xs text-muted-foreground">服务状态</div>
            <div className="mt-1 text-sm font-medium text-foreground">
              {serverStatus?.running ? "运行中" : "未启动"}
            </div>
          </div>
          <span className={`h-2.5 w-2.5 rounded-full ${serverStatus?.running ? "bg-emerald-400 shadow-[0_0_16px_rgba(52,211,153,0.85)]" : "bg-red-400"}`} />
        </div>
        <div className="flex items-start gap-3 rounded-2xl border border-white/6 bg-black/20 px-3 py-3 text-xs text-muted-foreground">
          <Server size={14} className={serverStatus?.running ? "text-emerald-400" : "text-red-400"} />
          <div className="min-w-0 flex-1">
            <div className="mb-1">访问地址</div>
            <div className="truncate font-mono text-[12px] text-foreground/88">
              {serverStatus?.running ? serverStatus.url : "等待服务启动"}
            </div>
          </div>
        </div>
      </div>
    </aside>
  );
}
