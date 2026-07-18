import { useEffect, useState } from "react";
import { channelApi } from "../lib/api";
import type { Channel } from "../types";
import { CHANNEL_TYPES, formatTime } from "../lib/constants";
import { Plus, Radio, Trash2, Play, Power, Edit, Gauge, Boxes } from "lucide-react";
import { ChannelForm } from "../components/ChannelForm";

export function ChannelsPage() {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [editing, setEditing] = useState<Channel | null>(null);
  const [testing, setTesting] = useState<string | null>(null);
  const [testResult, setTestResult] = useState<Record<string, { success: boolean; message: string; latency_ms: number }>>({});

  const load = () => channelApi.getAll().then(setChannels).catch(() => {});

  useEffect(() => { load(); }, []);

  const handleTest = async (id: string) => {
    setTesting(id);
    try {
      const result = await channelApi.test(id);
      setTestResult(prev => ({ ...prev, [id]: result }));
    } catch (e: any) {
      setTestResult(prev => ({ ...prev, [id]: { success: false, message: String(e), latency_ms: 0 } }));
    }
    setTesting(null);
  };

  const handleDelete = async (id: string) => {
    if (!confirm("确认删除此渠道？")) return;
    await channelApi.delete(id);
    load();
  };

  const handleToggle = async (ch: Channel) => {
    await channelApi.update({ id: ch.id, status: ch.status === 1 ? 0 : 1 });
    load();
  };

  return (
    <div className="page-shell space-y-6">
      <div className="page-header">
        <div>
          <h1 className="page-title">渠道管理</h1>
          <p className="page-subtitle">配置上游供应商、模型能力与调度优先级</p>
        </div>
        <button onClick={() => { setEditing(null); setShowForm(true); }} className="action-primary">
          <Plus size={16} /> 新建渠道
        </button>
      </div>

      {channels.length === 0 ? (
        <div className="surface empty-state">
          <Radio className="h-12 w-12 text-muted-foreground/70" />
          <p className="text-base font-medium">还没有配置任何渠道</p>
          <p className="text-sm text-muted-foreground">先添加一个上游服务商，即可开始分发请求</p>
        </div>
      ) : (
        <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
          {channels.map(ch => {
            const typeInfo = CHANNEL_TYPES.find(t => t.value === ch.type);
            const result = testResult[ch.id];
            return (
              <div key={ch.id} className="surface rounded-[24px] p-5">
                <div className="flex items-start justify-between gap-4">
                  <div className="min-w-0 flex-1">
                    <div className="mb-3 flex flex-wrap items-center gap-2">
                      <span className={`h-2.5 w-2.5 rounded-full ${ch.status === 1 ? "bg-emerald-400 shadow-[0_0_16px_rgba(52,211,153,0.8)]" : "bg-zinc-500"}`} />
                      <h3 className="text-lg font-semibold tracking-tight">{ch.name}</h3>
                      <span className="rounded-full border border-white/8 bg-white/5 px-2.5 py-1 text-xs text-muted-foreground">
                        {typeInfo?.label || ch.type}
                      </span>
                    </div>

                    <div className="surface-soft rounded-2xl px-3 py-3 text-xs font-mono text-foreground/80 break-all">
                      {ch.base_url}
                    </div>

                    <div className="mt-4 flex flex-wrap gap-2">
                      {ch.models.slice(0, 6).map(m => (
                        <span key={m} className="rounded-full bg-primary/12 px-2.5 py-1 text-xs text-primary">
                          {m}
                        </span>
                      ))}
                      {ch.models.length > 6 && <span className="px-1 text-xs text-muted-foreground">+{ch.models.length - 6}</span>}
                    </div>

                    <div className="mt-4 grid grid-cols-2 gap-3 text-sm">
                      <div className="surface-soft rounded-2xl px-3 py-3">
                        <div className="mb-1 flex items-center gap-2 text-muted-foreground"><Gauge size={14} /> 调度</div>
                        <div className="font-medium">优先级 {ch.priority} · 权重 {ch.weight}</div>
                      </div>
                      <div className="surface-soft rounded-2xl px-3 py-3">
                        <div className="mb-1 flex items-center gap-2 text-muted-foreground"><Boxes size={14} /> 模型数</div>
                        <div className="font-medium">{ch.models.length} 个</div>
                      </div>
                    </div>

                    {(ch.last_test_at || result) && (
                      <div className="mt-4 rounded-2xl border border-white/8 bg-black/16 px-3 py-3 text-xs">
                        {ch.last_test_at && (
                          <div className="text-muted-foreground">
                            最近测试: {formatTime(ch.last_test_at)}
                            {ch.last_test_ok !== null && (
                              <span className={ch.last_test_ok ? " ml-2 text-emerald-300" : " ml-2 text-red-300"}>
                                {ch.last_test_ok ? "成功" : "失败"}
                              </span>
                            )}
                          </div>
                        )}
                        {result && (
                          <div className={`mt-1 ${result.success ? "text-emerald-300" : "text-red-300"}`}>
                            {result.success ? "✓" : "✗"} {result.message} ({result.latency_ms}ms)
                          </div>
                        )}
                      </div>
                    )}
                  </div>

                  <div className="flex flex-col gap-2">
                    <button onClick={() => handleTest(ch.id)} disabled={testing === ch.id} className="action-secondary px-3 py-2 text-blue-300" title="测试">
                      <Play size={16} />
                    </button>
                    <button onClick={() => { setEditing(ch); setShowForm(true); }} className="action-secondary px-3 py-2" title="编辑">
                      <Edit size={16} />
                    </button>
                    <button onClick={() => handleToggle(ch)} className="action-secondary px-3 py-2" title={ch.status === 1 ? "禁用" : "启用"}>
                      <Power size={16} className={ch.status === 1 ? "text-emerald-300" : "text-zinc-400"} />
                    </button>
                    <button onClick={() => handleDelete(ch.id)} className="action-secondary px-3 py-2 text-red-300" title="删除">
                      <Trash2 size={16} />
                    </button>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {showForm && (
        <ChannelForm
          editing={editing}
          onClose={() => { setShowForm(false); setEditing(null); }}
          onSaved={() => { setShowForm(false); setEditing(null); load(); }}
        />
      )}
    </div>
  );
}
