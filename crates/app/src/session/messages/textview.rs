//! TextView 包装（消息划选一期）：agent **定稿**文本块换用 gpui-component
//! TextView（鼠标划选 + 根级 Ctrl+C 复制，见 shell/render.rs 兜底）；流式
//! 块仍走自研渲染器（TextView 文本变更是 200ms 防抖后台重解析，会拖慢流式
//! 显示）。用户消息不动（气泡/技能折叠逻辑特殊）。
//!
//! 用户气泡（划选二期，m00831「自定义复制」）：`UserBubbleText` 同样走
//! selectable TextView——但用户消息是**原文凭证**，自研渲染器
//! （markdown::render_user，html=false）把 HTML 标签原样显示（tests.rs
//! user_mode_shows_html_literals 锁死）；vendor 解析器会吃标签，所以上
//! 屏前先 `escape_lt` 把 `<`→`&lt;`：仅改起手是 `<` 的行/块级 `<`，实体
//! （`&amp;` 等）与代码围栏内不动，画出来的文字与原始文本逐字节一致，
//! 划选取到的也是原文。
//!
//! 为什么是 RenderOnce 包装而不是参数下传：list() 条目闭包里 `rt_view`
//! 不可变借用锁死 cx（turn_msgs 持 &Msg），再把 `&mut App` 传进渲染链必然
//! E0502；RenderOnce::render 是元素树实现期回调，天然持有 window+cx，
//! TextView::markdown（use_keyed_state 需要 &mut Window）在此构造零改造。

use gpui::{
    App, ElementId, IntoElement, RenderOnce, SharedString, Styled, Window, div, prelude::*, px,
    relative, rgb,
};
use gpui_component::ActiveTheme;
use gpui_component::text::TextView;

use crate::appearance::theme::theme as T;
use crate::editor::markdown;

#[derive(IntoElement, Clone)]
pub(crate) struct AgentMarkdown {
    id: ElementId,
    text: SharedString,
}

impl AgentMarkdown {
    /// id 必须带文本哈希：use_keyed_state 按全局 id 存解析树，会话切换/
    /// 分支跳转后同位同 id 会先画 200ms 旧内容（异步重解析防抖期）。
    /// 还必须带主题名（issue#4）：gpui-component CodeBlock 在解析期把
    /// highlight_theme 烘进逐 span 样式，id 不含主题时文本不变就不重
    /// 解析——浅色主题下渲染过的消息切到暗主题后，代码块永远停在浅色
    /// 语法盘上（暗底配 #0433ff/#0000A2，即用户截图的难读色）。
    pub(crate) fn new(id: SharedString, text: impl Into<SharedString>) -> Self {
        Self {
            id: ElementId::from(SharedString::from(format!(
                "{}#{}",
                id,
                crate::appearance::theme::theme_name()
            ))),
            text: text.into(),
        }
    }
}

impl RenderOnce for AgentMarkdown {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = T();
        div()
            .w_full()
            // 正文样式对齐自研渲染器：会话字体/字号、1.5 行高（gpui 0.2.2
            // StyledText 排版取容器继承链，字号/族必须挂容器）
            .font_family(crate::appearance::session_font().family)
            .text_size(crate::appearance::sess_size(0.))
            .line_height(relative(1.5))
            .text_color(rgb(t.text))
            .child(
                TextView::markdown(self.id, self.text, window, cx)
                    .selectable()
                    .style(text_view_style(cx)),
            )
    }
}

/// 用户消息气泡正文：selectable TextView + 行内 HTML 转义。
///
/// id 拼 `ub-` 前缀与 agent 块（`tv-`）隔离；同时带内容哈希（use_keyed_state
/// 同 id 换文本会先画旧内容直到 200ms 防抖重解析完成）与主题名（
/// AgentMarkdown::new 同款，CodeBlock 高亮盘烘进 span 样式，issue#4）。
#[derive(IntoElement, Clone)]
pub(crate) struct UserBubbleText {
    id: ElementId,
    text: SharedString,
}

impl UserBubbleText {
    pub(crate) fn new(id: SharedString, text: impl Into<SharedString>) -> Self {
        Self {
            id: ElementId::from(SharedString::from(format!(
                "{}#{}",
                id,
                crate::appearance::theme::theme_name()
            ))),
            text: SharedString::from(escape_lt(&text.into())),
        }
    }
}

