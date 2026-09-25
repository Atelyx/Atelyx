/**
 * settingsStore 失效提示词清理测试（pruneMissingPromptNotes：进入 Agent 设置页触发的自愈动作）。
 *
 * 覆盖：注册列表与 Agent 引用中指向已删除笔记的路径被清除（存在的保留）；
 * 引用路径不在注册列表时同样纳入校验；存在性校验失败视为存在（不确定不删）；
 * 写盘失败内存不动；校验在途期间的注册变更不被清理覆盖（写盘前重取最新态）；
 * 非激活仓库的编辑会话跳过；空列表不产生任何查询；清理完成后轻通知。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

// ===== 本地命令替身 =====

const h = vi.hoisted(() => {
  const state = {
    vaultConfig: {} as Record<string, unknown>,
    globalConfig: {} as Record<string, unknown>,
    keychain: new Map<string, string>(),
    /** 磁盘上真实存在的文件相对路径集合（file_exists 依据）。 */
    existing: new Set<string>(),
    /** file_exists 调用记录。 */
    existsCalls: [] as string[],
    /** file_exists 强制抛错的路径（校验失败分支）。 */
    existsErrors: new Set<string>(),
    /** file_exists 挂起路径与放行函数（构造「校验在途」时序）。 */
    holdExists: null as string | null,
    releaseExists: null as (() => void) | null,
    /** write_prompt_notes 收到的列表。 */
    promptNoteWrites: [] as string[][],
    /** write_prompt_notes 强制失败。 */
    failPromptNotesWrite: false,
    /** write_agents 收到的列表。 */
    agentWrites: [] as unknown[],
    /** write_agents 强制失败。 */
    failAgentsWrite: false,
  };
  return { state };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    const a = args ?? {};
    switch (cmd) {
      case "read_vault_config":
        return { config: h.state.vaultConfig, corruptBackup: null };
      case "read_global_config":
        return { config: h.state.globalConfig, corruptBackup: null };
      case "patch_global_config":
        return { config: h.state.globalConfig, corruptBackup: null };
      case "write_global_config":
        return null;
      case "get_api_key":
        return h.state.keychain.get(`${String(a.vaultRoot)}:${String(a.providerId)}`) ?? "";
      case "set_api_key":
        h.state.keychain.set(`${String(a.vaultRoot)}:${String(a.providerId)}`, String(a.key));
        return null;
      case "delete_api_key":
        h.state.keychain.delete(`${String(a.vaultRoot)}:${String(a.providerId)}`);
        return null;
      case "read_prompt_notes":
      case "read_agents":
        return [];
      case "read_folder_colors":
        return {};
      case "file_exists": {
        const file = String(a.file);
        h.state.existsCalls.push(file);
        if (h.state.existsErrors.has(file)) throw new Error("读失败（注入）");
        if (h.state.holdExists === file) {
          await new Promise<void>((r) => {
            h.state.releaseExists = r;
          });
        }
        return h.state.existing.has(file);
      }
      case "write_prompt_notes":
        if (h.state.failPromptNotesWrite) throw new Error("写失败（注入）");
        h.state.promptNoteWrites.push(a.files as string[]);
        return null;
      case "write_agents":
        if (h.state.failAgentsWrite) throw new Error("写失败（注入）");
        h.state.agentWrites.push(a.agents);
        return null;
      default:
        return null;
    }
  },
}));

type SettingsStore = typeof import("./settingsStore");
type NotificationStore = typeof import("./notificationStore");

let settings: SettingsStore;
let notifications: NotificationStore;

const AGENT = (id: string, systemPromptFile?: string) => ({
  id,
  name: `Agent ${id}`,
  tools: [],
  ...(systemPromptFile ? { systemPromptFile } : {}),
});

beforeEach(async () => {
  vi.resetModules();
  h.state.vaultConfig = {};
  h.state.globalConfig = {};
  h.state.keychain = new Map();
  h.state.existing = new Set();
  h.state.existsCalls = [];
  h.state.existsErrors = new Set();
  h.state.holdExists = null;
  h.state.releaseExists = null;
  h.state.promptNoteWrites = [];
  h.state.failPromptNotesWrite = false;
  h.state.agentWrites = [];
  h.state.failAgentsWrite = false;
  await import("./noteSessionStore");
  await import("./pluginStore");
  await import("./appStore");
  await import("@/services/content/factory");
  settings = await import("./settingsStore");
  notifications = await import("./notificationStore");
});

/** 直填激活态（绕过 loadVaultConfig：清理动作只读这两个字段，加载链路另有测试覆盖）。 */
function seedActivation(promptNotes: string[], agents: ReturnType<typeof AGENT>[]): void {
  settings.useSettingsStore.setState({ promptNotes, agents, loaded: true });
}

function notificationMessages(): string[] {
  return notifications.useNotificationStore.getState().items.map((n) => n.message);
}

