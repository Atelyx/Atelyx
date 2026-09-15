/**
 * 通用槽位宿主：渲染某具名 UI 槽（titlebar/toolbar/panelhead/settings/statusbar 等）的贡献；
 * 右键菜单项由 MenuSlot 渲染（contextmenu/<target>）。
 *
 * SlotListMount 渲染 list 槽全部贡献（priority 降序），OpenSlotList 渲染开放前缀 list 槽
 * （inspector/<nodeType> 按运行时 kind 动态查询），EmptyStateMount 渲染视图空态
 * （有胜出贡献则替换、否则回退宿主引导）。
 * 内容统一经 SlotDecoratedContent 被装饰器链包裹（ctx.slots.decorate）。
 * 订阅 pluginStore 按槽的修订号（slotRevisions）——槽注册/装饰变化只重渲染该槽宿主，防全局放大。
 * 每贡献包 ErrorBoundary（单个崩溃不拖垮宿主）。
 */
import { Component, useLayoutEffect, useRef, useState, type ComponentType, type ReactNode } from "react";
import { listDecorators, listSlot, resolveSlot, decoratorEpochOf, findSlotDeclarationRuntime } from "@/services/cordis/slots";
import type { SlotContribution } from "@/utils/cordis/slots";
import type { UiSlotPayload } from "@/services/cordis/slots";
import { usePluginStore } from "@/stores/pluginStore";
import { ErrorBoundary } from "@/components/common/ErrorBoundary";

const EMPTY_REJECTED: ReadonlySet<string> = new Set();

/** 装饰器崩溃守卫：装饰器抛错即剔除（宿主内容下一帧复原），崩溃不拖垮宿主。
 *  被剔除后该装饰器不再参与包裹——失败可见且可恢复，不静默吞掉宿主 UI。 */
class SlotDecoratorGuard extends Component<
  { id: string; onReject: (id: string) => void; children: ReactNode },
  { failed: boolean }
> {
  state = { failed: false };
  static getDerivedStateFromError(): { failed: boolean } {
    return { failed: true };
  }
  componentDidCatch(): void {
    this.props.onReject(this.props.id);
  }
  render(): ReactNode {
    return this.state.failed ? null : this.props.children;
  }
}

/**
 * 装饰器链包裹（外层 = 高 priority；无装饰器 = 原样返回 children）。
 * 轻量校验「装饰器必须渲染 children」：每层装饰器 children 内嵌零尺寸 marker，提交后检查
 * marker 是否挂载——从最外层起第一个缺失 = 该装饰器吞掉了 children（宿主 UI），剔除它并回退。
 * 逐层剔除有限次（≤ 装饰器数量），不会死循环。
 */
export function SlotDecoratedContent({ slot, children }: { slot: string; children?: ReactNode }): ReactNode {
  const decos = listDecorators(slot);
  const [rejected, setRejected] = useState<ReadonlySet<string>>(EMPTY_REJECTED);
  const active = decos.filter((d) => !rejected.has(d.id));
  const markers = useRef<(HTMLSpanElement | null)[]>([]);
  // 装饰器纪元号：注册集合变化（插件重载/启停，含同 id 重注册）→ 复位本地剔除记录，给修复后的
  // 装饰器重试机会——否则被剔除的装饰器会永久停留在拒绝态，插件重载也无法自愈。
  const checkedEpoch = useRef<number>(-1);
  useLayoutEffect(() => {
    const epoch = decoratorEpochOf(slot);
    if (epoch !== checkedEpoch.current) {
      checkedEpoch.current = epoch;
      if (rejected.size > 0) setRejected(EMPTY_REJECTED);
    }
    if (active.length === 0) return;
    for (let i = 0; i < active.length; i++) {
      if (!markers.current[i]) {
        setRejected((prev) => (prev.has(active[i].id) ? prev : new Set(prev).add(active[i].id)));
        break;
      }
    }
  }, [active, rejected, slot]);
  const build = (depth: number): ReactNode => {
    if (depth >= active.length) return children;
    const deco = active[depth];
    const Wrapper = deco.wrapper;
    return (
      <SlotDecoratorGuard
        id={deco.id}
        onReject={(id) => setRejected((prev) => (prev.has(id) ? prev : new Set(prev).add(id)))}
      >
        <Wrapper>
          <span
            data-slot-marker
            ref={(el) => {
              markers.current[depth] = el;
            }}
            style={{ display: "none" }}
          />
          {build(depth + 1)}
        </Wrapper>
      </SlotDecoratorGuard>
    );
  };
  if (active.length === 0) return children;
  return build(0);
}

