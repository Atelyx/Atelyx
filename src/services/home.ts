/**
 * 主页面板数据 service（日历/仓库历史）：带日期笔记扫描 + 全仓库历史版本聚合。
 * 只读、尽力而为（缺失/损坏文件由后端跳过）。两处都经内容面按激活仓库身份分派：
 * 个人仓库 = 本地 Rust 命令，协作空间 = 服务端端点。
 * 响应类型定义在 `types/home.ts`（组件可用的类型来源，本文件按服务层职责 re-export）。
 */
import { getActiveContentBackend } from "@/services/content/factory";
import type { DatedNote, RepoHistoryResult } from "@/types";

export type { DatedNote, RepoHistoryEntry, DailyCount, RepoHistoryResult } from "@/types";

/** 扫描仓库 `.md` frontmatter 的 date/due 字段（自动进日历）。 */
export async function listDatedNotes(): Promise<DatedNote[]> {
  return getActiveContentBackend().listDatedNotes();
}

/** 聚合历史侧文件全部版本（版本流 + 按日计数）。 */
export async function listRepoHistory(): Promise<RepoHistoryResult> {
  return getActiveContentBackend().repoHistoryAggregate();
}
