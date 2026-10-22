import { useEffect, useId, useMemo, useRef, useState, type CSSProperties } from "react";
import { Search } from "lucide-react";

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
  /** 分组标题（不可选中，仅作视觉分隔，替代原生 optgroup） */
  heading?: boolean;
}

interface SelectProps {
  value: string;
  onChange: (value: string) => void;
  options: SelectOption[];
  placeholder?: string;
  disabled?: boolean;
  /** 触发器尺寸：md 与 SELECT_CLS 的大号圆角输入框一致，sm 用于紧凑表单/工具栏 */
  size?: "md" | "sm";
  /** 追加到外层容器的布局类（宽度、flex 等），不影响基础视觉 */
  className?: string;
  /** 弹层最小宽度跟随触发器（默认）或内容自适应 */
  fullWidthList?: boolean;
  /** 可选项（不含分组标题）达到该数量时，弹层顶部显示搜索框；不传则不启用搜索 */
  searchThreshold?: number;
  /** 弹层网格列数：2 列宽弹层，适合模型较多的宽列表场景 */
  gridColumns?: 1 | 2;
  /** 宽弹层（gridColumns=2）的目标宽度，默认 460px，自动按视口收敛 */
  wideListWidth?: number;
  ariaLabel?: string;
}

// 统一自定义下拉框：替代原生 <select>（原生弹层是系统渲染的，样式不可控）。
// 设计语言与 SELECT_CLS / ProviderDropdown 对齐：rounded-2xl + border-border +
// bg-background/70，聚焦时 border-primary + primary 色环；弹层白底圆角卡片，
// 选中项 primary 色 + ✓，hover/键盘聚焦项 bg-muted。
// 增强能力：选项较多时（searchThreshold）顶部出现搜索框；gridColumns=2 时
// 弹层加宽为双列网格，分组标题横跨整行，宽弹层自动按视口边缘收敛防溢出。
export function Select({
  value,
  onChange,
  options,
  placeholder = "请选择",
  disabled = false,
  size = "md",
  className = "",
  fullWidthList = true,
  searchThreshold,
  gridColumns = 1,
  wideListWidth = 460,
  ariaLabel,
}: SelectProps) {
  const [open, setOpen] = useState(false);
  const [focusIdx, setFocusIdx] = useState(-1);
  const [dropUp, setDropUp] = useState(false);
  const [query, setQuery] = useState("");
  const [popStyle, setPopStyle] = useState<Pick<CSSProperties, "left" | "width">>({});
  const rootRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const listboxId = useId();

  const onChangeRef = useRef(onChange);
  useEffect(() => {
    onChangeRef.current = onChange;
  }, [onChange]);

  const wide = gridColumns === 2;
  const selectableCount = options.filter(o => !o.heading).length;
  const showSearch = searchThreshold != null && selectableCount >= searchThreshold;

  // 搜索过滤：按 label/value 包含匹配（大小写不敏感）。
  // 分组标题仅在其组内存在匹配项时保留，避免出现空分组。
  const visibleOptions = useMemo(() => {
    if (!query.trim()) return options;
    const q = query.trim().toLowerCase();
    const out: SelectOption[] = [];
    let pendingHeading: SelectOption | null = null;
    let groupHasMatch = false;
    for (const o of options) {
      if (o.heading) {
        if (pendingHeading && groupHasMatch) out.push(pendingHeading);
        pendingHeading = o;
        groupHasMatch = false;
      } else {
        const hit = o.label.toLowerCase().includes(q) || o.value.toLowerCase().includes(q);
        if (hit) {
          groupHasMatch = true;
          out.push(o);
        }
      }
    }
    if (pendingHeading && groupHasMatch) out.push(pendingHeading);
    return out;
  }, [options, query]);

  // 键盘导航跳过禁用项与分组标题（基于过滤后的可见列表）
  const enabledIndexes = visibleOptions
    .map((o, i) => (o.disabled || o.heading ? -1 : i))
    .filter(i => i >= 0);

  const current = options.find(o => o.value === value);

  // 点击组件外关闭
  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [open]);

  // 关闭时清空搜索词，下次打开从全量列表开始
  useEffect(() => {
    if (!open) setQuery("");
  }, [open]);

  // 打开时把焦点定位到当前选中项
  useEffect(() => {
    if (!open) return;
    const idx = visibleOptions.findIndex(o => o.value === value && !o.heading);
    setFocusIdx(idx >= 0 ? idx : enabledIndexes[0] ?? -1);
    // 仅在打开瞬间执行一次
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // 打开且启用搜索时，自动聚焦搜索框
  useEffect(() => {
    if (open && showSearch) searchRef.current?.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, showSearch]);

  // 打开时测量视口剩余空间：下方放不下弹层（约 288px 上限）则向上翻转展开
  useEffect(() => {
    if (!open || !rootRef.current) return;
    const rect = rootRef.current.getBoundingClientRect();
    const listHeight = Math.min(listRef.current?.scrollHeight ?? 0, 288);
    setDropUp(rect.bottom + listHeight + 12 > window.innerHeight && rect.top > listHeight + 12);
  }, [open]);

  // 宽弹层（双列）定位：目标宽度按视口收敛，右缘溢出时向左平移贴边
  useEffect(() => {
    if (!open || !wide || !rootRef.current) return;
    const rect = rootRef.current.getBoundingClientRect();
    const width = Math.min(Math.max(rect.width, wideListWidth), window.innerWidth - 16);
    let left = 0;
    if (rect.left + width > window.innerWidth - 8) {
      left = window.innerWidth - 8 - width - rect.left;
      if (rect.left + left < 8) left = 8 - rect.left;
    }
    setPopStyle({ width, left });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, wide, wideListWidth]);

  // 聚焦项滚动到可视区域
  useEffect(() => {
    if (!open || focusIdx < 0) return;
    listRef.current?.querySelector(`[data-idx="${focusIdx}"]`)?.scrollIntoView({ block: "nearest" });
  }, [open, focusIdx]);

  const moveFocus = (dir: 1 | -1) => {
    if (enabledIndexes.length === 0) return;
    const pos = enabledIndexes.indexOf(focusIdx);
    const next = pos < 0
      ? (dir === 1 ? enabledIndexes[0] : enabledIndexes[enabledIndexes.length - 1])
      : enabledIndexes[(pos + dir + enabledIndexes.length) % enabledIndexes.length];
    setFocusIdx(next);
  };

  const selectAt = (idx: number) => {
    const opt = visibleOptions[idx];
    if (!opt || opt.disabled) return;
    onChangeRef.current(opt.value);
    setOpen(false);
  };

  const close = () => setOpen(false);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (disabled) return;
    if (!open) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp" || e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        setOpen(true);
      }
      return;
    }
    if (e.key === "ArrowDown") { e.preventDefault(); moveFocus(1); }
    else if (e.key === "ArrowUp") { e.preventDefault(); moveFocus(-1); }
    else if (e.key === "Home") { e.preventDefault(); setFocusIdx(enabledIndexes[0] ?? -1); }
    else if (e.key === "End") { e.preventDefault(); setFocusIdx(enabledIndexes[enabledIndexes.length - 1] ?? -1); }
    else if (e.key === "Enter" || e.key === " ") { e.preventDefault(); selectAt(focusIdx); }
    else if (e.key === "Escape") { e.preventDefault(); close(); }
    else if (e.key === "Tab") { close(); }
  };

  const sizeCls = size === "sm"
    ? "rounded-lg px-3 py-2 text-sm"
    : "rounded-2xl px-4 py-3 text-sm";

  const popupPosCls = wide
    ? ""
    : fullWidthList
      ? "left-0 right-0"
      : "left-0 min-w-full";

  return (
    <div ref={rootRef} className={`relative ${className}`}>
      <button
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listboxId}
        aria-activedescendant={open && focusIdx >= 0 ? `${listboxId}-opt-${focusIdx}` : undefined}
        aria-label={ariaLabel}
        disabled={disabled}
        onClick={() => setOpen(o => !o)}
        onKeyDown={onKeyDown}
        className={`flex w-full items-center justify-between gap-2 border border-slate-200 bg-white text-left transition-all disabled:cursor-not-allowed disabled:opacity-50 ${sizeCls} ${
          open
            ? "border-blue-400 ring-2 ring-blue-100"
            : "hover:border-blue-400 hover:shadow-sm focus:border-blue-400 focus:outline-none focus:ring-2 focus:ring-blue-100"
        }`}
      >
        <span className={`min-w-0 flex-1 truncate ${current ? "" : "text-muted-foreground"}`}>
          {current ? current.label : placeholder}
        </span>
        <svg
          className={`h-4 w-4 shrink-0 text-muted-foreground transition-transform ${open ? "rotate-180" : ""}`}
          viewBox="0 0 20 20"
          fill="currentColor"
          aria-hidden="true"
        >
          <path d="M5.23 7.21a.75.75 0 011.06.02L10 11.06l3.71-3.83a.75.75 0 111.08 1.04l-4.25 4.39a.75.75 0 01-1.08 0L5.21 8.27a.75.75 0 01.02-1.06z" />
        </svg>
      </button>

      {open && (
        <div
          ref={listRef}
          id={listboxId}
          role="listbox"
          style={wide ? popStyle : undefined}
          className={`absolute z-50 max-h-80 overflow-y-auto rounded-xl border border-slate-200 bg-white p-1 shadow-[0_16px_40px_rgba(15,23,42,0.16)] ${
            dropUp ? "bottom-[calc(100%+6px)]" : "top-[calc(100%+6px)]"
          } ${popupPosCls}`}
        >
          {showSearch && (
            <div className="sticky top-0 z-10 border-b border-border bg-white p-2">
              <div className="relative">
                <Search size={14} className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-muted-foreground" />
                <input
                  ref={searchRef}
                  value={query}
                  onChange={e => { setQuery(e.target.value); setFocusIdx(-1); }}
                  onKeyDown={e => {
                    // 搜索框内拦截，避免与弹层键盘导航冲突
                    if (e.key === "Escape") { e.preventDefault(); close(); }
                    else if (e.key === "ArrowDown") { e.preventDefault(); moveFocus(1); }
                    else if (e.key === "ArrowUp") { e.preventDefault(); moveFocus(-1); }
                    else if (e.key === "Enter") { e.preventDefault(); selectAt(focusIdx); }
                  }}
                  placeholder="搜索模型..."
                  className="w-full rounded-lg border border-border bg-background/70 py-1.5 pl-8 pr-2 text-sm placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-2 focus:ring-primary/20"
                />
              </div>
            </div>
          )}

          {options.length === 0 && (
            <div className="px-3 py-2 text-sm text-muted-foreground">{placeholder}</div>
          )}
          {options.length > 0 && visibleOptions.length === 0 && (
            <div className="px-3 py-3 text-center text-sm text-muted-foreground">
              无匹配「{query.trim()}」的模型
            </div>
          )}
          <div className={wide ? "grid grid-cols-2 gap-x-1" : ""}>
            {visibleOptions.map((opt, i) => {
              if (opt.heading) {
                return (
                  <div
                    key={`${opt.value}-${i}`}
                    data-idx={i}
                    className={`px-3 pb-1 pt-2 text-[11px] font-bold tracking-wider text-muted-foreground ${wide ? "col-span-full" : ""}`}
                  >
                    {opt.label}
                  </div>
                );
              }
              const isCurrent = opt.value === value;
              const isFocused = i === focusIdx;
              return (
                <button
                  key={`${opt.value}-${i}`}
                  id={`${listboxId}-opt-${i}`}
                  data-idx={i}
                  type="button"
                  role="option"
                  aria-selected={isCurrent}
                  disabled={opt.disabled}
                  onClick={() => selectAt(i)}
                  onMouseEnter={() => !opt.disabled && setFocusIdx(i)}
                  title={opt.label}
                  className={`flex w-full items-center justify-between gap-2 rounded-lg px-3 py-2 text-left text-sm transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${
                    isFocused
                      ? "bg-slate-50 ring-1 ring-blue-200"
                      : isCurrent
                        ? "bg-blue-50"
                        : "hover:bg-slate-50"
                  }`}
                >
                  <span className={`min-w-0 flex-1 truncate ${isCurrent ? "font-semibold text-blue-600" : ""}`}>
                    {opt.label}
                  </span>
                  <span className="shrink-0 font-bold text-blue-600">{isCurrent ? "✓" : ""}</span>
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
