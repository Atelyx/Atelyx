# 自定义槽位：让其他插件扩展你的界面

宿主已提供一整套具名槽位（工具栏、设置页区块、右键菜单等，见 [ctx API](ctx-api.md)）。
当你的插件想开放**自己渲染的界面区域**给其他插件贡献时，用 `ctx.slots.declare` 声明新槽，
再用 `ctx.slots.host` 在自己面板里承载这些贡献——槽位注册表是全局的，UI 扩展不再有宿主/插件分界。

## 最小示例

以插件 A（自绘面板）为例：插件自己渲染一个面板，希望其他插件能给面板工具栏加按钮
（如「导出」「统计」，由不同插件各自贡献）。

**插件 A（面板插件）——声明 + 承载：**

```ts
export default {
  async apply(ctx) {
    // 声明前缀槽：toolbar/com.example.panel/* 下任意具体槽都合法（如 …/export）
    ctx.slots.declare({
      key: "toolbar/com.example.panel",
      prefix: true,                // 放行前缀下任意槽；也可写精确 key（prefix 省略）
      cardinality: "list",         // list = 多贡献按 priority 有序；single = 高 priority 胜出
      required: ["component"],     // 载荷契约：贡献方必须给 component 才会被 host 渲染
    });

    // host 返回一个 React 组件，放在面板工具栏里即可
    const PanelToolbarSlot = ctx.slots.host("toolbar/com.example.panel/export");

    ctx.slots.registerView({
      kind: "com.example.panel",
      label: "我的面板",
      component: () => (
        <div className="my-panel">
          <div className="panel-toolbar">
            <PanelToolbarSlot />
          </div>
          {/* …面板主体… */}
        </div>
      ),
    });
  },
};
```

**插件 B（导出按钮）——贡献：**

```ts
ctx.slots.registerUi({ slot: "toolbar/com.example.panel/export", component: ExportButton });
```

`registerUi` 只传 `component`；声明方经 `host(slot)` 渲染贡献的 `component` 字段，
所以渲染类槽位应声明 `required: ["component"]`。

## 命名空间：先到先得

- key 未被宿主或他插件占用即可声明；**先到先得**，无优先级竞争。
- 与既有声明重叠即失败，并**指名占用者**：同名 key、声明前缀吞并他人已占的精确 key、
  声明落在他人前缀的覆盖集内（子前缀）、两侧前缀互相覆盖，全部按冲突处理。
- **宿主槽位受保护**：宿主已声明的固定槽与开放前缀（`view`/`node`/`edge`/`tableview`/`empty`/`inspector`）
  不能声明，与之重叠的宽前缀也不能声明——插件声明不了会吞并宿主座位的位置。

建议用反向域名式 key（如 `com.example.panel/controls`）降低撞名概率；撞名时错误信息会
给出占用者插件 id，可据此改名或与对方协商。

## 载荷契约

`required` / `optional` 声明贡献方必须/可以提供的字段（字段名 + 类型约定：`label` 为字符串、
`component`/`onClick` 为函数，其余字段按字面提供）。贡献方缺必需字段、带未知字段即失败，
该插件行标 failed + 可读原因，不静默。声明方经 `host(slot)` 渲染时读取贡献载荷渲染——
要能渲染出组件，契约里应包含 `required: ["component"]`。

## 生命周期

- 声明与贡献都随各自插件的 fiber 撤销：停用贡献方 → 按钮消失；停用声明方 → 其面板卸载，
  槽与贡献一并消失（贡献方不需要也不能清理别人声明的槽）。
- 停用声明方后，他插件再向该槽贡献会失败（「未声明的槽位」），不会静默落到无处渲染。
- 插件 A 的声明和 `host` 调用都在 `apply` 里（先 `declare` 后 `host`）；若要托管**其他插件**声明的槽，
  需该声明插件先挂载（装配顺序确定：随应用分发插件在前、已装插件按 id 追加）。

## 发现

`ctx.slots.list()` 返回宿主声明表与全部插件运行时声明的合并冻结视图——任何插件都能据此发现
当前可贡献的位置（key / 基数 / 载荷字段 / 用途）。
