/**
 * 元数据双源分发层契约测试（services/metadata/index.ts）。
 *
 * 覆盖：协作空间下元数据读写按激活仓库身份分发到 team meta（space meta）/
 * user meta（meta/me）或本机命令；个人仓库保持本地命令直通；
 * 只读团队层写入被拒且通知可见；append 读改写与身份中途切换守卫。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

// ===== 空间客户端替身（team meta = 服务端空间级，my meta = 服务端个人级）=====

const space = vi.hoisted(() => {
  const state = {
    teamValues: {} as Record<string, string>,
    myValues: {} as Record<string, string>,
    calls: [] as Array<{ group: string; method: string; args: unknown[] }>,
    /** 非空时 getMyMeta 挂起，直到 resolveGate（模拟读在途期间身份切换）。 */
    holdMyMeta: null as Promise<void> | null,
    resolveGate: null as (() => void) | null,
    /** 非空时 team meta 写入抛该消息（模拟 viewer 被服务端拒绝/掉线）。 */
    failPatch: null as string | null,
  };
  function makeClient(_serverUrl: string) {
    const meta = {
      getSpaceMeta: async () => {
        state.calls.push({ group: "meta", method: "getSpaceMeta", args: [] });
        return { values: { ...state.teamValues } };
      },
      patchSpaceMeta: async (_spaceId: string, body: { values: Record<string, string> }) => {
        state.calls.push({ group: "meta", method: "patchSpaceMeta", args: [body] });
        if (state.failPatch) throw new Error(state.failPatch);
        Object.assign(state.teamValues, body.values);
        return {};
      },
      deleteSpaceMeta: async (_spaceId: string, key: string) => {
        state.calls.push({ group: "meta", method: "deleteSpaceMeta", args: [key] });
        delete state.teamValues[key];
      },
      getMyMeta: async () => {
        state.calls.push({ group: "meta", method: "getMyMeta", args: [] });
        if (state.holdMyMeta) await state.holdMyMeta;
        return { values: { ...state.myValues } };
      },
      patchMyMeta: async (_spaceId: string, body: { values: Record<string, string> }) => {
        state.calls.push({ group: "meta", method: "patchMyMeta", args: [body] });
        Object.assign(state.myValues, body.values);
        return {};
      },
      deleteMyMeta: async (_spaceId: string, key: string) => {
        state.calls.push({ group: "meta", method: "deleteMyMeta", args: [key] });
        delete state.myValues[key];
      },
    };
    return {
      auth: {},
      spaces: {},
      meta,
      content: {},
    };
  }
  return { state, makeClient };
});

vi.mock("@/services/space/client", () => ({
  createSpaceClient: (_serverUrl: string) => space.makeClient(_serverUrl),
}));

// ===== 本地命令替身 =====

const h = vi.hoisted(() => {
  const state = {
    /** space_config_patch 收到的补丁（serverKey → 补丁列表）。 */
    spaceConfigPatches: [] as Array<{ serverKey: string; patch: Record<string, unknown> }>,
    /** 挂起 space_config_patch 的应答（身份中途切换测试用）。 */
    holdSpacePatch: null as Promise<void> | null,
    localCalls: [] as Array<{ cmd: string; args: Record<string, unknown> }>,
    /** keychain 条目（get_api_key 返回）。 */
    keychain: {} as Record<string, string>,
  };
  return { state };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    const a = args ?? {};
    switch (cmd) {
      case "space_config_patch":
        h.state.spaceConfigPatches.push({
          serverKey: String(a.serverKey),
          patch: a.patch as Record<string, unknown>,
        });
        if (h.state.holdSpacePatch) await h.state.holdSpacePatch;
        return null;
      case "read_global_config":
        return { config: {}, corruptBackup: null };
      case "get_api_key":
        return h.state.keychain[`${String(a.vaultRoot)}:${String(a.providerId)}`] ?? "";
      case "write_global_config":
        return null;
      // 本地命令：录制即直通（本地分支只断言「转发了对应命令」）
      default:
        h.state.localCalls.push({ cmd, args: a });
        return null;
    }
  },
}));

let metadata: typeof import("./index");
let factory: typeof import("@/services/content/factory");

