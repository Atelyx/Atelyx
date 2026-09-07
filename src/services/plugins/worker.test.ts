/**
 * 插件桥代理源测试（services/plugins/worker）。
 *
 * 代理源是无依赖的经典 worker 脚本字符串：验证其语法合法、暴露白名单入口，
 * 防止拼接进插件 blob 后整段失效。
 */
import { describe, it, expect } from "vitest";
import { runInNewContext } from "node:vm";
import { buildProxySource } from "./worker";

describe("buildProxySource", () => {
  it("产出语法合法的脚本", () => {
    const src = buildProxySource();
    // 只解析不执行（node 环境无 worker 全局）。
    expect(() => new Function(src)).not.toThrow();
  });

  it("暴露 registerTool 与白名单方法入口", () => {
    const src = buildProxySource();
    expect(src).toContain("bridge.registerTool");
    expect(src).toContain("bridge.registerCommand");
    // 白名单方法以括号属性形式生成。
    expect(src).toContain('bridge["stateRead"]');
    expect(src).toContain('bridge["stateWrite"]');
    expect(src).toContain('bridge["ready"]');
    expect(src).toContain("bridge.on");
    // UI 类注册不在 worker 平面（主线程平面承载）。
    expect(src).not.toContain("registerPanel");
    expect(src).not.toContain("applyTheme");
  });

  it("能力注册表面：registerCapability / call / callStream / emit", () => {
    const src = buildProxySource();
    expect(src).toContain("bridge.registerCapability");
    expect(src).toContain("methodIds");
    expect(src).toContain("bridge.callStream");
    expect(src).toContain("bridge.call");
    expect(src).toContain("bridge.emit");
    // 流式帧与 fnId 序列化标记。
    expect(src).toContain('m.kind==="stream"');
    expect(src).toContain("$fn");
    expect(src).toContain("ctx.stream");
  });

  it("协议常量：工具/命令函数以 fnId 序列化", () => {
    const src = buildProxySource();
    expect(src).toContain("executeId");
    expect(src).toContain("parallelSafe");
    expect(src).toContain("runId");
  });

  it("代理行为：registerTool 发 call（execute 存 fnId）；invoke 仅追加一次 ctx 并回包", async () => {
    // 用 vm 沙箱评估代理源码 + 插件代码，驱动真实消息流（行为级，非子串断言）。
    // self 指向沙箱全局：`self.bridge = bridge` 即全局 bridge，插件顶层可直接引用——
    // 与真实 Worker 经典脚本（sloppy 全局）语义一致。
    const messages: unknown[] = [];
    const holder: { handler: ((e: { data: unknown }) => void) | null } = { handler: null };
    const sandbox: Record<string, unknown> = {};
    sandbox.addEventListener = (_t: string, h: (e: { data: unknown }) => void) => {
      holder.handler = h;
    };
    sandbox.postMessage = (m: unknown) => {
      messages.push(m);
    };
    sandbox.self = sandbox;
    sandbox.Promise = Promise;
    sandbox.setTimeout = setTimeout;
    sandbox.console = console;
    const pluginCode = `
      bridge.registerTool({
        name: "t",
        description: "d",
        parameters: {},
        execute: (args, ctx) => ({ args, hasCtx: !!ctx }),
      });
    `;
    runInNewContext(`${buildProxySource()}\n;\n${pluginCode}`, sandbox);
    // 顶层 registerTool → 一条 call，execute 被存为 fnId（而非函数本身）。
    const reg = messages[0] as { kind: string; method: string; args: [{ executeId: string }] };
    expect(reg.kind).toBe("call");
    expect(reg.method).toBe("registerTool");
    expect(typeof reg.args[0].executeId).toBe("string");
    // 宿主回 reply 放行顶层（不阻塞后续）。
    holder.handler?.({ data: { kind: "reply", seq: 1, ok: true, result: true } });
    // 宿主 invoke 插件 execute：args 只含调用参数，ctx 由代理统一追加（恰一次）。
    holder.handler?.({
      data: { kind: "invoke", seq: 7, fnId: reg.args[0].executeId, args: ["x"] },
    });
    await new Promise((r) => setTimeout(r, 0));
    const reply = messages[messages.length - 1] as {
      kind: string;
      seq: number;
      ok: boolean;
      result: { args: unknown; hasCtx: boolean };
    };
    expect(reply.kind).toBe("reply");
    expect(reply.seq).toBe(7);
    expect(reply.ok).toBe(true);
    // 代理 apply 展开参数：插件 execute 的 args 参数收到首个实参 "x"，ctx 恰注入一次。
    expect(reply.result.args).toBe("x");
    expect(reply.result.hasCtx).toBe(true);
  });
});
