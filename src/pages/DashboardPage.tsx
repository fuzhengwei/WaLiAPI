import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { statsApi } from "../lib/api";
import type { DashboardStats } from "../types";
import { formatNumber, formatDuration } from "../lib/constants";
import {
  Activity,
  Radio,
  Key,
  Zap,
  TrendingUp,
  ShieldCheck,
  Workflow,
  Plus,
  Settings,
  BookOpen,
  LayoutGrid,
  FileText,
  Globe,
  HelpCircle,
  X,
  Check,
} from "lucide-react";

export function DashboardPage() {
  const [stats, setStats] = useState<DashboardStats | null>(null);
  const [showHelp, setShowHelp] = useState(false);
  const navigate = useNavigate();

  useEffect(() => {
    statsApi.getDashboard().then(setStats).catch(() => {});
    const interval = setInterval(() => statsApi.getDashboard().then(setStats).catch(() => {}), 10000);
    return () => clearInterval(interval);
  }, []);

  if (!stats) {
    return <div className="page-shell text-sm text-slate-500">加载中...</div>;
  }

  const statCards = [
    { label: "今日请求", value: formatNumber(stats.today_requests), icon: Activity, color: "text-blue-600", tone: "bg-blue-50" },
    { label: "今日 Token", value: formatNumber(stats.today_total_tokens), icon: Zap, color: "text-amber-600", tone: "bg-amber-50" },
    { label: "活跃渠道", value: `${stats.active_channels}/${stats.total_channels}`, icon: Radio, color: "text-emerald-600", tone: "bg-emerald-50" },
    { label: "密钥数量", value: stats.total_api_keys.toString(), icon: Key, color: "text-indigo-600", tone: "bg-indigo-50" },
  ];

  const quickActions = [
    { title: "新建渠道", icon: Plus, action: () => navigate("/channels") },
    { title: "管理密钥", icon: Key, action: () => navigate("/api-keys") },
    { title: "接入示例", icon: BookOpen, action: () => navigate("/usage") },
    { title: "请求日志", icon: FileText, action: () => navigate("/logs") },
    { title: "安全设置", icon: ShieldCheck, action: () => navigate("/settings") },
    { title: "服务配置", icon: Settings, action: () => navigate("/settings") },
    { title: "渠道管理", icon: Globe, action: () => navigate("/channels") },
    { title: "面板总览", icon: LayoutGrid, action: () => navigate("/") },
  ];

  const summaryItems = [
    {
      label: "累计请求",
      value: formatNumber(stats.total_requests),
      hint: "服务吞吐",
      icon: TrendingUp,
      color: "text-indigo-700",
      tone: "border-indigo-100 bg-indigo-50",
    },
    {
      label: "累计 Token",
      value: formatNumber(stats.total_tokens),
      hint: "模型消耗",
      icon: Zap,
      color: "text-amber-700",
      tone: "border-amber-100 bg-amber-50",
    },
    {
      label: "服务可用率",
      value: stats.total_channels > 0 ? `${Math.round((stats.active_channels / stats.total_channels) * 100)}%` : "0%",
      hint: "健康度",
      icon: ShieldCheck,
      color: "text-emerald-700",
      tone: "border-emerald-100 bg-emerald-50",
    },
    {
      label: "平均延迟",
      value: formatDuration(Math.round(stats.avg_latency_ms)),
      hint: "响应性能",
      icon: Workflow,
      color: "text-blue-700",
      tone: "border-blue-100 bg-blue-50",
    },
  ];

  return (
    <div className="page-shell space-y-5">
      {/* 顶部：欢迎 + 数据展示 */}
      <section className="surface rounded-[24px] p-6 md:p-7">
        <div className="flex flex-col gap-6 xl:flex-row xl:items-start xl:justify-between">
          <div className="max-w-2xl">
            <div className="inline-flex items-center gap-2 rounded-full border border-blue-100 bg-blue-50 px-3 py-1 text-xs font-medium text-blue-700">
              <Workflow className="h-3.5 w-3.5" /> 控制台首页
            </div>
            <div className="mt-4 flex items-center gap-2">
              <h1 className="text-3xl font-semibold tracking-[-0.03em] text-slate-900">欢迎使用 WaLiAPI</h1>
              <button
                onClick={() => setShowHelp(true)}
                className="inline-flex items-center justify-center rounded-full border border-slate-200 bg-slate-50 p-1 text-slate-400 transition-all hover:border-blue-200 hover:bg-blue-50 hover:text-blue-600"
                title="使用帮助"
              >
                <HelpCircle className="h-4 w-4" />
              </button>
            </div>
            <p className="mt-2.5 text-sm leading-6 text-slate-500 md:text-[15px]">
              在一个统一入口中管理上游模型渠道、下游密钥、请求统计与故障切换，让本地 LLM 网关更稳定、更清晰、更易运维。
            </p>

            {/* 快速操作按钮 */}
            <div className="mt-5 flex flex-wrap gap-2">
              {quickActions.map(({ title, icon: Icon, action }) => (
                <button
                  key={title}
                  onClick={action}
                  className="inline-flex items-center gap-1.5 rounded-full border border-slate-200 bg-slate-50 px-3 py-1.5 text-xs font-medium text-slate-700 transition-all hover:border-blue-200 hover:bg-white hover:text-blue-700 hover:shadow-sm"
                >
                  <Icon className="h-3.5 w-3.5" />
                  {title}
                </button>
              ))}
            </div>
          </div>

          {/* 数据展示：原控制台摘要 */}
          <div className="grid grid-cols-2 gap-2.5 xl:w-[440px]">
            {summaryItems.map(({ label, value, hint, icon: Icon, color, tone }) => (
              <div key={label} className={`rounded-2xl border p-3.5 ${tone}`}>
                <div className="flex items-center gap-1.5 text-xs">
                  <Icon className={`h-3.5 w-3.5 ${color}`} />
                  <span className={color}>{label}</span>
                </div>
                <div className="mt-1.5 text-xl font-semibold text-slate-900">{value}</div>
                <div className="text-[11px] text-slate-500">{hint}</div>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* 今日请求等卡片 */}
      <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-4">
        {statCards.map(({ label, value, icon: Icon, color, tone }) => (
          <div key={label} className="surface data-card">
            <div className="flex items-start justify-between">
              <div>
                <div className="text-sm text-slate-500">{label}</div>
                <div className="mt-3 text-3xl font-semibold tracking-tight text-slate-900">{value}</div>
              </div>
              <div className={`rounded-2xl border border-white/0 px-3 py-3 ${tone}`}>
                <Icon className={`h-5 w-5 ${color}`} />
              </div>
            </div>
          </div>
        ))}
      </div>

      {/* 运行摘要 + 运维建议 */}
      <div className="grid grid-cols-1 gap-4 xl:grid-cols-[1.3fr_0.9fr]">
        <section className="surface rounded-[20px] p-6">
          <div className="flex items-center justify-between">
            <div>
              <h2 className="text-lg font-semibold text-slate-900">运行摘要</h2>
              <p className="mt-1 text-sm text-slate-500">核心指标帮助快速判断当前系统健康度</p>
            </div>
            <div className="rounded-2xl border border-slate-200 bg-slate-50 p-3">
              <TrendingUp className="h-5 w-5 text-slate-500" />
            </div>
          </div>

          <div className="mt-6 grid grid-cols-1 gap-4 md:grid-cols-3">
            <div className="rounded-2xl border border-blue-100 bg-blue-50 p-4">
              <div className="text-sm text-blue-700">平均延迟</div>
              <div className="mt-2 text-2xl font-semibold text-blue-900">{formatDuration(Math.round(stats.avg_latency_ms))}</div>
              <div className="mt-2 flex items-center gap-1 text-xs text-blue-700">
                <TrendingUp className="h-3.5 w-3.5" /> 响应性能稳定
              </div>
            </div>
            <div className="rounded-2xl border border-amber-100 bg-amber-50 p-4">
              <div className="text-sm text-amber-700">总 Token</div>
              <div className="mt-2 text-2xl font-semibold text-amber-900">{formatNumber(stats.total_tokens)}</div>
              <div className="mt-2 text-xs text-amber-700">累计消耗量</div>
            </div>
            <div className="rounded-2xl border border-emerald-100 bg-emerald-50 p-4">
              <div className="text-sm text-emerald-700">可用渠道率</div>
              <div className="mt-2 text-2xl font-semibold text-emerald-900">
                {stats.total_channels > 0 ? `${Math.round((stats.active_channels / stats.total_channels) * 100)}%` : "0%"}
              </div>
              <div className="mt-2 text-xs text-emerald-700">已启用 / 总渠道</div>
            </div>
          </div>
        </section>

        <section className="surface rounded-[20px] p-6">
          <h2 className="text-lg font-semibold text-slate-900">运维建议</h2>
          <div className="mt-5 space-y-3">
            <div className="rounded-2xl border border-slate-200 bg-slate-50 p-4">
              <div className="text-sm font-medium text-slate-900">优先检查渠道健康度</div>
              <div className="mt-1 text-sm text-slate-500">若活跃渠道偏少，建议前往渠道页执行测试并及时启用备用线路。</div>
            </div>
            <div className="rounded-2xl border border-slate-200 bg-slate-50 p-4">
              <div className="text-sm font-medium text-slate-900">关注 API Key 配额</div>
              <div className="mt-1 text-sm text-slate-500">当配额接近上限时，及时新增或调整下游密钥策略，避免服务中断。</div>
            </div>
            <div className="rounded-2xl border border-slate-200 bg-slate-50 p-4">
              <div className="text-sm font-medium text-slate-900">结合日志定位异常</div>
              <div className="mt-1 text-sm text-slate-500">若平均延迟波动明显，可在日志页筛查失败请求与上游模型表现。</div>
            </div>
          </div>
        </section>
      </div>

      {/* 使用帮助弹窗 */}
      {showHelp && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center overflow-y-auto bg-black/40 p-4 backdrop-blur-sm"
          onClick={() => setShowHelp(false)}
        >
          <div
            className="relative my-auto w-full max-w-lg max-h-[85vh] overflow-y-auto rounded-3xl bg-white p-7 shadow-2xl"
            onClick={e => e.stopPropagation()}
          >
            <button
              onClick={() => setShowHelp(false)}
              className="absolute right-5 top-5 rounded-full p-1 text-slate-400 transition-colors hover:bg-slate-100 hover:text-slate-600"
            >
              <X className="h-5 w-5" />
            </button>

            <div className="flex items-center gap-2">
              <div className="rounded-2xl border border-blue-100 bg-blue-50 p-2.5">
                <HelpCircle className="h-5 w-5 text-blue-600" />
              </div>
              <div>
                <h2 className="text-lg font-semibold text-slate-900">快速上手指南</h2>
                <p className="text-xs text-slate-500">几步完成本地 LLM 网关配置</p>
              </div>
            </div>

            <div className="mt-5 space-y-3.5">
              {[
            {
              num: "1",
              required: true,
              title: "添加上游渠道",
              desc: "进入「渠道管理」页面，点击「新建渠道」，填写名称、Base URL、API Key 和支持的模型，保存即可。",
              route: "/channels",
              routeLabel: "前往渠道管理",
            },
            {
              num: "2",
              required: true,
              title: "创建本地密钥",
              desc: "进入「API 密钥」页面，点击「新建密钥」生成 `sk-waliapi-*` 格式的本地访问令牌，用于下游客户端调用。",
              route: "/api-keys",
              routeLabel: "前往 API 密钥",
            },
            {
              num: "3",
              required: true,
              title: "查看接入示例",
              desc: "进入「接入示例」页面，复制 cURL / Python / Node.js 代码，将 `base_url` 指向 `http://127.0.0.1:8777/v1`，使用本地密钥即可调用。",
              route: "/usage",
              routeLabel: "前往接入示例",
            },
            {
              num: "4",
              required: false,
              title: "配置服务与重试",
              desc: "在「设置 → 服务配置」中调整监听地址与端口；在「重试策略」中开启失败自动重试，提升服务稳定性。",
              route: "/settings",
              routeLabel: "前往设置",
            },
            {
              num: "5",
              required: false,
              title: "开启安全审计",
              desc: "在「设置 → 安全审计」中启用请求风险检测，自动识别凭证泄露、敏感路径、工具外联与 Unicode 隐写。",
              route: "/settings",
              routeLabel: "前往安全设置",
            },
          ].map(step => (
                <div
                  key={step.num}
                  className="rounded-2xl border border-slate-200 bg-slate-50 p-4"
                >
                  <div className="flex items-start gap-3">
                    <div className={`flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-xs font-semibold text-white ${step.required ? "bg-blue-600" : "bg-slate-400"}`}>
                      {step.num}
                    </div>
                    <div className="flex-1">
                      <div className="flex items-center gap-2">
                        <span className="text-sm font-medium text-slate-900">{step.title}</span>
                        {step.required ? (
                          <span className="inline-flex items-center gap-0.5 rounded-full bg-blue-100 px-1.5 py-0.5 text-[10px] font-medium text-blue-700">
                            <Check className="h-2.5 w-2.5" />必选
                          </span>
                        ) : (
                          <span className="inline-flex items-center rounded-full bg-slate-100 px-1.5 py-0.5 text-[10px] font-medium text-slate-500">
                            可选
                          </span>
                        )}
                      </div>
                      <p className="mt-1 text-sm leading-5 text-slate-500">{step.desc}</p>
                      <button
                        onClick={() => {
                          navigate(step.route);
                          setShowHelp(false);
                        }}
                        className="mt-2 inline-flex items-center gap-1 text-xs font-medium text-blue-600 hover:text-blue-700"
                      >
                        {step.routeLabel}
                        <span aria-hidden>→</span>
                      </button>
                    </div>
                  </div>
                </div>
              ))}
            </div>

            <div className="mt-5 rounded-2xl border border-emerald-100 bg-emerald-50 p-4">
              <div className="flex items-start gap-2.5">
                <FileText className="mt-0.5 h-4 w-4 shrink-0 text-emerald-600" />
                <div>
                  <div className="text-sm font-medium text-emerald-900">调用后可查看请求日志</div>
                  <p className="mt-1 text-xs leading-5 text-emerald-700">
                    发起请求后，进入「请求日志」页面查看每次调用的状态码、Token 消耗、工具调用、安全风险等级与上游路由详情。
                  </p>
                  <button
                    onClick={() => {
                      navigate("/logs");
                      setShowHelp(false);
                    }}
                    className="mt-2 inline-flex items-center gap-1 text-xs font-medium text-emerald-700 hover:text-emerald-800"
                  >
                    前往请求日志<span aria-hidden>→</span>
                  </button>
                </div>
              </div>
            </div>

            <div className="mt-4 flex items-center justify-between rounded-2xl bg-slate-100 px-4 py-3">
              <span className="text-xs text-slate-500">
                <span className="font-semibold text-slate-700">1、2、3</span> 为必选步骤 ·{" "}
                <span className="font-semibold text-slate-700">4、5</span> 为可选增强
              </span>
              <button
                onClick={() => setShowHelp(false)}
                className="rounded-full bg-slate-900 px-4 py-1.5 text-xs font-medium text-white transition-colors hover:bg-slate-700"
              >
                我知道了
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
