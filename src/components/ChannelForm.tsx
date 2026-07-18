import { useState } from "react";
import { channelApi } from "../lib/api";
import type { Channel, CreateChannelInput } from "../types";
import { CHANNEL_TYPES } from "../lib/constants";
import { X, Plus, Check, ArrowRight } from "lucide-react";

function ModelMappingInput({ models, existingMapping, onAdd }: {
  models: string[];
  existingMapping: Record<string, string>;
  onAdd: (upstream: string, alias: string) => void;
}) {
  const [upstream, setUpstream] = useState("");
  const [alias, setAlias] = useState("");
  const [showCustom, setShowCustom] = useState(false);

  // All aliases already used (keys of mapping)
  const usedAliases = Object.keys(existingMapping);
  // All upstream models already mapped (values of mapping)
  const usedUpstream = Object.values(existingMapping);

  const handleAdd = () => {
    if (!upstream || !alias) return;
    // Don't add duplicate aliases
    if (usedAliases.includes(alias)) return;
    onAdd(upstream, alias);
    setUpstream("");
    setAlias("");
    setShowCustom(false);
  };

  return (
    <div className="flex items-center gap-2">
      {/* Left: upstream model — allow reusing models that are already mapped */}
      <select
        value={upstream}
        onChange={e => setUpstream(e.target.value)}
        className="flex-1 px-2 py-1.5 rounded-lg border border-border bg-background text-xs font-mono"
      >
        <option value="">选择上游模型</option>
        {models.map(m => (
          <option key={m} value={m}>{m}{usedUpstream.includes(m) ? " (已映射)" : ""}</option>
        ))}
      </select>
      <ArrowRight size={14} className="text-muted-foreground shrink-0" />
      {/* Right: alias name */}
      {showCustom ? (
        <div className="flex-1 flex gap-1">
          <input
            value={alias}
            onChange={e => setAlias(e.target.value)}
            onKeyDown={e => { if (e.key === "Enter") { e.preventDefault(); handleAdd(); } }}
            className="flex-1 px-2 py-1.5 rounded-lg border border-border bg-background text-xs font-mono"
            placeholder="输入自定义名称"
            autoFocus
          />
          <button type="button" onClick={() => { setShowCustom(false); setAlias(""); }} className="p-1 hover:bg-muted rounded">
            <X size={14} />
          </button>
        </div>
      ) : (
        <select
          value={alias}
          onChange={e => {
            if (e.target.value === "__custom__") {
              setShowCustom(true);
              setAlias("");
            } else {
              setAlias(e.target.value);
            }
          }}
          className="flex-1 px-2 py-1.5 rounded-lg border border-border bg-background text-xs font-mono"
        >
          <option value="">映射为（对外名称）</option>
          {/* Show all models as alias options, even if already mapped */}
          {models.map(m => (
            <option key={m} value={m} disabled={usedAliases.includes(m)}>
              {m}{usedAliases.includes(m) ? " (已使用)" : ""}
            </option>
          ))}
          <option value="__custom__">+ 自定义名称...</option>
        </select>
      )}
      <button
        type="button"
        onClick={handleAdd}
        disabled={!upstream || !alias || usedAliases.includes(alias)}
        className="p-1.5 rounded-lg border border-border hover:bg-muted disabled:opacity-50"
      >
        <Plus size={14} />
      </button>
    </div>
  );
}

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
    model_mapping: editing?.model_mapping || {},
  });
  const [modelInput, setModelInput] = useState("");
  // model_mapping: key=对外名称(alias), value=上游实际模型(upstream)
  // Normalize: ensure values are strings (backend may return serde_json::Value)
  const rawMapping = form.model_mapping || {};
  const normalizedMapping: Record<string, string> = {};
  for (const [k, v] of Object.entries(rawMapping)) {
    normalizedMapping[k] = typeof v === "string" ? v : String(v);
  }
  const mappingEntries = Object.entries(normalizedMapping);

  const addMapping = (upstream: string, alias: string) => {
    if (!upstream || !alias) return;
    setForm(prev => ({
      ...prev,
      model_mapping: { ...(prev.model_mapping || {}), [alias]: upstream },
    }));
  };

  const removeMapping = (alias: string) => {
    setForm(prev => {
      const m: Record<string, string> = {};
      for (const [k, v] of Object.entries(prev.model_mapping || {})) {
        if (k !== alias) m[k] = typeof v === "string" ? v : String(v);
      }
      return { ...prev, model_mapping: m };
    });
  };

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
        model_mapping: form.model_mapping,
      });
    } else {
      await channelApi.create(form);
    }
    onSaved();
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={onClose}>
      <div className="bg-card rounded-xl border border-border w-full max-w-lg max-h-[90vh] overflow-auto" onClick={e => e.stopPropagation()}>
        <div className="flex items-center justify-between p-4 border-b border-border">
          <h2 className="font-bold">{editing ? "编辑渠道" : "新建渠道"}</h2>
          <button onClick={onClose} className="p-1 hover:bg-muted rounded"><X size={18} /></button>
        </div>
        <form onSubmit={handleSubmit} className="p-4 space-y-4">
          <div>
            <label className="text-sm font-medium block mb-1">名称</label>
            <input
              value={form.name}
              onChange={e => setForm(prev => ({ ...prev, name: e.target.value }))}
              className="w-full px-3 py-2 rounded-lg border border-border bg-background text-sm"
              placeholder="渠道名称"
              required
            />
          </div>

          <div>
            <label className="text-sm font-medium block mb-1">类型</label>
            <select
              value={form.type}
              onChange={e => onTypeChange(e.target.value)}
              className="w-full px-3 py-2 rounded-lg border border-border bg-background text-sm"
            >
              {CHANNEL_TYPES.map(t => (
                <option key={t.value} value={t.value}>{t.label}</option>
              ))}
            </select>
          </div>

          <div>
            <label className="text-sm font-medium block mb-1">Base URL</label>
            <input
              value={form.base_url}
              onChange={e => setForm(prev => ({ ...prev, base_url: e.target.value }))}
              className="w-full px-3 py-2 rounded-lg border border-border bg-background text-sm font-mono"
              placeholder="https://api.example.com/v1"
              required
            />
          </div>

          <div>
            <label className="text-sm font-medium block mb-1">API Key</label>
            <input
              type="password"
              value={form.api_key}
              onChange={e => setForm(prev => ({ ...prev, api_key: e.target.value }))}
              className="w-full px-3 py-2 rounded-lg border border-border bg-background text-sm font-mono"
              placeholder={editing ? "留空则不修改" : "sk-..."}
              required={!editing}
            />
          </div>

          <div>
            <label className="text-sm font-medium block mb-1">模型列表</label>
            <div className="flex gap-2 mb-2">
              <input
                value={modelInput}
                onChange={e => setModelInput(e.target.value)}
                onKeyDown={e => { if (e.key === "Enter") { e.preventDefault(); addModel(); } }}
                className="flex-1 px-3 py-2 rounded-lg border border-border bg-background text-sm"
                placeholder="输入模型名称，回车添加"
              />
              <button type="button" onClick={addModel} className="px-3 py-2 rounded-lg border border-border hover:bg-muted">
                <Plus size={16} />
              </button>
            </div>
            <div className="flex flex-wrap gap-1">
              {form.models.map(m => (
                <span key={m} className="text-xs px-2 py-1 rounded bg-primary/10 text-primary flex items-center gap-1">
                  {m}
                  <button type="button" onClick={() => removeModel(m)} className="hover:text-red-500">
                    <X size={12} />
                  </button>
                </span>
              ))}
            </div>
          </div>

          {/* Model Mapping */}
          <div>
            <label className="text-sm font-medium block mb-1">模型映射</label>
            <p className="text-xs text-muted-foreground mb-2">
              左侧为渠道实际模型，右侧为对外暴露的名称。调用右侧名称时，实际转发给左侧模型。
            </p>
            {/* Existing mappings */}
            {mappingEntries.length > 0 && (
              <div className="space-y-1 mb-2">
                {mappingEntries.map(([alias, upstream]) => (
                  <div key={alias} className="flex items-center gap-2">
                    <div className="flex-1 px-2 py-1 rounded bg-muted text-xs font-mono truncate">
                      <span className="text-green-400">{alias}</span>
                      <span className="text-muted-foreground mx-1">→</span>
                      <span className="text-blue-400">{upstream}</span>
                    </div>
                    <button type="button" onClick={() => removeMapping(alias)} className="p-1 hover:text-red-500">
                      <X size={14} />
                    </button>
                  </div>
                ))}
              </div>
            )}
            {/* Add new mapping */}
            <ModelMappingInput models={form.models} existingMapping={normalizedMapping} onAdd={addMapping} />
          </div>

          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="text-sm font-medium block mb-1">优先级</label>
              <input
                type="number"
                value={form.priority}
                onChange={e => setForm(prev => ({ ...prev, priority: parseInt(e.target.value) || 0 }))}
                className="w-full px-3 py-2 rounded-lg border border-border bg-background text-sm"
              />
            </div>
            <div>
              <label className="text-sm font-medium block mb-1">权重</label>
              <input
                type="number"
                value={form.weight}
                onChange={e => setForm(prev => ({ ...prev, weight: parseInt(e.target.value) || 1 }))}
                className="w-full px-3 py-2 rounded-lg border border-border bg-background text-sm"
              />
            </div>
          </div>

          <div className="flex justify-end gap-2 pt-2">
            <button type="button" onClick={onClose} className="px-4 py-2 rounded-lg border border-border text-sm hover:bg-muted">
              取消
            </button>
            <button type="submit" className="flex items-center gap-1 px-4 py-2 rounded-lg bg-primary text-white text-sm hover:opacity-90">
              <Check size={16} /> 保存
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
