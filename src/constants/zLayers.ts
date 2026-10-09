/** 模态浮层层级表（单一事实源）。画布内浮层（PopupLayer）在 1100+ 段，与此表互不重叠。 */
export const Z_LAYERS = {
  /** 文件历史弹窗。 */
  historyModal: 90,
  /** 表格图片灯箱全屏遮罩（其内部右键菜单为 contextMenu）。 */
  lightbox: 100,
  /** 右键菜单（须高过灯箱遮罩）。 */
  contextMenu: 110,
  /** 插件详情弹窗（可从右键菜单路径打开，须高过右键菜单段）。 */
  pluginDetails: 180,
  /** 移动端全屏对话框。 */
  mobileDialog: 185,
  /** 通用模态弹窗（确认框、成员/邀请管理等）。 */
  dialog: 200,
} as const;
