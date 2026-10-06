/**
 * 两级模型选择菜单：触发器显示「模型名 · 推理等级」，点击弹出根菜单——两行「模型 ›」「推理等级 ›」
 * 各自进入独立子面板，可分别设置模型或推理等级。
 *
 * 复用 `PopupLayer` + `usePopupAnchor` 统一弹层机制；模型与推理等级为正交覆盖，见 onSelectEffort。
 */
import { Check, ChevronDown, ChevronLeft, ChevronRight } from "lucide-react";
import { useRef, useState, type CSSProperties, type ReactNode } from "react";
import { PopupLayer } from "@/components/common/PopupLayer";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";
import { REASONING_EFFORT_OPTIONS, reasoningEffortLabel } from "@/constants/ai";
import { modelDisplayLabel } from "@/utils/text";
import type { ProviderConfig, ReasoningEffort } from "@/types";

type Pane = "root" | "model" | "effort";

interface ModelSelectProps {
  /** 全部供应商（模型列表分组来源）。 */
  providers: ProviderConfig[];
  /** 当前模型覆盖的供应商 id；缺省 = 跟随仓库默认。 */
  providerId?: string;
  /** 当前模型覆盖的模型 id；缺省 = 跟随仓库默认。 */
  model?: string;
  /** 当前推理等级覆盖；undefined = 默认（不指定）。 */
  effort?: ReasoningEffort;
  /** 选择模型（null = 跟随仓库默认）。 */
  onSelectModel: (sel: { providerId: string; model: string } | null) => void;
  /** 选择推理等级（undefined = 默认/不指定）。与模型正交：跟随仓库默认模型时也可单独设置。 */
  onSelectEffort: (effort: ReasoningEffort | undefined) => void;
  /** 跟随仓库默认时触发器/菜单显示的生效模型名（缺省 null → 显示「模型」）。 */
  defaultModelDisplay?: string | null;
  /** 触发按钮 label 前的图标（面板工具条图标按钮用）。 */
  prefixIcon?: ReactNode;
  /** 触发按钮样式（尺寸/颜色/边框等），与 DropdownSelect 同约定。 */
  className?: string;
  style?: CSSProperties;
  title?: string;
}

/** 菜单项通用行（根菜单 / 子面板返回行）。 */
function Row({
  onClick,
  children,
  value,
  chevron,
  back,
}: {
  onClick: () => void;
  children: ReactNode;
  value?: string;
  chevron?: boolean;
  back?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="w-full text-left px-3 py-1.5 text-ui hover:bg-[var(--hover)] inline-flex items-center justify-between gap-2"
      style={{ color: "var(--text-primary)" }}
    >
      <span className="inline-flex items-center gap-1.5 min-w-0">
        {back && <ChevronLeft size={13} className="flex-shrink-0" />}
        <span className="truncate">{children}</span>
      </span>
      {value != null && (
        <span className="truncate text-caption" style={{ color: "var(--text-muted)" }}>
          {value}
        </span>
      )}
      {chevron && <ChevronRight size={13} className="flex-shrink-0" style={{ color: "var(--text-muted)" }} />}
    </button>
  );
}

/** 单选选项行（模型 / 推理等级 / 跟随默认）。 */
function Option({
  selected,
  onClick,
  children,
  sub,
}: {
  selected: boolean;
  onClick: () => void;
  children: ReactNode;
  sub?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="w-full text-left px-3 py-1.5 text-ui hover:bg-[var(--hover)] inline-flex items-center justify-between gap-2"
      style={{ color: selected ? "var(--accent)" : "var(--text-primary)" }}
    >
      <span className="min-w-0 truncate">
        {children}
        {sub && (
          <span className="ml-1 text-micro truncate" style={{ color: "var(--text-muted)" }}>
            {sub}
          </span>
        )}
      </span>
      {selected && <Check size={13} className="flex-shrink-0" />}
    </button>
  );
}

