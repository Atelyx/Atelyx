/**
 * 工具：任务清单（todo_write）。整单替换当前会话/画布对话的持久任务清单（pending/in_progress/completed），返回各状态计数。
 * 跨轮次以**尾部上下文块**带出当前清单（currentTodosBlock，不进系统提示词——保前缀缓存命中）供模型跟踪长任务进度。
 * 清单随会话存 `.atelyx/todos/`（隐藏屏蔽的刻意豁免，内部能力写入）；非 parallelSafe（有副作用），依赖 `capabilities.writeTodos`。
 */
import { ToolArgsError, errText } from "@/types";
import type { TodoItem } from "@/types";
import { defineTool } from "./defineTool";

const STATUSES = ["pending", "in_progress", "completed"] as const;

export interface TodoWriteArgs {
  todos: TodoItem[];
}

export const TODO_WRITE_TOOL = defineTool<TodoWriteArgs>({
  name: "todo_write",
  description:
    "记录并更新当前会话的持久任务清单：todos 传**完整清单**（每次调用整单替换，非增量）。" +
    "status：pending 未开始 / in_progress 进行中 / completed 已完成；至多一项 in_progress（串行工作）。" +
    "返回各状态计数；下一轮对话会自动带出当前清单。单步简单任务无需调用。",
  parameters: {
    type: "object",
    properties: {
      todos: {
        type: "array",
        items: {
          type: "object",
          properties: {
            content: { type: "string", description: "任务内容（一句话）" },
            status: {
              type: "string",
              enum: [...STATUSES],
              description: "pending（未开始）| in_progress（进行中）| completed（已完成）",
            },
          },
          required: ["content", "status"],
        },
        description: "完整任务清单（整单替换；空数组 = 清空）",
      },
    },
    required: ["todos"],
  },
  validate: (args) => {
    const raw = (args as { todos?: unknown } | undefined) ?? {};
    if (!Array.isArray(raw.todos)) throw new ToolArgsError("缺少 todos 清单");
    const todos: TodoItem[] = [];
    const seen = new Set<string>();
    let active = 0;
    for (const item of raw.todos) {
      if (typeof item !== "object" || item === null) {
        throw new ToolArgsError("todos 项须为对象");
      }
      const { content, status } = item as { content?: unknown; status?: unknown };
      if (typeof content !== "string" || !content.trim()) {
        throw new ToolArgsError("todos 项的 content 须为非空字符串");
      }
      if (typeof status !== "string" || !(STATUSES as readonly string[]).includes(status)) {
        throw new ToolArgsError("todos 项的 status 非法（pending/in_progress/completed）");
      }
      const text = content.trim();
      if (seen.has(text)) {
        throw new ToolArgsError(`todos 存在重复项：「${text.slice(0, 30)}」`);
      }
      seen.add(text);
      if (status === "in_progress") active += 1;
      todos.push({ content: text, status: status as TodoItem["status"] });
    }
    if (active > 1) throw new ToolArgsError("至多一项任务可处于 in_progress（串行工作）");
    return { todos };
  },
  summarize: () => "更新任务清单",
  execute: async (args, exec) => {
    const cap = exec.capabilities.writeTodos;
    if (!cap) return { ok: false, summary: "任务清单能力未启用" };
    try {
      await cap(args.todos);
    } catch (e) {
      return { ok: false, summary: `保存任务清单失败：${errText(e)}` };
    }
    const count = (s: TodoItem["status"]) => args.todos.filter((t) => t.status === s).length;
    const pending = count("pending");
    const inProgress = count("in_progress");
    const completed = count("completed");
    const summary = `已更新任务清单：${pending} 待办 / ${inProgress} 进行中 / ${completed} 完成`;
    const body =
      args.todos.length === 0
        ? "(empty todo list)"
        : args.todos.map((t) => `[${t.status}] ${t.content}`).join("\n");
    return { ok: true, summary, content: `${summary}\n\n${body}` };
  },
});
