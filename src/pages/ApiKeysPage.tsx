import { useEffect, useState } from "react";
import { apiKeyApi } from "../lib/api";
import type { ApiKey, CreateApiKeyInput } from "../types";
import { formatTime } from "../lib/constants";
import { Plus, Key, Trash2, Power, X, Check, Copy, ShieldCheck, CalendarClock, Database } from "lucide-react";

export function ApiKeysPage() {
  const [keys, setKeys] = useState<ApiKey[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);

  const load = () => apiKeyApi.getAll().then(setKeys).catch(() => {});

  useEffect(() => { load(); }, []);

  const handleDelete = async (id: string) => {
    if (!confirm("确认删除此密钥？")) return;
    await apiKeyApi.delete(id);
    load();
  };

  const handleToggle = async (k: ApiKey) => {
    await apiKeyApi.update(k.id, k.status === 1 ? 0 : 1);
    load();
  };

  const copyKey = (key: string) => {
    navigator.clipboard.writeText(key);
    setCopied(key);
    setTimeout(() => setCopied(null), 2000);
  };

  return (
    <div className="page-shell space-y-6">
      <div className="page-header">
        <div>
          <h1 className="page-title">密钥管理</h1>
          <p className="page-subtitle">为下游应用生成访问凭证，并跟踪配额与有效期</p>
        </div>
        <button onClick={() => setShowForm(true)} className="action-primary">
          <Plus size={16} /> 新建密钥
        </button>
      </div>

      {keys.length === 0 ? (
        <div className="surface empty-state">
          <Key className="h-12 w-12 text-muted-foreground/70" />
          <p className="text-base font-medium">还没有创建任何密钥</p>
          <p className="text-sm text-muted-foreground">创建后即可让客户端通过 OpenAI 兼容协议接入 WaLiAPI</p>
        </div>
      ) : (
        <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
          {keys.map(k => (
            <div key={k.id} className="surface rounded-[24px] p-5">
              <div className="flex items-start justify-between gap-4">
                <div className="min-w-0 flex-1">
                  <div className="mb-3 flex items-center gap-2">
                    <span className={`h-2.5 w-2.5 rounded-full ${k.status === 1 ? "bg-emerald-400 shadow-[0_0_16px_rgba(52,211,153,0.8)]" : "bg-zinc-500"}`} />
                    <h3 className="text-lg font-semibold tracking-tight">{k.name}</h3>
                  </div>

                  <div className="flex items-center gap-2 rounded-2xl border border-white/8 bg-black/16 px-3 py-3">
                    <code className="min-w-0 flex-1 truncate text-xs font-mono text-foreground/90">{k.key}</code>
                    <button onClick={() => copyKey(k.key)} className="action-secondary px-3 py-2" title="复制">
                      {copied === k.key ? <Check size={14} className="text-emerald-300" /> : <Copy size={14} />}
                    </button>
                  </div>

                  <div className="mt-4 grid grid-cols-1 gap-3 md:grid-cols-2">
                    <div className="surface-soft rounded-2xl px-4 py-3 text-sm">
                      <div className="mb-1 flex items-center gap-2 text-muted-foreground"><ShieldCheck size={14} /> 状态</div>
                      <div className="font-medium">{k.status === 1 ? "已启用" : "已禁用"}</div>
                    </div>
                    <div className="surface-soft rounded-2xl px-4 py-3 text-sm">
                      <div className="mb-1 flex items-center gap-2 text-muted-foreground"><Database size={14} /> 配额</div>
                      <div className="font-medium">{k.quota_limit > 0 ? `${k.quota_used} / ${k.quota_limit}` : "无限制"}</div>
                    </div>
                    <div className="surface-soft rounded-2xl px-4 py-3 text-sm md:col-span-2">
                      <div className="mb-1 flex items-center gap-2 text-muted-foreground"><CalendarClock size={14} /> 时间信息</div>
                      <div className="space-y-1 text-sm">
                        {k.expires_at && <div>过期时间：{formatTime(k.expires_at)}</div>}
                        <div>创建时间：{formatTime(k.created_at)}</div>
                      </div>
                    </div>
                  </div>

                  {k.allowed_models.length > 0 && (
                    <div className="mt-4 flex flex-wrap gap-2">
                      {k.allowed_models.map(model => (
                        <span key={model} className="rounded-full bg-primary/12 px-2.5 py-1 text-xs text-primary">{model}</span>
                      ))}
                    </div>
                  )}
                </div>

                <div className="flex flex-col gap-2">
                  <button onClick={() => handleToggle(k)} className="action-secondary px-3 py-2" title={k.status === 1 ? "禁用" : "启用"}>
                    <Power size={16} className={k.status === 1 ? "text-emerald-300" : "text-zinc-400"} />
                  </button>
                  <button onClick={() => handleDelete(k.id)} className="action-secondary px-3 py-2 text-red-300" title="删除">
                    <Trash2 size={16} />
                  </button>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      {showForm && (
        <ApiKeyForm
          onClose={() => setShowForm(false)}
          onSaved={() => { setShowForm(false); load(); }}
        />
      )}
    </div>
  );
}

function ApiKeyForm({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const [form, setForm] = useState<CreateApiKeyInput>({
    name: "",
    quota_limit: -1,
  });

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    await apiKeyApi.create(form);
    onSaved();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-sm" onClick={onClose}>
      <div className="surface w-full max-w-md rounded-[28px]" onClick={e => e.stopPropagation()}>
        <div className="flex items-center justify-between border-b border-border px-5 py-4">
          <h2 className="text-lg font-semibold">新建密钥</h2>
          <button onClick={onClose} className="action-secondary px-3 py-2"><X size={18} /></button>
        </div>
        <form onSubmit={handleSubmit} className="space-y-4 p-5">
          <div>
            <label className="mb-2 block text-sm font-medium">名称</label>
            <input
              value={form.name}
              onChange={e => setForm(prev => ({ ...prev, name: e.target.value }))}
              className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm"
              placeholder="密钥名称"
              required
            />
          </div>
          <div>
            <label className="mb-2 block text-sm font-medium">配额限制 (-1 为无限)</label>
            <input
              type="number"
              value={form.quota_limit ?? -1}
              onChange={e => setForm(prev => ({ ...prev, quota_limit: parseInt(e.target.value) || -1 }))}
              className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm"
            />
          </div>
          <div className="flex justify-end gap-2 pt-2">
            <button type="button" onClick={onClose} className="action-secondary">取消</button>
            <button type="submit" className="action-primary">
              <Check size={16} /> 创建
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
