/**
 * 移动端本地仓库的数据契约（自研目录浏览与系统能力命令的返回形状，
 * 命令对应 `src-tauri/src/commands/mobile.rs`）。
 */

/** 目录浏览条目（只列子目录）。 */
export interface AbsoluteDirEntry {
  name: string;
  /** 子目录绝对路径（规范化后） */
  path: string;
  /** 能否进入（权限不足的目录为 false，UI 置灰不可点） */
  readable: boolean;
}

/** 列出子目录的结果。 */
export interface AbsoluteDirListing {
  /** 规范化后的当前目录绝对路径 */
  path: string;
  /** 上一级目录（文件系统根为 null） */
  parent: string | null;
  entries: AbsoluteDirEntry[];
}
