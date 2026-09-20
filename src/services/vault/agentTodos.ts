/**
 * AI 任务清单的仓库持久化（todo_write 工具后端）。
 *
 * 清单按会话/画布对话 id 隔离，落盘位置按激活仓库身份分流（services/metadata）：
 * 个人仓库 = `.atelyx/todos/<encodeURIComponent(id)>.json`（`{ updatedAt, todos }`）；
 * 协作空间 = user meta 键 `todos/<encodeURIComponent(id)>`。缺失/损坏 → 空清单
 * （尽力而为，不阻塞对话）。供两 store 注入 writeTodos capability，
 * 并跨轮次以尾部上下文块带出当前清单（currentTodosBlock）。
 */
import { readSessionTodosRaw, writeSessionTodosRaw, deleteSessionTodosRaw } from "@/services/metadata";
import type { TodoItem } from "@/types";

interface TodosFile {
  updatedAt: number;
  todos: TodoItem[];
}

/** 读某会话/画布对话的任务清单（缺失/损坏 → 空清单，读失败静默降级）。 */
export async function readAgentTodos(id: string): Promise<TodoItem[]> {
  const raw = await readSessionTodosRaw(id);
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as TodosFile;
    return Array.isArray(parsed.todos) ? parsed.todos : [];
  } catch {
    return [];
  }
}

/** 整单替换写入某会话/画布对话的任务清单（失败抛出，由工具降级回填）。 */
export async function writeAgentTodos(id: string, todos: TodoItem[]): Promise<void> {
  const file: TodosFile = { updatedAt: Date.now(), todos };
  await writeSessionTodosRaw(id, JSON.stringify(file, null, 2));
}

/** 删除某会话/画布对话的任务清单侧车（删除会话/节点时清理孤儿；无侧车报错由调用方吞掉）。 */
export async function deleteAgentTodos(id: string): Promise<void> {
  await deleteSessionTodosRaw(id);
}

/** 任务清单的条目行（`- [status] content`；空清单 → 空串）。仅 currentTodosBlock 内部使用。 */
function formatAgentTodosText(todos: TodoItem[]): string {
  if (todos.length === 0) return "";
  return todos.map((t) => `- [${t.status}] ${t.content}`).join("\n");
}

/**
 * 任务清单的尾部上下文块（ephemeral，随请求折叠进末条 user 消息、不入会话存储）。
 * 与笔记当前上下文块同模式：**不进系统提示词**（系统前缀稳定 → 前缀缓存命中），
 * 仅尾部块随清单变化而变；清单不变时逐字稳定复现，命中至该消息原文。
 */
export function currentTodosBlock(todos: TodoItem[]): string {
  const lines = formatAgentTodosText(todos);
  if (!lines) return "";
  return ["<context>", "当前任务清单（用 todo_write 更新，逐步推进）：", lines, "</context>"].join("\n");
}
