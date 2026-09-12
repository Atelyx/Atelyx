/**
 * 笔记属性区：胶囊行式融入笔记正文顶部，可视化展示 + 内联编辑。
 * `.md` 开头 YAML Frontmatter（渲染/实时预览编辑模式显示；源码模式由 textarea 显示 YAML 原文）。
 *
 * 纯受控展示组件，无持久化逻辑：data 由 NoteEditor 解析传入，onUpdate 提交新 data（NoteEditor 拼回完整
 * content 走既有 debounce 保存链路）。类型化渲染：
 * - tags/aliases/cssclasses → `#` 徽章；其余数组 → Clock 垂直列表；
 * - string → 键值对行；未知类型（数字/布尔/嵌套）→ 只读键值对行 String(value)（编辑会破坏 YAML 类型，不做）。
 *
 * 输入体验（槽位模型 + 连续编辑）：
 * - 每个属性 = 键槽 + 值槽（数组每项一槽）；Enter/Tab 提交并进入下一条可编辑槽位，Shift+Tab 回退，
 *   末槽后自动打开「添加属性」；Esc 取消；键名→值连贯编辑（键槽提交后直入本行值槽）。
 * - 数组项/徽章输入含中英文逗号自动拆分为多项提交（单值字段不拆）。
 * - 两段式添加：键名 Enter → 键名变胶囊、值输入自动聚焦 → 值 Enter 落盘；键名输入弹常用键名建议
 *   （COMMON_PROPERTY_KEYS 子串过滤、排除已有，↑↓ 选择）。
 * - tags 输入候选：全仓库标签词汇表（tagCandidates，NoteEditor 按需加载），子串过滤、排除已有、
 *   ↑↓ 选择、Enter 选中提交、Esc 先关建议再取消。
 */
import {
  Calendar,
  ChevronDown,
  ChevronRight,
  Clock,
  Hash,
  List,
  Plus,
  Tag,
  ToggleLeft,
  Type,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { PopupLayer, type PopupAnchor } from "@/components/common/PopupLayer";
import { COMMON_PROPERTY_KEYS } from "@/constants/notes";
import {
  convertPropertyValue,
  formatDate,
  inferPropertyType,
  isBadgeKey,
  type PropertyValueType,
} from "@/utils/frontmatter";

interface Props {
  /** 解析后的 frontmatter（宽类型：未知类型值只读展示，防序列化破坏类型）。 */
  data: Record<string, unknown>;
  /** YAML 格式错误：显示红条并禁用编辑，防 stringify 丢弃格式错误的旧数据。 */
  parseError: boolean;
  /** 提交新 data（NoteEditor 负责拼回完整 content）。 */
  onUpdate: (next: Record<string, unknown>) => void;
  /** 切到源码模式（格式错误时用户可查看/修复原始 YAML）。 */
  onOpenSource: () => void;
  /** 全仓库标签词汇表（tags 输入候选；空数组 = 暂无候选）。 */
  tagCandidates: string[];
  /** tags 输入首次打开时触发加载（NoteEditor 调 vaultStore.loadVaultTags）。 */
  onRequestTagCandidates?: () => void;
}

/** 编辑槽位：键槽（part=key）或值槽（part=value，数组项带 index）。 */
interface Slot {
  part: "key" | "value";
  key: string;
  index?: number;
}

const slotKey = (s: Slot) => `${s.part}:${s.key}:${s.index ?? ""}`;

/** 逗号（中英文）分隔拆数组；无逗号时原样单段。 */
const splitComma = (text: string): string[] =>
  text
    .split(/[,，]/)
    .map((t) => t.trim())
    .filter(Boolean);

/** tags/aliases/cssclasses 徽章胶囊样式：淡强调底色 + 强调色字体（与正文内联标签胶囊一致）。 */
const TAG_BADGE_STYLE = {
  background: "color-mix(in srgb, var(--accent) 14%, transparent)",
  color: "var(--accent)",
};

/** tags 候选展示上限：全仓库标签列表 + 滚动条（滚动可达全部；防超大仓库一次渲染过千条 DOM）。 */
const MAX_TAG_CANDIDATES = 500;

/** 属性类型菜单：顺序即展示顺序（类型 = 值转换目标，见 utils/frontmatter.ts convertPropertyValue）。 */
const PROPERTY_TYPES: PropertyValueType[] = ["text", "tag", "date", "number", "boolean", "list"];
const TYPE_LABELS: Record<PropertyValueType, string> = {
  text: "文本",
  tag: "标签",
  date: "日期",
  number: "数字",
  boolean: "布尔",
  list: "列表",
};
const TYPE_ICONS: Record<PropertyValueType, React.ReactNode> = {
  text: <Type size={13} />,
  tag: <Tag size={13} />,
  date: <Calendar size={13} />,
  number: <Hash size={13} />,
  boolean: <ToggleLeft size={13} />,
  list: <List size={13} />,
};

/** 建议下拉锚点：触发输入框可能是 flex-1 撑满表单行（宽数百 px），minWidth 不得跟随触发器宽度——
 *  菜单宽度上限由内容 max-w-72 截断（超长文本省略号 + hover title），minWidth 只保证不小于 96px。 */
const suggestAnchorFromRect = (r: DOMRect): PopupAnchor => ({
  x: r.left,
  y: r.bottom + 4,
  minWidth: Math.min(Math.max(r.width, 96), 288),
  flipY: r.top - 8,
});

/**
 * 内联编辑输入框：Enter/Tab 提交并前进（dir=1/-1，由 onSubmit 决策），Shift+Tab 回退，
 * Esc 取消，失焦提交（doneRef 防 Enter 提交后 blur 重复提交）。自动聚焦并全选。
 */
function ValueInput({
  initial,
  autoWidth,
  onSubmit,
  onCancel,
  onTextChange,
  onKeyDownBefore,
  onFocus,
  inputRef,
  initialWidth,
}: {
  initial: string;
  /** true = 按内容自适应宽度（徽章/列表项）；false = flex-1 撑满（单值行）。 */
  autoWidth?: boolean;
  /** 提交（dir 提供 = 提交并前进/后退；缺省 = 提交关闭编辑，用于失焦）。 */
  onSubmit: (text: string, dir?: 1 | -1) => void;
  onCancel: () => void;
  /** 文本变化上报（候选过滤用）。 */
  onTextChange?: (text: string) => void;
  /** 按键预处理（候选下拉导航等）：返回 true = 已处理，本组件不再走提交/取消默认逻辑。 */
  onKeyDownBefore?: (e: React.KeyboardEvent<HTMLInputElement>) => boolean;
  onFocus?: () => void;
  /** 外部拿到底层 input 元素（建议下拉锚点定位用）。 */
  inputRef?: React.RefObject<HTMLInputElement>;
  /** 固定初始宽度（px）：点击编辑前捕获显示元素宽度，防 chip→输入框宽度跳变推挤行内容。 */
  initialWidth?: number;
}) {
  const [text, setText] = useState(initial);
  const doneRef = useRef(false);
  const done = (fn: () => void) => {
    if (doneRef.current) return;
    doneRef.current = true;
    fn();
  };
  // 无边框透明、字号与正文一致（text-sm）：点击编辑不再出现「chip 字号 → 浏览器默认输入框」的跳变
  const widthStyle =
    initialWidth !== undefined
      ? { width: initialWidth }
      : autoWidth
        ? { width: `calc(${Math.max(text.length, 2)}ch + 1.25rem)` }
        : undefined;
  return (
    <input
      ref={inputRef}
      autoFocus
      spellCheck={false}
      value={text}
      onChange={(e) => {
        setText(e.target.value);
        onTextChange?.(e.target.value);
      }}
      onFocus={(e) => {
        e.target.select();
        onFocus?.();
      }}
      onBlur={() => done(() => onSubmit(text))}
      onKeyDown={(e) => {
        if (onKeyDownBefore?.(e)) return;
        if (e.key === "Enter") done(() => onSubmit(text, 1));
        else if (e.key === "Tab") {
          e.preventDefault();
          done(() => onSubmit(text, e.shiftKey ? -1 : 1));
        } else if (e.key === "Escape") done(onCancel);
      }}
      className={`bg-transparent outline-none border-none text-sm leading-none ${
        autoWidth ? "py-0.5 px-0" : "flex-1 min-w-0"
      }`}
      style={widthStyle}
    />
  );
}

/** 建议项列表（渲染在 PopupLayer 壳内）：高亮当前项，mousedown preventDefault 防输入框失焦先提交；
 *  外层滚动容器（max-h + overflow-y-auto）支持鼠标滚轮滚动；max-w 限宽——菜单 shrink-to-fit 无宽度钳制，
 *  放开全仓库候选后单个超长标签会把菜单撑出窗口，截断 + hover title 展示全名。 */
function SuggestionItems({
  items,
  index,
  onSelect,
  onHover,
}: {
  items: string[];
  index: number;
  onSelect: (value: string) => void;
  onHover: (index: number) => void;
}) {
  return (
    <div className="max-h-40 max-w-72 overflow-y-auto">
      {items.map((it, i) => (
        <button
          key={it}
          className="w-full text-left px-2 py-1 text-xs truncate"
          title={it}
          style={{
            background:
              i === index ? "color-mix(in srgb, var(--accent) 15%, transparent)" : "transparent",
            color: "var(--text-primary)",
          }}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => onSelect(it)}
          onMouseEnter={() => onHover(i)}
        >
          {it}
        </button>
      ))}
    </div>
  );
}

