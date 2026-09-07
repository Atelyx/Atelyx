/**
 * 示例插件：worker 平面入口。
 *
 * 注册一个 AI 工具 + 一个命令 + 订阅仓库切换事件，并用桥的状态能力记住上次称呼。
 * 运行在独立 Web Worker：无 window、无系统访问，只能经 bridge 与 App 通信。
 */
(async () => {
  // 读上次称呼（state 能力，已在清单 declares 披露）。
  let saved = {};
  try {
    saved = (await bridge.stateRead()) ?? {};
  } catch {
    saved = {};
  }

  // AI 工具：模型可调用。
  bridge.registerTool({
    name: "hello",
    description: "向用户打个招呼（可带称呼）。",
    parameters: {
      type: "object",
      properties: {
        name: { type: "string", description: "称呼（可选）" },
      },
    },
    parallelSafe: true,
    execute: async (args) => {
      const name = typeof args.name === "string" && args.name ? args.name : saved.name ?? "世界";
      await bridge.stateWrite({ ...saved, name, lastRun: Date.now() });
      return { ok: true, summary: `你好，${name}！` };
    },
  });

  // 命令：可被命令面板/管理 UI 执行。
  bridge.registerCommand({
    id: "hello",
    label: "你好",
    run: async () => {
      await bridge.stateWrite({ ...saved, lastRun: Date.now() });
      return { ok: true, summary: "你好！" };
    },
  });

  // 事件订阅：仓库切换时打印（后台能力示例）。
  bridge.on("vault:switch", (payload) => {
    console.log("hello-tool: 切换仓库", payload);
  });

  // 初始化完成（可选；首个 bridge 调用已视为激活）。
  bridge.ready();
})();
