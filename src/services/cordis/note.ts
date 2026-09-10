/**
 * 笔记服务提供器（ctx.note）：由 builtin.note 行挂载时经 ctx.provide 提供。
 *
 * 实现 = 注入的笔记访问对象（pluginStore 接线填充，见 stores/pluginStore ensureNoteAccess）——
 * 停用/卸载 builtin.note 时服务随之消失（ctx.effect 撤销）。
 */
import { getPluginNoteAccess } from "./access";
import type { NoteService } from "./types";

/** 构造笔记服务（要求访问已接线：pluginStore.ensureNoteAccess 已填充）。 */
export function createNoteService(): NoteService {
  const access = getPluginNoteAccess();
  if (!access) throw new Error("笔记能力未就绪");
  return {
    currentFile: () => access.currentFile(),
    open: (file, title) => access.open(file, title),
    read: (file) => access.read(file),
    write: (content) => access.write(content),
    save: () => access.save(),
  };
}
