//! 用量统计域（091）：会话底部用量条、每轮消耗行、「用量统计」弹窗
//! （每天 × 每模型消耗明细）。
//!
//! 数据源三条，口径各自独立：
//! - 底部条 = 当前会话累计，直接读 RPC `get_session_stats` 的
//!   [`pi_link::protocol::SessionStats`]（runtime.stats，每轮结束刷新），
//!   零额外 IO；
//! - 每轮行 = 该轮内全部 assistant 消息的 `usage` 就地求和（消息已解码在
//!   内存里，纯加法）；
//! - 统计弹窗 = `pi_link::usage_stats::scan_default()`，扫全部会话 JSONL，
//!   **必须后台线程**（首扫可能几十 MB）→ 算完回主线程 notify。缓存按
//!   字节偏移增量续读（见 pi_link::usage_stats），二次打开近乎零成本。

mod dialog;
mod footer;

use gpui::SharedString;

use crate::i18n::tr;
use gpui::prelude::*;

pub(crate) use dialog::usage_stats_dialog;
pub(crate) use footer::{session_footer, turn_usage_line};

/// 「用量统计」弹窗的页面状态（挂 `Chat::aux.usage`）。
#[derive(Default)]
pub(crate) struct UsageUi {
    /// 最近一次扫描结果（None = 从未加载/正在加载）
    pub(crate) stats: Option<std::sync::Arc<pi_link::usage_stats::UsageStats>>,
    /// 后台扫描进行中（弹窗显示「正在统计…」，按钮防连点）
    pub(crate) loading: bool,
    /// 明细列表滚动位（psp 滚动条跨渲染读同一份）
    pub(crate) scroll: gpui::ScrollHandle,
    /// 搜索输入框（懒建：TextInput::new 不需 Window，弹窗首开时建一次）
    pub(crate) search_input: Option<gpui::Entity<crate::ui::TextInput>>,
    /// 搜索词镜像（日期前缀或模型子串，不区分大小写）
    pub(crate) filter: String,
}

/// 搜索过滤：日期前缀匹配 + 模型子串（不区分大小写）。
/// 词为空 = 不过滤；跨两个维度 OR（用户不知道这行是日期还是模型时两边都试）。
pub(crate) fn day_matches(day: &pi_link::usage_stats::UsageDay, q: &str) -> bool {
    let q = q.trim();
    if q.is_empty() {
        return true;
    }
    let lq = q.to_lowercase();
    if day.date.to_lowercase().contains(&lq) {
        return true;
    }
    day.models
        .iter()
        .any(|r| r.model.to_lowercase().contains(&lq))
}

impl crate::Chat {
    /// 打开「用量统计」弹窗：**先开窗**（数据已有就立刻可看），再后台重算
    /// 一次（增量缓存命中时毫秒级；首扫几十 MB 也不阻塞首帧——铁律）。
    pub(crate) fn open_usage_stats(&mut self, cx: &mut gpui::Context<crate::Chat>) {
        // 搜索框懒建一次（TextInput::new 不需 Window）：on_change 镜像词 →
        // notify（重画即重过滤），同值不 notify（打字不抖）
        if self.aux.usage.search_input.is_none() {
            let weak = cx.weak_entity();
            let input = cx.new(|cx| {
                crate::ui::TextInput::new(cx)
                    .flat()
                    .placeholder(tr("usage.search_ph"))
                    .on_change(Box::new(move |q: &str, cx: &mut gpui::App| {
                        let q = q.to_string();
                        let _ = weak.update(cx, |c, cx| {
                            if c.aux.usage.filter != q {
                                c.aux.usage.filter = q;
                                c.aux
                                    .usage
                                    .scroll
                                    .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
                                cx.notify();
                            }
                        });
                    }))
            });
            self.aux.usage.search_input = Some(input);
        }
        self.dlg.dialog = Some(crate::Dialog::UsageStats);
        self.reload_usage_stats(cx);
        cx.notify();
    }

