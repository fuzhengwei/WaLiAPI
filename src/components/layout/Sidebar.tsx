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
  ChevronRight,
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
    <aside className="w-72 h-screen flex-col border-r border-slate-200 bg-[#eef3f8] px-3 py-3 hidden md:flex">
      <div className="surface rounded-[22px] p-5">
        <div className="flex items-start gap-4">
          <div className="flex h-11 w-11 items-center justify-center rounded-2xl bg-[linear-gradient(180deg,#3c82f6,#2f6fed)] text-white shadow-[0_10px_20px_rgba(47,111,237,0.18)]">
            <Sparkles size={17} />
          </div>
          <div className="min-w-0 flex-1">
            <div className="text-[10px] font-semibold uppercase tracking-[0.24em] text-slate-500">AI网关，统一模型配置和负载</div>
            <div className="mt-1 text-[24px] font-semibold tracking-[-0.03em] text-slate-900">XAPI</div>
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
                  ? "border border-blue-100 bg-white text-slate-900 shadow-[0_8px_18px_rgba(15,23,42,0.05)]"
                  : "text-slate-600 hover:bg-white/70 hover:text-slate-900"
              }`
            }
          >
            <span className="flex h-9 w-9 items-center justify-center rounded-xl border border-slate-200 bg-white group-hover:bg-slate-50">
              <Icon size={17} />
            </span>
            <span className="font-medium">{label}</span>
            <ChevronRight size={15} className="ml-auto opacity-0 transition-opacity group-hover:opacity-40" />
          </NavLink>
        ))}
      </nav>

      <div className="surface-soft rounded-[20px] p-4">
        <div className="mb-3 flex items-center justify-between">
          <div>
            <div className="text-xs text-slate-500">服务状态</div>
            <div className="mt-1 text-sm font-medium text-slate-900">
              {serverStatus?.running ? "运行中" : "未启动"}
            </div>
          </div>
          <span className={`h-2.5 w-2.5 rounded-full ${serverStatus?.running ? "bg-emerald-500" : "bg-rose-500"}`} />
        </div>
        <div className="flex items-start gap-3 rounded-2xl border border-slate-200 bg-white px-3 py-3 text-xs text-slate-500">
          <Server size={14} className={serverStatus?.running ? "text-emerald-500" : "text-rose-500"} />
          <div className="min-w-0 flex-1">
            <div className="mb-1">访问地址</div>
            <div className="truncate font-mono text-[12px] text-slate-700">
              {serverStatus?.running ? serverStatus.url : "等待服务启动"}
            </div>
          </div>
        </div>
      </div>
    </aside>
  );
}
