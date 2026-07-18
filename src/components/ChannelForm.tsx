import { useState } from "react";
import { channelApi } from "../lib/api";
import type { Channel, CreateChannelInput } from "../types";
import { CHANNEL_TYPES } from "../lib/constants";
import { X, Plus, Check } from "lucide-react";

export function ChannelForm({ editing, onClose, onSaved }: {
  editing: Channel | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [form, setForm] = useState<CreateChannelInput>({
    name: editing?.name || "",
    type: editing?.type || "openai",
    base_url: editing?.base_url || "https://api.openai.com/v1",
    api_key: "",
    models: editing?.models || ["gpt-4o-mini"],
    priority: editing?.priority ?? 0,
    weight: editing?.weight ?? 1,
  });
  const [modelInput, setModelInput] = useState("");

  const onTypeChange = (type: string) => {
    const info = CHANNEL_TYPES.find(t => t.value === type);
    setForm(prev => ({
      ...prev,
      type,
      base_url: info?.default_base_url || prev.base_url,
      models: info?.models || [],
    }));
  };

  const addModel = () => {
    if (modelInput.trim()) {
      setForm(prev => ({ ...prev, models: [...prev.models, modelInput.trim()] }));
      setModelInput("");
    }
  };

  const removeModel = (m: string) => {
    setForm(prev => ({ ...prev, models: prev.models.filter(x => x !== m) }));
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (editing) {
      await channelApi.update({
        id: editing.id,
        name: form.name,
        type: form.type,
        base_url: form.base_url,
        api_key: form.api_key || undefined,
        models: form.models,
        priority: form.priority,
        weight: form.weight,
      });
    } else {
      await channelApi.create(form);
    }
    onSaved();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-sm" onClick={onClose}>
      <div className="surface w-full max-w-2xl max-h-[92vh] overflow-auto rounded-[28px]" onClick={e => e.stopPropagation()}>
        <div className="flex items-center justify-between border-b border-border px-5 py-4">
          <h2 className="text-lg font-semibold">{editing ? "编辑渠道" : "新建渠道"}</h2>
          <button onClick={onClose} className="action-secondary px-3 py-2"><X size={18} /></button>
        </div>
        <form onSubmit={handleSubmit} className="space-y-5 p-5">
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <div>
              <label className="mb-2 block text-sm font-medium">名称</label>
              <input
                value={form.name}
                onChange={e => setForm(prev => ({ ...prev, name: e.target.value }))}
                className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm"
                placeholder="渠道名称"
                required
              />
            </div>

            <div>
              <label className="mb-2 block text-sm font-medium">类型</label>
              <select
                value={form.type}
                onChange={e => onTypeChange(e.target.value)}
                className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm"
              >
                {CHANNEL_TYPES.map(t => (
                  <option key={t.value} value={t.value}>{t.label}</option>
                ))}
              </select>
            </div>
          </div>

          <div>
            <label className="mb-2 block text-sm font-medium">Base URL</label>
            <input
              value={form.base_url}
              onChange={e => setForm(prev => ({ ...prev, base_url: e.target.value }))}
              className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm font-mono"
              placeholder="https://api.example.com/v1"
              required
            />
          </div>

          <div>
            <label className="mb-2 block text-sm font-medium">API Key</label>
            <input
              type="password"
              value={form.api_key}
              onChange={e => setForm(prev => ({ ...prev, api_key: e.target.value }))}
              className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm font-mono"
              placeholder={editing ? "留空则不修改" : "sk-..."}
              required={!editing}
            />
          </div>

          <div>
            <label className="mb-2 block text-sm font-medium">模型列表</label>
            <div className="mb-3 flex gap-2">
              <input
                value={modelInput}
                onChange={e => setModelInput(e.target.value)}
                onKeyDown={e => { if (e.key === "Enter") { e.preventDefault(); addModel(); } }}
                className="flex-1 rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm"
                placeholder="输入模型名称，回车添加"
              />
              <button type="button" onClick={addModel} className="action-secondary px-4 py-3">
                <Plus size={16} />
              </button>
            </div>
            <div className="flex flex-wrap gap-2">
              {form.models.map(m => (
                <span key={m} className="inline-flex items-center gap-1 rounded-full bg-primary/12 px-3 py-1.5 text-xs text-primary">
                  {m}
                  <button type="button" onClick={() => removeModel(m)} className="hover:text-red-300">
                    <X size={12} />
                  </button>
                </span>
              ))}
            </div>
          </div>

          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <div>
              <label className="mb-2 block text-sm font-medium">优先级</label>
              <input
                type="number"
                value={form.priority}
                onChange={e => setForm(prev => ({ ...prev, priority: parseInt(e.target.value) || 0 }))}
                className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm"
              />
            </div>
            <div>
              <label className="mb-2 block text-sm font-medium">权重</label>
              <input
                type="number"
                value={form.weight}
                onChange={e => setForm(prev => ({ ...prev, weight: parseInt(e.target.value) || 1 }))}
                className="w-full rounded-2xl border border-border bg-background/70 px-4 py-3 text-sm"
              />
            </div>
          </div>

          <div className="flex justify-end gap-2 pt-2">
            <button type="button" onClick={onClose} className="action-secondary">取消</button>
            <button type="submit" className="action-primary">
              <Check size={16} /> 保存
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
