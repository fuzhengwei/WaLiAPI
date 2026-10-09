import { useState } from "react";
import { writeClipboard } from "../../lib/runtime";

interface AuditTextPanelProps {
  title: string;
  description?: string;
  text: string | null;
  copyLabel?: string;
}

export default function AuditTextPanel({ title, description, text, copyLabel = "复制原文" }: AuditTextPanelProps) {
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);

  return (
    <div className="space-y-2 min-w-0">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-xs font-medium text-slate-600">{title}</span>
        {text !== null && (
          <button
            onClick={async () => {
              try {
                await writeClipboard(text);
                setCopyError(false);
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              } catch {
                setCopyError(true);
              }
            }}
            className="rounded-md border border-slate-200 bg-white px-2 py-1 text-xs text-slate-600 hover:border-blue-300 hover:text-blue-600"
          >
            {copied ? "已复制" : copyLabel}
          </button>
        )}
      </div>
      {description && <p className="text-[11px] leading-relaxed text-slate-500">{description}</p>}
      {copyError && <p role="alert" className="text-xs text-red-600">复制失败，请重试。</p>}
      {text !== null ? (
        <pre className="max-h-[420px] w-full max-w-full overflow-y-auto overflow-x-hidden rounded-xl border border-slate-200 bg-slate-50 p-3 text-xs font-mono whitespace-pre-wrap break-words [overflow-wrap:anywhere] text-slate-700">{text}</pre>
      ) : (
        <p className="text-xs text-slate-400">无内容记录</p>
      )}
    </div>
  );
}
