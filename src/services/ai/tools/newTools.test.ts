/**
 * 新工具契约测试：隐藏段屏蔽（hasHiddenSegment + 各文件工具 validate）、read_file offset 越界降级、delete_dir/todo_write/read_history/append_file 的校验与执行。
 * 沿用 registry.test.ts 的注入 capability 模式（execute 只依赖注入能力，不触 store）。
 */
import { describe, expect, it } from "vitest";
import type { ToolExecContext } from "@/types";
import { HIDDEN_PATH_ERROR, hasHiddenSegment } from "@/constants/tools";
import { READ_FILE_TOOL } from "./readFile";
import { WRITE_FILE_TOOL } from "./writeFile";
import { EDIT_FILE_TOOL } from "./editFile";
import { RENAME_FILE_TOOL } from "./renameFile";
import { MOVE_FILE_TOOL } from "./moveFile";
import { DELETE_FILE_TOOL } from "./deleteFile";
import { GLOB_TOOL } from "./glob";
import { GREP_TOOL } from "./grep";
import { LIST_DIR_TOOL } from "./listDir";
import { DELETE_DIR_TOOL } from "./deleteDir";
import { APPEND_FILE_TOOL } from "./appendFile";
import { TODO_WRITE_TOOL } from "./todoWrite";
import { READ_HISTORY_TOOL, renderHistoryResult } from "./readHistory";
import { assembleAgentSystemPrompt, buildAgentTools } from "./index";
import { currentTodosBlock } from "@/services/vault/agentTodos";

const ctx = (capabilities: ToolExecContext["capabilities"]): ToolExecContext => ({
  signal: new AbortController().signal,
  capabilities,
});

describe("hasHiddenSegment", () => {
  it("识别隐藏目录/文件段", () => {
    expect(hasHiddenSegment(".atelyx/config.json")).toBe(true);
    expect(hasHiddenSegment(".git/x")).toBe(true);
    expect(hasHiddenSegment("a/.hidden/b.md")).toBe(true);
    expect(hasHiddenSegment(".gitignore")).toBe(true);
  });

  it("普通路径与 ./.. 段不误判", () => {
    expect(hasHiddenSegment("a/b.md")).toBe(false);
    expect(hasHiddenSegment("笔记/方案.txt")).toBe(false);
    expect(hasHiddenSegment("./a.md")).toBe(false);
    // `..` 属父目录段：不按隐藏处理，由路径穿越校验（safe_join）拒绝，报错语义更准确
    expect(hasHiddenSegment("..")).toBe(false);
    expect(hasHiddenSegment("../x")).toBe(false);
    expect(hasHiddenSegment("a")).toBe(false);
    expect(hasHiddenSegment("")).toBe(false);
  });
});

describe("文件工具 validate 屏蔽隐藏段", () => {
  it("read_file 拒绝隐藏路径，普通路径放行", () => {
    expect(() => READ_FILE_TOOL.validate({ path: ".atelyx/config.json" })).toThrow(
      HIDDEN_PATH_ERROR,
    );
    expect(READ_FILE_TOOL.validate({ path: "a/b.md" }).path).toBe("a/b.md");
  });

  it("write_file / edit_file / append_file 拒绝隐藏路径", () => {
    expect(() => WRITE_FILE_TOOL.validate({ path: ".git/x", content: "y" })).toThrow(
      HIDDEN_PATH_ERROR,
    );
    expect(() => EDIT_FILE_TOOL.validate({ path: "a/.h/b.md", edits: [] })).toThrow(
      HIDDEN_PATH_ERROR,
    );
    expect(() => APPEND_FILE_TOOL.validate({ path: ".env", content: "k=v" })).toThrow(
      HIDDEN_PATH_ERROR,
    );
  });

  it("rename_file / move_file 源或目标含隐藏段均拒绝", () => {
    expect(() => RENAME_FILE_TOOL.validate({ oldPath: ".git/config", newName: "x" })).toThrow(
      HIDDEN_PATH_ERROR,
    );
    expect(() => MOVE_FILE_TOOL.validate({ oldPath: "a.md", newDir: ".git" })).toThrow(
      HIDDEN_PATH_ERROR,
    );
    expect(MOVE_FILE_TOOL.validate({ oldPath: "a.md", newDir: "b" }).newDir).toBe("b");
  });

  it("delete_file 隐藏路径拒绝", () => {
    expect(() => DELETE_FILE_TOOL.validate({ path: ".atelyx/config.json", confirm: true })).toThrow(
      HIDDEN_PATH_ERROR,
    );
  });
});

