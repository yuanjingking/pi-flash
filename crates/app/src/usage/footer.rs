//! 两块「用量」界面：会话底部累计条（[`session_footer`]）与每轮消耗行
//! （[`turn_usage_line`]）。
//!
//! 尺寸/颜色全部取自规范令牌（`appearance::ui_size` + 主题色），无手搓控件：
//! 底部条是纯文本行（点击目标），轮末行是静态文本行。

use gpui::{AnyElement, App, MouseButton, SharedString, div, prelude::*, px, rgb};

use super::{fmt_cost, fmt_tokens};
use crate::Chat;
use crate::Msg;
use crate::appearance::theme::Theme;
use crate::i18n::tr;
use crate::session::messages::Role;

/// 一段「标签 + 数值」（标签浅一档，数值同色）。
fn pair(label: &str, value: String, t: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(3.))
        .child(
            div()
                .text_color(rgb(t.text_dim))
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .text_color(rgb(t.text_faint))
                .child(SharedString::from(value)),
        )
        .into_any_element()
}

/// 圆点分隔符。
fn dot(t: &Theme) -> AnyElement {
    div()
        .text_color(rgb(t.text_dim))
        .child("·")
        .into_any_element()
}

/// `↑输入 ↓输出 · 缓存 · $费用` 三组数值（底部条与轮末行共用）。
fn usage_parts(input: u64, output: u64, cache: u64, cost: f64, t: &Theme) -> Vec<AnyElement> {
    vec![
        pair("↑", fmt_tokens(input), t),
        pair("↓", fmt_tokens(output), t),
        dot(t),
        pair(tr("usage.cache"), fmt_tokens(cache), t),
        dot(t),
        div()
            .text_color(rgb(t.text_faint))
            .child(SharedString::from(fmt_cost(cost)))
            .into_any_element(),
    ]
}

/// 会话底部用量条：当前会话累计 token / 缓存 / 费用，点击开「用量统计」。
///
/// 数据 = RPC `get_session_stats`（runtime.stats，轮末刷新）；新会话页/草稿
/// （还没跑过 agent）→ 不渲染，底部保持干净。
pub(crate) fn session_footer(
    chat: &Chat,
    weak: &gpui::WeakEntity<Chat>,
    t: &Theme,
    cx: &App,
) -> Option<AnyElement> {
    let stats = chat.rt().read(cx).stats.clone()?;
    if stats.tokens_total == 0 && stats.cost <= 0.0 {
        return None;
    }
    let weak_open = weak.clone();
    // ui-raw: ok 会话底部用量条——胶囊下沿 absolute 定位行（area.rs 摆位），
    // 几何取规范间距档（gap4/py0），点击开统计弹窗
    let row = div()
        .id("session-usage-footer")
        // ui-raw: ok 会话底部用量条（胶囊下沿 absolute 行，间距取规范档）
        .flex()
        .items_center()
        .gap(px(4.))
        .text_size(crate::appearance::ui_size(11.))
        .text_color(rgb(t.text_faint))
        .cursor_pointer()
        .hover(|s| s.text_color(rgb(t.text)))
        .children(usage_parts(
            stats.input,
            stats.output,
            stats.cache_read + stats.cache_write,
            stats.cost,
            t,
        ))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            let _ = weak_open.update(cx, |c, cx| c.open_usage_stats(cx));
        });
    Some(row.into_any_element())
}

/// 该轮 assistant 消息的 usage 合计（纯加法，消息已在内存里）。
/// 与旧「只取末条」相反：用户要的「当次消耗」= 这一轮全部模型调用的总和。
pub(crate) fn turn_totals(turn: &[&Msg]) -> Option<pi_link::usage_stats::UsageTotals> {
    let mut total = pi_link::usage_stats::UsageTotals::default();
    let mut any = false;
    for m in turn {
        if m.role != Role::Assistant {
            continue;
        }
        if let Some(u) = &m.usage {
            any = true;
            total.input += u.input;
            total.output += u.output;
            total.cache_read += u.cache_read;
            total.cache_write += u.cache_write;
            total.cost += u.cost;
            total.calls += 1;
        }
    }
    any.then_some(total)
}

/// 轮末消耗行：`本次 ↑x ↓y · 缓存 z · $cost`（常显，不随 hover 操作栏显影）。
pub(crate) fn turn_usage_line(turn: &[&Msg], t: &Theme) -> Option<AnyElement> {
    let total = turn_totals(turn)?;
    let row = div()
        .flex()
        .items_center()
        .gap(px(4.))
        .mt(px(4.))
        .text_size(crate::appearance::ui_size(11.))
        .text_color(rgb(t.text_faint))
        .child(
            div()
                .text_color(rgb(t.text_dim))
                .child(SharedString::from(tr("usage.turn"))),
        )
        .children(usage_parts(
            total.input,
            total.output,
            total.cache_read + total.cache_write,
            total.cost,
            t,
        ));
    Some(row.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_link::protocol::Block;

    fn msg(role: Role, usage: Option<(u64, u64, u64, u64, f64)>) -> Msg {
        let mut m = Msg {
            role,
            blocks: Vec::<Block>::new(),
            usage: None,
            entry_id: None,
            ts: None,
            end_ts: None,
            stop_reason: None,
            error_message: None,
            custom_type: None,
            custom_display: true,
            details: None,
            model: None,
            bash: None,
        };
        m.usage = usage.map(|(input, output, cache_read, cache_write, cost)| {
            crate::session::messages::UsageLine {
                input,
                output,
                cache_read,
                cache_write,
                cost,
            }
        });
        m
    }

    /// 一轮 = 全部 assistant 消息求和；用户消息/无 usage 的不计。
    #[test]
    fn turn_totals_sums_the_whole_turn() {
        let a = msg(Role::Assistant, Some((100, 10, 50, 5, 0.1)));
        let b = msg(Role::Assistant, Some((200, 20, 70, 7, 0.2)));
        let u = msg(Role::User, None);
        let n = msg(Role::Assistant, None);
        let total = turn_totals(&[&u, &a, &n, &b]).expect("有 usage");
        assert_eq!(total.input, 300);
        assert_eq!(total.output, 30);
        assert_eq!(total.cache_read, 120);
        assert_eq!(total.cache_write, 12);
        assert!((total.cost - 0.3).abs() < 1e-9);
        assert_eq!(total.calls, 2);
        // 整轮都没有 usage → 不渲染任何行
        assert!(turn_totals(&[&u, &n]).is_none());
    }
}
