/**
 * 移动端本地仓库编排：存储授权查询/申请、私有回落目录、自研目录浏览的数据与状态。
 *
 * 命令在 `services/mobilePlatform`（桌面端一律返回可读错误）；本 store 是组件访问它的唯一入口——
 * 组件层不 import services（分层约束）。进入仓库经 `appStore.selectVault`（其失败已弹通知）。
 */
import { create } from "zustand";
import { useAppStore } from "@/stores/appStore";
import {
  hasAllFilesAccess,
  listAbsoluteDir,
  privateVaultPath,
  requestAllFilesAccess,
  storageRoot,
} from "@/services/mobilePlatform";
import { errText } from "@/types/tool";
import type { AbsoluteDirListing } from "@/types";

interface MobileVaultStore {
  /** 「所有文件访问权限」是否已授予（null = 尚未查询）。 */
  allFilesAccess: boolean | null;
  /** 目录浏览是否打开。 */
  browsing: boolean;
  /** 当前浏览到的目录（null = 尚未读到）。 */
  listing: AbsoluteDirListing | null;
  listingLoading: boolean;
  /** 权限申请/目录读取/私有目录进入的失败原因（可读）。 */
  error: string;
  /** 进入仓库进行中（按钮禁用）。 */
  busy: boolean;

  /** 查询存储授权（打开入口对话框、从系统设置返回时调用）。 */
  refreshPermission: () => Promise<void>;
  /** 拉起系统设置页申请存储授权（拉起即返回，需用户手动开启）。 */
  requestPermission: () => Promise<void>;
  /** 打开目录浏览（从文件系统根开始）。 */
  openBrowser: () => Promise<void>;
  /** 进入某个子目录。 */
  enterDir: (path: string) => Promise<void>;
  /** 关闭目录浏览（回到入口对话框）。 */
  closeBrowser: () => void;
  /** 把某个绝对路径作为仓库打开；返回是否成功。 */
  openVaultAt: (path: string) => Promise<boolean>;
  /** 用应用内部私有目录作为仓库打开；返回是否成功。 */
  openPrivateVault: () => Promise<boolean>;
  /** 清空浏览态与错误（关闭入口对话框时调用）。 */
  reset: () => void;
}

export const useMobileVaultStore = create<MobileVaultStore>()((set, get) => ({
  allFilesAccess: null,
  browsing: false,
  listing: null,
  listingLoading: false,
  error: "",
  busy: false,

  refreshPermission: async () => {
    try {
      // 成功即清错误：此前失败留下的文案不得在权限已开/已补齐后继续显示
      set({ allFilesAccess: await hasAllFilesAccess(), error: "" });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  requestPermission: async () => {
    set({ error: "" });
    try {
      await requestAllFilesAccess();
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  openBrowser: async () => {
    set({ browsing: true, listing: null, error: "" });
    // 起点按授权状态定：外部存储根可直接读；文件系统根（/）对应用不可读，不能作起点。
    // 浏览入口只在授权后出现，私有目录是异常路径的兜底。
    let start: string;
    try {
      start = get().allFilesAccess ? await storageRoot() : await privateVaultPath();
    } catch (e) {
      set({ error: errText(e) });
      return;
    }
    await get().enterDir(start);
  },

  enterDir: async (path) => {
    set({ listingLoading: true, error: "" });
    try {
      set({ listing: await listAbsoluteDir(path) });
    } catch (e) {
      // 读取失败即清空当前目录：否则错误提示会与上一级目录的路径/条目并存，误导用户
      set({ listing: null, error: errText(e) });
    } finally {
      set({ listingLoading: false });
    }
  },

  closeBrowser: () => set({ browsing: false, listing: null, error: "" }),

  openVaultAt: async (path) => {
    set({ busy: true, error: "" });
    try {
      return await useAppStore.getState().selectVault(path);
    } catch (e) {
      set({ error: errText(e) });
      return false;
    } finally {
      set({ busy: false });
    }
  },

  openPrivateVault: async () => {
    try {
      return await get().openVaultAt(await privateVaultPath());
    } catch (e) {
      set({ error: errText(e) });
      return false;
    }
  },

  reset: () => set({ browsing: false, listing: null, listingLoading: false, error: "", busy: false }),
}));
