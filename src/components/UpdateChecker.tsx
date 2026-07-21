import { useEffect, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getVersion } from "@tauri-apps/api/app";
import {
  X,
  Download,
  Loader2,
  RefreshCw,
  CheckCircle2,
  AlertCircle,
  Globe,
  Zap,
} from "lucide-react";
import {
  SOURCES,
  type UpdaterSource,
} from "../lib/updater-sources";

type Stage =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "latest"; source: UpdaterSource }
  | { kind: "available"; source: UpdaterSource; update: Update }
  | { kind: "downloading"; source: UpdaterSource; update: Update; percent: number }
  | { kind: "ready" }
  | { kind: "error"; message: string; probeResults?: ProbeResult[] };

type ProbeResult = { source: UpdaterSource; status: "ok" | "fail"; latencyMs?: number };

/**
 * 探测所有源的网络情况(用于 UI 展示 + 决定默认源)。
 * 实际下载走 Tauri 内置的 plugin-updater,会按 tauri.conf.json 的 endpoints 顺序自动 fallback。
 */
async function probeAllSources(): Promise<{
  results: ProbeResult[];
  firstOk: UpdaterSource | null;
}> {
  const results = await Promise.all(
    SOURCES.map(async (s) => {
      const start = performance.now();
      try {
        const ctrl = new AbortController();
        const timer = setTimeout(() => ctrl.abort(), 3000);
        const res = await fetch(s.latestJsonUrl, {
          method: "HEAD",
          signal: ctrl.signal,
          cache: "no-store",
        });
        clearTimeout(timer);
        if (res.ok || res.status === 405) {
          return {
            source: s,
            status: "ok" as const,
            latencyMs: Math.round(performance.now() - start),
          };
        }
        return { source: s, status: "fail" as const };
      } catch {
        return { source: s, status: "fail" as const };
      }
    }),
  );
  return {
    results,
    firstOk: results.find((r) => r.status === "ok")?.source ?? null,
  };
}

