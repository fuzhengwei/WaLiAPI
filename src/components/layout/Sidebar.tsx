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
    <aside className="w-60 h-screen flex flex-col border-r border-border bg-sidebar">
      {/* Logo */}
      <div className="h-14 flex items-center gap-2 px-4 border-b border-border">
        <div className="w-8 h-8 rounded-lg bg-gradient-to-br from-primary to-accent flex items-center justify-center">
          <span className="text-white font-bold text-sm">X</span>
        </div>
        <span className="font-bold text-lg">xapi</span>
      </div>

      {/* Nav */}
      <nav className="flex-1 py-4 px-2 space-y-1">
        {navItems.map(({ to, icon: Icon, label }) => (
          <NavLink
            key={to}
            to={to}
            className={({ isActive }) =>
              `flex items-center gap-3 px-3 py-2 rounded-lg text-sm transition-colors ${
                isActive || (to === "/" && location.pathname === "/")
                  ? "bg-primary/10 text-primary font-medium"
                  : "text-muted-foreground hover:bg-muted hover:text-foreground"
              }`
            }
          >
            <Icon size={18} />
            {label}
          </NavLink>
        ))}
      </nav>

      {/* Server Status */}
      <div className="p-3 border-t border-border">
        <div className="flex items-center gap-2 px-3 py-2 rounded-lg bg-muted/50 text-xs">
          <Server size={14} className={serverStatus?.running ? "text-green-500" : "text-red-500"} />
          <div className="flex-1 min-w-0">
            <div className="text-muted-foreground">服务状态</div>
            <div className="font-mono truncate">
              {serverStatus?.running ? serverStatus.url : "未运行"}
            </div>
          </div>
          <span className={`w-2 h-2 rounded-full ${serverStatus?.running ? "bg-green-500" : "bg-red-500"}`} />
        </div>
      </div>
    </aside>
  );
}
