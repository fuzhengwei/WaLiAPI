import { useEffect, useState, useMemo } from "react";
import { channelApi, apiKeyApi, serverApi } from "../lib/api";
import type { Channel, ApiKey, ServerStatus } from "../types";
import { BookOpen, Copy, Check, Play, Loader2 } from "lucide-react";

type Platform = "curl-mac" | "curl-windows" | "javascript" | "typescript" | "java";
type TestState = "idle" | "running" | "success" | "error";

const tabs: { id: Platform; label: string; color: string }[] = [
  { id: "curl-mac", label: "cURL Mac/Linux", color: "text-green-400 border-green-400" },
  { id: "curl-windows", label: "cURL Windows", color: "text-blue-400 border-blue-400" },
  { id: "javascript", label: "JavaScript", color: "text-yellow-400 border-yellow-400" },
  { id: "typescript", label: "TypeScript", color: "text-blue-500 border-blue-500" },
  { id: "java", label: "Java", color: "text-orange-400 border-orange-400" },
];

export function UsagePage() {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [keys, setKeys] = useState<ApiKey[]>([]);
  const [ss, setSs] = useState<ServerStatus | null>(null);
  const [selKey, setSelKey] = useState("");
  const [selModel, setSelModel] = useState("");
  const [copied, setCopied] = useState<string | null>(null);
  const [testState, setTestState] = useState<TestState>("idle");
  const [testResult, setTestResult] = useState("");
  const [activeTab, setActiveTab] = useState<Platform>("curl-mac");

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

  const baseUrl = ss?.running ? `${ss.url}/v1` : "http://127.0.0.1:8777/v1";
  const models = useMemo(() => { const ms: string[] = []; channels.forEach(c => c.models.forEach(m => { if (!ms.includes(m)) ms.push(m); })); return ms; }, [channels]);

  const copy = (text: string, id: string) => { navigator.clipboard.writeText(text); setCopied(id); setTimeout(() => setCopied(null), 2000); };

  const scripts: Record<Platform, string> = {
    "curl-mac": `curl ${baseUrl}/chat/completions \\
  -H "Content-Type: application/json" \\
  -H "Authorization: Bearer ${selKey}" \\
  -d '{
    "model": "${selModel}",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'`,
    "curl-windows": `curl ${baseUrl}/chat/completions ^
  -H "Content-Type: application/json" ^
  -H "Authorization: Bearer ${selKey}" ^
  -d "{\\"model\\": \\"${selModel}\\", \\"messages\\": [{\\"role\\": \\"user\\", \\"content\\": \\"Hello!\\"}]}"`,
    "javascript": `import OpenAI from "openai";

const client = new OpenAI({
  baseURL: "${baseUrl}",
  apiKey: "${selKey}",
});

const response = await client.chat.completions.create({
  model: "${selModel}",
  messages: [{ role: "user", content: "Hello!" }],
});
console.log(response.choices[0].message.content);`,
    "typescript": `import OpenAI from "openai";

const client = new OpenAI({
  baseURL: "${baseUrl}",
  apiKey: "${selKey}",
});

async function main() {
  const response = await client.chat.completions.create({
    model: "${selModel}",
    messages: [{ role: "user" as const, content: "Hello!" }],
  });
  console.log(response.choices[0].message.content);
}
main();`,
    "java": `import java.net.URI;
import java.net.http.*;

public class XapiTest {
  public static void main(String[] args) throws Exception {
    HttpClient client = HttpClient.newHttpClient();
    String body = "{\\"model\\": \\"${selModel}\\", \\"messages\\": [{\\"role\\": \\"user\\", \\"content\\": \\"Hello!\\"}]}";
    HttpRequest req = HttpRequest.newBuilder()
      .uri(URI.create("${baseUrl}/chat/completions"))
      .header("Content-Type", "application/json")
      .header("Authorization", "Bearer ${selKey}")
      .POST(HttpRequest.BodyPublishers.ofString(body))
      .build();
    HttpResponse<String> resp = client.send(req, HttpResponse.BodyHandlers.ofString());
    System.out.println(resp.body());
  }
}`,
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

  return (
    <div className="p-6 space-y-6 max-w-4xl">
      <div>
        <h1 className="text-2xl font-bold flex items-center gap-2"><BookOpen className="w-6 h-6 text-primary" /> 使用</h1>
        <p className="text-muted-foreground text-sm mt-1">快速接入 xapi 本地 API 网关</p>
      </div>

      {/* Base URL */}
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

      {/* Selectors with copy */}
      <div className="rounded-xl border border-border bg-card p-5 space-y-4">
        <h2 className="font-semibold">配置选择</h2>
        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className="text-sm font-medium block mb-1">API Key</label>
            <div className="flex gap-2">
              <select value={selKey} onChange={e => setSelKey(e.target.value)} className="flex-1 px-3 py-2 rounded-lg border border-border bg-background text-sm font-mono">
                {keys.length === 0 && <option value="">请先创建密钥</option>}
                {keys.map(k => <option key={k.id} value={k.key}>{k.name} ({k.key.slice(0, 12)}...)</option>)}
              </select>
              <button onClick={() => selKey && copy(selKey, "key")} disabled={!selKey} className="p-2 rounded-lg hover:bg-muted border border-border disabled:opacity-50">
                {copied === "key" ? <Check size={16} className="text-green-500" /> : <Copy size={16} />}
              </button>
            </div>
          </div>
          <div>
            <label className="text-sm font-medium block mb-1">Model</label>
            <div className="flex gap-2">
              <select value={selModel} onChange={e => setSelModel(e.target.value)} className="flex-1 px-3 py-2 rounded-lg border border-border bg-background text-sm font-mono">
                {models.length === 0 && <option value="">请先配置渠道</option>}
                {models.map(m => <option key={m} value={m}>{m}</option>)}
              </select>
              <button onClick={() => selModel && copy(selModel, "model")} disabled={!selModel} className="p-2 rounded-lg hover:bg-muted border border-border disabled:opacity-50">
                {copied === "model" ? <Check size={16} className="text-green-500" /> : <Copy size={16} />}
              </button>
            </div>
          </div>
        </div>
      </div>

      {/* Test */}
      <div className="rounded-xl border border-border bg-card p-5">
        <div className="flex items-center justify-between mb-4">
          <h2 className="font-semibold">连接测试</h2>
          <button onClick={handleTest} disabled={testState === "running" || !selKey || !selModel}
            className="flex items-center gap-2 px-4 py-2 rounded-lg bg-primary text-white text-sm hover:opacity-90 disabled:opacity-50">
            {testState === "running" ? <Loader2 size={16} className="animate-spin" /> : <Play size={16} />}
            {testState === "running" ? "测试中..." : "发送测试请求"}
          </button>
        </div>
        {testResult && (
          <pre className={`p-4 rounded-lg text-sm font-mono whitespace-pre-wrap max-h-64 overflow-auto ${testState === "success" ? "bg-green-500/10 text-green-400" : testState === "error" ? "bg-red-500/10 text-red-400" : "bg-muted"}`}>{testResult}</pre>
        )}
      </div>

      {/* Code Examples - Horizontal Tabs */}
      <div className="rounded-xl border border-border bg-card p-5">
        <h2 className="font-semibold mb-4">代码示例</h2>
        {/* Tab bar */}
        <div className="flex gap-1 mb-4 border-b border-border overflow-x-auto">
          {tabs.map(t => (
            <button key={t.id} onClick={() => setActiveTab(t.id)}
              className={`px-3 py-2 text-sm font-medium border-b-2 whitespace-nowrap transition-colors ${activeTab === t.id ? t.color : "text-muted-foreground border-transparent hover:text-foreground"}`}>
              {t.label}
            </button>
          ))}
        </div>
        {/* Code content */}
        <div className="relative">
          <pre className="p-4 text-sm font-mono overflow-auto max-h-96 bg-muted/30 rounded-lg">{scripts[activeTab]}</pre>
          <button onClick={() => copy(scripts[activeTab], activeTab)} className="absolute top-2 right-2 p-2 rounded-lg hover:bg-muted border border-border bg-card">
            {copied === activeTab ? <Check size={14} className="text-green-500" /> : <Copy size={14} />}
          </button>
        </div>
      </div>
    </div>
  );
}
