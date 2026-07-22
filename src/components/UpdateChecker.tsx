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
} from "lucide-react";

type Stage =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "latest" }
  | { kind: "available"; update: Update }
  | { kind: "downloading"; update: Update; percent: number }
  | { kind: "ready" }
  | { kind: "error"; message: string };

export function UpdateChecker({
  onClose,
  onUpdateStarted,
}: {
  onClose?: () => void;
  onUpdateStarted?: () => void;
}) {
  const [stage, setStage] = useState<Stage>({ kind: "idle" });
  const [currentVersion, setCurrentVersion] = useState("");

  useEffect(() => {
    getVersion().then(setCurrentVersion).catch(() => {});
  }, []);

  const startCheck = async () => {
    setStage({ kind: "checking" });
    try {
      const update = await check();
      if (update) {
        setStage({ kind: "available", update });
      } else {
        setStage({ kind: "latest" });
      }
    } catch (e: any) {
      setStage({ kind: "error", message: String(e?.message ?? e) });
    }
  };

  // 打开即自动检查
  useEffect(() => {
    startCheck();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleUpdate = async (update: Update) => {
    onUpdateStarted?.();
    setStage({ kind: "downloading", update, percent: 0 });
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
      setStage({ kind: "error", message: String(e?.message ?? e) });
    }
  };

  // ===== 状态卡片 =====
  const StatusCard = ({
    icon,
    title,
    subtitle,
    iconBg,
    borderColor,
  }: {
    icon: React.ReactNode;
    title: string;
    subtitle?: string;
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
        <p className="text-[15px] font-semibold leading-snug text-white">
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

  const content = (() => {
    switch (stage.kind) {
      case "idle":
      case "checking":
        return (
          <StatusCard
            icon={<Loader2 size={24} className="animate-spin text-sky-300" />}
            title="正在检查更新"
            subtitle="正在连接更新服务器,请稍候…"
            iconBg="bg-sky-500/20"
            borderColor="border-sky-400/30"
          />
        );
      case "latest":
        return (
          <>
            <StatusCard
              icon={<CheckCircle2 size={24} className="text-emerald-300" />}
              title="已是最新版本"
              subtitle={`当前版本 v${currentVersion},您已使用最新版本,无需更新。`}
              iconBg="bg-emerald-500/20"
              borderColor="border-emerald-400/30"
            />
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
              iconBg="bg-sky-500/20"
              borderColor="border-sky-400/30"
            />
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
              subtitle="下载期间请勿关闭应用,完成后将自动准备安装。"
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
              iconBg="bg-red-500/20"
              borderColor="border-red-400/30"
            />
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
        {/* 顶部装饰线 */}
        <div className="pointer-events-none absolute -top-px left-1/2 h-px w-1/2 -translate-x-1/2 bg-gradient-to-r from-transparent via-sky-400/60 to-transparent" />

        {/* 标题栏 */}
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