export function ModelSelect({
  providers,
  providerId,
  model,
  effort,
  onSelectModel,
  onSelectEffort,
  defaultModelDisplay,
  prefixIcon,
  className,
  style,
  title,
}: ModelSelectProps) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const { anchor, toggle, close } = usePopupAnchor(triggerRef);
  const [pane, setPane] = useState<Pane>("root");

  const hasOverride = !!(providerId && model);
  const overrideProvider = providers.find((p) => p.id === providerId);
  // 触发器：override 态按真实供应商作用域显示（同名模型跨供应商时带「供应商名 · 」前缀消歧；
  // 供应商已删回退裸 ID）；跟随默认用调用方传入的默认生效模型显示
  const modelLabel = hasOverride
    ? overrideProvider
      ? modelDisplayLabel(providers, overrideProvider, model!)
      : model!
    : defaultModelDisplay || "模型";
  const effortValue = effort ?? "";

  const open = () => {
    setPane("root");
    toggle();
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={open}
        title={title}
        aria-haspopup="menu"
        aria-expanded={!!anchor}
        className={`flex items-center gap-1 min-w-0 cursor-pointer outline-none focus:ring-1 focus:ring-[var(--accent)] ${className ?? ""}`}
        style={style}
      >
        {prefixIcon}
        <span className="flex-1 min-w-0 truncate text-left">
          {modelLabel}
          {/* 恒显示档位（跟随默认 = 「· 默认」），让当前生效档位始终可见 */}
          <span className="ml-1" style={{ color: "var(--text-muted)" }}>
            · {reasoningEffortLabel(effort)}
          </span>
        </span>
        <ChevronDown size={12} className="flex-shrink-0" />
      </button>

      <PopupLayer
        anchor={anchor}
        onClose={close}
        triggerRef={triggerRef}
        zClass="z-[1100]"
        widthClass="w-48"
        repositionDeps={[pane]}
      >
        <div className="max-h-72 overflow-y-auto">
          {pane === "root" && (
            <>
              <Row onClick={() => setPane("model")} value={modelLabel} chevron>
                模型
              </Row>
              <Row onClick={() => setPane("effort")} value={reasoningEffortLabel(effort)} chevron>
                推理等级
              </Row>
            </>
          )}

          {pane === "model" && (
            <>
              <Row onClick={() => setPane("root")} back>
                返回
              </Row>
              <Option
                selected={!hasOverride}
                onClick={() => {
                  onSelectModel(null);
                  close();
                }}
                sub={defaultModelDisplay ?? undefined}
              >
                跟随仓库默认
              </Option>
              {providers.map((p) =>
                p.models.length === 0 ? null : (
                  <div key={p.id}>
                    <div
                      className="px-3 pt-1.5 pb-0.5 text-micro select-none"
                      style={{ color: "var(--text-muted)" }}
                    >
                      {p.name}
                    </div>
                    {p.models.map((m) => {
                      const selected = hasOverride && providerId === p.id && model === m.id;
                      return (
                        <Option
                          key={m.id}
                          selected={selected}
                          onClick={() => {
                            onSelectModel({ providerId: p.id, model: m.id });
                            close();
                          }}
                        >
                          {m.nickname ?? m.id}
                        </Option>
                      );
                    })}
                  </div>
                ),
              )}
              {providers.length === 0 && (
                <div className="px-3 py-2 text-micro" style={{ color: "var(--text-muted)" }}>
                  暂无已配置模型（请在设置中添加供应商）
                </div>
              )}
            </>
          )}

          {pane === "effort" && (
            <>
              <Row onClick={() => setPane("root")} back>
                返回
              </Row>
              {REASONING_EFFORT_OPTIONS.map((o) => (
                <Option
                  key={o.value}
                  selected={effortValue === o.value}
                  onClick={() => {
                    onSelectEffort(o.value === "" ? undefined : (o.value as ReasoningEffort));
                    close();
                  }}
                >
                  {o.label}
                </Option>
              ))}
            </>
          )}
        </div>
      </PopupLayer>
    </>
  );
}
