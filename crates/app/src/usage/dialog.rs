//! 「用量统计」弹窗：每天 × 每模型的 token / 缓存 / 费用明细。
//!
//! 窗体复用 `ui::overlay::big_card`（设置弹窗同款 0.7×0.98 大卡），顶部固定
//! 汇总条（四格：总量 / 输入输出 / 缓存 / 费用），下方滚动区按日期降序排列
//! 每日卡片，卡片内每个模型一行（输入·输出·缓存·费用·调用次数）。
//!
//! 数据在 [`Chat::open_usage_stats`] 里后台线程算好（见 `usage/mod.rs`），
//! 这里只做渲染——弹窗每帧零 IO。

use gpui::{AnyElement, SharedString, div, prelude::*, px, rgb};

use super::{day_matches, fmt_cost, fmt_hit_rate, fmt_tokens};
use crate::Chat;
use crate::appearance::theme::{Theme, border_alpha};
use crate::editor::markdown::MONO_FAMILY;
use crate::i18n::tr;
use crate::services::format::fmt_thousand;
use pi_link::usage_stats::{UsageDay, UsageEntry, UsageStats};

/// 数值列宽（明细表六列对齐的单一来源）。
const NUM_W: f32 = 84.;

/// 一次最多画多少天（超出部分提示见页脚；几百天全画会把单帧变重）。
const MAX_DAYS: usize = 120;

/// 单日单模型最多画多少条会话明细（超长日期卡片也不至于拖帧；同模型下
/// 会话按费用/token 降序，砍尾不砍头）。
const MAX_SESSIONS_PER_DAY: usize = 50;

/// 弹窗整体（含浮层外壳）。
pub(crate) fn usage_stats_dialog(
    chat: &Chat,
    weak: &gpui::WeakEntity<Chat>,
    t: &Theme,
) -> AnyElement {
    let weak_dismiss = weak.clone();
    let dismiss = move |_w: &mut gpui::Window, cx: &mut gpui::App| {
        let _ = weak_dismiss.update(cx, |c, cx| {
            c.dlg.dialog = None;
            cx.notify();
        });
    };
    let body = dialog_body(chat, weak, t);
    crate::ui::overlay::layer(true, Some(&chat.dlg.dialog_focus), dismiss.clone())
        .flex()
        .items_center()
        .justify_center()
        .child(crate::ui::overlay::big_card(
            tr("usage.stats_title"),
            None,
            body,
            t,
            dismiss,
        ))
        .into_any_element()
}