export function UpdateChecker({ onClose }: { onClose?: () => void }) {
  const [stage, setStage] = useState<Stage>({ kind: "idle" });
  const [currentVersion, setCurrentVersion] = useState("");
  const [probeResults, setProbeResults] = useState<ProbeResult[]>([]);

  useEffect(() => {
    getVersion().then(setCurrentVersion).catch(() => {});
  }, []);

  const startCheck = async () => {
    setStage({ kind: "checking" });
    try {
      // 1. 探测所有源(只用于 UI 展示,不阻塞 check)
      const { results, firstOk } = await probeAllSources();
      setProbeResults(results);

      if (!firstOk) {
        setStage({
          kind: "error",
          message: "所有更新源均不可达,请检查网络后重试。",
          probeResults: results,
        });
        return;
      }

      // 2. 调用 Tauri 内置 check() - 它会按 tauri.conf.json 的 endpoints 顺序自动 fallback
      //    网络不可达的源会被它自动跳过,我们不需要在这里手动选
      const update = await check();
      if (update) {
        setStage({ kind: "available", source: firstOk, update });
      } else {
        setStage({ kind: "latest", source: firstOk });
      }
    } catch (e: any) {
      setStage({
        kind: "error",
        message: String(e?.message ?? e),
        probeResults,
      });
    }
  };

  // 打开即自动检查一次
  useEffect(() => {
    startCheck();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleUpdate = async (update: Update) => {
    const source =
      stage.kind === "available" || stage.kind === "downloading"
        ? stage.source
        : SOURCES[0];
    setStage({ kind: "downloading", source, update, percent: 0 });
    try {
      let downloaded = 0;
      let total = 0;
      await update.downloadAndInstall((event) => {
        if (event.event === "Started" && event.data.contentLength) {
          total = event.data.contentLength;
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          if (total > 0) {
            setStage((s) =>
              s.kind === "downloading"
                ? { ...s, percent: Math.min(99, Math.round((downloaded / total) * 100)) }
                : s,
            );
          }
        } else if (event.event === "Finished") {
          setStage((s) => (s.kind === "downloading" ? { ...s, percent: 100 } : s));
        }
      });
      setStage({ kind: "ready" });
    } catch (e: any) {
      setStage({ kind: "error", message: String(e?.message ?? e), probeResults });
    }
  };

  // ===== UI 组件 =====

  const StatusCard = ({
    icon,
    title,
    subtitle,
    tone,
    iconBg,
    borderColor,
  }: {
    icon: React.ReactNode;
    title: string;
    subtitle?: string;
    tone: "success" | "info" | "danger";
    iconBg: string;
    borderColor: string;
  }) => (
    <div
      className={`flex items-start gap-3.5 rounded-xl border ${borderColor} bg-white/5 px-4 py-4 shadow-sm`}
    >
      <div
        className={`flex h-12 w-12 shrink-0 items-center justify-center rounded-full ${iconBg}`}
      >
        {icon}
      </div>
      <div className="min-w-0 flex-1 pt-0.5">
        <p
          className={`text-[15px] font-semibold leading-snug ${
            tone === "success"
              ? "text-emerald-400"
              : tone === "danger"
              ? "text-red-400"
              : "text-white"
          }`}
        >
          {title}
        </p>
        {subtitle && (
          <p className="mt-1.5 text-[13px] leading-relaxed text-white/70">
            {subtitle}
          </p>
        )}
      </div>
    </div>
  );

  // 更新源状态面板
  const SourceIndicator = ({
    showTitle = true,
  }: {
    showTitle?: boolean;
  }) => {
    if (probeResults.length === 0) return null;
    return (
      <div className="space-y-1.5">
        {showTitle && (
          <p className="text-[11px] uppercase tracking-wider text-white/40">
            更新源探测
          </p>
        )}
        {SOURCES.map((s) => {
          const r = probeResults.find((x) => x.source.id === s.id);
          const ok = r?.status === "ok";
          return (
            <div
              key={s.id}
              className={`flex items-center justify-between rounded-lg border px-3 py-1.5 text-[12px] ${
                ok
                  ? "border-emerald-400/25 bg-emerald-500/10 text-emerald-200"
                  : "border-red-400/25 bg-red-500/10 text-red-200"
              }`}
            >
              <div className="flex items-center gap-2">
                {ok ? <CheckCircle2 size={13} /> : <AlertCircle size={13} />}
                <span className="font-medium">{s.label}</span>
              </div>
              <span className="font-mono text-[11px] opacity-80">
                {ok ? `${r?.latencyMs}ms` : "不可达"}
              </span>
            </div>
          );
        })}
      </div>
    );
  };

  // 当前所用源徽标
  const CurrentSourceBadge = ({ source }: { source: UpdaterSource }) => {
    const r = probeResults.find((x) => x.source.id === source.id);
    const ok = r?.status === "ok";
    return (
      <div
        className={`flex items-center gap-1.5 text-[12px] ${
          ok ? "text-emerald-300" : "text-white/50"
        }`}
      >
        {ok ? <Zap size={12} /> : <Globe size={12} />}
        <span>检测源:</span>
        <span className="font-medium text-white/90">{source.label}</span>
        {ok && r?.latencyMs && (
          <span className="font-mono text-[11px] text-white/50">
            ({r.latencyMs}ms)
          </span>
        )}
      </div>
    );
  };

  const content = (() => {
    switch (stage.kind) {
      case "idle":
      case "checking":
        return (
          <>
            <StatusCard
              icon={<Loader2 size={24} className="animate-spin text-sky-300" />}
              title="正在检查更新"
              subtitle="正在探测可用更新源,请稍候…"
              tone="info"
              iconBg="bg-sky-500/20"
              borderColor="border-sky-400/30"
            />
            <div className="mt-4">
              <SourceIndicator />
            </div>
          </>
        );
      case "latest":
        return (
          <>
            <StatusCard
              icon={<CheckCircle2 size={24} className="text-emerald-300" />}
              title="已是最新版本"
              subtitle={`当前版本 v${currentVersion},您已使用最新版本,无需更新。`}
              tone="success"
              iconBg="bg-emerald-500/20"
              borderColor="border-emerald-400/30"
            />
            <div className="mt-3 flex justify-between">
              <CurrentSourceBadge source={stage.source} />
            </div>
            <div className="mt-4 flex justify-end gap-2">
              <button
                onClick={startCheck}
                className="flex items-center gap-1.5 rounded-lg bg-white/5 px-4 py-2 text-[13px] text-white/80 ring-1 ring-white/15 transition hover:bg-white/10"
              >
                <RefreshCw size={13} /> 重新检查
              </button>
              <button
                onClick={onClose}
                className="rounded-lg bg-white/10 px-5 py-2 text-[14px] font-medium text-white ring-1 ring-white/20 transition hover:bg-white/20"
              >
                知道了
              </button>
            </div>
          </>
        );
      case "available":
        return (
          <div className="space-y-3">
            <StatusCard
              icon={<Download size={24} className="text-sky-300" />}
              title={`发现新版本 v${stage.update.version}`}
              subtitle={`当前版本 v${currentVersion},建议更新到最新版本。`}
              tone="info"
              iconBg="bg-sky-500/20"
              borderColor="border-sky-400/30"
            />
            <div className="rounded-lg bg-white/5 px-3 py-2">
              <CurrentSourceBadge source={stage.source} />
            </div>
            {stage.update.body && (
              <div className="max-h-32 overflow-y-auto rounded-xl border border-white/10 bg-black/30 p-3.5 text-[13px] leading-relaxed text-white/75 whitespace-pre-wrap">
                {stage.update.body}
              </div>
            )}
            <div className="flex justify-end gap-2 pt-1">
              <button
                onClick={onClose}
                className="rounded-lg bg-white/5 px-4 py-2 text-[14px] text-white/80 ring-1 ring-white/15 transition hover:bg-white/10"
              >
                稍后
              </button>
              <button
                onClick={() => handleUpdate(stage.update)}
                className="flex items-center gap-1.5 rounded-lg bg-sky-500 px-4 py-2 text-[14px] font-medium text-white shadow-lg shadow-sky-500/30 transition hover:bg-sky-400"
              >
                <Download size={15} /> 立即更新
              </button>
            </div>
          </div>
        );
      case "downloading":
        return (
          <div className="space-y-4">
            <StatusCard
              icon={<Download size={24} className="text-sky-300" />}
              title={`正在下载 v${stage.update.version}`}
              subtitle={`下载期间请勿关闭应用,完成后将自动准备安装。`}
              tone="info"
              iconBg="bg-sky-500/20"
              borderColor="border-sky-400/30"
            />
            <div className="space-y-2">
              <div className="flex items-center justify-between text-[13px]">
                <span className="text-white/70">下载进度</span>
                <span className="font-mono font-semibold text-sky-300">
                  {stage.percent}%
                </span>
              </div>
              <div className="h-2 w-full overflow-hidden rounded-full bg-white/10">
                <div
                  className="h-full bg-gradient-to-r from-sky-400 to-sky-300 transition-all"
                  style={{ width: `${stage.percent}%` }}
                />
              </div>
            </div>
          </div>
        );
      case "ready":
        return (
          <div className="space-y-3">
            <StatusCard
              icon={<CheckCircle2 size={24} className="text-emerald-300" />}
              title="更新已准备就绪"
              subtitle="新版本已下载完成,重启应用后生效。"
              tone="success"
              iconBg="bg-emerald-500/20"
              borderColor="border-emerald-400/30"
            />
            <div className="flex justify-end gap-2">
              <button
                onClick={onClose}
                className="rounded-lg bg-white/5 px-4 py-2 text-[14px] text-white/80 ring-1 ring-white/15 transition hover:bg-white/10"
              >
                稍后
              </button>
              <button
                onClick={() => relaunch()}
                className="flex items-center gap-1.5 rounded-lg bg-emerald-500 px-4 py-2 text-[14px] font-medium text-white shadow-lg shadow-emerald-500/30 transition hover:bg-emerald-400"
              >
                <RefreshCw size={15} /> 重启应用
              </button>
            </div>
          </div>
        );
      case "error":
        return (
          <div className="space-y-3">
            <StatusCard
              icon={<AlertCircle size={24} className="text-red-300" />}
              title="检查更新失败"
              subtitle={stage.message}
              tone="danger"
              iconBg="bg-red-500/20"
              borderColor="border-red-400/30"
            />
            {stage.probeResults && stage.probeResults.length > 0 && (
              <SourceIndicator showTitle={false} />
            )}
            <div className="flex justify-end gap-2">
              <button
                onClick={onClose}
                className="rounded-lg bg-white/5 px-4 py-2 text-[14px] text-white/80 ring-1 ring-white/15 transition hover:bg-white/10"
              >
                关闭
              </button>
              <button
                onClick={startCheck}
                className="flex items-center gap-1.5 rounded-lg bg-sky-500 px-4 py-2 text-[14px] font-medium text-white shadow-lg shadow-sky-500/30 transition hover:bg-sky-400"
              >
                <RefreshCw size={15} /> 重试
              </button>
            </div>
          </div>
        );
      default:
        return null;
    }
  })();

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4 backdrop-blur-md"
      onClick={onClose}
    >
      <div
        className="relative w-full max-w-sm overflow-hidden rounded-2xl border border-white/10 bg-zinc-900/95 p-6 shadow-2xl shadow-black/60 ring-1 ring-white/5"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="pointer-events-none absolute -top-px left-1/2 h-px w-1/2 -translate-x-1/2 bg-gradient-to-r from-transparent via-sky-400/60 to-transparent" />

        <div className="mb-5 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <div className="flex h-7 w-7 items-center justify-center rounded-md bg-sky-500/15 ring-1 ring-sky-400/30">
              <Download size={15} className="text-sky-300" />
            </div>
            <h3 className="text-[16px] font-semibold text-white">软件更新</h3>
          </div>
          {onClose && (
            <button
              onClick={onClose}
              className="rounded-md p-1.5 text-white/60 transition hover:bg-white/10 hover:text-white"
              aria-label="关闭"
            >
              <X size={18} />
            </button>
          )}
        </div>
        {content}
      </div>
    </div>
  );
}