/** 渲染 list 槽全部贡献（priority 降序；缺贡献 = null），内容可被装饰器链包裹。 */
export function SlotListMount({ slot }: { slot: string }): ReactNode {
  usePluginStore((s) => s.slotRevisions[slot] ?? 0);
  const contribs = listSlot(slot) as SlotContribution<UiSlotPayload>[];
  if (contribs.length === 0) return null;
  return (
    <SlotDecoratedContent slot={slot}>
      {contribs.map((c) => {
        const Comp = c.payload.component;
        return <ErrorBoundary key={c.id}>{Comp ? <Comp /> : null}</ErrorBoundary>;
      })}
    </SlotDecoratedContent>
  );
}

/** 渲染开放前缀 list 槽（inspector/<nodeType> 等）：按运行时 kind 动态查询（前缀槽无法静态枚举，
 *  与 view/ 分派同构）。缺贡献 = null。 */
export function OpenSlotList({ slot }: { slot: string }): ReactNode {
  usePluginStore((s) => s.slotRevisions[slot] ?? 0);
  const contribs = listSlot(slot) as SlotContribution<UiSlotPayload>[];
  if (contribs.length === 0) return null;
  return (
    <SlotDecoratedContent slot={slot}>
      {contribs.map((c) => {
        const Comp = c.payload.component;
        return <ErrorBoundary key={c.id}>{Comp ? <Comp /> : null}</ErrorBoundary>;
      })}
    </SlotDecoratedContent>
  );
}

/** 视图空态槽：有胜出贡献则渲染贡献替换空态，否则回退宿主兜底引导（empty/<viewKind> single 槽）。 */
export function EmptyStateMount({ viewKind, fallback }: { viewKind: string; fallback: ReactNode }): ReactNode {
  const slot = `empty/${viewKind}`;
  usePluginStore((s) => s.slotRevisions[slot] ?? 0);
  const winner = resolveSlot(slot);
  if (!winner) return <>{fallback}</>;
  const Comp = (winner.payload as { component?: ComponentType }).component;
  if (!Comp) return <>{fallback}</>;
  return (
    <SlotDecoratedContent slot={slot}>
      <ErrorBoundary key={winner.id}>
        <Comp />
      </ErrorBoundary>
    </SlotDecoratedContent>
  );
}

/** 插件侧槽位渲染宿主（ctx.slots.host(slot) 返回的组件）：按声明基数渲染 single 胜出者或全部 list 贡献，
 *  内容经 SlotDecoratedContent 被装饰器链包裹（与宿主 SlotListMount 同语义，插件面板内复用）。 */
export function PluginSlotHost({ slot }: { slot: string }): ReactNode {
  usePluginStore((s) => s.slotRevisions[slot] ?? 0);
  const decl = findSlotDeclarationRuntime(slot);
  // host() 调用时已保证声明存在；此处声明缺失只发生在声明方停用后——无契约可依，渲染空。
  if (!decl) return null;
  if (decl.cardinality === "single") {
    const winner = resolveSlot(slot);
    if (!winner) return null;
    const Comp = (winner.payload as { component?: ComponentType }).component;
    if (!Comp) return null;
    return (
      <SlotDecoratedContent slot={slot}>
        <ErrorBoundary key={winner.id}>
          <Comp />
        </ErrorBoundary>
      </SlotDecoratedContent>
    );
  }
  const contribs = listSlot(slot) as SlotContribution<UiSlotPayload>[];
  if (contribs.length === 0) return null;
  return (
    <SlotDecoratedContent slot={slot}>
      {contribs.map((c) => {
        const Comp = c.payload.component;
        return <ErrorBoundary key={c.id}>{Comp ? <Comp /> : null}</ErrorBoundary>;
      })}
    </SlotDecoratedContent>
  );
}
