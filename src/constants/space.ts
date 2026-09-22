/**
 * 协作空间 UI 门控常量。
 */

/** 空间内本账号为查看者时的写入口提示：团队层写入按角色裁决，查看者只能看。 */
export const SPACE_VIEWER_NOTICE = "你在该空间内是查看者，无法修改（由所有者或编辑者维护）";

/** 空间内团队共享配置的提示（Agent 与提示词库对所有成员生效）。 */
export const SPACE_TEAM_SHARED_NOTICE = "协作空间内 Agent 与提示词库为团队共享：改动对所有成员生效";

/** 空间内模型供应商与 API key 的归属提示（由所有者/编辑者统一配置，全员共用一份）。 */
export const SPACE_TEAM_AI_NOTICE =
  "空间的模型供应商与 API key 由所有者或编辑者统一配置，对所有成员生效";