impl RenderOnce for UserBubbleText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = T();
        div()
            .w_full()
            // 气泡内字号跟会话字号档（sess_size），正文/列表色由气泡容器
            // 继承的 t.text 负责，此处只补字号/行高/代码块底色
            .font_family(crate::appearance::session_font().family)
            .text_size(crate::appearance::sess_size(0.))
            .line_height(relative(1.5))
            .text_color(rgb(t.text))
            .child(
                TextView::markdown(self.id, self.text, window, cx)
                    .selectable()
                    .style(text_view_style(cx)),
            )
    }
}

/// 起手是 `<` 的行/块级标签转 `&lt;`（用户消息 HTML 标签原样可见语义）。
///
/// 不碰：代码围栏（```/~~~ 段内原样，vendor 解析器自己处理）、`&` 开头
/// 的实体（`&lt;` 之类进 vendor 解析器会解回 `<`，原文精确）。行内裸
/// 标签（如 `a <b>x</b>` 中段）不在此列——vendor 行内 Html Err 时
/// 也返回原文，行为与自研渲染器一致；Ok 丢文本是 vendor 级已知边界，
/// 留给实测验证（提交说明注明）。
fn escape_lt(src: &str) -> String {
    if !src.contains('<') {
        return src.to_string();
    }
    let mut out = String::with_capacity(src.len() + 16);
    let mut in_fence = false;
    for line in src.split('\n') {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed
            .strip_prefix("```")
            .or_else(|| trimmed.strip_prefix("~~~"))
        {
            // 围栏开/关行（含 ```lang）原样；空围栏标记也算一次开关
            let _ = rest;
            in_fence = !in_fence;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_fence {
            out.push_str(line);
        } else if line.starts_with('<') {
            out.push_str("&lt;");
            out.push_str(&line[1..]);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    // split 不会多出元素，但末尾换行会被 push 多算——原样去一格
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// PF 主题 → TextViewStyle：标题梯度抄 markdown::size_for_level（pi-web
/// em 档，相对 14px 基准的绝对差），代码块底色抄 render_code_block 的
/// mix(t.bg, t.bg_panel, 0.92)，高亮盘用已随 sync_gpui_tokens 映射的
/// gpui-component Theme（选区色/前景色组件内自取，无需在此传）。
fn text_view_style(cx: &App) -> gpui_component::text::TextViewStyle {
    let t = T();
    // Styled 方法是 move 链式（self -> Self），只能整体一条链不能逐语句调
    // 边框对齐 pi-web .markdown-code-block（1px var(--border) + 圆角）——
    // 语言栏/复制/行号是 vendor 级活（pi-flash-lamn 二期），此处先补框
    let code_block = gpui::StyleRefinement::default()
        .bg(gpui::rgb(crate::appearance::theme::mix_rgb(
            t.bg, t.bg_panel, 0.92,
        )))
        .border_1()
        .border_color(rgb(t.border))
        .rounded(px(7.))
        .font_family(markdown::MONO_FAMILY)
        .text_size(crate::appearance::sess_size(-1.));
    gpui_component::text::TextViewStyle {
        // 块间距：自研渲染器正文块 CSS 规格 8px（root 16px 下 0.5rem）
        paragraph_gap: gpui::rems(0.5),
        heading_base_font_size: crate::appearance::sess_size(0.),
        heading_font_size: Some(std::sync::Arc::new(|level, base| {
            let delta = match level {
                1 => 14. * 0.16,
                2 => 14. * 0.08,
                3 => -14. * 0.02,
                _ => 0.,
            };
            px(f32::from(base) + delta)
        })),
        highlight_theme: cx.theme().highlight_theme.clone(),
        code_block,
        is_dark: t.dark,
    }
}

#[cfg(test)]
mod tests {
    use super::escape_lt;

    #[test]
    fn escapes_block_leading_tags_only() {
        // 行首 < 转义；行内普通文字、非标签 < 不动
        assert_eq!(escape_lt("<b>bold</b>"), "&lt;b>bold</b>");
        assert_eq!(escape_lt("a < b"), "a < b");
        assert_eq!(escape_lt("line1\n<em>x</em>"), "line1\n&lt;em>x</em>");
    }

    #[test]
    fn fences_and_entities_kept_verbatim() {
        // 围栏内原样（含起手 < ）
        let src = "```\n<tag>\n```";
        assert_eq!(escape_lt(src), src);
        // 实体不重复转义
        assert_eq!(escape_lt("&lt;already"), "&lt;already");
        // 无 < 短路
        assert_eq!(escape_lt("plain"), "plain");
    }
}