/**
 * 候选输入（属性名 / tags 值共用）：内联编辑 + 候选下拉。
 * 聚焦即展示候选（query 为空 = 显示候选池顶部推荐 N 条），键入后子串过滤、排除已有；
 * ↑↓ 选择、Enter 选中提交、Esc 先关下拉再取消；mousedown 防失焦先提交。
 */
function CandidateInput({
  initial,
  autoWidth,
  candidates,
  existing,
  onSubmit,
  onCancel,
  onRequest,
  initialWidth,
  maxItems = 8,
}: {
  initial: string;
  autoWidth?: boolean;
  /** 候选池（推荐序：tags 按出现次数降序、属性名按预置顺序）。 */
  candidates: string[];
  /** 当前已有项（候选排除）。 */
  existing: string[];
  onSubmit: (text: string, dir?: 1 | -1) => void;
  onCancel: () => void;
  /** 候选首次打开时触发加载（全仓库标签词汇）；属性名候选为静态常量无需传。 */
  onRequest?: () => void;
  /** 固定初始宽度（px；点击编辑前捕获显示元素宽度，防跳变）。 */
  initialWidth?: number;
  /** 候选展示上限（tags = 全仓库列表 + 滚动条；属性名 = 默认 8）。 */
  maxItems?: number;
}) {
  const [open, setOpen] = useState(false);
  const [idx, setIdx] = useState(0);
  /** 候选过滤查询：聚焦时为空（显示推荐候选顶部），键入后按输入过滤。 */
  const [query, setQuery] = useState("");
  /** 建议下拉锚点（PopupLayer portal：脱离折叠动画的 overflow-hidden 裁剪；打开时取输入框 rect）。 */
  const [anchor, setAnchor] = useState<PopupAnchor | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const requestedRef = useRef(false);
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const base = q ? candidates.filter((c) => c.toLowerCase().includes(q)) : candidates;
    return base.filter((c) => !existing.includes(c)).slice(0, maxItems);
  }, [query, candidates, existing, maxItems]);
  /** 过滤收窄后 idx 可能越界（高亮消失但 Enter 仍提交末项）——渲染/选择统一用钳制后的安全下标。 */
  const displayIdx = Math.min(idx, filtered.length - 1);
  const openDropdown = () => {
    // 未打开或锚点未建时重算锚点：autoFocus 的 onFocus 可能早于 ref 挂载（ref 为空 → 锚点缺失），
    // 挂载 effect / 键入再调本函数补建；已打开且锚点有效则不再强制读布局
    if (!open || !anchor) {
      const el = inputRef.current;
      if (el) {
        setAnchor(suggestAnchorFromRect(el.getBoundingClientRect()));
      }
      if (!open) {
        setOpen(true);
        setIdx(0);
      }
    }
  };
  const requestOnce = () => {
    if (requestedRef.current) return;
    requestedRef.current = true;
    onRequest?.();
  };
  /** 挂载即聚焦 + 打开下拉 + 触发候选加载：不依赖 autoFocus 的 onFocus 时序
   * （`+` 新增/点击编辑两种入口都保证点击后立即出现候选）。 */
  useEffect(() => {
    inputRef.current?.focus();
    openDropdown();
    requestOnce();
    // 仅挂载时执行一次（openDropdown/requestOnce 内部幂等，StrictMode 双调用无副作用）
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (filtered.length === 0) return false;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setIdx((i) => Math.min(i + 1, filtered.length - 1));
      return true;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      setIdx((i) => Math.max(i - 1, 0));
      return true;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      const pick = filtered[displayIdx];
      if (pick) onSubmit(pick, 1);
      setOpen(false);
      return true;
    }
    if (e.key === "Escape") {
      // 先关下拉，再按一次 Esc 才取消编辑（交还 ValueInput 默认处理）
      if (open) {
        setOpen(false);
        return true;
      }
      return false;
    }
    return false;
  };
  return (
    <>
      <ValueInput
        inputRef={inputRef}
        initial={initial}
        autoWidth={autoWidth}
        initialWidth={initialWidth}
        onSubmit={onSubmit}
        onCancel={onCancel}
        onTextChange={(t) => {
          setQuery(t);
          openDropdown();
          requestOnce();
        }}
        onFocus={() => {
          openDropdown();
          requestOnce();
        }}
        onKeyDownBefore={handleKeyDown}
      />
      {open && filtered.length > 0 && anchor && (
        <PopupLayer
          anchor={anchor}
          onClose={() => setOpen(false)}
          triggerRef={inputRef}
          widthClass=""
          contentClassName="py-0.5"
        >
          <SuggestionItems
            items={filtered}
            index={displayIdx}
            onHover={setIdx}
            onSelect={(v) => {
              onSubmit(v, 1);
              setOpen(false);
            }}
          />
        </PopupLayer>
      )}
    </>
  );
}

