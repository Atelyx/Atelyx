# 发布插件

## 插件仓库结构

```
你的插件仓库/
├── package.json        # 清单（npm 标准字段 + atelyx 块，见 manifest.md）
├── index.ts            # 入口（main；默认导出 apply(ctx)，见 ctx-api.md）
└── README.md           # 建议附使用说明
```

## 插件 = git 仓库

Atelyx 的安装一律「取源码」：市场安装会把你的仓库 git clone 到插件目录（本机无 git 时
回退下载 GitHub 自动生成的源码包）。因此**仓库最新提交即发布版本**，无需任何打包步骤。
约束：

- `package.json` 必须位于**仓库根目录**（git clone 后以此为插件根）。

## 进入市场

给仓库打 GitHub topic：`atelyx-plugin`。

市场聚合（官方索引 CI）每 6 小时扫描一次该 topic 的仓库并收录进市场索引。收录后，
所有 Atelyx 用户都可在内置市场搜到并安装你的插件。

> 打上 topic 即自动收录、零申请零审核。发布者对插件质量、安全与合规负全部责任。

## 发布检查清单

- [ ] `package.json` 齐全：name（反向域名、不可变）/ version / main / atelyx.type
- [ ] `declares` 如实披露将访问的服务（`table`/`vault`/`shell` 等；敏感服务务必列出）
- [ ] 入口为 `.js`/`.ts`/`.tsx`，默认导出 `apply(ctx)`，自包含（无运行时 import）
- [ ] UI 注册经 `ctx.slots`（视图/表格视图），订阅/接线经 `ctx.effect` 包裹
- [ ] `atelyxVersionMin` 与目标宿主版本匹配
- [ ] `package.json` 位于仓库根，入口文件在仓库内
- [ ] 仓库已打 `atelyx-plugin` topic
- [ ] 自测：在 Atelyx 市场安装 → 启用 → 对应位置生效（面板可打开、表格视图可切换等）

## 徽标

- **精选**：优质第三方插件可被授予精选徽标（按 `owner/repo` 提交到官方索引仓库的
  `endorsed.json`）。徽标是信任信号，不设上架门槛——任何插件都可以靠 topic 上架。