/// 弹窗内容：加载态 / 空态 / 汇总条 + 搜索行 + 明细滚动区。
fn dialog_body(chat: &Chat, weak: &gpui::WeakEntity<Chat>, t: &Theme) -> AnyElement {
    let ui = &chat.aux.usage;
    if let Some(stats) = ui.stats.as_deref()
        && !stats.is_empty()
    {
        return div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(summary_block(stats, t))
            .child(search_row(chat, weak, t))
            .child(days_list(stats, chat, weak, t))
            .into_any_element();
    }
    let text = if ui.loading {
        tr("usage.loading")
    } else {
        tr("usage.empty")
    };
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .text_size(crate::appearance::ui_size(12.))
        .text_color(rgb(t.text_faint))
        .italic()
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

/// 顶部固定汇总条：总量 / 输入输出 / 缓存 / 费用 四格。
fn summary_block(stats: &UsageStats, t: &Theme) -> AnyElement {
    let tot = &stats.totals;
    let cells = vec![
        (
            tr("usage.total_tokens").to_string(),
            Some(fmt_tokens(tot.tokens())),
            format!("{} {}", fmt_thousand(tot.calls), tr("usage.calls")),
        ),
        (
            tr("usage.io").to_string(),
            Some(format!(
                "↑{} ↓{}",
                fmt_tokens(tot.input),
                fmt_tokens(tot.output)
            )),
            format!(
                "{} {} · {} {}",
                tr("stats.input"),
                fmt_thousand(tot.input),
                tr("stats.output"),
                fmt_thousand(tot.output)
            ),
        ),
        (
            tr("usage.cache").to_string(),
            Some(fmt_tokens(tot.cache_tokens())),
            match fmt_hit_rate(tot.cache_hit_rate()) {
                Some(r) => format!(
                    "{} {} · {} {}",
                    tr("stats.cache_read"),
                    fmt_thousand(tot.cache_read),
                    tr("stats.cache_hit_rate"),
                    r
                ),
                None => format!(
                    "{} {} · {} {}",
                    tr("stats.cache_read"),
                    fmt_thousand(tot.cache_read),
                    tr("stats.cache_write"),
                    fmt_thousand(tot.cache_write)
                ),
            },
        ),
        (
            tr("stats.total_cost").to_string(),
            Some(fmt_cost(tot.cost)),
            format!("{} {}", stats.files, tr("usage.files")),
        ),
    ];
    div()
        .flex_shrink_0()
        .flex()
        .gap(px(12.))
        .pt(px(14.))
        .pb(px(12.))
        .px(px(30.))
        .border_b_1()
        .border_color(gpui::rgba(border_alpha(t, 0x40)))
        .children(cells.into_iter().map(|(label, value, sub)| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(crate::appearance::ui_size(11.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(t.text_dim))
                        .child(SharedString::from(label)),
                )
                .child(
                    div()
                        .text_size(crate::appearance::ui_size(16.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(t.text))
                        .child(SharedString::from(value.unwrap_or_default())),
                )
                .child(
                    div()
                        .text_size(crate::appearance::ui_size(11.))
                        .text_color(rgb(t.text_faint))
                        .child(SharedString::from(sub)),
                )
                .into_any_element()
        }))
        .into_any_element()
}

/// 表头（六列）——与 [`entry_row`] 同列宽。
fn column_header(t: &Theme) -> AnyElement {
    let head = |label: &str, w: Option<f32>| {
        let mut d = div()
            .text_size(crate::appearance::ui_size(11.))
            .text_color(rgb(t.text_faint))
            .child(SharedString::from(label.to_string()));
        if let Some(w) = w {
            d = d.w(px(w)).text_right();
        }
        d
    };
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .pb(px(6.))
        .child(head(tr("usage.model"), None).flex_1().min_w_0())
        .child(head(tr("stats.input"), Some(NUM_W)))
        .child(head(tr("stats.output"), Some(NUM_W)))
        .child(head(tr("usage.cache"), Some(NUM_W)))
        .child(head(tr("stats.cost"), Some(NUM_W)))
        .child(head(tr("usage.calls"), Some(NUM_W)))
        .into_any_element()
}

/// 搜索行（091-1）：日期 / 模型 关键词过滤。输入框实体在
/// `open_usage_stats` 里懒建（TextInput::new 不需 Window），这里只挂树；
/// on_change 镜像词 + notify 重画；清空后滚动位复位。
fn search_row(chat: &Chat, weak: &gpui::WeakEntity<Chat>, t: &Theme) -> AnyElement {
    let weak_clear = weak.clone();
    // 弹窗搜索行：输入框是 ui::TextInput 实体（kit 无对应件），几何沿用顶部
    // 汇总条同款边距 + 规范字号档
    // ui-raw: ok 弹窗搜索行（kit 无 TextInput 组合件，边距取规范档）
    div()
        // ui-raw: ok 弹窗搜索行（kit 无 TextInput 组合件，边距取规范档）
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(30.))
        .py(px(8.))
        .border_b_1()
        .border_color(gpui::rgba(border_alpha(t, 0x40)))
        .child(
            div()
                .text_size(crate::appearance::ui_size(11.))
                .text_color(rgb(t.text_faint))
                .child(SharedString::from(tr("usage.search"))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(match chat.aux.usage.search_input.as_ref() {
                    Some(ent) => ent.clone().into_any_element(),
                    None => div().into_any_element(),
                }),
        )
        .when(!chat.aux.usage.filter.is_empty(), |d| {
            d.child(
                div()
                    .cursor_pointer()
                    .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                        let _ = weak_clear.update(cx, |c, cx| {
                            c.aux.usage.filter.clear();
                            if let Some(inp) = c.aux.usage.search_input.as_mut() {
                                inp.update(cx, |i, cx| i.set_value(String::new(), cx));
                            }
                            c.aux
                                .usage
                                .scroll
                                .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
                            cx.notify();
                        });
                    })
                    .text_size(crate::appearance::ui_size(11.))
                    .text_color(rgb(t.text_dim))
                    .child(SharedString::from(tr("usage.clear"))),
            )
        })
        .into_any_element()
}