describe("发现层工具（glob/grep/list_dir）path/dir 指向隐藏段拒绝", () => {
  it("隐藏段拒绝，普通目录放行", () => {
    expect(() => GLOB_TOOL.validate({ pattern: "**/*.md", path: ".git" })).toThrow(
      HIDDEN_PATH_ERROR,
    );
    expect(() => GREP_TOOL.validate({ pattern: "x", path: ".atelyx" })).toThrow(
      HIDDEN_PATH_ERROR,
    );
    expect(() => LIST_DIR_TOOL.validate({ dir: "a/.hidden" })).toThrow(HIDDEN_PATH_ERROR);
    expect(GLOB_TOOL.validate({ pattern: "*.md", path: "a" }).path).toBe("a");
    expect(GREP_TOOL.validate({ pattern: "x", path: "a" }).path).toBe("a");
    expect(LIST_DIR_TOOL.validate({ dir: "a" }).dir).toBe("a");
  });
});

describe("delete_dir", () => {
  it("缺 confirm / 仓库根 / 隐藏段拒绝", () => {
    expect(() => DELETE_DIR_TOOL.validate({ path: "a", confirm: false })).toThrow(/confirm/);
    expect(() => DELETE_DIR_TOOL.validate({ path: "/" })).toThrow(/confirm/);
    expect(() => DELETE_DIR_TOOL.validate({ path: ".git", confirm: true })).toThrow(
      HIDDEN_PATH_ERROR,
    );
    expect(() => DELETE_DIR_TOOL.validate({ path: "", confirm: true })).toThrow(/缺少目录路径/);
    expect(() => DELETE_DIR_TOOL.validate({ path: "/", confirm: true })).toThrow(/仓库根/);
    expect(() => DELETE_DIR_TOOL.validate({ path: ".", confirm: true })).toThrow(/仓库根/);
    expect(() => DELETE_DIR_TOOL.validate({ path: "..", confirm: true })).toThrow(/父目录/);
    expect(() => DELETE_DIR_TOOL.validate({ path: "a/../b", confirm: true })).toThrow(/父目录/);
  });

  it("非空目录需 force 确认，带 force 后删除", async () => {
    const cap = {
      deleteDir: async (dir: string, force?: boolean) =>
        force
          ? { ok: true, summary: `已删除目录「${dir}」`, needsConfirm: false, itemCount: 0 }
          : { ok: false, summary: `目录非空（5 项）`, needsConfirm: true, itemCount: 5 },
    };
    const first = await DELETE_DIR_TOOL.execute(
      DELETE_DIR_TOOL.validate({ path: "a", confirm: true }),
      ctx(cap),
    );
    expect(first.ok).toBe(false);
    expect(first.summary).toContain("force: true");
    const second = await DELETE_DIR_TOOL.execute(
      DELETE_DIR_TOOL.validate({ path: "a", confirm: true, force: true }),
      ctx(cap),
    );
    expect(second.ok).toBe(true);
    expect(second.summary).toContain("已删除目录");
  });
});

describe("todo_write", () => {
  it("非法清单拒绝：空 content / 重复 / 多 in_progress / 非法 status", () => {
    expect(() => TODO_WRITE_TOOL.validate({ todos: [{ content: "", status: "pending" }] })).toThrow(
      /content/,
    );
    expect(() =>
      TODO_WRITE_TOOL.validate({
        todos: [
          { content: "a", status: "pending" },
          { content: "a", status: "pending" },
        ],
      }),
    ).toThrow(/重复/);
    expect(() =>
      TODO_WRITE_TOOL.validate({
        todos: [
          { content: "a", status: "in_progress" },
          { content: "b", status: "in_progress" },
        ],
      }),
    ).toThrow(/in_progress/);
    expect(() =>
      TODO_WRITE_TOOL.validate({ todos: [{ content: "a", status: "done" }] }),
    ).toThrow(/status/);
  });

  it("合法清单整单替换并返回计数", async () => {
    let written: unknown = null;
    const cap = {
      writeTodos: async (todos: unknown) => {
        written = todos;
      },
    };
    const res = await TODO_WRITE_TOOL.execute(
      TODO_WRITE_TOOL.validate({
        todos: [
          { content: "调研", status: "in_progress" },
          { content: "落地", status: "pending" },
          { content: "收尾", status: "completed" },
        ],
      }),
      ctx(cap),
    );
    expect(res.ok).toBe(true);
    expect(res.summary).toContain("1 待办 / 1 进行中 / 1 完成");
    expect(written).toEqual([
      { content: "调研", status: "in_progress" },
      { content: "落地", status: "pending" },
      { content: "收尾", status: "completed" },
    ]);
  });
});

describe("read_file offset 越界降级", () => {
  it("capability 抛越界错误 → execute ok:false 且 summary 含 offset 与总行数", async () => {
    const cap = {
      readFile: async () => {
        throw new Error("offset 999 超出文件范围（文件共 20 行）");
      },
    };
    const res = await READ_FILE_TOOL.execute(
      READ_FILE_TOOL.validate({ path: "a.md", offset: 999 }),
      ctx(cap),
    );
    expect(res.ok).toBe(false);
    expect(res.summary).toContain("offset 999");
    expect(res.summary).toContain("20");
  });
});

