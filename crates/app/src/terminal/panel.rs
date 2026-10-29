//! 底部终端停靠面板（2026-11 用户定案）：内容区下沿通栏、自带标签条，
//! 支持多终端标签切换。**终端不再是内容区 topbar tab**——打开终端 =
//! 展开这块面板并在其中新开一个标签（`Chat::open_terminal`）。
//!
//! 版式（自上而下）：
//! - 上沿命中带 12px（§5.12 统一分隔线：常驻 1px / hover·拖拽 4px accent），
//!   拖动改面板高度（落盘 `UiState::term_h`）；
//! - 标签条 38px（`tokens::panel_ctrl::BAR_H`），tab 高 = 条高 −4（规范
//!   「bar 内 tab 高」）；激活 tab 底缘融进终端暗面；右侧两钮：新建终端、
//!   收起面板；
//! - 终端体（`TerminalElement`，网格尺寸随元素 bounds 自适应）。
//!
//! 收起 = 仅隐藏，PTY 保活（`Chat::toggle_term_dock`）；配色是固定暗面
//! （终端区所有主题一致，与内容区时代的终端视图同款）。

use gpui::{ElementId, MouseButton, MouseDownEvent, SharedString, div, prelude::*, px, rgb};

use crate::Chat;
use crate::terminal::{FONT_FAMILY, TERM_BG, TERM_CURSOR, TERM_FG};
use crate::ui::ScrollAxisExt;
use crate::ui::tokens::{divider as divider_tk, icon as icon_tk, panel_ctrl, radius, space};
use crate::ui::tooltip::WithTip;

/// 标签条底色（比终端体亮一档，仍是固定暗面）
const BAR_BG: u32 = 0x171b22;
/// 标签条文字（未激活）
const TAB_TEXT: u32 = 0x8b94a3;
/// 悬浮底（标签条内钮、未激活 tab）
const HOVER_BG: u32 = 0x232a34;
/// 退出横幅文字
const BANNER_FG: u32 = 0x6e8a7d;
/// 标签条图标钮盒 = 规范 §3 IB_SM(22)（tokens 未落点，按钮盒内联）
const BTN_BOX: f32 = 22.;
/// 标签 max 宽（同 topbar 自由 tab 的 200/300 两档取紧档：面板标签条更挤）
const TAB_MAX_W: f32 = 180.;

/// 面板整体（内容区 content-col 的最后一个 child）。
pub(crate) fn term_panel(
    chat: &mut Chat,
    weak: &gpui::WeakEntity<Chat>,
    cx: &mut gpui::Context<Chat>,
) -> gpui::AnyElement {
    let h = chat.layout.term.h;
    let active = chat.layout.term.hover || chat.layout.term.drag.is_some();
    let d_hit = divider_tk::HIT;
    // §5.12 统一分隔线：hover/拖拽状态位存进字段（不用 group_hover——后者
    // 受 has_active_drag 门控，粗细拿不到，见 ui/divider.rs 头注）
    let resizer = div()
        .id("term-dock-resizer")
        .with_tip("terminal.resize_tip")
        .h(px(d_hit))
        .flex_shrink_0()
        .relative()
        .cursor_row_resize()
        .on_hover(cx.listener(|this, h: &bool, _, cx| {
            if this.layout.term.hover != *h {
                this.layout.term.hover = *h;
                cx.notify();
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                if ev.click_count == 2 {
                    // 双击复位默认高度（与 dock 分区线同语义）
                    this.layout.term.h = crate::shell::chat::TERM_PANEL_H;
                    this.layout.term.drag = None;
                    this.persist_ui();
                } else {
                    let y: f32 = ev.position.y.into();
                    this.layout.term.drag = Some((y, this.layout.term.h));
                }
                cx.notify();
            }),
        )
        .child(crate::ui::divider::line(
            crate::ui::divider::LineSpec::horizontal(0., d_hit),
            active,
            crate::appearance::theme::theme(),
        ));

    let body = terminal_body(chat, weak, cx);

    div()
        .id("term-dock")
        .flex_shrink_0()
        .h(px(h))
        .flex()
        .flex_col()
        .bg(rgb(TERM_BG))
        .child(resizer)
        .child(term_bar(chat, cx))
        .child(body)
        .into_any_element()
}

/// 标签条：终端标签（横向可滚）+ 新建 / 收起两钮。
fn term_bar(chat: &mut Chat, cx: &mut gpui::Context<Chat>) -> gpui::AnyElement {
    let rows: Vec<(usize, SharedString, bool)> = chat
        .ws
        .terminals
        .iter()
        .enumerate()
        .map(|(ix, t)| {
            let label: SharedString = t
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| t.cwd.to_string_lossy().to_string())
                .into();
            (t.id, label, Some(ix) == chat.ws.active_terminal)
        })
        .collect();
    // `track_scroll` 挂句柄后 gpui 每帧记 child bounds → `activate_terminal`
    // 里的 `scroll_to_item` 才能把激活标签滚进视口（zed tab bar 同款）
    let mut host = div()
        .id("term-dock-tabs")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_end()
        .overflow_x_scroll()
        .track_scroll(&chat.layout.term.tabs_scroll)
        .restrict_scroll_to_axis();
    for (ix, (id, label, active)) in rows.into_iter().enumerate() {
        // 同一个 id 的关闭动作要挂两处（中键点标签、左键点 ×）：cx.listener
        // 返回不可克隆的 opaque 闭包，故分别构造
        let close_mid = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            this.close_terminal(id, cx);
        });
        let close_x = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            this.close_terminal(id, cx);
        });
        let tab = div()
            .id(ElementId::from(("term-tab", ix)))
            .flex_shrink_0()
            .max_w(px(TAB_MAX_W))
            .h(px(panel_ctrl::BAR_H - 4.))
            .flex()
            .items_center()
            .gap(px(space::SP2))
            // 左距对齐 topbar tab 的 12（`pl` 12 = align::ICON_LINE）
            .pl(px(space::SP5))
            .pr(px(space::SP1))
            .rounded_t(px(radius::CTRL))
            .text_size(crate::appearance::ui_size(12.))
            .cursor_pointer()
            .when(active, |d| d.bg(rgb(TERM_BG)).text_color(rgb(TERM_FG)))
            .when(!active, |d| {
                d.text_color(rgb(TAB_TEXT))
                    .hover(|s| s.bg(rgb(HOVER_BG)).text_color(rgb(TERM_FG)))
            })
            .on_click(cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
                this.activate_terminal(ix, window, cx);
            }))
            .on_mouse_down(MouseButton::Middle, close_mid)
            .child(
                div()
                    .flex_shrink_0()
                    .child(crate::ui::icon(
                        "icon-terminal-solid",
                        icon_tk::XS,
                        if active { TERM_CURSOR } else { TAB_TEXT },
                    )),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label),
            )
            // 行尾 ×（关标签）：盒 20 = topbar 关闭钮同档，图标 XS(12)
            .child(
                div()
                    .id(ElementId::from(("term-tab-x", ix)))
                    .flex_shrink_0()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(radius::XS))
                    .text_color(rgb(TAB_TEXT))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(HOVER_BG)).text_color(rgb(TERM_FG)))
                    .on_mouse_down(MouseButton::Left, close_x)
                    .child(crate::ui::icon("x", icon_tk::XS, TAB_TEXT)),
            );
        host = host.child(tab);
    }
    // 标签右余量吸收剩余横向空间（标签少时不贴到右钮上）
    host = host.child(div().flex_grow().h_full().child(""));

    div()
        .id("term-dock-bar")
        .h(px(panel_ctrl::BAR_H))
        .flex_shrink_0()
        .flex()
        .items_end()
        .bg(rgb(BAR_BG))
        .child(host)
        // 新建终端：以当前激活终端的 cwd 为落地目录（无终端 = 工作区）
        .child(bar_btn(
            "term-dock-new",
            "plus",
            "terminal.new_tab",
            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                let cwd = this
                    .ws
                    .active_terminal
                    .and_then(|ix| this.ws.terminals.get(ix))
                    .map(|t| t.cwd.clone())
                    .or_else(|| this.ws.cwd.clone());
                if let Some(cwd) = cwd {
                    this.open_terminal(Some(cwd), window, cx);
                }
            }),
        ))
        // 收起（隐藏面板，PTY 保活；Ctrl+` 再展开）
        .child(bar_btn(
            "term-dock-hide",
            "chevron-down",
            "terminal.hide",
            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                this.toggle_term_dock(window, cx);
            }),
        ))
        .into_any_element()
}

