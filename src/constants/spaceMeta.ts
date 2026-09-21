/**
 * 协作空间团队元数据键（服务端按「键 → 字符串值」存，无键白名单）。
 *
 * 团队层 = 全员共享的仓库级设定，与个人仓库 `.atelyx/` 下的对应文件一一对应；写权限由服务端按角色
 * 裁决（owner/editor 可写，viewer 拒绝）。键名在此一处定义：元数据分发层（读写）与内容后端
 * （消费附件夹设定）共用，防两处漂移。
 */

export const SPACE_TEAM_META = {
  /** AI 配置本体（团队共享，按字段分键：一次改动只写自己那一个键，与其它字段互不覆盖）：
   *  供应商数组（含 apiKey）/ 默认模型 / 默认模型所属供应商 / 话题自动命名开关与模型 / 搜索源（含 tavilyApiKey）。 */
  aiProviders: "ai-providers",
  aiModel: "ai-model",
  aiModelProvider: "ai-model-provider",
  aiAutoNamingEnabled: "ai-auto-naming-enabled",
  aiAutoNamingModel: "ai-auto-naming-model",
  aiSearch: "ai-search",
  /** 文件面板排序方式（对应 config.json 的 `fileExplorerSort`）。 */
  sort: "sort",
  /** 排除文件夹（对应 config.json 的 `excludeFolders`）。 */
  exclusions: "exclusions",
  /** 附件导入默认文件夹（对应 config.json 的 `attachmentFolder`）。 */
  attachmentFolder: "attachment-folder",
  /** 文件夹图标颜色（对应 `.atelyx/folder-colors.json`）。 */
  folderColors: "folder-colors",
  /** 系统提示词标记（对应 `.atelyx/prompt-notes.json`）。 */
  promptNotes: "prompt-notes",
  /** Agent 配置（对应 `.atelyx/agents.json`）。 */
  agents: "agents",
} as const;

/**
 * 团队元数据里的标量取值：写入侧统一 `JSON.stringify`，但服务端可被手工改动，
 * 故裸串（非 JSON）也照原样取用（解析不出即当空）。
 *
 * 只有标量（附件夹这类字符串）用本函数；数组/对象（sort/exclusions/folder-colors 等）走严格 JSON 解析，
 * 形状不对时回落默认值比取到半个对象更安全。
 */
export function spaceMetaScalar(raw: string | undefined): string {
  if (raw === undefined) return "";
  try {
    const value: unknown = JSON.parse(raw);
    return typeof value === "string" ? value : "";
  } catch {
    return raw;
  }
}