describe("read_history", () => {
  it("版本列表渲染", () => {
    const r = renderHistoryResult({
      ok: true,
      summary: "共 2 个版本",
      versions: [
        { seq: 1, ts: 0, authorName: "AI Agent", action: "edit", summary: "+5 行" },
        { seq: 2, ts: 1000, authorName: "设备", action: "edit", summary: "−2 行", note: "备注" },
      ],
    });
    expect(r.content).toContain("v1");
    expect(r.content).toContain("AI Agent(edit)");
    expect(r.content).toContain("备注");
  });

  it("单版本快照透传", () => {
    const r = renderHistoryResult({ ok: true, summary: "v3 快照", content: "hello" });
    expect(r.content).toBe("hello");
  });

  it("失败透传", () => {
    const r = renderHistoryResult({ ok: false, summary: "无历史记录" });
    expect(r.ok).toBe(false);
    expect(r.summary).toBe("无历史记录");
  });

  it("validate 拒绝隐藏路径与非法 version", () => {
    expect(() => READ_HISTORY_TOOL.validate({ path: ".atelyx/x.md" })).toThrow(
      HIDDEN_PATH_ERROR,
    );
    expect(() => READ_HISTORY_TOOL.validate({ path: "a.md", version: 0 })).toThrow(/version/);
  });
});

describe("append_file", () => {
  it("execute 转发 capability 结果", async () => {
    const cap = { appendFile: async () => ({ ok: true, summary: "已追加内容到「a.md」" }) };
    const res = await APPEND_FILE_TOOL.execute(
      APPEND_FILE_TOOL.validate({ path: "a.md", content: "x" }),
      ctx(cap),
    );
    expect(res.ok).toBe(true);
    expect(res.summary).toContain("已追加");
  });

  it("空 content 拒绝", () => {
    expect(() => APPEND_FILE_TOOL.validate({ path: "a.md", content: "" })).toThrow(/content/);
  });
});

describe("buildAgentTools 勾选生效（全量按勾选并入）", () => {
  it("空勾选 → 空名册（纯对话）", () => {
    const { tools, skippedWebSearch } = buildAgentTools([], true);
    expect(tools).toEqual([]);
    expect(skippedWebSearch).toBe(false);
  });

  it("只读工具仅当勾选时并入", () => {
    expect(buildAgentTools([], true).tools.map((t) => t.name)).not.toContain("read_file");
    const names = buildAgentTools(["read_file"], true).tools.map((t) => t.name);
    expect(names).toContain("read_file");
  });

  it("写入类工具按勾选门控", () => {
    const names = buildAgentTools(["write_file"], true).tools.map((t) => t.name);
    expect(names).toEqual(["write_file"]);
  });

  it("web_search 勾选但搜索源未配置 → 剔除并标记", () => {
    const out = buildAgentTools(["web_search"], false);
    expect(out.tools.map((t) => t.name)).not.toContain("web_search");
    expect(out.skippedWebSearch).toBe(true);
  });

  it("web_search 勾选且搜索源已配置 → 并入", () => {
    const out = buildAgentTools(["web_search"], true);
    expect(out.tools.map((t) => t.name)).toContain("web_search");
    expect(out.skippedWebSearch).toBe(false);
  });

  it("未知 id 静默忽略", () => {
    expect(buildAgentTools(["no_such_tool"], true).tools).toEqual([]);
  });
});

describe("assembleAgentSystemPrompt 缓存红线（易变上下文不进系统提示词）", () => {
  const toolSchema = (name: string) => ({ name, description: name, parameters: {} });

  it("todo_write 开启时也不注入任务清单（由尾部块 currentTodosBlock 承载）", () => {
    const out = assembleAgentSystemPrompt("你好", [toolSchema("todo_write")]);
    expect(out).not.toContain("任务清单");
    expect(out).not.toContain("todo");
    expect(out).toBe("你好");
  });

  it("含 read_file 时注入引用读取引导", () => {
    const out = assembleAgentSystemPrompt(undefined, [toolSchema("read_file")]);
    expect(out).toContain("read_file");
  });

  it("空系统提示词且无引导 → undefined", () => {
    expect(assembleAgentSystemPrompt(undefined, [])).toBeUndefined();
  });
});

describe("currentTodosBlock", () => {
  it("空清单 → 空串（不注入）", () => {
    expect(currentTodosBlock([])).toBe("");
  });

  it("非空清单 → context 尾部块含条目", () => {
    const block = currentTodosBlock([
      { content: "调研", status: "in_progress" },
      { content: "落地", status: "pending" },
    ]);
    expect(block).toContain("<context>");
    expect(block).toContain("当前任务清单");
    expect(block).toContain("- [in_progress] 调研");
    expect(block).toContain("- [pending] 落地");
  });
});