const SPACE_IDENTITY = { kind: "space" as const, serverUrl: "http://s1", spaceId: "sp1" };

beforeEach(async () => {
  vi.resetModules();
  space.state.teamValues = {};
  space.state.myValues = {};
  space.state.calls = [];
  space.state.holdMyMeta = null;
  space.state.resolveGate = null;
  space.state.failPatch = null;
  h.state.spaceConfigPatches = [];
  h.state.holdSpacePatch = null;
  h.state.localCalls = [];
  h.state.keychain = {};
  factory = await import("@/services/content/factory");
  metadata = await import("./index");
});

/** 通知替身接线（metadata 经 cordis access 注入点弹通知）。 */
async function hookNotifications(): Promise<{ items: Array<{ level?: string; message: string }> }> {
  const access = await import("@/services/cordis/access");
  const items: Array<{ level?: string; message: string }> = [];
  access.setPluginNotificationAccess({ notify: (input) => (items.push(input), "n1"), dismiss: () => {} });
  return { items };
}

describe("team meta 分发（sort/exclusions/folder-colors/prompt-notes/agents）", () => {
  it("空间内读 prompt-notes / agents 返回团队层数据", async () => {
    factory.activateContentIdentity(SPACE_IDENTITY);
    space.state.teamValues["prompt-notes"] = JSON.stringify(["笔记/提示词.md"]);
    space.state.teamValues["agents"] = JSON.stringify([{ id: "a1", name: "团队", tools: [] }]);

    expect(await metadata.readPromptNotes()).toEqual(["笔记/提示词.md"]);
    expect(await metadata.readAgents()).toEqual([{ id: "a1", name: "团队", tools: [] }]);
  });

  it("空间内写 prompt-notes / agents 落团队元数据（写权限由服务端按角色裁决）", async () => {
    factory.activateContentIdentity(SPACE_IDENTITY);

    await metadata.writePromptNotes(["x.md"]);
    await metadata.writeAgents([{ id: "a", name: "n", tools: [] }]);

    expect(space.state.teamValues["prompt-notes"]).toBe(JSON.stringify(["x.md"]));
    expect(space.state.teamValues["agents"]).toBe(
      JSON.stringify([{ id: "a", name: "n", tools: [] }]),
    );
    // 写入走 team meta 的 PATCH（不是本地文件命令），空间与本地互不串
    expect(
      space.state.calls.filter((c) => c.method === "patchSpaceMeta").length,
    ).toBe(2);
    expect(h.state.localCalls.length).toBe(0);
  });

  it("空间内写被服务端拒绝（如查看者）：通知可见并如实抛错，不静默丢弃", async () => {
    factory.activateContentIdentity(SPACE_IDENTITY);
    const { items } = await hookNotifications();
    space.state.failPatch = "需要所有者或编辑者权限";

    await expect(
      metadata.writeAgents([{ id: "a", name: "n", tools: [] }]),
    ).rejects.toThrow("需要所有者或编辑者权限");
    expect(space.state.teamValues["agents"]).toBeUndefined();
    expect(items.some((n) => n.message.includes("协作空间元数据保存失败"))).toBe(true);
  });

  it("显式空间目标写 agents：按目标身份落该空间的团队元数据", async () => {    // 未激活任何仓库：写入目标必须自带 serverUrl/spaceId（按目标而非激活身份取连接）
    await metadata.writeAgents([{ id: "b", name: "外部", tools: [] }], {
      kind: "space",
      serverUrl: "http://s2",
      spaceId: "sp2",
      name: "外部空间",
      role: "editor",
    });
    expect(space.state.teamValues["agents"]).toBe(
      JSON.stringify([{ id: "b", name: "外部", tools: [] }]),
    );
    expect(h.state.localCalls.length).toBe(0);
  });

  it("folderColors 空间往返：写 = team meta PATCH，读 = team meta 解析", async () => {
    factory.activateContentIdentity(SPACE_IDENTITY);
    await metadata.writeFolderColors({ "目录A/": "#ff0000" });

    expect(space.state.calls.some((c) => c.method === "patchSpaceMeta")).toBe(true);
    expect(space.state.teamValues["folder-colors"]).toBe(JSON.stringify({ "目录A/": "#ff0000" }));
    expect(await metadata.readFolderColors()).toEqual({ "目录A/": "#ff0000" });
  });

  it("patchVaultConfig（空间）：排序/排除夹与 AI 字段各落自己的团队键（不写本机 spaceConfigs）", async () => {
    factory.activateContentIdentity(SPACE_IDENTITY);
    space.state.teamValues["ai-providers"] = JSON.stringify([
      { id: "p1", name: "A", baseUrl: "u", models: [], apiKey: "sk-1" },
    ]);

    await metadata.patchVaultConfig({
      fileExplorerSort: "name-asc",
      excludeFolders: ["草稿"],
      model: "m1",
    });

    const metaPatch = space.state.calls.find((c) => c.method === "patchSpaceMeta");
    expect(metaPatch).toBeDefined();
    // AI 配置按字段分键：只写本次改动的模型键，供应商键原样不动
    expect((metaPatch!.args[0] as { values: Record<string, string> }).values).toEqual({
      sort: JSON.stringify("name-asc"),
      exclusions: JSON.stringify(["草稿"]),
      "ai-model": JSON.stringify("m1"),
    });
    expect(space.state.teamValues["ai-providers"]).toBe(
      JSON.stringify([{ id: "p1", name: "A", baseUrl: "u", models: [], apiKey: "sk-1" }]),
    );
    expect(h.state.spaceConfigPatches).toHaveLength(0);
  });

  it("patchVaultConfig（空间）：syncKeys 无对应设定被忽略，不产生任何写", async () => {
    factory.activateContentIdentity(SPACE_IDENTITY);
    await metadata.patchVaultConfig({ syncKeys: true });
    expect(space.state.calls.filter((c) => c.method.startsWith("patch")).length).toBe(0);
    expect(h.state.spaceConfigPatches).toHaveLength(0);
  });

  it("patchVaultConfig（空间）：excludeFolders 清空 = 删 team meta 键；附件夹设定落 team meta", async () => {
    factory.activateContentIdentity(SPACE_IDENTITY);
    space.state.teamValues["exclusions"] = JSON.stringify(["旧"]);

    await metadata.patchVaultConfig({ excludeFolders: null, attachmentFolder: "assets" });

    expect(space.state.teamValues["exclusions"]).toBeUndefined();
    expect(space.state.calls.some((c) => c.method === "deleteSpaceMeta")).toBe(true);
    expect(space.state.teamValues["attachment-folder"]).toBe(JSON.stringify("assets"));
    // 附件夹有专属落点（team meta），不进 spaceConfigs 补丁
    expect(h.state.spaceConfigPatches).toHaveLength(0);
  });

  it("readVaultConfig（空间）= 团队元数据 AI 配置本体 + sort/exclusions/attachment-folder 合并", async () => {
    factory.activateContentIdentity(SPACE_IDENTITY);
    space.state.teamValues["ai-providers"] = JSON.stringify([
      { id: "p1", name: "A", baseUrl: "u", models: [], apiKey: "sk-1" },
    ]);
    space.state.teamValues["ai-model"] = JSON.stringify("m1");
    space.state.teamValues.sort = JSON.stringify("name-desc");
    space.state.teamValues.exclusions = JSON.stringify(["tmp"]);
    space.state.teamValues["attachment-folder"] = JSON.stringify("素材");

    const { config, corruptBackup } = await metadata.readVaultConfig();
    expect(corruptBackup).toBeNull();
    expect(config.model).toBe("m1");
    expect(config.fileExplorerSort).toBe("name-desc");
    expect(config.excludeFolders).toEqual(["tmp"]);
    expect(config.attachmentFolder).toBe("素材");
    // key 随团队配置本体一起读回（服务端统一承载，不落本机 keychain）
    expect(config.providers).toEqual([
      { id: "p1", name: "A", baseUrl: "u", models: [], apiKey: "sk-1" },
    ]);
  });

  it("显式目标：本地仓库走 *_at 命令（root 透传），不读激活仓库的状态", async () => {
    const target = { kind: "local", root: "E:/其他仓库", name: "其他" } as const;
    await metadata.readVaultConfig(target);
    await metadata.patchVaultConfig({ model: "m2" }, target);
    await metadata.readAgents(target);
    await metadata.readPromptNotes(target);

    const at = (cmd: string) => h.state.localCalls.find((c) => c.cmd === cmd);
    expect(at("read_vault_config_at")?.args).toEqual({ root: "E:/其他仓库" });
    expect(at("vault_config_patch_at")?.args).toEqual({
      root: "E:/其他仓库",
      patch: { model: "m2" },
    });
    expect(at("read_agents_at")?.args).toEqual({ root: "E:/其他仓库" });
    expect(at("read_prompt_notes_at")?.args).toEqual({ root: "E:/其他仓库" });
    // 不带目标的调用仍走激活仓库命令
    expect(h.state.localCalls.some((c) => c.cmd === "read_vault_config")).toBe(false);
  });

  it("显式目标：协作空间按目标身份读写（无需先把该空间设为激活）", async () => {
    // 激活身份为空：目标必须自己带 serverUrl/spaceId
    const target = {
      kind: "space",
      serverUrl: "http://s2",
      spaceId: "sp2",
      name: "外部空间",
      role: "editor",
    } as const;
    space.state.teamValues["ai-model"] = JSON.stringify("m9");
    space.state.teamValues.sort = JSON.stringify("name-asc");

    const { config } = await metadata.readVaultConfig(target);
    expect(config.model).toBe("m9");
    expect(config.fileExplorerSort).toBe("name-asc");

    await metadata.patchVaultConfig({ excludeFolders: ["草稿"] }, target);
    expect(space.state.teamValues.exclusions).toBe(JSON.stringify(["草稿"]));
  });
});

