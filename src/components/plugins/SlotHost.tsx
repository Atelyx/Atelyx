/**
 * 通用槽位宿主：渲染某具名 UI 槽（titlebar/toolbar/panelhead/settings/statusbar 等）的贡献；
 * 右键菜单项由 MenuSlot 渲染（contextmenu/<target>）。
 *
 * SlotListMount 渲染 list 槽全部贡献（priority 降序）。
 * 订阅 pluginStore.uiRevision（槽注册/卸载时重渲染）；每贡献包 ErrorBoundary（单个崩溃不拖垮宿主）。
 */
import type { ReactNode } from "react";
import { listSlot, type UiSlotPayload } from "@/services/cordis/slots";
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
