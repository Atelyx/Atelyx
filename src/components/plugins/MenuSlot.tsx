/**
 * 右键菜单槽宿主：渲染某菜单目标（contextmenu/<target>）的插件菜单项。
 * 订阅 pluginStore.uiRevision（注册/卸载重渲染）；缺贡献 = null（不显示分隔线）。
 */
import type { ReactNode } from "react";
import { listSlot } from "@/services/cordis/slots";
import { usePluginStore } from "@/stores/pluginStore";
import { MenuItem, MenuDivider } from "@/components/common/Menu";

/** 插件菜单项载荷（ctx.slots.registerMenu 注册，label + 回调）。 */
interface MenuItemPayload {
  label: string;
  onClick: () => void;
}

/** 渲染某菜单目标的插件菜单项（priority 降序；空 = null）。 */
export function MenuSlotList({ target }: { target: string }): ReactNode {
  usePluginStore((s) => s.uiRevision);
  const items = listSlot(`contextmenu/${target}`) as Array<{ id: string; payload: MenuItemPayload }>;
  if (items.length === 0) return null;
  return (
    <>
      <MenuDivider />
      {items.map((c) => (
        <MenuItem key={c.id} onClick={c.payload.onClick}>
          {c.payload.label}
        </MenuItem>
      ))}
    </>
  );
}