/// 标签条右侧图标钮：盒 22（IB_SM）、图标 14（ICON_SM），底缘留 4 对齐
/// 标签底缘。
fn bar_btn(
    id: &'static str,
    icon_name: &'static str,
    tip: &'static str,
    handler: impl Fn(&MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl gpui::IntoElement {
    div()
        .id(id)
        .with_tip(tip)
        .size(px(BTN_BOX))
        .flex_shrink_0()
        .mb(px(4.))
        .mr(px(space::SP1))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::XS))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(HOVER_BG)))
        .on_mouse_down(MouseButton::Left, handler)
        .child(crate::ui::icon(icon_name, icon_tk::SM, TAB_TEXT))
}

/// 终端体：退出横幅（进程已亡时）+ 终端网格。网格尺寸由 TerminalElement
/// 在 prepaint 里按元素 bounds 自适应——面板拖动改高无需额外处理。
fn terminal_body(
    chat: &mut Chat,
    weak: &gpui::WeakEntity<Chat>,
    cx: &mut gpui::Context<Chat>,
) -> gpui::AnyElement {
    let Some(ix) = chat.shown_terminal_index() else {
        return div().flex_1().min_h_0().bg(rgb(TERM_BG)).into_any_element();
    };
    let tab = &chat.ws.terminals[ix];
    let mut col = div().flex_1().min_h_0().flex().flex_col().bg(rgb(TERM_BG));
    match &tab.status {
        crate::terminal::TermStatus::Exited(code) => {
            let code_text = code
                .map(|c| c.to_string())
                .unwrap_or_else(|| crate::i18n::tr("exit.unknown").to_string());
            col = col.child(
                div()
                    .py(px(space::SP2))
                    .px(px(space::SP3))
                    .text_size(crate::appearance::ui_size(11.))
                    .font_family(FONT_FAMILY)
                    .text_color(rgb(BANNER_FG))
                    .child(SharedString::from(crate::i18n::tf(
                        "exit.process_exited",
                        &[("code_text", code_text)],
                    ))),
            );
        }
        crate::terminal::TermStatus::Ready => {}
    }
    col.child(
        div()
            .flex_1()
            .min_h_0()
            .bg(rgb(TERM_BG))
            .child(
                crate::terminal::TerminalElement::new(tab, weak.clone())
                    .blink_on(chat.ws.term_cursor_on)
                    .track_focus(&tab.focus)
                    .flex_1()
                    .h_full()
                    .on_mouse_down(MouseButton::Left, {
                        let f = tab.focus.clone();
                        move |_, window, _cx| {
                            window.focus(&f);
                        }
                    })
                    .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _w, cx| {
                        this.terminal_key(ev, cx);
                    })),
            ),
    )
    .into_any_element()
}
