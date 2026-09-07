# -*- coding: utf-8 -*-
"""示例插件（Python 运行时）：AI 工具 + 命令 + 事件订阅。

在 main.py 顶层直接用注入的全局 `bridge`（无需 import，宿主经 stdio 桥注入）。
宿主需安装 Python 并加入 PATH（`python`/`python3`）。
"""


def execute_hello(args, ctx):
    """AI 工具：模型可调用。"""
    name = args.get("name") or "世界"
    saved = bridge.stateRead()
    saved["name"] = name
    saved["lastRun"] = __import__("time").time()
    bridge.stateWrite(saved)
    return {"ok": True, "summary": f"你好，{name}！"}


bridge.registerTool({
    "name": "hello",
    "description": "向用户打个招呼（可带称呼）。",
    "parameters": {
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "称呼（可选）"},
        },
    },
    "parallelSafe": True,
    "execute": execute_hello,
})


def run_greet(ctx):
    return {"ok": True, "summary": "你好！"}


bridge.registerCommand({
    "id": "hello",
    "label": "你好",
    "run": run_greet,
})


def on_switch(payload):
    print(f"hello-python: 切换仓库 {payload}", flush=True)


bridge.on("vault:switch", on_switch)

bridge.ready()
