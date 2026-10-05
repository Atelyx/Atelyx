/**
 * 插件浮层承载服务（ctx.ui）：内核平台能力，宿主代管浮层的定位/层级/Esc 收起语义。
 *
 * 登记按调用方插件记账（tracker + ctx.effect），插件停用/卸载浮层自动收起；store 数据经
 * access 注入（pluginStore 接线），本模块不 import store。
 */
import { symbols } from "@atelyx/cordis";
import type { Context } from "@atelyx/cordis";
import { pluginIdOf } from "./loader";
import { getPluginFloatingLayerAccess } from "./access";
import type { FloatingLayerHandle, FloatingLayerOptions, UiService } from "./types";

/** 浮层服务实例（tracker 注入调用方插件上下文：登记归属按调用方记账、随其 fiber 撤销）。 */
interface UiServiceInstance extends UiService {
  ctx: Context;
}

/** placement 形状校验（"center" 或数字坐标对象）。 */
function isPlacement(value: unknown): boolean {
  if (value === "center") return true;
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { x?: unknown }).x === "number" &&
    typeof (value as { y?: unknown }).y === "number"
  );
}

export function createUiService(): UiService {
  const api: UiService = {
    showFloatingLayer(this: UiServiceInstance, options: FloatingLayerOptions): FloatingLayerHandle {
      if (!pluginIdOf(this.ctx)) throw new Error("浮层只能由插件上下文发起");
      if (typeof options?.component !== "function") throw new Error("浮层需要 component 组件");
      const placement = options.placement ?? "center";
      if (!isPlacement(placement)) throw new Error('浮层 placement 须为 "center" 或 { x, y } 坐标');
      if (
        options.width !== undefined &&
        (typeof options.width !== "number" || !Number.isFinite(options.width) || options.width <= 0)
      ) {
        throw new Error("浮层 width 须为正数");
      }
      const access = getPluginFloatingLayerAccess();
      if (!access) throw new Error("浮层能力未就绪");
      const ctx = this.ctx;
      // id 由 effect 内登记产生（effect 同步执行，句柄闭包取到的 id 必已就位）；
      // 撤销与句柄 close 都走 store.close（不存在 = no-op），双路径收起不重复触发 onClose。
      let id = "";
      ctx.effect(() => {
        id = access.open({
          pluginId: pluginIdOf(ctx) ?? "plugin",
          component: options.component,
          placement,
          ...(options.width !== undefined ? { width: options.width } : {}),
          closeOnOutsideClick: options.closeOnOutsideClick === true,
          ...(options.onClose ? { onClose: options.onClose } : {}),
        });
        return () => access.close(id);
      });
      return {
        close: () => access.close(id),
      };
    },
  };
  Object.defineProperty(api, symbols.tracker, { value: { property: "ctx" } });
  return api;
}
