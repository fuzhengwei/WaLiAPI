// 更新源配置(共享给前端 UI + Rust 构建时)
// 实际下载走 Tauri plugin-updater,会按 tauri.conf.json 的 endpoints 顺序自动 fallback。
// 本文件仅用于前端 UI 展示(探测网络延迟 + 标识当前使用的源)。

import sourcesJson from "../../src-tauri/updater-config.json";

export type UpdaterSource = {
  id: string;
  label: string;
  latestJsonUrl: string;
};

export const SOURCES: UpdaterSource[] = sourcesJson.sources as UpdaterSource[];
export const PROBE_TIMEOUT_MS: number = sourcesJson.probeTimeoutMs ?? 3000;
