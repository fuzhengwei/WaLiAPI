import { useState, useMemo } from "react";
import { channelApi } from "../lib/api";
import type { Channel, CreateChannelInput } from "../types";
import { CHANNEL_TYPES, CHANNEL_CATEGORIES } from "../lib/constants";
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
  const [showTypePicker, setShowTypePicker] = useState(false);

  const onTypeChange = (type: string) => {
    const info = CHANNEL_TYPES.find(t => t.value === type);
    setForm(prev => ({
      ...prev,
      type,
      base_url: info?.default_base_url || prev.base_url,
      models: info?.models || [],
    }));
    setShowTypePicker(false);
  };

  // Group channel types by category
  const groupedTypes = useMemo(() => {
    const groups: Record<string, typeof CHANNEL_TYPES> = {};
    for (const t of CHANNEL_TYPES) {
      if (!groups[t.category]) groups[t.category] = [];
      groups[t.category].push(t);
    }
    return groups;
  }, []);

  const selectedType = CHANNEL_TYPES.find(t => t.value === form.type);

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

            <div className="relative">
              <label className="mb-2 block text-sm font-medium">类型</label>
              <button
                type="button"
                onClick={() => setShowTypePicker(!showTypePicker)}
                className="flex w-full items-center justify-between rounded-2xl border border-border bg-white px-4 py-3 text-sm font-medium transition-all hover:border-primary/40 hover:shadow-sm"
              >
                <span className="flex items-center gap-2.5">
                  <span className="text-lg">{selectedType?.icon || "❓"}</span>
                  <span>{selectedType?.label || "选择类型"}</span>
                  <span className="rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground">
                    {CHANNEL_CATEGORIES[selectedType?.category || ""]?.label || ""}
                  </span>
                </span>
                <svg className={`h-4 w-4 text-muted-foreground transition-transform ${showTypePicker ? "rotate-180" : ""}`} fill="none" stroke="currentColor" strokeWidth="2" viewBox="0 0 24 24"><path strokeLinecap="round" strokeLinejoin="round" d="M19 9l-7 7-7-7" /></svg>
              </button>

              {showTypePicker && (
                <div className="absolute left-0 right-0 top-full z-30 mt-1.5 max-h-[320px] overflow-auto rounded-2xl border border-border bg-white p-3 shadow-xl">
                  {Object.entries(groupedTypes).map(([catKey, types]) => (
                    <div key={catKey} className="mb-2 last:mb-0">
                      <div className="mb-1.5 flex items-center gap-1.5 px-1 text-xs font-semibold text-muted-foreground">
                        <span>{CHANNEL_CATEGORIES[catKey]?.icon}</span>
                        <span>{CHANNEL_CATEGORIES[catKey]?.label}</span>
                      </div>
                      <div className="grid grid-cols-2 gap-1.5">
                        {types.map(t => (
                          <button
                            key={t.value}
                            type="button"
                            onClick={() => onTypeChange(t.value)}
                            className={`flex items-center gap-2 rounded-xl border px-3 py-2.5 text-sm transition-all ${
                              form.type === t.value
                                ? "border-primary/40 bg-primary/8 text-primary font-semibold shadow-sm"
                                : "border-border bg-white text-foreground hover:border-primary/30 hover:bg-muted/50"
                            }`}
                          >
                            <span className="text-base">{t.icon}</span>
                            <span className="truncate">{t.label}</span>
                            {form.type === t.value && <Check size={14} className="ml-auto shrink-0" />}
                          </button>
                        ))}
                      </div>
                    </div>
                  ))}
                </div>
              )}
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
