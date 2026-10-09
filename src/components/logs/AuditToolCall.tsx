import { useState } from "react";
import { toolArgumentsSource } from "../../lib/auditLog";
import { formatToolArguments } from "../../lib/toolArguments";
import { writeClipboard } from "../../lib/runtime";

interface AuditToolCallProps {
  toolCall: {
    id?: string;
    function?: { name?: string; arguments?: unknown };
  };
}

export default function AuditToolCall({ toolCall }: AuditToolCallProps) {
  const [jsonView, setJsonView] = useState(false);
  const [copiedMode, setCopiedMode] = useState<string | null>(null);
  const [copyError, setCopyError] = useState(false);
  const argumentsValue = toolCall.function?.arguments;
  const sourceArguments = toolArgumentsSource(argumentsValue);
  const formattedArguments = formatToolArguments(argumentsValue);
  const formattedJson = JSON.stringify(toolCall, null, 2);

  const copy = async (text: string, mode: string) => {
    try {
      await writeClipboard(text);
      setCopyError(false);
      setCopiedMode(mode);
      setTimeout(() => setCopiedMode(null), 1500);
    } catch {
      setCopyError(true);
    }
  };

  return (
    <div className="py-2 min-w-0 space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-1.5 min-w-0">
          <span className="inline-flex items-center gap-1 rounded-md border border-slate-200 bg-slate-100 px-1.5 py-0.5 text-[10px] font-semibold text-slate-700 break-all">
            🔧 {toolCall.function?.name}
          </span>
          {toolCall.id && <span className="truncate text-[10px] font-mono text-slate-400">{toolCall.id}</span>}
        </div>
        <div className="flex flex-wrap items-center gap-1 text-[10px]">
          <button
            onClick={() => copy(sourceArguments, "source")}
            title="复制解析后的参数字段；字符串不做二次反转义，对象参数序列化为 JSON。完整入库存文请使用源数据视图。"
            className="rounded-md border border-slate-200 px-2 py-1 text-slate-600 hover:border-blue-300 hover:text-blue-600"
          >
            {copiedMode === "source" ? "已复制原文" : typeof argumentsValue === "string" ? "复制原文" : "复制参数 JSON"}
          </button>
          <button
            onClick={() => copy(formattedArguments, "readable")}
            className="rounded-md border border-slate-200 px-2 py-1 text-slate-600 hover:border-blue-300 hover:text-blue-600"
          >
            {copiedMode === "readable" ? "已复制可读内容" : "复制可读内容"}
          </button>
          <button
            onClick={() => setJsonView(!jsonView)}
            className="rounded-md border border-slate-200 px-2 py-1 text-slate-600 hover:border-blue-300 hover:text-blue-600"
          >
            {jsonView ? "可读参数" : "工具调用 JSON"}
          </button>
        </div>
      </div>
      <div className="overflow-hidden rounded border border-slate-200 bg-slate-50">
        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-slate-200 px-3 py-1.5 text-[10px] text-slate-500">
          <span>{jsonView ? "工具调用 JSON（格式化展示）" : "工具参数（可读展示，可能展开字符串转义）"}</span>
          {jsonView && <button onClick={() => copy(formattedJson, "json")} className="text-blue-600">{copiedMode === "json" ? "已复制 JSON" : "复制格式化 JSON"}</button>}
        </div>
        <pre className="max-h-[360px] max-w-full overflow-y-auto overflow-x-hidden p-3 text-xs leading-6 font-mono text-slate-700 whitespace-pre-wrap break-words [overflow-wrap:anywhere]">{jsonView ? formattedJson : formattedArguments}</pre>
      </div>
      {copyError && <p role="alert" className="text-xs text-red-600">复制失败，请重试。</p>}
    </div>
  );
}