export function NotePropertiesView({
  data,
  parseError,
  onUpdate,
  onOpenSource,
  tagCandidates,
  onRequestTagCandidates,
}: Props) {
  /** 折叠状态（默认展开；点击标题栏切换，仅内存态不持久化）。 */
  const [collapsed, setCollapsed] = useState(false);
  /** 正在编辑的槽位（null = 未编辑）。 */
  const [editing, setEditing] = useState<Slot | null>(null);
  /** 数组行尾「+ 新增项」的 key（非空时显示输入框）。 */
  const [addingItem, setAddingItem] = useState<string | null>(null);
  /** 「添加属性」表单开关 + 两段式步骤（键名 → 值）。 */
  const [addOpen, setAddOpen] = useState(false);
  const [addStep, setAddStep] = useState<"key" | "value">("key");
  const [newKey, setNewKey] = useState("");
  const [newValue, setNewValue] = useState("");
  /** 添加属性时键名已存在的提示（存该键名；非空即展示，改键名/关闭表单后清除）。 */
  const [duplicateKey, setDuplicateKey] = useState<string | null>(null);
  /** 键名建议下拉状态（两段式 key 步）。 */
  const [keySuggestOpen, setKeySuggestOpen] = useState(false);
  const [keySuggestIdx, setKeySuggestIdx] = useState(0);
  /** 键名建议下拉锚点（PopupLayer portal，脱离折叠动画 overflow-hidden 裁剪）。 */
  const [keySuggestAnchor, setKeySuggestAnchor] = useState<PopupAnchor | null>(null);
  /** 两段式 value 步（键名 = tags）的候选下拉状态：聚焦即展示全仓库标签候选。 */
  const [tagSuggestOpen, setTagSuggestOpen] = useState(false);
  const [tagSuggestIdx, setTagSuggestIdx] = useState(0);
  const [tagSuggestAnchor, setTagSuggestAnchor] = useState<PopupAnchor | null>(null);
  const tagSuggestRequestedRef = useRef(false);
  /** 属性类型切换菜单：typeMenuKey = 正在切类型的属性键（null = 关闭）。 */
  const [typeMenuKey, setTypeMenuKey] = useState<string | null>(null);
  const [typeMenuAnchor, setTypeMenuAnchor] = useState<PopupAnchor | null>(null);
  const keyInputRef = useRef<HTMLInputElement>(null);
  const valueInputRef = useRef<HTMLInputElement>(null);

  const entries = Object.entries(data);
  /** 当前笔记已有 tags（候选排除）。 */
  const existingTags = useMemo(
    () =>
      Array.isArray(data.tags) ? data.tags.filter((t): t is string => typeof t === "string") : [],
    [data.tags],
  );

  /** 两段式表单聚焦：key 步聚焦键名，value 步聚焦值；聚焦后重算候选锚点——
   *  autoFocus 的 onFocus 可能早于 ref 挂载导致锚点未建，effect 在 layout 后补建（下拉才展开）。
   *  openKeySuggest/openTagSuggest 每次渲染重建但只读 ref/稳定回调，行为一致，无需入依赖。 */
  useEffect(() => {
    if (!addOpen) return;
    if (addStep === "key") {
      keyInputRef.current?.focus();
      openKeySuggest();
    } else {
      valueInputRef.current?.focus();
      if (newKey === "tags") openTagSuggest();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [addOpen, addStep]);

  /** 线性槽位序：每个属性 = 键槽 + 值槽（数组每项一槽）。 */
  const buildSlots = (list: [string, unknown][]): Slot[] => {
    const slots: Slot[] = [];
    for (const [key, value] of list) {
      slots.push({ part: "key", key });
      if (Array.isArray(value)) {
        for (let i = 0; i < value.length; i++) slots.push({ part: "value", key, index: i });
      } else {
        slots.push({ part: "value", key });
      }
    }
    return slots;
  };

  /** 槽位是否可编辑（键恒可；值仅 string——数字/布尔/嵌套只读防破坏 YAML 类型）。 */
  const isEditable = (slot: Slot): boolean => {
    if (slot.part === "key") return true;
    const raw = data[slot.key];
    if (Array.isArray(raw)) {
      return slot.index !== undefined && typeof raw[slot.index] === "string";
    }
    return typeof raw === "string";
  };

  /** 槽位当前文本（渲染时从最新 data 派生；非 string 值不进入编辑，防御返回空串）。 */
  const slotText = (slot: Slot): string => {
    if (slot.part === "key") return slot.key;
    const raw = data[slot.key];
    if (Array.isArray(raw) && slot.index !== undefined) {
      const item = raw[slot.index];
      return typeof item === "string" ? item : "";
    }
    return typeof raw === "string" ? raw : "";
  };

  const isEditing = (slot: Slot) =>
    !!editing &&
    editing.part === slot.part &&
    editing.key === slot.key &&
    editing.index === slot.index;

  /** 提交槽位编辑：返回槽位是否保留（空提交删除 = 槽消失 → 不前进）。 */
  const commitSlot = (slot: Slot, text: string): boolean => {
    if (slot.part === "key") {
      const k = text.trim();
      if (!k) {
        const next = { ...data };
        delete next[slot.key];
        onUpdate(next);
        return false;
      }
      if (k !== slot.key) {
        // 改名目标已存在：不覆盖原值（防数据丢失），保守退出编辑（候选建议已排除已有键，此路径仅手输可达）
        if (k in data) {
          setEditing(null);
          return false;
        }
        // 改名：值迁移 + 删旧键；entries 顺序重排槽位可能移动，保守退出编辑不前进
        const next = { ...data };
        next[k] = data[slot.key];
        delete next[slot.key];
        onUpdate(next);
        return false;
      }
      return true;
    }
    const raw = data[slot.key];
    if (Array.isArray(raw) && slot.index !== undefined) {
      if (text.trim() === "") {
        const next = raw.slice();
        next.splice(slot.index, 1);
        onUpdate({ ...data, [slot.key]: next });
        return false;
      }
      const parts = splitComma(text);
      const next = raw.slice();
      if (parts.length > 1) next.splice(slot.index, 1, ...parts);
      else next[slot.index] = text.trim();
      onUpdate({ ...data, [slot.key]: next });
      return true;
    }
    if (text.trim() === "") {
      const next = { ...data };
      delete next[slot.key];
      onUpdate(next);
      return false;
    }
    onUpdate({ ...data, [slot.key]: text });
    return true;
  };

  /** 从某槽位前进/后退到下一个可编辑槽位；越界：前进 = 打开添加属性，后退 = 关闭编辑。 */
  const advanceFrom = (slot: Slot, dir: 1 | -1) => {
    const slots = buildSlots(entries);
    const idx = slots.findIndex((s) => slotKey(s) === slotKey(slot));
    let i = idx + dir;
    while (i >= 0 && i < slots.length && !isEditable(slots[i])) i += dir;
    if (i >= 0 && i < slots.length) {
      setAddingItem(null);
      setAddOpen(false);
      setEditing(slots[i]);
    } else if (dir === 1) {
      setEditing(null);
      openAddField();
    } else {
      setEditing(null);
    }
  };

  /** 提交 + 前进（Enter/Tab/候选选中共用）：空提交删除的槽位不前进。 */
  const submitSlot = (slot: Slot, text: string, dir?: 1 | -1) => {
    const survived = commitSlot(slot, text);
    if (dir === undefined || !survived) {
      setEditing(null);
      return;
    }
    advanceFrom(slot, dir);
  };

  const openAddField = () => {
    setEditing(null);
    setAddingItem(null);
    setNewKey("");
    setNewValue("");
    setDuplicateKey(null);
    setAddStep("key");
    setKeySuggestOpen(false);
    setTagSuggestOpen(false);
    setAddOpen(true);
  };

  const closeAddField = () => {
    setAddOpen(false);
    setDuplicateKey(null);
    setKeySuggestOpen(false);
    setTagSuggestOpen(false);
  };

  /** 常用键名建议（聚焦即展示推荐顶部，键入后子串过滤、排除已有）。 */
  const keySuggestions = useMemo(() => {
    const q = newKey.trim().toLowerCase();
    const base = q ? COMMON_PROPERTY_KEYS.filter((k) => k.toLowerCase().includes(q)) : COMMON_PROPERTY_KEYS;
    return base.filter((k) => !(k in data)).slice(0, 8);
  }, [newKey, data]);

  /** 打开键名建议下拉：按键名输入框当前 rect 定位（PopupLayer portal）。 */
  const openKeySuggest = () => {
    const el = keyInputRef.current;
    if (el) {
      setKeySuggestAnchor(suggestAnchorFromRect(el.getBoundingClientRect()));
    }
    setKeySuggestOpen(true);
    setKeySuggestIdx(0);
  };

  /** 两段式 key 步按键：↑↓ 选建议，Enter 提交（有建议 = 选高亮项，无 = 手输键名）进值步，Esc 关建议/取消。 */
  const handleKeyInputKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (keySuggestions.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setKeySuggestIdx((i) => Math.min(i + 1, keySuggestions.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setKeySuggestIdx((i) => Math.max(i - 1, 0));
        return;
      }
    }
    if (e.key === "Enter") {
      const k =
        keySuggestions.length > 0
          ? keySuggestions[Math.min(keySuggestIdx, keySuggestions.length - 1)]
          : newKey.trim();
      if (!k) return;
      setNewKey(k);
      setDuplicateKey(null);
      setKeySuggestOpen(false);
      setAddStep("value");
      return;
    }
    if (e.key === "Escape") {
      if (keySuggestOpen) setKeySuggestOpen(false);
      else closeAddField();
    }
  };

  /** 两段式 value 步（键名 = tags）的候选：聚焦即展示全仓库标签（滚动可达），键入后子串过滤、排除已有。 */
  const tagValueSuggestions = useMemo(() => {
    const q = newValue.trim().toLowerCase();
    const base = q ? tagCandidates.filter((c) => c.toLowerCase().includes(q)) : tagCandidates;
    return base.filter((c) => !existingTags.includes(c)).slice(0, MAX_TAG_CANDIDATES);
  }, [newValue, tagCandidates, existingTags]);

  /** 打开 value 步 tags 候选下拉：按值输入框 rect 定位 + 首次触发词汇表加载。 */
  const openTagSuggest = () => {
    const el = valueInputRef.current;
    if (el) {
      setTagSuggestAnchor(suggestAnchorFromRect(el.getBoundingClientRect()));
    }
    setTagSuggestOpen(true);
    setTagSuggestIdx(0);
    if (!tagSuggestRequestedRef.current) {
      tagSuggestRequestedRef.current = true;
      onRequestTagCandidates?.();
    }
  };

  /** 两段式 value 步按键：tags 值允许「只有一个就结束」——Enter 恒提交当前值（候选只影响展示/点击追加），
   *  非 tags 键 Enter 同样提交；↑↓ 仅在有候选时导航，Esc 先关候选再关表单。 */
  const handleValueInputKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    const isTags = newKey === "tags";
    if (isTags && tagValueSuggestions.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setTagSuggestIdx((i) => Math.min(i + 1, tagValueSuggestions.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setTagSuggestIdx((i) => Math.max(i - 1, 0));
        return;
      }
      if (e.key === "Escape" && tagSuggestOpen) {
        setTagSuggestOpen(false);
        return;
      }
    }
    if (e.key === "Enter") addField();
    else if (e.key === "Escape") closeAddField();
  };

  /** value 步失焦：「点击其他位置」= 结束输入——值非空提交（所有键一致，不丢输入），空则取消表单。
   *  重名时不退回键名步（`rollbackOnDuplicate: false`）——退回会改变 `addStep`，聚焦 effect 随之重跑
   *  把焦点抢回键名输入框，用户「点外部关表单」的意图被否决。 */
  const handleValueBlur = () => {
    if (newValue.trim() !== "") addField({ rollbackOnDuplicate: false });
    else closeAddField();
  };

  /** 添加新属性：Value 含逗号（中英文）自动转数组；空值 = 取消表单（不产生 `key: ""` 垃圾属性）。
   *  徽章类键（tags/aliases/cssclasses）值恒为数组——单个标签也存数组（其余键单值存字符串）。
   *  键已存在时：徽章键追加进数组（如给已有 tags 补标签）；其余键不覆盖原值（防数据丢失），
   *  默认退回键名步并提示「该属性已存在」——值步的键名胶囊不可改，退回才能改键名；newValue 保留，
   *  改名后进值步随即带出，不静默丢弃刚输入的值。 */
  const addField = (opts?: { rollbackOnDuplicate?: boolean }) => {
    const key = newKey.trim();
    if (!key) return;
    if (newValue.trim() === "") {
      closeAddField();
      return;
    }
    const parts = splitComma(newValue);
    if (key in data) {
      if (isBadgeKey(key) && Array.isArray(data[key])) {
        onUpdate({ ...data, [key]: [...(data[key] as unknown[]), ...parts] });
        closeAddField();
        return;
      }
      if (opts?.rollbackOnDuplicate === false) {
        closeAddField();
        return;
      }
      setDuplicateKey(key);
      setAddStep("key");
      return;
    }
    onUpdate({ ...data, [key]: parts.length > 1 || isBadgeKey(key) ? parts : newValue.trim() });
    closeAddField();
  };

  /** 打开属性类型菜单：按图标按钮 rect 定位（PopupLayer portal）。 */
  const openTypeMenu = (key: string, e: React.MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    setTypeMenuAnchor({ x: r.left, y: r.bottom + 4, minWidth: Math.max(r.width, 96), flipY: r.top - 8 });
    setTypeMenuKey(key);
  };

  const closeTypeMenu = () => {
    setTypeMenuKey(null);
    setTypeMenuAnchor(null);
  };

  /** 应用类型切换：值转换为目标 YAML 类型落盘；不可转换保留原值（不破坏数据）。
   *  预置徽章键（tags/aliases/cssclasses）类型固定为标签，不可切其他类型。 */
  const applyType = (key: string, type: PropertyValueType) => {
    if (isBadgeKey(key) && type !== "tag") return;
    const converted = convertPropertyValue(data[key], type);
    if (converted !== null) onUpdate({ ...data, [key]: converted });
    closeTypeMenu();
  };

  const removeArrayItem = (key: string, index: number) => {
    const raw = data[key];
    if (!Array.isArray(raw)) return;
    const next = raw.slice();
    next.splice(index, 1);
    onUpdate({ ...data, [key]: next });
  };

  /** 数组新增项（逗号分隔拆多项）。 */
  const addArrayItem = (key: string, text: string) => {
    const base = Array.isArray(data[key]) ? (data[key] as unknown[]).slice() : [];
    onUpdate({ ...data, [key]: [...base, ...splitComma(text)] });
    setAddingItem(null);
  };

  /** 新增项提交：非空 = 逗号分隔加项并关闭；空 = 直接关闭（空回车/点击别处失焦退出新增态，不留待输入）。 */
  const submitAddItem = (key: string, text: string) => {
    if (text.trim() !== "") addArrayItem(key, text);
    else setAddingItem(null);
  };

  const removeField = (key: string) => {
    const next = { ...data };
    delete next[key];
    onUpdate(next);
  };

  /** 键名 chip 点击时的原始宽度（编辑输入框沿用，防 chip→输入框宽度跳变）。 */
  const keyChipWidthRef = useRef<number | undefined>(undefined);

  /** 属性行键名（无底纹文字，hover 才高亮；w-24 固定宽 = 类型图标 + 键名，所有属性值起点对齐）：
   *  键名前类型图标（类型推断自值；预置徽章键固定为标签）点击切换类型；点击键名改名。 */
  const renderKeyCapsule = (key: string) => {
    const slot: Slot = { part: "key", key };
    const type = isBadgeKey(key) ? "tag" : inferPropertyType(key, data[key]);
    return (
      <div className="w-24 flex-shrink-0 flex items-center gap-0.5 min-w-0">
        <button
          className="flex-shrink-0 p-0.5 rounded hover:bg-[var(--hover)] hover:opacity-90 transition-colors cursor-pointer"
          style={{ color: "var(--text-muted)" }}
          onClick={(e) => openTypeMenu(key, e)}
          title={`类型：${TYPE_LABELS[type]}（点击切换）`}
        >
          {TYPE_ICONS[type]}
        </button>
        {isEditing(slot) ? (
          <CandidateInput
            key={slotKey(slot)}
            initial={key}
            autoWidth
            initialWidth={keyChipWidthRef.current}
            candidates={COMMON_PROPERTY_KEYS}
            existing={Object.keys(data)}
            onSubmit={(t, dir) => submitSlot(slot, t, dir)}
            onCancel={() => setEditing(null)}
          />
        ) : (
          <button
            className="flex-1 min-w-0 truncate text-left text-sm leading-none py-0.5 px-1 rounded-sm hover:bg-[var(--hover)] hover:opacity-90 cursor-pointer transition-colors"
            style={{ color: "var(--text-muted)" }}
            onClick={(e) => {
              keyChipWidthRef.current = e.currentTarget.offsetWidth;
              setAddingItem(null);
              setAddOpen(false);
              setEditing(slot);
            }}
            title={key}
          >
            {key}
          </button>
        )}
      </div>
    );
  };

  /** 徽章（tags/aliases/cssclasses）：`#` 前缀胶囊，点击编辑、hover 删除、行尾 + 新增；tags 输入带全仓库候选。 */
  const renderBadges = (key: string, arr: unknown[]) => (
    <div className="flex-1 flex flex-wrap items-center gap-1 min-w-0">
      {arr.map((item, i) =>
        typeof item !== "string" ? (
          // 非字符串项只读展示，但可删除（删数组元素不破坏 YAML 类型）
          <span
            key={i}
            className="group px-1.5 py-0.5 rounded-full text-xs"
            style={TAG_BADGE_STYLE}
          >
            #{String(item)}
            <button
              className="ml-1 align-middle opacity-0 group-hover:opacity-100 hover:text-red-400"
              onClick={(e) => {
                e.stopPropagation();
                removeArrayItem(key, i);
              }}
              title="删除"
            >
              <X size={10} />
            </button>
          </span>
        ) : isEditing({ part: "value", key, index: i }) ? (
          key === "tags" ? (
            <CandidateInput
              key={`badge:${key}:${i}`}
              initial={item}
              autoWidth
              maxItems={MAX_TAG_CANDIDATES}
              candidates={tagCandidates}
              existing={existingTags}
              onRequest={onRequestTagCandidates ?? (() => {})}
              onSubmit={(t, dir) => submitSlot({ part: "value", key, index: i }, t, dir)}
              onCancel={() => setEditing(null)}
            />
          ) : (
            <ValueInput
              key={`badge:${key}:${i}`}
              initial={item}
              autoWidth
              onSubmit={(t, dir) => submitSlot({ part: "value", key, index: i }, t, dir)}
              onCancel={() => setEditing(null)}
            />
          )
        ) : (
          <span
            key={i}
            className="group px-1.5 py-0.5 rounded-full text-xs cursor-pointer hover:opacity-90"
            style={TAG_BADGE_STYLE}
            onClick={() => {
              setAddingItem(null);
              setAddOpen(false);
              setEditing({ part: "value", key, index: i });
            }}
            title="点击编辑"
          >
            <span className="opacity-60 mr-0.5">#</span>
            {item}
            <button
              className="ml-1 align-middle opacity-0 group-hover:opacity-100 hover:text-red-400"
              onClick={(e) => {
                e.stopPropagation();
                removeArrayItem(key, i);
              }}
              title="删除"
            >
              <X size={10} />
            </button>
          </span>
        )
      )}
      {addingItem === key ? (
        key === "tags" ? (
          <CandidateInput
            key={`add:${key}`}
            initial=""
            autoWidth
            maxItems={MAX_TAG_CANDIDATES}
            candidates={tagCandidates}
            existing={existingTags}
            onRequest={onRequestTagCandidates ?? (() => {})}
            onSubmit={(t) => submitAddItem(key, t)}
            onCancel={() => setAddingItem(null)}
          />
        ) : (
          <ValueInput
            key={`add:${key}`}
            initial=""
            autoWidth
            onSubmit={(t) => submitAddItem(key, t)}
            onCancel={() => setAddingItem(null)}
          />
        )
      ) : (
        <button
          className="w-4 h-4 rounded-full flex items-center justify-center hover:opacity-80 flex-shrink-0"
          style={{ color: "var(--text-muted)" }}
          onClick={() => {
            setEditing(null);
            setAddOpen(false);
            setAddingItem(key);
          }}
          title="添加"
        >
          <Plus size={11} />
        </button>
      )}
    </div>
  );

  /** 列表（其余数组，如 timeline）：Clock 图标垂直列表，点击编辑、hover 删除、底部 + 新增。 */
  const renderList = (key: string, arr: unknown[]) => (
    <div className="flex-1 flex flex-col gap-0.5 min-w-0">
      {arr.map((item, i) =>
        isEditing({ part: "value", key, index: i }) ? (
          <ValueInput
            key={`list:${key}:${i}`}
            initial={typeof item === "string" ? item : ""}
            autoWidth
            onSubmit={(t, dir) => submitSlot({ part: "value", key, index: i }, t, dir)}
            onCancel={() => setEditing(null)}
          />
        ) : (
          <div
            key={i}
            className="group flex items-center gap-1.5 text-sm cursor-pointer"
            onClick={() => {
              if (typeof item !== "string") return;
              setAddingItem(null);
              setAddOpen(false);
              setEditing({ part: "value", key, index: i });
            }}
          >
            <Clock size={12} style={{ color: "var(--text-muted)" }} className="flex-shrink-0" />
            <span className="truncate">{String(item)}</span>
            {/* 删除钮恒显示（非字符串项也可删，删数组元素不破坏 YAML 类型） */}
            <button
              className="ml-auto opacity-0 group-hover:opacity-100 hover:text-red-400 flex-shrink-0"
              onClick={(e) => {
                e.stopPropagation();
                removeArrayItem(key, i);
              }}
              title="删除"
            >
              <X size={12} />
            </button>
          </div>
        )
      )}
      {addingItem === key ? (
        <ValueInput
          initial=""
          autoWidth
          onSubmit={(t) => submitAddItem(key, t)}
          onCancel={() => setAddingItem(null)}
        />
      ) : (
        <button
          className="flex items-center gap-0.5 text-xs hover:opacity-80"
          style={{ color: "var(--text-muted)" }}
          onClick={() => {
            setEditing(null);
            setAddOpen(false);
            setAddingItem(key);
          }}
        >
          <Plus size={11} /> 添加
        </button>
      )}
    </div>
  );

  const hasProps = entries.length > 0;

  return (
    <div
      className="flex-shrink-0 px-4 py-2 select-none text-sm"
      style={{ borderBottom: "1px solid var(--border)" }}
    >
      {/* 标题栏：点击整行折叠/展开（Chevron 指示），默认展开；无属性时不显示标题 */}
      {hasProps && (
        <button
          className="w-full flex items-center gap-1 text-xs mb-1.5 rounded hover:opacity-80 cursor-pointer"
          style={{ color: "var(--text-muted)" }}
          onClick={() => setCollapsed((v) => !v)}
          title={collapsed ? "展开笔记属性" : "折叠笔记属性"}
        >
          {collapsed ? <ChevronRight size={13} /> : <ChevronDown size={13} />}
          <span>笔记属性（{entries.length}）</span>
        </button>
      )}

      {/* 内容区：grid-rows 过渡动画（0fr↔1fr，WebView2 支持；折叠时状态保留） */}
      <div
        className={hasProps ? "grid transition-[grid-template-rows] duration-200" : undefined}
        style={hasProps ? { gridTemplateRows: collapsed ? "0fr" : "1fr" } : undefined}
      >
        <div className={hasProps ? "overflow-hidden" : undefined}>
          {parseError ? (
            <div className="text-xs py-1 flex items-center gap-2" style={{ color: "#f87171" }}>
              <span>YAML 格式错误，请检查（属性面板暂不可编辑）</span>
              <button
                className="px-1.5 py-0.5 rounded border hover:opacity-80 flex-shrink-0"
                style={{ borderColor: "#f87171" }}
                onClick={onOpenSource}
                title="切换到源码模式查看并修复 YAML"
              >
                打开源码模式
              </button>
            </div>
          ) : (
            <>
              {hasProps && (
                <div className="flex flex-col gap-1">
                  {entries.map(([key, value]) => (
                    <div key={key} className="flex items-center gap-2 group">
                      {renderKeyCapsule(key)}
                      {Array.isArray(value) ? (
                        isBadgeKey(key) ? (
                          renderBadges(key, value)
                        ) : (
                          renderList(key, value)
                        )
                      ) : (
                        <div className="flex-1 flex items-center gap-1 min-w-0">
                          {isEditing({ part: "value", key }) ? (
                            <ValueInput
                              key={`v:${key}`}
                              initial={slotText({ part: "value", key })}
                              onSubmit={(t, dir) => submitSlot({ part: "value", key }, t, dir)}
                              onCancel={() => setEditing(null)}
                            />
                          ) : typeof value === "string" ? (
                            <button
                              className="flex-1 min-w-0 text-left text-sm truncate hover:opacity-80 cursor-pointer"
                              onClick={() => {
                                setAddingItem(null);
                                setAddOpen(false);
                                setEditing({ part: "value", key });
                              }}
                              title="点击编辑"
                            >
                              {value}
                            </button>
                          ) : (
                            <span
                              className="flex-1 min-w-0 text-sm truncate"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              {value === null || value === undefined
                                ? ""
                                : value instanceof Date
                                  ? formatDate(value)
                                  : String(value)}
                            </span>
                          )}
                          <button
                            className="opacity-0 group-hover:opacity-100 hover:text-red-400 flex-shrink-0"
                            onClick={() => removeField(key)}
                            title="删除属性"
                          >
                            <X size={12} />
                          </button>
                        </div>
                      )}
                    </div>
                  ))}
                </div>
              )}

              {/* 底部：添加属性（parseError 时禁用，防 stringify 覆盖格式错误的旧数据）——
                  两段式：键名 Enter → 键名变胶囊、值输入自动聚焦 → 值 Enter 落盘；
                  键已存在（非徽章键）时退回键名步并提示，输入值保留待改名后带出 */}
              {addOpen ? (
                <div className="mt-1.5 flex flex-col gap-1">
                  <div className="flex items-center gap-1.5">
                    {addStep === "key" ? (
                      <span className="flex items-center gap-1.5 flex-1 min-w-0">
                        <input
                          ref={keyInputRef}
                          autoFocus
                          spellCheck={false}
                          placeholder="属性名"
                          value={newKey}
                          onChange={(e) => {
                            setNewKey(e.target.value);
                            setDuplicateKey(null);
                            openKeySuggest();
                          }}
                          onFocus={openKeySuggest}
                          onBlur={closeAddField}
                          onKeyDown={handleKeyInputKeyDown}
                          className="flex-1 min-w-0 bg-transparent text-sm outline-none"
                          style={{ borderBottom: "1px dashed var(--border)" }}
                        />
                        {keySuggestOpen && keySuggestions.length > 0 && keySuggestAnchor && (
                          <PopupLayer
                            anchor={keySuggestAnchor}
                            onClose={() => setKeySuggestOpen(false)}
                            triggerRef={keyInputRef}
                            widthClass=""
                            contentClassName="py-0.5"
                          >
                            <SuggestionItems
                              items={keySuggestions}
                              index={Math.min(keySuggestIdx, keySuggestions.length - 1)}
                              onHover={setKeySuggestIdx}
                              onSelect={(k) => {
                                setNewKey(k);
                                setDuplicateKey(null);
                                setKeySuggestOpen(false);
                                setAddStep("value");
                              }}
                            />
                          </PopupLayer>
                        )}
                      </span>
                    ) : (
                      <span className="flex items-center gap-1.5 flex-1 min-w-0">
                        <span
                          className="w-20 flex-shrink-0 truncate px-1 py-0.5 rounded-sm text-sm leading-none"
                          style={{ color: "var(--text-muted)" }}
                          title={newKey}
                        >
                          {newKey}
                        </span>
                        <input
                          ref={valueInputRef}
                          autoFocus
                          spellCheck={false}
                          placeholder="属性值（逗号分隔为数组）"
                          value={newValue}
                          onChange={(e) => {
                            setNewValue(e.target.value);
                            if (newKey === "tags") openTagSuggest();
                          }}
                          onFocus={() => {
                            if (newKey === "tags") openTagSuggest();
                          }}
                          onBlur={handleValueBlur}
                          onKeyDown={handleValueInputKeyDown}
                          className="flex-1 min-w-0 bg-transparent text-sm outline-none"
                          style={{ borderBottom: "1px dashed var(--border)" }}
                        />
                        {newKey === "tags" &&
                          tagSuggestOpen &&
                          tagValueSuggestions.length > 0 &&
                          tagSuggestAnchor && (
                            <PopupLayer
                              anchor={tagSuggestAnchor}
                              onClose={() => setTagSuggestOpen(false)}
                              triggerRef={valueInputRef}
                              widthClass=""
                              contentClassName="py-0.5"
                            >
                              <SuggestionItems
                                items={tagValueSuggestions}
                                index={Math.min(tagSuggestIdx, tagValueSuggestions.length - 1)}
                                onHover={setTagSuggestIdx}
                                onSelect={(v) => {
                                  setNewValue((prev) => (prev ? `${prev}，${v}` : v));
                                  setTagSuggestOpen(false);
                                }}
                              />
                            </PopupLayer>
                          )}
                      </span>
                    )}
                  </div>
                  {duplicateKey !== null && (
                    <div className="text-xs flex items-center gap-2" style={{ color: "#f87171" }}>
                      <span className="truncate">属性「{duplicateKey}」已存在，请改键名或编辑该属性</span>
                    </div>
                  )}
                </div>
              ) : (
                <button
                  className="mt-1.5 flex items-center gap-1 text-xs rounded px-1.5 py-0.5 hover:opacity-80"
                  style={{ color: "var(--text-muted)" }}
                  onClick={openAddField}
                  title="添加属性"
                >
                  <Plus size={12} /> 添加属性
                </button>
              )}
            </>
          )}
        </div>
      </div>

      {/* 属性类型切换菜单（键名前图标触发；PopupLayer portal）：选择 = 值转换为目标 YAML 类型落盘。
          预置徽章键（tags/aliases/cssclasses）类型固定为标签——其余类型低亮不可选中。 */}
      {typeMenuKey !== null && typeMenuAnchor && (
        <PopupLayer anchor={typeMenuAnchor} onClose={closeTypeMenu} widthClass="" contentClassName="py-0.5">
          {(() => {
            const typeLocked = isBadgeKey(typeMenuKey);
            const currentType = typeLocked ? "tag" : inferPropertyType(typeMenuKey, data[typeMenuKey]);
            return PROPERTY_TYPES.map((t) => {
              const disabled = typeLocked && t !== currentType;
              return (
                <button
                  key={t}
                  disabled={disabled}
                  className={`w-full flex items-center gap-2 px-2 py-1 text-xs text-left ${
                    disabled ? "opacity-40 cursor-default" : ""
                  }`}
                  style={{
                    background:
                      t === currentType
                        ? "color-mix(in srgb, var(--accent) 15%, transparent)"
                        : "transparent",
                    color: "var(--text-primary)",
                  }}
                  onClick={() => applyType(typeMenuKey, t)}
                  title={disabled ? "预置属性键类型固定" : TYPE_LABELS[t]}
                >
                  {TYPE_ICONS[t]}
                  <span className="truncate">{TYPE_LABELS[t]}</span>
                </button>
              );
            });
          })()}
        </PopupLayer>
      )}
    </div>
  );
}