/// 会话明细行（091-1）：一天内每个会话一行，点击 → 跳进对应会话。
fn entry_row(entry: &UsageEntry, weak: &gpui::WeakEntity<Chat>, t: &Theme) -> AnyElement {
    let label = entry
        .file
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| entry.file.to_string_lossy().to_string());
    let num = |text: String| {
        div()
            .w(px(NUM_W))
            .text_right()
            .text_size(crate::appearance::ui_size(11.5))
            .text_color(rgb(t.text_faint))
            .child(SharedString::from(text))
    };
    // 弹窗明细列表行：几何复刻原 model_row 列宽（NUM_W 单一来源），hover
    // 手法与消息轮 turn.rs 一致（text_color 提亮），无 kit 按钮件可复用
    div()
        .id(gpui::ElementId::Name(
            format!("usage-entry-{}", entry.file.to_string_lossy()).into(),
        ))
        // ui-raw: ok 弹窗明细行（kit 无此组合件，列宽 NUM_W 单一来源）
        .flex()
        .items_center()
        .gap(px(8.))
        .py(px(3.))
        .pl(px(16.))
        .cursor_pointer()
        .text_size(crate::appearance::ui_size(11.))
        .text_color(rgb(t.text_faint))
        .hover(|s| s.text_color(rgb(t.text_muted)))
        .on_mouse_down(gpui::MouseButton::Left, {
            let file = entry.file.clone();
            let weak = weak.clone();
            move |_, _, cx| {
                let _ = weak.update(cx, |c, cx| c.goto_usage_session(file.clone(), cx));
            }
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .font_family(MONO_FAMILY)
                .child(SharedString::from(format!(
                    "{} · {}",
                    tr("usage.jump"),
                    label
                ))),
        )
        .child(num(fmt_thousand(entry.totals.input)))
        .child(num(fmt_thousand(entry.totals.output)))
        .child(num(fmt_thousand(entry.totals.cache_tokens())))
        .child(num(fmt_cost(entry.totals.cost)))
        .child(num(fmt_thousand(entry.totals.calls)))
        .into_any_element()
}

/// 一天的卡片：日期 + 当日合计 + 该日各会话明细（可点击跳转）。
fn day_card(day: &UsageDay, weak: &gpui::WeakEntity<Chat>, t: &Theme) -> AnyElement {
    let day_line = |label: &str, val: String| format!("{label} {val}");
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .py(px(10.))
        .border_b_1()
        .border_color(gpui::rgba(border_alpha(t, 0x30)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .text_size(crate::appearance::ui_size(12.5))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(t.text))
                        .child(SharedString::from(day.date.clone())),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(crate::appearance::ui_size(11.))
                        .text_color(rgb(t.text_faint))
                        .child(SharedString::from(
                            [
                                day_line("↑", fmt_tokens(day.totals.input)),
                                day_line("↓", fmt_tokens(day.totals.output)),
                                day_line(tr("usage.cache"), fmt_tokens(day.totals.cache_tokens())),
                                fmt_cost(day.totals.cost),
                                format!("{} {}", fmt_thousand(day.totals.calls), tr("usage.calls")),
                            ]
                            .join(" · "),
                        )),
                ),
        )
        .children(
            day.sessions
                .iter()
                .take(MAX_SESSIONS_PER_DAY)
                .map(|e| entry_row(e, weak, t)),
        )
        .into_any_element()
}

/// 明细滚动区（psp 滚动条贴右缘，与系统提示词面板同构）。
fn days_list(
    stats: &UsageStats,
    chat: &Chat,
    weak: &gpui::WeakEntity<Chat>,
    t: &Theme,
) -> AnyElement {
    let q = chat.aux.usage.filter.trim().to_lowercase();
    let shown = stats
        .days
        .iter()
        .filter(|d| day_matches(d, &q))
        .take(MAX_DAYS)
        .map(|d| day_card(d, weak, t));
    let total_days = stats.days.iter().filter(|d| day_matches(d, &q)).count();
    let more = total_days.saturating_sub(MAX_DAYS);
    let scroll = &chat.aux.usage.scroll;
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .relative()
        .flex()
        .flex_col()
        .child(
            div()
                .id("usage-stats-scroll")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .px(px(30.))
                .pt(px(10.))
                .pb(px(30.))
                .flex()
                .flex_col()
                .child(column_header(t))
                .children(shown)
                .when(more > 0, |d| {
                    d.child(
                        div()
                            .pt(px(8.))
                            .text_size(crate::appearance::ui_size(11.))
                            .text_color(rgb(t.text_faint))
                            .child(SharedString::from(format!("+{more}"))),
                    )
                }),
        )
        .child(
            div()
                .absolute()
                .top(px(2.))
                .bottom(px(2.))
                .right(px(0.))
                .w(px(10.))
                .child(crate::ui::psp_scrollbar::menu_scrollbar(scroll)),
        )
        .into_any_element()
}
