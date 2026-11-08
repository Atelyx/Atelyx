//! keyed_column 对账测试：同 key 位置的控件种类在两次 diff 间变化时，
//! 复用子树必须经 tag 校验整树重建，而不是把旧 State 留给新部件（下一帧 downcast panic）。
//! 机制说明见 `patches/README.md`。

use iced::advanced::text::Renderer as TextRenderer;
use iced::advanced::widget::tree::Tag;
use iced::advanced::widget::Tree;
use iced::widget::{button, keyed, text, text_input};
use iced::{Element, Theme};

type Elem<'a> = Element<'a, (), Theme, iced::Renderer>;
/// `text_input()` 实际使用的段落实现（其 State 按它参数化）。
type InputState = <iced::Renderer as TextRenderer>::Paragraph;

fn base<'a>() -> Elem<'a> {
    let a: Elem<'a> = text("x").into();
    let c: Elem<'a> = button("y").into();
    keyed::Column::new().push("a", a).push("c", c).into()
}

fn with_middle_input<'a>() -> Elem<'a> {
    let a: Elem<'a> = text("x").into();
    let b: Elem<'a> = text_input("", "").into();
    let c: Elem<'a> = button("y").into();
    keyed::Column::new().push("a", a).push("b", b).push("c", c).into()
}

fn without_input<'a>() -> Elem<'a> {
    let a: Elem<'a> = text("x").into();
    let b: Elem<'a> = text("y").into();
    let c: Elem<'a> = button("y").into();
    keyed::Column::new().push("a", a).push("b", b).push("c", c).into()
}

#[test]
fn keyed_diff_rebuilds_subtree_when_kind_changes_under_same_key() {
    let element = base();
    let mut tree = Tree::new(&element);

    // 中间插入带状态的 text_input：新 key b 的槽位必须是 text_input 的树
    let element = with_middle_input();
    tree.diff(&element);

    let middle = &tree.children[1];
    assert_eq!(
        middle.tag,
        Tag::of::<text_input::State<InputState>>(),
        "新 key 槽位应为 text_input 的树"
    );
    // downcast 成功即 state 类型正确；错配时此处直接 panic
    let _ = middle.state.downcast_ref::<text_input::State<InputState>>();

    // 同 key「换类型」：b 槽 text_input → 纯文本（无状态），子树必须整树重建
    // （重建的判据 = tag 不再是 text_input 的 State；带旧 State 复用会保留原 tag）
    let element = without_input();
    tree.diff(&element);

    let middle = &tree.children[1];
    assert_ne!(
        middle.tag,
        Tag::of::<text_input::State<InputState>>(),
        "text_input 换成 text 后子树应整树重建，不得残留旧 State"
    );
}
