/**
 * 笔记换路跟随：本地改名/移动后打开的笔记切到新路径，协作对端改名后同样跟随。
 *
 * 落点是最易错的一环——工作区联动（`ProjectWorkspacePage`）由「打开的笔记从列表消失」触发：
 * 它按重命名记录把面板切到新文件，查不到记录就判成外部删除并关掉面板（关掉后 `currentNoteFile`
 * 为空、effect 早退，再也不会回到新文件）。本测试按其口径装一个等价 effect 替身，
 * 使「列表刷新那一刻重命名记录是否已就位」这一时序可断言（记录晚于列表刷新即失败）。
 *
 * 读写 .md 经 Tauri 命令：`list_vault_tree` 喂入当前假磁盘的树，`rename_note` 返回链接改写清单。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({ tree: [] as unknown[] }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string) => {
    if (cmd === "list_vault_tree") return h.tree;
    if (cmd === "rename_note") return { rewritten: [] };
    return "";
  },
}));

import { lastFolderRenameTarget, lastNoteRenameTarget, useVaultStore } from "@/stores/vaultStore";
import { useAppStore } from "@/stores/appStore";
import { registerNoteCollabWiring } from "@/stores/noteCollabStore";
import { dispatchCollabChannel } from "@/utils/collabHost";
import { bytesToBase64 } from "@/utils/base64";
import { isCollabNoteRelocatePath } from "@/utils/noteCollabRelocate";
import { noteTitleFromFile } from "@/utils/filename";
import { encodeNoteRelocate } from "@/services/noteCollab/frame";

/** 假磁盘：仓库根下这些 .md（path 即相对路径）。 */
function treeOf(files: string[]): unknown[] {
  return files.map((file) => ({
    name: file,
    path: file,
    isDir: false,
    updatedAt: 0,
    children: [],
  }));
}

/** 工作区联动 effect 替身（口径同 ProjectWorkspacePage）：列表变化且打开的笔记不在列表 →
 *  按重命名记录切到新文件，记录缺失则视为外部删除关掉面板。 */
function installWorkspaceFollowEffect(): () => void {
  const unsub = useVaultStore.subscribe((s, prev) => {
    if (s.noteList === prev.noteList) return;
    const app = useAppStore.getState();
    const current = app.currentNoteFile;
    if (!current) return;
    if (s.noteList.some((n) => n.file === current)) return;
    const newFile = lastNoteRenameTarget(current) ?? lastFolderRenameTarget(current);
    if (newFile) app.openNote(newFile, noteTitleFromFile(newFile));
    else app.closeNote();
  });
  return unsub;
}

/** 装好假磁盘并把仓库状态置为「正打开 oldFile」。 */
async function openNoteAt(oldFile: string): Promise<void> {
  h.tree = treeOf([oldFile]);
  await useVaultStore.getState().loadFiles();
  useAppStore.setState({ currentNoteFile: oldFile, currentNoteTitle: noteTitleFromFile(oldFile) });
}

let offFollow: () => void;

beforeEach(() => {
  offFollow = installWorkspaceFollowEffect();
});

afterEach(() => {
  offFollow();
  useAppStore.getState().closeNote();
  useVaultStore.setState({ tree: [], noteList: [], tableList: [] });
});

describe("本地改名/移动", () => {
  it("改名后列表刷新时重命名记录已就位，打开的笔记切到新文件", async () => {
    await openNoteAt("笔记/a.md");
    h.tree = treeOf(["笔记/新名.md"]);

    await useVaultStore.getState().renameNote("笔记/a.md", "新名");

    expect(useAppStore.getState().currentNoteFile).toBe("笔记/新名.md");
    expect(useAppStore.getState().currentNoteTitle).toBe("新名");
    expect(lastNoteRenameTarget("笔记/a.md")).toBe("笔记/新名.md");
  });

  it("移动后同样跟随（新目录下的新路径）", async () => {
    await openNoteAt("a.md");
    h.tree = treeOf(["子/a.md"]);

    await useVaultStore.getState().moveNote("a.md", "子");

    expect(useAppStore.getState().currentNoteFile).toBe("子/a.md");
  });

  it("真删除（列表消失且无重命名记录）仍关闭面板", async () => {
    await openNoteAt("a.md");
    h.tree = treeOf([]);

    await useVaultStore.getState().loadFiles();

    expect(useAppStore.getState().currentNoteFile).toBeNull();
  });
});

describe("协作对端换路", () => {
  let offWiring: () => void;

  beforeEach(() => {
    offWiring = registerNoteCollabWiring();
  });

  afterEach(() => {
    offWiring();
  });

  it("收到换路帧：本端打开的同一笔记跟随到新路径，并抑制 watcher 回波", async () => {
    await openNoteAt("笔记/a.md");
    h.tree = treeOf(["笔记/新名.md"]);

    dispatchCollabChannel(
      "note-sync",
      7,
      "笔记/a.md",
      bytesToBase64(encodeNoteRelocate("笔记/a.md", "笔记/新名.md")),
    );

    await vi.waitFor(() =>
      expect(useAppStore.getState().currentNoteFile).toBe("笔记/新名.md"),
    );
    expect(useAppStore.getState().currentNoteTitle).toBe("新名");
    expect(lastNoteRenameTarget("笔记/a.md")).toBe("笔记/新名.md");
    // watcher 随后仍会报旧路径变化/新路径出现：窗口内按帧的结果跳过外部修改处理
    expect(isCollabNoteRelocatePath("笔记/a.md")).toBe(true);
    expect(isCollabNoteRelocatePath("笔记/新名.md")).toBe(true);
  });

  it("通道与载荷路径不符的帧丢弃（陈旧/串文件载荷不动本端）", async () => {
    await openNoteAt("隔离/旧.md");
    h.tree = treeOf(["隔离/新.md"]);

    dispatchCollabChannel(
      "note-sync",
      7,
      "其它.md",
      bytesToBase64(encodeNoteRelocate("隔离/旧.md", "隔离/新.md")),
    );

    await Promise.resolve();
    expect(useAppStore.getState().currentNoteFile).toBe("隔离/旧.md");
    expect(isCollabNoteRelocatePath("隔离/旧.md")).toBe(false);
  });

  it("本端未打开该笔记：路径身份跟上但不谈跟随切换", async () => {
    h.tree = treeOf(["其它.md"]);
    await useVaultStore.getState().loadFiles();
    await useVaultStore.getState().renameNote("其它.md", "另一个");

    dispatchCollabChannel(
      "note-sync",
      7,
      "笔记/a.md",
      bytesToBase64(encodeNoteRelocate("笔记/a.md", "笔记/新名.md")),
    );

    await vi.waitFor(() => expect(lastNoteRenameTarget("笔记/a.md")).toBe("笔记/新名.md"));
    expect(useAppStore.getState().currentNoteFile).toBeNull();
  });
});
