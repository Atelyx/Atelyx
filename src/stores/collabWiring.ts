/**
 * 协作域接线装配点：笔记/画布/表格域的协作接线（消息通道/重连/拆卸/presence 合并/广播注入）
 * 经此一次性注册到通用协作宿主（collabStore）——宿主保持域无关，只做查表分发与 presence 合并。
 * 触发：panelStore.syncCollabHost 的 isHost 分支（collab.init 前，幂等一次）。
 * 注册顺序有约束：表格域须先于画布域——重连补发的 presence 按注册序广播，
 * 画布打开时其 presence 覆盖表格槽（画布为主工作区，锁/流式跨视图保活）。
 */
import { ensureCanvasCollabWiring } from "@/stores/canvasStore";
import { ensureNoteCollabWiring } from "@/stores/noteCollabStore";
import { ensureTableCollabWiring } from "@/stores/tableStore";

let wired = false;

/** 注册域协作接线（幂等：只执行一次，宿主生命周期内注册保持常驻）。 */
export function wireCollabDomains(): void {
  if (wired) return;
  wired = true;
  ensureNoteCollabWiring();
  ensureTableCollabWiring();
  ensureCanvasCollabWiring();
}