describe("pruneMissingPromptNotes 失效提示词清理", () => {
  it("注册列表与 Agent 引用中指向缺失笔记的路径被清除，存在的保留", async () => {
    h.state.existing = new Set(["存在.md"]);
    seedActivation(["存在.md", "丢失.md"], [AGENT("a1", "丢失.md"), AGENT("a2", "存在.md")]);

    await settings.useSettingsStore.getState().pruneMissingPromptNotes();

    const s = settings.useSettingsStore.getState();
    expect(s.promptNotes).toEqual(["存在.md"]);
    expect(s.agents[0].systemPromptFile).toBeUndefined();
    expect(s.agents[1].systemPromptFile).toBe("存在.md");
    expect(h.state.promptNoteWrites).toEqual([["存在.md"]]);
    expect(h.state.agentWrites).toHaveLength(1);
    expect(notificationMessages()).toHaveLength(1);
  });

  it("引用路径不在注册列表但笔记存在：保留不清（只清指向缺失路径的引用）", async () => {
    h.state.existing = new Set(["存在.md"]);
    seedActivation([], [AGENT("a1", "存在.md")]);

    await settings.useSettingsStore.getState().pruneMissingPromptNotes();

    const s = settings.useSettingsStore.getState();
    expect(s.agents[0].systemPromptFile).toBe("存在.md");
    expect(h.state.agentWrites).toHaveLength(0);
    expect(notificationMessages()).toHaveLength(0);
  });

  it("存在性校验失败视为存在（不确定缺失不得清理）", async () => {
    h.state.existsErrors = new Set(["坏.md"]);
    seedActivation(["坏.md"], [AGENT("a1", "坏.md")]);

    await settings.useSettingsStore.getState().pruneMissingPromptNotes();

    const s = settings.useSettingsStore.getState();
    expect(s.promptNotes).toEqual(["坏.md"]);
    expect(s.agents[0].systemPromptFile).toBe("坏.md");
    expect(h.state.promptNoteWrites).toHaveLength(0);
    expect(h.state.agentWrites).toHaveLength(0);
    expect(notificationMessages()).toHaveLength(0);
  });

  it("注册列表写盘失败：内存不动，Agent 引用也不清理", async () => {
    h.state.failPromptNotesWrite = true;
    seedActivation(["丢失.md"], [AGENT("a1", "丢失.md")]);

    await settings.useSettingsStore.getState().pruneMissingPromptNotes();

    const s = settings.useSettingsStore.getState();
    expect(s.promptNotes).toEqual(["丢失.md"]);
    expect(s.agents[0].systemPromptFile).toBe("丢失.md");
    expect(h.state.agentWrites).toHaveLength(0);
    expect(notificationMessages()).toHaveLength(0);
  });

  it("校验在途期间的注册变更不被清理覆盖（写盘前重取最新态应用缺失集合）", async () => {
    // 两个注册都缺失：file_exists 并发校验，a.md 挂起，挂起窗口内另一写者改内存
    h.state.holdExists = "a.md";
    seedActivation(["a.md", "b.md"], []);

    const pending = settings.useSettingsStore.getState().pruneMissingPromptNotes();
    await vi.waitFor(() => expect(h.state.releaseExists).not.toBeNull());
    // 模拟校验在途期间的其他写者：注销 b.md、新增 c.md（清理须基于该最新态写盘）
    settings.useSettingsStore.setState({ promptNotes: ["a.md", "c.md"] });
    h.state.releaseExists?.();
    await pending;

    const s = settings.useSettingsStore.getState();
    expect(s.promptNotes).toEqual(["c.md"]);
    expect(h.state.promptNoteWrites).toEqual([["c.md"]]);
  });

  it("非激活仓库的编辑会话跳过：不产生任何存在性查询与写盘", async () => {
    seedActivation(["丢失.md"], [AGENT("a1", "丢失.md")]);
    settings.useSettingsStore.setState({
      settingsSession: {
        id: 1,
        target: { kind: "local", root: "E:/other", name: "其他仓库" },
        loaded: true,
        error: null,
        readOnly: false,
        corruptBackup: null,
        vaultConfig: {},
        config: { providers: [] },
        searchConfig: { provider: "tavily", searxngUrl: "" },
        tavilyKey: "",
        agents: [],
        promptNotes: [],
        digest: "",
        keys: new Map(),
        strayTavilyKey: false,
        writeChain: Promise.resolve(),
        lastPatchError: "",
      },
    });

    await settings.useSettingsStore.getState().pruneMissingPromptNotes();

    expect(h.state.existsCalls).toHaveLength(0);
    expect(h.state.promptNoteWrites).toHaveLength(0);
    expect(h.state.agentWrites).toHaveLength(0);
    expect(settings.useSettingsStore.getState().promptNotes).toEqual(["丢失.md"]);
  });

  it("空注册列表且无 Agent 引用：不产生任何查询", async () => {
    seedActivation([], []);

    await settings.useSettingsStore.getState().pruneMissingPromptNotes();

    expect(h.state.existsCalls).toHaveLength(0);
  });
});