    /// 后台扫描用量（`pi_link::usage_stats::scan_default`）→ 回主线程落位。
    /// `loading` 期间重复调用直接返回：连点入口不会叠线程。
    pub(crate) fn reload_usage_stats(&mut self, cx: &mut gpui::Context<crate::Chat>) {
        if self.aux.usage.loading {
            return;
        }
        self.aux.usage.loading = true;
        let scan = cx
            .background_executor()
            .spawn(async move { pi_link::usage_stats::scan_default() });
        cx.spawn(async move |this, cx| {
            let stats = scan.await;
            let _ = this.update(cx, |c, cx| {
                c.aux.usage.loading = false;
                c.aux.usage.stats = Some(std::sync::Arc::new(stats));
                cx.notify();
            });
        })
        .detach();
    }
    /// 点击明细行 → 关弹窗并跳进对应会话（091-1）。用 detached 入口：
    /// cwd 解析（会话池 → psp 项目组 → 当前工作区兜底）在 pool 内同源完成；
    /// 唤醒后再把注意力切过去（detached 不动桌面注意力）。
    pub(crate) fn goto_usage_session(
        &mut self,
        path: std::path::PathBuf,
        cx: &mut gpui::Context<crate::Chat>,
    ) {
        self.dlg.dialog = None;
        let rt = self.open_session_detached(path, cx);
        if let Some(rt) = rt {
            self.switch_to(rt, cx);
        } else {
            cx.notify();
        }
    }
}

/// 紧凑 token 数：1,234 / 12.3K / 4.56M。会话底部与轮末行共用
/// （`fmt_thousand` 是千分位长格式，用于详情页表格）。
pub(crate) fn fmt_tokens(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        let v = n as f64 / 1_000.;
        if v < 10. {
            format!("{v:.2}K")
        } else if v < 100. {
            format!("{v:.1}K")
        } else {
            format!("{v:.0}K")
        }
    } else {
        let v = n as f64 / 1_000_000.;
        if v < 100. {
            format!("{v:.2}M")
        } else {
            format!("{v:.1}M")
        }
    }
}

/// 费用：<0.01 给四位小数（小额调用不至于显示成 $0.00），否则两位。
pub(crate) fn fmt_cost(cost: f64) -> String {
    if cost <= 0.0 {
        "$0".to_string()
    } else if cost < 0.01 {
        format!("${cost:.4}")
    } else if cost < 100. {
        format!("${cost:.2}")
    } else {
        format!("${cost:.0}")
    }
}

/// 缓存命中率文本（分母口径见 `UsageTotals::cache_hit_rate`）。
pub(crate) fn fmt_hit_rate(rate: Option<f64>) -> Option<SharedString> {
    rate.map(|r| SharedString::from(format!("{:.0}%", r * 100.)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_matches_filters_by_date_and_model() {
        use pi_link::usage_stats::{UsageDay, UsageTotals};
        let mk = |date: &str, model: &str| UsageDay {
            date: date.into(),
            totals: UsageTotals::default(),
            models: vec![pi_link::usage_stats::UsageRow {
                provider: String::new(),
                model: model.into(),
                totals: UsageTotals::default(),
                sessions: Vec::new(),
            }],
            sessions: Vec::new(),
        };
        let day = mk("2026-10-29", "gpt-5");
        assert!(day_matches(&day, ""));
        assert!(day_matches(&day, "2026"));
        assert!(day_matches(&day, "10-29"));
        assert!(day_matches(&day, "GPT"));
        assert!(!day_matches(&day, "2025"));
        assert!(!day_matches(&day, "claude"));
    }

    #[test]
    fn token_units() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_234), "1.23K");
        assert_eq!(fmt_tokens(12_345), "12.3K");
        assert_eq!(fmt_tokens(123_456), "123K");
        assert_eq!(fmt_tokens(1_234_567), "1.23M");
    }

    #[test]
    fn cost_units() {
        assert_eq!(fmt_cost(0.0), "$0");
        assert_eq!(fmt_cost(0.0042), "$0.0042");
        assert_eq!(fmt_cost(0.4213), "$0.42");
        assert_eq!(fmt_cost(1234.0), "$1234");
    }
}
