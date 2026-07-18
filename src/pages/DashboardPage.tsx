import { useEffect, useState } from "react";
import { statsApi } from "../lib/api";
import type { DashboardStats } from "../types";
import { formatNumber, formatDuration } from "../lib/constants";
import {
  Activity, Radio, Key, Zap, TrendingUp, ArrowUpRight, Sparkle,
} from "lucide-react";

export function DashboardPage() {
  const [stats, setStats] = useState<DashboardStats | null>(null);

  useEffect(() => {
    statsApi.getDashboard().then(setStats).catch(() => {});
    const interval = setInterval(() => statsApi.getDashboard().then(setStats).catch(() => {}), 10000);
    return () => clearInterval(interval);
  }, []);

  if (!stats) {
    return <div className="page-shell text-sm text-muted-foreground">加载中...</div>;
  }

  const cards = [
    { label: "今日请求", value: formatNumber(stats.today_requests), icon: Activity, color: "text-sky-300", tone: "from-sky-500/20 to-transparent" },
    { label: "今日 Token", value: formatNumber(stats.today_total_tokens), icon: Zap, color: "text-amber-300", tone: "from-amber-500/20 to-transparent" },
    { label: "活跃渠道", value: `${stats.active_channels}/${stats.total_channels}`, icon: Radio, color: "text-emerald-300", tone: "from-emerald-500/20 to-transparent" },
    { label: "密钥数量", value: stats.total_api_keys.toString(), icon: Key, color: "text-violet-300", tone: "from-violet-500/20 to-transparent" },
  ];

  return (
    <div className="page-shell space-y-6">
      <div className="page-header">
        <div>
          <h1 className="page-title">仪表盘</h1>
          <p className="page-subtitle">系统概览、服务吞吐与运行状态一目了然</p>
        </div>
        <div className="surface-soft rounded-2xl px-4 py-3 text-right">
          <div className="text-xs text-muted-foreground">总请求量</div>
          <div className="mt-1 flex items-center justify-end gap-2 text-lg font-semibold">
            <TrendingUp className="h-4 w-4 text-primary" />
            {formatNumber(stats.total_requests)}
          </div>
        </div>
      </div>

      <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-4">
        {cards.map(({ label, value, icon: Icon, color, tone }) => (
          <div key={label} className={`surface data-card relative overflow-hidden bg-gradient-to-br ${tone}`}>
            <div className="absolute right-0 top-0 h-28 w-28 rounded-full bg-white/4 blur-3xl" />
            <div className="relative flex items-start justify-between">
              <div>
                <div className="text-sm text-muted-foreground">{label}</div>
                <div className="mt-3 text-3xl font-semibold tracking-tight">{value}</div>
              </div>
              <div className="rounded-2xl border border-white/8 bg-white/6 p-3">
                <Icon className={`h-5 w-5 ${color}`} />
              </div>
            </div>
          </div>
        ))}
      </div>

      <div className="grid grid-cols-1 gap-4 xl:grid-cols-[1.3fr_0.9fr]">
        <section className="surface rounded-[24px] p-6">
          <div className="flex items-center justify-between">
            <div>
              <h2 className="text-lg font-semibold">运行摘要</h2>
              <p className="mt-1 text-sm text-muted-foreground">核心指标帮助快速判断当前系统健康度</p>
            </div>
            <div className="rounded-2xl border border-white/8 bg-white/5 p-3">
              <Sparkle className="h-5 w-5 text-primary" />
            </div>
          </div>

          <div className="mt-6 grid grid-cols-1 gap-4 md:grid-cols-3">
            <div className="surface-soft rounded-2xl p-4">
              <div className="text-sm text-muted-foreground">平均延迟</div>
              <div className="mt-2 text-2xl font-semibold">{formatDuration(Math.round(stats.avg_latency_ms))}</div>
              <div className="mt-2 flex items-center gap-1 text-xs text-emerald-300">
                <ArrowUpRight className="h-3.5 w-3.5" /> 响应性能稳定
              </div>
            </div>
            <div className="surface-soft rounded-2xl p-4">
              <div className="text-sm text-muted-foreground">总 Token</div>
              <div className="mt-2 text-2xl font-semibold">{formatNumber(stats.total_tokens)}</div>
              <div className="mt-2 text-xs text-muted-foreground">累计消耗量</div>
            </div>
            <div className="surface-soft rounded-2xl p-4">
              <div className="text-sm text-muted-foreground">可用渠道率</div>
              <div className="mt-2 text-2xl font-semibold">
                {stats.total_channels > 0 ? `${Math.round((stats.active_channels / stats.total_channels) * 100)}%` : "0%"}
              </div>
              <div className="mt-2 text-xs text-muted-foreground">已启用 / 总渠道</div>
            </div>
          </div>
        </section>

        <section className="surface rounded-[24px] p-6">
          <h2 className="text-lg font-semibold">运维建议</h2>
          <div className="mt-5 space-y-3">
            <div className="surface-soft rounded-2xl p-4">
              <div className="text-sm font-medium">优先检查渠道健康度</div>
              <div className="mt-1 text-sm text-muted-foreground">若活跃渠道偏少，建议前往渠道页执行测试并及时启用备用线路。</div>
            </div>
            <div className="surface-soft rounded-2xl p-4">
              <div className="text-sm font-medium">关注 API Key 配额</div>
              <div className="mt-1 text-sm text-muted-foreground">当配额接近上限时，及时新增或调整下游密钥策略，避免服务中断。</div>
            </div>
            <div className="surface-soft rounded-2xl p-4">
              <div className="text-sm font-medium">结合日志定位异常</div>
              <div className="mt-1 text-sm text-muted-foreground">若平均延迟波动明显，可在日志页筛查失败请求与上游模型表现。</div>
            </div>
          </div>
        </section>
      </div>
    </div>
  );
}
