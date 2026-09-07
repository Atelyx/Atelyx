# 发布插件

## 插件仓库结构

```
你的插件仓库/
├── atelyx.json        # 清单（见 manifest.md）
├── main.js            # 逻辑入口（main；也可为 main.ts / main.py）
├── ui.js              # 主线程 UI 入口（mainUi，可选；任何语言都可附 JS UI）
└── README.md          # 建议附使用说明
```

## 插件 = git 仓库

Atelyx 的安装一律「取源码」：市场安装会把你的仓库 git clone 到插件目录（本机无 git 时
回退下载 GitHub 自动生成的源码包）。因此**仓库最新提交即发布版本**，无需任何打包步骤。
约束：

- `atelyx.json` 必须位于**仓库根目录**（git clone 后以此为插件根）。
- JS/TS 入口单文件上限 16 MB（读入后整体注入 blob 的内存护栏，正常插件远低于此）；
  Python 子进程入口不经此限制。

## 进入市场

给仓库打 GitHub topic：`atelyx-plugin`。

市场聚合（官方索引 CI）每 6 小时扫描一次该 topic 的仓库并收录进市场索引。收录后，
所有 Atelyx 用户都可在内置市场搜到并安装你的插件。

> 打上 topic 即自动收录、零申请零审核。发布者对插件质量、安全与合规负全部责任。

## 发布检查清单

- [ ] `atelyx.json` 齐全：schemaVersion / id（反向域名、不可变）/ name / version / type / main
- [ ] `declares` 如实披露将调用的能力命名空间（`state`/`shell` 等宿主命名空间，或他插件反向域名；
  执行外部程序等敏感能力务必列出）
- [ ] 非 js 语言：`runtime` 正确（ts/python），入口文件在仓库内
- [ ] 功能类型正确（tool/panel/node/…），双平面入口（main/mainUi）指向正确文件
- [ ] `atelyxVersionMin` 与目标宿主版本匹配
- [ ] `atelyx.json` 位于仓库根，入口文件在仓库内
- [ ] 仓库已打 `atelyx-plugin` topic
- [ ] 自测：在 Atelyx 市场安装 → 启用 → 对应位置生效（工具可被模型调用、面板可打开等）

## 徽标

- **精选**：优质第三方插件可被授予精选徽标（按 `owner/repo` 提交到 `Xuhang944/Atelyx-plugin-index` 的 `endorsed.json`）。徽标是信任信号，不设上架门槛——任何插件都可以靠 topic 上架。
