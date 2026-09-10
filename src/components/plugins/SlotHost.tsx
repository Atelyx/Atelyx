/**
 * 通用槽位宿主：渲染某具名 UI 槽（titlebar/toolbar/panelhead/contextmenu/settings/statusbar 等）的贡献。
 *
 * SlotListMount 渲染 list 槽全部贡献（priority 降序）；SlotMount 渲染 single 槽胜出贡献。
 * 订阅 pluginStore.uiRevision（槽注册/卸载时重渲染）；每贡献包 ErrorBoundary（单个崩溃不拖垮宿主）。
 */
import type { ReactNode } from "react";
import { listSlot, resolveSlot, type UiSlotPayload } from "@/services/cordis/slots";
import { usePluginStore } from "@/stores/pluginStore";
import { ErrorBoundary } from "@/components/common/ErrorBoundary";

/** 渲染 list 槽全部贡献（priority 降序；缺贡献 = null）。 */
export function SlotListMount({ slot }: { slot: string }): ReactNode {
  usePluginStore((s) => s.uiRevision);
  const contribs = listSlot(slot) as Array<{ id: string; payload: UiSlotPayload }>;
  if (contribs.length === 0) return null;
  return (
    <>
      {contribs.map((c) => {
        const Comp = c.payload.component;
        return <ErrorBoundary key={c.id}>{Comp ? <Comp /> : null}</ErrorBoundary>;
      })}
    </>
  );
}

/** 渲染 single 槽胜出贡献（缺贡献 = null）。 */
export function SlotMount({ slot }: { slot: string }): ReactNode {
  usePluginStore((s) => s.uiRevision);
  const winner = resolveSlot(slot) as { payload: UiSlotPayload } | undefined;
  if (!winner) return null;
  const Comp = winner.payload.component;
  return <ErrorBoundary>{Comp ? <Comp /> : null}</ErrorBoundary>;
}
