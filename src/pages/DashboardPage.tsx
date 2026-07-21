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
  BookOpen,
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

  const availability = stats.total_channels > 0 ? Math.round((stats.active_channels / stats.total_channels) * 100) : 0;

  // 统一 6 卡片网格：今日请求 / 今日Token / 累计请求 / 累计Token / 活跃渠道 / 平均延迟
  const metrics = [
    { label: "今日请求", value: formatNumber(stats.today_requests), icon: Activity, color: "text-blue-600", tone: "bg-blue-50" },
    { label: "今日 Token", value: formatNumber(stats.today_total_tokens), icon: Zap, color: "text-amber-600", tone: "bg-amber-50" },
    { label: "累计请求", value: formatNumber(stats.total_requests), icon: TrendingUp, color: "text-indigo-600", tone: "bg-indigo-50" },
    { label: "累计 Token", value: formatNumber(stats.total_tokens), icon: Zap, color: "text-orange-600", tone: "bg-orange-50" },
    { label: "活跃渠道", value: `${stats.active_channels}/${stats.total_channels}`, icon: Radio, color: "text-emerald-600", tone: "bg-emerald-50" },
    { label: "平均延迟", value: formatDuration(Math.round(stats.avg_latency_ms)), icon: Workflow, color: "text-violet-600", tone: "bg-violet-50" },
  ];

  const quickActions = [
    { title: "新建渠道", icon: Plus, action: () => navigate("/channels") },
    { title: "管理密钥", icon: Key, action: () => navigate("/api-keys") },
    { title: "接入示例", icon: BookOpen, action: () => navigate("/usage") },
    { title: "审计日志", icon: FileText, action: () => navigate("/logs") },
    { title: "安全设置", icon: ShieldCheck, action: () => navigate("/settings") },
    { title: "渠道管理", icon: Globe, action: () => navigate("/channels") },
  ];

  return (
    <div className="page-shell space-y-5">
      {/* 顶部：欢迎 + 快速操作 */}
      <section className="surface rounded-[24px] p-6 md:p-7">
        <div className="flex flex-col gap-5 xl:flex-row xl:items-start xl:justify-between">
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

          {/* 健康度徽章 */}
          <div className="flex gap-3 xl:w-auto">
            <div className={`flex items-center gap-2.5 rounded-2xl border px-4 py-3 ${availability >= 80 ? "border-emerald-200 bg-emerald-50" : availability >= 50 ? "border-amber-200 bg-amber-50" : "border-rose-200 bg-rose-50"}`}>
              <ShieldCheck className={`h-5 w-5 ${availability >= 80 ? "text-emerald-600" : availability >= 50 ? "text-amber-600" : "text-rose-600"}`} />
              <div>
                <div className="text-xs text-slate-500">服务可用率</div>
                <div className={`text-lg font-semibold ${availability >= 80 ? "text-emerald-700" : availability >= 50 ? "text-amber-700" : "text-rose-700"}`}>{availability}%</div>
              </div>
            </div>
          </div>
        </div>
      </section>

      {/* 统一指标卡片 */}
      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6">
        {metrics.map(({ label, value, icon: Icon, color, tone }) => (
          <div key={label} className="surface data-card">
            <div className="flex items-center justify-between">
              <div className={`rounded-xl ${tone} p-2`}>
                <Icon className={`h-4 w-4 ${color}`} />
              </div>
            </div>
            <div className="mt-3 text-2xl font-semibold tracking-tight text-slate-900">{value}</div>
            <div className="text-xs text-slate-500">{label}</div>
          </div>
        ))}
      </div>

      {/* 运维建议 */}
      <section className="surface rounded-[20px] p-6">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-lg font-semibold text-slate-900">运维建议</h2>
            <p className="mt-1 text-sm text-slate-500">根据当前系统状态给出的运维参考</p>
          </div>
          <TrendingUp className="h-5 w-5 text-slate-400" />
        </div>
        <div className="mt-5 grid grid-cols-1 gap-3 md:grid-cols-3">
          <div className="rounded-2xl border border-slate-200 bg-slate-50 p-4">
            <div className="flex items-center gap-2">
              <Radio className="h-4 w-4 text-emerald-600" />
              <span className="text-sm font-medium text-slate-900">渠道健康度</span>
            </div>
            <p className="mt-1.5 text-sm text-slate-500">
              {availability >= 80
                ? "当前渠道运行正常，各线路可用。"
                : availability >= 50
                  ? "部分渠道不可用，建议检查并启用备用线路。"
                  : "活跃渠道较少，请前往渠道页测试并启用。"}
            </p>
          </div>
          <div className="rounded-2xl border border-slate-200 bg-slate-50 p-4">
            <div className="flex items-center gap-2">
              <Key className="h-4 w-4 text-indigo-600" />
              <span className="text-sm font-medium text-slate-900">密钥配额</span>
            </div>
            <p className="mt-1.5 text-sm text-slate-500">
              {stats.total_api_keys > 0
                ? `共 ${stats.total_api_keys} 个密钥，定期检查配额使用情况。`
                : "尚未创建密钥，请前往 API 密钥页创建。"}
            </p>
          </div>
          <div className="rounded-2xl border border-slate-200 bg-slate-50 p-4">
            <div className="flex items-center gap-2">
              <Activity className="h-4 w-4 text-blue-600" />
              <span className="text-sm font-medium text-slate-900">性能监控</span>
            </div>
            <p className="mt-1.5 text-sm text-slate-500">
              {stats.avg_latency_ms < 2000
                ? `平均延迟 ${formatDuration(Math.round(stats.avg_latency_ms))}，响应正常。`
                : `平均延迟 ${formatDuration(Math.round(stats.avg_latency_ms))}，建议查看日志排查慢请求。`}
            </p>
          </div>
        </div>
      </section>

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
                  <div className="text-sm font-medium text-emerald-900">调用后可查看审计日志</div>
                  <p className="mt-1 text-xs leading-5 text-emerald-700">
                    发起请求后，进入「审计日志」页面查看每次调用的状态码、Token 消耗、工具调用、安全风险等级与上游路由详情。
                  </p>
                  <button
                    onClick={() => {
                      navigate("/logs");
                      setShowHelp(false);
                    }}
                    className="mt-2 inline-flex items-center gap-1 text-xs font-medium text-emerald-700 hover:text-emerald-800"
                  >
                    前往审计日志<span aria-hidden>→</span>
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
