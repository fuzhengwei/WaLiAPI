import { useEffect, useState, useMemo } from "react";
import { channelApi, apiKeyApi, serverApi } from "../lib/api";
import type { Channel, ApiKey, ServerStatus } from "../types";
import { BookOpen, Copy, Check, Play, Terminal, Code2, Loader2, ChevronDown, ChevronUp } from "lucide-react";

type Platform = "curl-mac" | "curl-windows" | "javascript" | "typescript" | "java";
type TestState = "idle" | "running" | "success" | "error";

export function UsagePage() {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [keys, setKeys] = useState<ApiKey[]>([]);
  const [ss, setSs] = useState<ServerStatus | null>(null);
  const [selKey, setSelKey] = useState("");
  const [selModel, setSelModel] = useState("");
  const [copied, setCopied] = useState<string | null>(null);
  const [testState, setTestState] = useState<TestState>("idle");
  const [testResult, setTestResult] = useState("");
  const [expanded, setExpanded] = useState<Platform | null>("curl-mac");

  useEffect(() => {
    Promise.all([
      channelApi.getAll().catch(() => []), apiKeyApi.getAll().catch(() => []),
      serverApi.getStatus().catch(() => null),
    ]).then(([ch, ks, s]) => {
      setChannels(ch as Channel[]); setKeys(ks as ApiKey[]); setSs(s as ServerStatus | null);
      if ((ks as ApiKey[]).length > 0) setSelKey((ks as ApiKey[])[0].key);
      const ms: string[] = [];
      (ch as Channel[]).forEach(c => c.models.forEach(m => { if (!ms.includes(m)) ms.push(m); }));
      if (ms.length > 0) setSelModel(ms[0]);
    });
    const iv = setInterval(() => serverApi.getStatus().then(setSs).catch(() => {}), 5000);
    return () => clearInterval(iv);
  }, []);

  const baseUrl = ss?.running ? `${ss.url}/v1` : "http://127.0.0.1:PORT/v1";
  const models = useMemo(() => { const ms: string[] = []; channels.forEach(c => c.models.forEach(m => { if (!ms.includes(m)) ms.push(m); })); return ms; }, [channels]);

  const copy = (text: string, id: string) => { navigator.clipboard.writeText(text); setCopied(id); setTimeout(() => setCopied(null), 2000); };

  const scripts: Record<Platform, { label: string; code: string }> = {
    "curl-mac": { label: "cURL (Mac/Linux)", code: `curl ${baseUrl}/chat/completions \\\n  -H "Content-Type: application/json" \\\n  -H "Authorization: Bearer ${selKey}" \\\n  -d '{\n    "model": "${selModel}",\n    "messages": [{"role": "user", "content": "Hello!"}]\n  }'` },
    "curl-windows": { label: "cURL (Windows)", code: `curl ${baseUrl}/chat/completions ^\n  -H "Content-Type: application/json" ^\n  -H "Authorization: Bearer ${selKey}" ^\n  -d "{\\"model\\": \\"${selModel}\\", \\"messages\\": [{\\"role\\": \\"user\\", \\"content\\": \\"Hello!\\"}]}"` },
    "javascript": { label: "JavaScript (OpenAI SDK)", code: `import OpenAI from "openai";\n\nconst client = new OpenAI({\n  baseURL: "${baseUrl}",\n  apiKey: "${selKey}",\n});\n\nconst response = await client.chat.completions.create({\n  model: "${selModel}",\n  messages: [{ role: "user", content: "Hello!" }],\n});\nconsole.log(response.choices[0].message.content);` },
    "typescript": { label: "TypeScript (OpenAI SDK)", code: `import OpenAI from "openai";\n\nconst client = new OpenAI({\n  baseURL: "${baseUrl}",\n  apiKey: "${selKey}",\n});\n\nasync function main() {\n  const response = await client.chat.completions.create({\n    model: "${selModel}",\n    messages: [{ role: "user" as const, content: "Hello!" }],\n  });\n  console.log(response.choices[0].message.content);\n}\nmain();` },
    "java": { label: "Java (HttpClient)", code: `import java.net.URI;\nimport java.net.http.*;\n\npublic class XapiTest {\n  public static void main(String[] args) throws Exception {\n    HttpClient client = HttpClient.newHttpClient();\n    String body = "{\\"model\\": \\"${selModel}\\", \\"messages\\": [{\\"role\\": \\"user\\", \\"content\\": \\"Hello!\\"}]}";\n    HttpRequest req = HttpRequest.newBuilder()\n      .uri(URI.create("${baseUrl}/chat/completions"))\n      .header("Content-Type", "application/json")\n      .header("Authorization", "Bearer ${selKey}")\n      .POST(HttpRequest.BodyPublishers.ofString(body))\n      .build();\n    HttpResponse<String> resp = client.send(req, HttpResponse.BodyHandlers.ofString());\n    System.out.println(resp.body());\n  }\n}` },
  };

  const handleTest = async () => {
    if (!selKey || !selModel) return;
    setTestState("running"); setTestResult("");
    try {
      const resp = await fetch(`${baseUrl}/chat/completions`, {
        method: "POST", headers: { "Content-Type": "application/json", "Authorization": `Bearer ${selKey}` },
        body: JSON.stringify({ model: selModel, messages: [{ role: "user", content: "Say hello in one sentence" }] }),
      });
      const data = await resp.json();
      if (resp.ok) { setTestState("success"); setTestResult(`OK ${resp.status}\n\n${data.choices?.[0]?.message?.content || JSON.stringify(data, null, 2)}`); }
      else { setTestState("error"); setTestResult(`Error ${resp.status} ${resp.statusText}\n\n${JSON.stringify(data, null, 2)}`); }
    } catch (e: any) { setTestState("error"); setTestResult(`Request failed: ${e.message || String(e)}\n\nCauses:\n1. Server not running\n2. Invalid key\n3. Upstream channel error`); }
  };

  const order: Platform[] = ["curl-mac", "curl-windows", "javascript", "typescript", "java"];

  return (
    <div className="p-6 space-y-6 max-w-4xl">
      <div>
        <h1 className="text-2xl font-bold flex items-center gap-2"><BookOpen className="w-6 h-6 text-primary" /> 使用</h1>
        <p className="text-muted-foreground text-sm mt-1">快速接入 xapi 本地 API 网关</p>
      </div>

      <div className="rounded-xl border border-border bg-card p-5">
        <h2 className="font-semibold mb-3">Base URL</h2>
        <div className="flex items-center gap-2">
          <code className="flex-1 px-3 py-2 rounded-lg bg-muted text-sm font-mono break-all">{baseUrl}</code>
          <button onClick={() => copy(baseUrl, "baseurl")} className="p-2 rounded-lg hover:bg-muted border border-border">
            {copied === "baseurl" ? <Check size={16} className="text-green-500" /> : <Copy size={16} />}
          </button>
        </div>
        <p className="text-xs text-muted-foreground mt-2">在 OpenAI SDK 或兼容客户端中配置此地址为 Base URL</p>
      </div>

      <div className="rounded-xl border border-border bg-card p-5 space-y-4">
        <h2 className="font-semibold">配置选择</h2>
        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className="text-sm font-medium block mb-1">API Key</label>
            <select value={selKey} onChange={e => setSelKey(e.target.value)} className="w-full px-3 py-2 rounded-lg border border-border bg-background text-sm font-mono">
              {keys.length === 0 && <option value="">请先创建密钥</option>}
              {keys.map(k => <option key={k.id} value={k.key}>{k.name} ({k.key.slice(0, 12)}...)</option>)}
            </select>
          </div>          <div>
            <label className="text-sm font-medium block mb-1">Model</label>
            <select value={selModel} onChange={e => setSelModel(e.target.value)} className="w-full px-3 py-2 rounded-lg border border-border bg-background text-sm font-mono">
              {models.length === 0 && <option value="">请先配置渠道</option>}
              {models.map(m => <option key={m} value={m}>{m}</option>)}
            </select>
          </div>
        </div>
      </div>

      {/* Test Button */}
      <div className="rounded-xl border border-border bg-card p-5">
        <div className="flex items-center justify-between mb-4">
          <h2 className="font-semibold">连接测试</h2>
          <button
            onClick={handleTest}
            disabled={testState === "running" || !selKey || !selModel}
            className="flex items-center gap-2 px-4 py-2 rounded-lg bg-primary text-white text-sm hover:opacity-90 disabled:opacity-50"
          >
            {testState === "running" ? <Loader2 size={16} className="animate-spin" /> : <Play size={16} />}
            {testState === "running" ? "测试中..." : "发送测试请求"}
          </button>
        </div>
        {testResult && (
          <pre className={`p-4 rounded-lg text-sm font-mono whitespace-pre-wrap max-h-64 overflow-auto ${testState === "success" ? "bg-green-500/10 text-green-400" : testState === "error" ? "bg-red-500/10 text-red-400" : "bg-muted"}`}>
            {testResult}
          </pre>
        )}
      </div>

      {/* Code Examples */}
      <div className="rounded-xl border border-border bg-card p-5">
        <h2 className="font-semibold mb-4">代码示例</h2>
        <div className="space-y-2">
          {order.map(p => {
            const s = scripts[p];
            const isExpanded = expanded === p;
            return (
              <div key={p} className="rounded-lg border border-border overflow-hidden">
                <button
                  onClick={() => setExpanded(isExpanded ? null : p)}
                  className="w-full flex items-center justify-between px-4 py-3 hover:bg-muted text-sm"
                >
                  <span className="flex items-center gap-2 font-medium">
                    {p.startsWith("curl") ? <Terminal size={16} /> : <Code2 size={16} />}
                    {s.label}
                  </span>
                  {isExpanded ? <ChevronUp size={16} /> : <ChevronDown size={16} />}
                </button>
                {isExpanded && (
                  <div className="border-t border-border">
                    <div className="relative">
                      <pre className="p-4 text-sm font-mono overflow-auto max-h-96 bg-muted/30">{s.code}</pre>
                      <button
                        onClick={() => copy(s.code, p)}
                        className="absolute top-2 right-2 p-2 rounded-lg hover:bg-muted border border-border"
                      >
                        {copied === p ? <Check size={14} className="text-green-500" /> : <Copy size={14} />}
                      </button>
                    </div>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