describe("user meta 分发（chat/calendar/todos）", () => {
  beforeEach(() => {
    factory.activateContentIdentity(SPACE_IDENTITY);
  });

  it("会话消息写/读往返（键 chat/messages/<id>）", async () => {
    const file = ".atelyx/对话历史/s1.jsonl";
    await metadata.writeChatMessages(file, "{\"id\":\"m1\"}\n");

    expect(space.state.myValues["chat/messages/s1"]).toBe("{\"id\":\"m1\"}\n");
    expect(await metadata.readChatMessages(file)).toBe("{\"id\":\"m1\"}\n");
  });

  it("appendChatMessages = 读改写整文件：缺尾换行补分隔，内容拼在既有消息之后", async () => {
    space.state.myValues["chat/messages/s1"] = "{\"id\":\"m0\"}";
    const file = ".atelyx/对话历史/s1.jsonl";
    await metadata.appendChatMessages(file, [
      { id: "m1", role: "user", content: "hi", createdAt: 1 },
    ]);

    expect(space.state.myValues["chat/messages/s1"]).toBe(
      `{"id":"m0"}\n${JSON.stringify({ id: "m1", role: "user", content: "hi", createdAt: 1 })}\n`,
    );
  });

  it("读在途期间空间切换：在途追加被丢弃，不写新空间的 user meta", async () => {
    space.state.myValues["chat/messages/s1"] = "{\"id\":\"m0\"}\n";
    space.state.holdMyMeta = new Promise<void>((r) => (space.state.resolveGate = r));
    const file = ".atelyx/对话历史/s1.jsonl";

    const pending = metadata.appendChatMessages(file, [
      { id: "m1", role: "user", content: "hi", createdAt: 1 },
    ]);
    // 读在途：切到另一个空间
    factory.activateContentIdentity({ kind: "space", serverUrl: "http://s2", spaceId: "sp2" });
    space.state.resolveGate!();
    await pending;

    expect(space.state.myValues["chat/messages/s1"]).toBe("{\"id\":\"m0\"}\n");
    expect(space.state.calls.some((c) => c.method === "patchMyMeta")).toBe(false);
  });

  it("会话清单 = 扫 user meta 消息键，文件路径保持本地约定（.atelyx/对话历史/<id>.jsonl）", async () => {
    space.state.myValues["chat/messages/b.jsonl-x"] = "{\"id\":\"x\"}\n";
    space.state.myValues["chat/messages/a"] = "{\"id\":\"1\"}\n";
    space.state.myValues["chat/sessions/a"] = JSON.stringify({ id: "a", title: "标题" });
    space.state.myValues["别的键"] = "不相关";

    const rows = await metadata.listChatSessions();
    expect(rows.map((r) => r.id)).toEqual(["a", "b.jsonl-x"]);
    expect(rows[0].file).toBe(".atelyx/对话历史/a.jsonl");
    expect(rows[0].meta).toEqual({ id: "a", title: "标题" });
  });

  it("会话元数据侧车与面板覆盖往返（键 chat/sessions/<id> 与 chat/editor-meta）", async () => {
    const metaFile = ".atelyx/对话历史/s1.meta.json";
    await metadata.writeChatSessionMeta(metaFile, { id: "s1", title: "T" });
    expect(space.state.myValues["chat/sessions/s1"]).toBe(JSON.stringify({ id: "s1", title: "T" }));
    expect(await metadata.readChatSessionMeta(metaFile)).toEqual({ id: "s1", title: "T" });

    await metadata.writeEditorChatsMeta({
      schema: "atelyx-editor-chats-meta/v1",
      modelOverride: { providerId: "p1", model: "m1" },
      effortOverride: null,
    });
    const editorMeta = await metadata.readEditorChatsMeta();
    expect(editorMeta.modelOverride).toEqual({ providerId: "p1", model: "m1" });

    await metadata.deleteChatSessionMeta(metaFile);
    expect(space.state.myValues["chat/sessions/s1"]).toBeUndefined();
    await metadata.deleteChatMessages(".atelyx/对话历史/s1.jsonl");
    expect(space.state.myValues["chat/messages/s1"]).toBeUndefined();
  });

  it("日历往返：写 = user meta 键 calendar，读缺失返回 null", async () => {
    await metadata.writeCalendarRaw("{\"schema\":\"atelyx-calendar/v1\",\"items\":[]}");
    expect(space.state.myValues["calendar"]).toBe("{\"schema\":\"atelyx-calendar/v1\",\"items\":[]}");
    expect(await metadata.readCalendarRaw()).toBe("{\"schema\":\"atelyx-calendar/v1\",\"items\":[]}");

    space.state.myValues = {};
    expect(await metadata.readCalendarRaw()).toBeNull();
  });

  it("待办按会话 id 隔离：键 todos/<encodeURIComponent(id)>，删除幂等", async () => {
    await metadata.writeSessionTodosRaw("会话/1", "{\"todos\":[]}");
    expect(space.state.myValues[`todos/${encodeURIComponent("会话/1")}`]).toBe("{\"todos\":[]}");
    expect(await metadata.readSessionTodosRaw("会话/1")).toBe("{\"todos\":[]}");

    await metadata.deleteSessionTodosRaw("会话/1");
    expect(space.state.myValues[`todos/${encodeURIComponent("会话/1")}`]).toBeUndefined();
  });
});

