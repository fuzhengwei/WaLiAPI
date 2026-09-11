import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { apiKeyApi } from "../lib/api";
import type { ApiKey } from "../types";
import { writeClipboard } from "../lib/runtime";

export function KnowledgeConnectionPanel({ kbId }: { kbId: string }) {
  const [keys, setKeys] = useState<ApiKey[]>([]);
  const [keyId, setKeyId] = useState("");
  const [loading, setLoading] = useState(true);
  const [testing, setTesting] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const load = async () => {
    setLoading(true); setError(""); setMessage("");
    try {
      const all = await apiKeyApi.getAll();
      const grants = await Promise.all(all.map(key => apiKeyApi.getKnowledgeAccess(key.id)));
      const eligible = all.filter((key, i) => key.status === 1 && grants[i].includes(kbId));
      setKeys(eligible); setKeyId(eligible[0]?.id || "");
    } catch (e) { setError(String(e)); }
    finally { setLoading(false); }
  };
  useEffect(() => { void load(); }, [kbId]);
  const test = async () => {
    setTesting(true); setMessage(""); setError("");
    try {
      const result = await apiKeyApi.testKnowledgeAccess(keyId, kbId);
      setMessage(`REST：${result.rest_ok ? "授权通过" : `未通过（HTTP ${result.rest_status}）`}；MCP：${result.mcp_ok ? "授权通过" : `未通过（HTTP ${result.mcp_status}，请检查 MCP 开关及权限）`}`);
    } catch (e) { setError(String(e)); }
    finally { setTesting(false); }
  };
  const copy = async () => {
    try { await writeClipboard(await apiKeyApi.getFull(keyId)); setMessage("API Key 已复制，请将它配置为客户端的 Bearer Token。"); }
    catch (e) { setError(String(e)); }
  };
  return (
    <div className="space-y-3 rounded-xl border border-slate-200 bg-white p-4 text-sm">
      <p className="font-semibold">API Key 授权</p>
      <p className="text-xs text-slate-500">在<Link to="/api-keys" className="text-blue-600 underline">密钥 → 知识库查询权限</Link>中勾选此 RAG，再使用同一个 API Key 连接 REST 或 MCP，无需设置环境变量 Token。</p>
      <div className="flex flex-wrap gap-2">
        <select aria-label="已授权的 API Key" value={keyId} disabled={loading || testing} onChange={e => { setKeyId(e.target.value); setMessage(""); setError(""); }} className="min-w-0 flex-1 rounded-lg border border-slate-200 px-3 py-2">
          {!keys.length && <option value="">{loading ? "正在加载…" : "尚无已授权的 API Key"}</option>}
          {keys.map(key => <option key={key.id} value={key.id}>{key.name} · {key.key}</option>)}
        </select>
        <button className="action-secondary" onClick={load} disabled={loading || testing}>刷新</button>
        <button className="action-secondary" onClick={copy} disabled={!keyId || loading || testing}>复制 API Key</button>
        <button className="action-primary disabled:opacity-50" onClick={test} disabled={!keyId || loading || testing}>{testing ? "检查中…" : "测试授权连接"}</button>
      </div>
      <p className="text-xs text-slate-500">连接测试只验证本机 REST / MCP 的查询授权，不调用模型。向量检索还需授权 {"Embedding"} 模型，问答还需授权生成模型并保有额度。</p>
      {message && <p role="status" className="text-xs text-slate-700">{message}</p>}
      {error && <p role="alert" className="text-xs text-red-600">{error}</p>}
    </div>
  );
}
