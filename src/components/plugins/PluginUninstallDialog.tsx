/**
 * 插件卸载三选一对话框：保留配置卸载（默认）/ 彻底卸载（含配置）/ 取消。
 * 仅用于有落位目录的仓库/Git 来源插件；随应用分发行与本地链接行用普通确认框即可
 * （前者无磁盘数据可保留，后者数据在源目录内、卸载不受影响）。
 */
import { Button } from "@/components/common/Button";
import { ConfirmDialogFrame } from "@/components/common/ConfirmDialogFrame";

export function PluginUninstallDialog({
  title,
  description,
  onKeepData,
  onDeleteAll,
  onCancel,
}: {
  title: string;
  description: string;
  /** 保留配置卸载（默认）：插件数据搬到保留区，重装同 id 自动恢复。 */
  onKeepData: () => void;
  /** 彻底卸载：插件目录连同数据一并删除，不可恢复。 */
  onDeleteAll: () => void;
  onCancel: () => void;
}) {
  return (
    <ConfirmDialogFrame title={title} onCancel={onCancel}>
      <p className="text-xs mb-3 whitespace-pre-wrap break-words" style={{ color: "var(--text-muted)" }}>
        {description}
      </p>
      <div className="flex flex-col gap-2 mt-4">
        <Button variant="primary" size="sm" block onClick={onKeepData}>
          保留配置卸载（重装时自动恢复）
        </Button>
        <Button variant="dangerSolid" size="sm" block onClick={onDeleteAll}>
          彻底卸载（删除全部数据）
        </Button>
        <Button variant="ghost" size="sm" block onClick={onCancel}>
          取消
        </Button>
      </div>
    </ConfirmDialogFrame>
  );
}