describe("个人仓库直通（本地命令不变）", () => {
  beforeEach(() => {
    factory.activateContentIdentity({ kind: "local", root: "E:\\v1" });
  });

  it("promptNotes/folderColors/chat/config 走既有本地命令", async () => {
    await metadata.writePromptNotes(["a.md"]);
    await metadata.writeFolderColors({ "d/": "#000000" });
    await metadata.writeChatMessages(".atelyx/对话历史/s1.jsonl", "x\n");
    await metadata.writeChatSessionMeta(".atelyx/对话历史/s1.meta.json", { id: "s1" });
    await metadata.writeEditorChatsMeta({
      schema: "atelyx-editor-chats-meta/v1",
      modelOverride: null,
      effortOverride: null,
    });
    await metadata.writeCalendarRaw("{}");
    await metadata.writeSessionTodosRaw("s1", "{}");
    await metadata.deleteChatMessages(".atelyx/对话历史/s1.jsonl");
    await metadata.patchVaultConfig({ model: "m1" });

    const cmds = h.state.localCalls.map((c) => c.cmd);
    expect(cmds).toContain("write_prompt_notes");
    expect(cmds).toContain("write_folder_colors");
    expect(cmds).toContain("write_chat_messages");
    expect(cmds).toContain("write_chat_session_meta");
    expect(cmds).toContain("write_editor_chats_meta");
    expect(cmds).toContain("write_vault_file");
    expect(cmds).toContain("delete_chat_messages");
    expect(cmds).toContain("vault_config_patch");
    expect(space.state.calls.length).toBe(0);
  });
});
