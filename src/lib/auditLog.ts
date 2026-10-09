export interface AuditStreamSegment {
  seq: number;
  content: string;
}

export interface AuditSource {
  title: string;
  description: string;
  text: string | null;
}

export function requestLogSource(requestBody: string | null): AuditSource {
  return {
    title: "保存的请求 JSON",
    description: "直接展示入库存文，不重新序列化或反转义。请求入库前会进行日志脱敏，简要日志还可能裁剪消息；这里不是原始 HTTP 字节。",
    text: requestBody,
  };
}

export function responseLogSource(responseChoices: string | null, segments: AuditStreamSegment[]): AuditSource {
  if (segments.length > 0) {
    return {
      title: "保存的下游 SSE",
      description: "按记录顺序直接拼接保存的 SSE，不解析或反转义。可能包含网关协议转换及中断前的部分内容，不代表上游原始 HTTP 字节。",
      text: [...segments].sort((first, second) => first.seq - second.seq).map(segment => segment.content).join(""),
    };
  }
  return {
    title: "保存的响应摘要 JSON",
    description: "此日志未保存下游 SSE，仅能核对入库的响应摘要。摘要由响应提取重组，并非原始响应；此处展示和复制不再格式化。",
    text: responseChoices,
  };
}

export function toolArgumentsSource(argumentsValue: unknown): string {
  if (typeof argumentsValue === "string") return argumentsValue;
  return JSON.stringify(argumentsValue) ?? "";
}

export function contentToString(content: unknown): string {
  if (typeof content === "string") return content;
  if (content === undefined || content === null) return "";
  if (Array.isArray(content)) {
    return content
      .map((block) => {
        if (block && typeof block === "object" && !Array.isArray(block)) {
          const record = block as Record<string, unknown>;
          if (typeof record.text === "string" && record.text) return record.text;
          if ((record.type === "input_text" || record.type === "output_text") && typeof record.text === "string") return record.text;
          if (typeof record.type === "string") return `[${record.type}]`;
          return "";
        }
        return String(block);
      })
      .filter(Boolean)
      .join("\n");
  }
  return JSON.stringify(content) ?? "";
}
