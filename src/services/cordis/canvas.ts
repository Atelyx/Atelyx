/**
 * 画布服务提供器（ctx.canvas）：由第一方 builtin.canvas 插件挂载时经 ctx.provide 提供。
 *
 * 实现 = 注入的画布访问对象（registerCanvasPluginWiring 填充，见 stores/canvasStore）——
 * 停用/卸载 builtin.canvas 时 access 撤销、服务随之消失。
 */
import { getPluginCanvasAccess } from "./access";
import type { CanvasService } from "./types";

/** 构造画布服务（要求访问已接线：builtin.canvas 挂载时经 registerCanvasPluginWiring 填充）。 */
export function createCanvasService(): CanvasService {
  const access = getPluginCanvasAccess();
  if (!access) throw new Error("画布能力未就绪");
  return {
    snapshot: () => access.snapshot(),
    addNode: (node) => access.addNode(node),
    updateNode: (nodeId, patch) => access.updateNode(nodeId, patch),
    moveNode: (nodeId, position) => access.moveNode(nodeId, position),
    deleteNode: (nodeId) => access.deleteNode(nodeId),
    addEdge: (edge) => access.addEdge(edge),
    deleteEdge: (edgeId) => access.deleteEdge(edgeId),
    selectNode: (nodeId) => access.selectNode(nodeId),
  };
}
