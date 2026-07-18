// Channel type definitions
export const CHANNEL_CATEGORIES: Record<string, { label: string; icon: string }> = {
  international: { label: "国际", icon: "🌍" },
  domestic: { label: "国内", icon: "🇨🇳" },
  local: { label: "本地", icon: "💻" },
  custom: { label: "自定义", icon: "⚙️" },
};

export const CHANNEL_TYPES: Array<{
  value: string;
  label: string;
  category: string;
  icon: string;
  default_base_url: string;
  models: string[];
}> = [
  { value: "openai", label: "OpenAI", category: "international", icon: "🟢", default_base_url: "https://api.openai.com/v1", models: ["gpt-5.4", "gpt-5.5", "gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "gpt-3.5-turbo"] },
  { value: "deepseek", label: "DeepSeek", category: "international", icon: "🐋", default_base_url: "https://api.deepseek.com/v1", models: ["deepseek-chat", "deepseek-coder", "deepseek-reasoner"] },
  { value: "claude", label: "Anthropic Claude", category: "international", icon: "🤖", default_base_url: "https://api.anthropic.com", models: ["claude-sonnet-4-20250514", "claude-3-7-sonnet-20250219", "claude-3-5-haiku-20241022"] },
  { value: "gemini", label: "Google Gemini", category: "international", icon: "💎", default_base_url: "https://generativelanguage.googleapis.com", models: ["gemini-2.5-flash", "gemini-2.5-pro", "gemini-2.0-flash"] },
  { value: "qwen", label: "通义千问", category: "domestic", icon: "🔮", default_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", models: ["qwen-max", "qwen-plus", "qwen-turbo"] },
  { value: "zhipu", label: "智谱 GLM", category: "domestic", icon: "✨", default_base_url: "https://open.bigmodel.cn/api/paas/v4", models: ["glm-4-plus", "glm-4-flash", "glm-4-air"] },
  { value: "moonshot", label: "Moonshot AI", category: "domestic", icon: "🌙", default_base_url: "https://api.moonshot.cn/v1", models: ["moonshot-v1-8k", "moonshot-v1-32k", "moonshot-v1-128k"] },
  { value: "doubao", label: "字节豆包", category: "domestic", icon: "🫘", default_base_url: "https://ark.cn-beijing.volces.com/api/v3", models: ["doubao-pro-32k", "doubao-pro-128k", "doubao-lite-32k"] },
  { value: "ollama", label: "Ollama (本地)", category: "local", icon: "🦙", default_base_url: "http://localhost:11434/v1", models: ["llama3.1", "qwen2.5", "mistral"] },
  { value: "custom", label: "自定义", category: "custom", icon: "⚙️", default_base_url: "", models: [] },
];

export function getChannelType(value: string) {
  return CHANNEL_TYPES.find(t => t.value === value);
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export function formatNumber(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return n.toString();
}

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(2)}s`;
}

export function formatTime(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleString("zh-CN", { hour12: false });
}
