import { useEffect, useState, useMemo } from "react";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import { channelApi, apiKeyApi, serverApi } from "../lib/api";
import type { Channel, ApiKey, ServerStatus } from "../types";
import { BookOpen, Copy, Check, Play, Loader2, Link2, KeyRound, Bot } from "lucide-react";

type Platform = "curl-mac" | "curl-windows" | "javascript" | "typescript" | "java";
type TestState = "idle" | "running" | "success" | "error";

const tabs: { id: Platform; label: string; color: string; lang: string }[] = [
  { id: "curl-mac", label: "cURL Mac/Linux", color: "text-emerald-700 border-emerald-600", lang: "bash" },
  { id: "curl-windows", label: "cURL Windows", color: "text-blue-700 border-blue-600", lang: "batch" },
  { id: "javascript", label: "JavaScript", color: "text-amber-700 border-amber-600", lang: "javascript" },
  { id: "typescript", label: "TypeScript", color: "text-blue-700 border-blue-600", lang: "typescript" },
  { id: "java", label: "Java", color: "text-orange-700 border-orange-600", lang: "java" },
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

  const resultStyle = testState === "success"
    ? "border-emerald-200 bg-emerald-50 text-emerald-900"
    : testState === "error"
      ? "border-rose-200 bg-rose-50 text-rose-900"
      : "border-slate-200 bg-slate-50 text-slate-900";

  return (
    <div className="page-shell space-y-6 max-w-6xl text-slate-900">
      <div className="page-header">
        <div>
          <h1 className="page-title flex items-center gap-3"><BookOpen className="h-7 w-7 text-blue-600" />使用</h1>
          <p className="page-subtitle">按平台生成接入代码，并直接验证本地网关连通性</p>
        </div>
      </div>

      <div className="grid grid-cols-1 gap-4 xl:grid-cols-[1.1fr_0.9fr]">
        <div className="surface rounded-[24px] p-5 space-y-4">
          <h2 className="text-lg font-semibold text-slate-900">接入信息</h2>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
            <div className="surface-soft rounded-2xl p-4 md:col-span-3">
              <div className="mb-2 flex items-center gap-2 text-sm font-medium text-slate-600"><Link2 size={14} /> Base URL</div>
              <div className="flex items-center gap-2">
                <code className="flex-1 break-all rounded-2xl border border-slate-200 bg-white px-3 py-3 text-sm font-mono text-slate-900">{baseUrl}</code>
                <button onClick={() => copy(baseUrl, "baseurl")} className="action-secondary px-3 py-2">
                  {copied === "baseurl" ? <Check size={16} className="text-emerald-700" /> : <Copy size={16} />}
                </button>
              </div>
            </div>
            <div className="surface-soft rounded-2xl p-4 md:col-span-2">
              <div className="mb-2 flex items-center gap-2 text-sm font-medium text-slate-600"><KeyRound size={14} /> API Key</div>
              <div className="flex gap-2">
                <select value={selKey} onChange={e => setSelKey(e.target.value)} className="flex-1 rounded-2xl border border-slate-200 bg-white px-4 py-3 text-sm font-mono text-slate-900 shadow-sm">
                  {keys.length === 0 && <option value="">请先创建密钥</option>}
                  {keys.map(k => <option key={k.id} value={k.key}>{k.name} ({k.key.slice(0, 12)}...)</option>)}
                </select>
                <button onClick={() => selKey && copy(selKey, "key")} disabled={!selKey} className="action-secondary px-3 py-2 disabled:opacity-50">
                  {copied === "key" ? <Check size={16} className="text-emerald-700" /> : <Copy size={16} />}
                </button>
              </div>
            </div>
            <div className="surface-soft rounded-2xl p-4">
              <div className="mb-2 flex items-center gap-2 text-sm font-medium text-slate-600"><Bot size={14} /> Model</div>
              <div className="flex gap-2">
                <select value={selModel} onChange={e => setSelModel(e.target.value)} className="flex-1 rounded-2xl border border-slate-200 bg-white px-4 py-3 text-sm font-mono text-slate-900 shadow-sm">
                  {models.length === 0 && <option value="">请先配置渠道</option>}
                  {models.map(m => <option key={m} value={m}>{m}</option>)}
                </select>
                <button onClick={() => selModel && copy(selModel, "model")} disabled={!selModel} className="action-secondary px-3 py-2 disabled:opacity-50">
                  {copied === "model" ? <Check size={16} className="text-emerald-700" /> : <Copy size={16} />}
                </button>
              </div>
            </div>
          </div>
        </div>

        <div className="surface rounded-[24px] p-5">
          <div className="mb-4 flex items-center justify-between">
            <h2 className="text-lg font-semibold text-slate-900">连接测试</h2>
            <button onClick={handleTest} disabled={testState === "running" || !selKey || !selModel} className="action-primary disabled:opacity-50">
              {testState === "running" ? <Loader2 size={16} className="animate-spin" /> : <Play size={16} />}
              {testState === "running" ? "测试中..." : "发送测试请求"}
            </button>
          </div>
          <div className="surface-soft rounded-2xl p-4 text-sm text-slate-600">
            将使用当前选中的 Base URL、Key 和 Model 发起一次标准 chat/completions 请求。
          </div>
          {testResult && (
            <pre className={`mt-4 max-h-72 overflow-auto rounded-2xl border p-4 text-sm font-mono leading-6 whitespace-pre-wrap ${resultStyle}`}>{testResult}</pre>
          )}
        </div>
      </div>

      <div className="surface rounded-[24px] p-5">
        <h2 className="mb-4 text-lg font-semibold text-slate-900">代码示例</h2>
        <div className="mb-4 flex gap-2 overflow-x-auto border-b border-slate-200 pb-2">
          {tabs.map(t => (
            <button key={t.id} onClick={() => setActiveTab(t.id)} className={`rounded-xl border-b-2 px-3 py-2 text-sm font-medium whitespace-nowrap transition-colors ${activeTab === t.id ? t.color : "border-transparent text-slate-500 hover:text-slate-900"}`}>
              {t.label}
            </button>
          ))}
        </div>
        <div className="relative overflow-hidden rounded-2xl border border-slate-200">
          <SyntaxHighlighter
            language={tabs.find(t => t.id === activeTab)?.lang || "bash"}
            style={oneDark}
            customStyle={{
              margin: 0,
              borderRadius: "1rem",
              fontSize: "0.875rem",
              maxHeight: "28rem",
              overflow: "auto",
              background: "#111827",
            }}
          >
            {scripts[activeTab]}
          </SyntaxHighlighter>
          <button onClick={() => copy(scripts[activeTab], activeTab)} className="absolute right-3 top-3 action-secondary px-3 py-2">
            {copied === activeTab ? <Check size={14} className="text-emerald-700" /> : <Copy size={14} />}
          </button>
        </div>
      </div>
    </div>
  );
}
