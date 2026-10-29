//! 内容区 (v54): topbar-r 之下的一切。状态机 chat / md —— markdown 预览
//! 以 topbar tab 打开（Obsidian 式），聊天为默认视图。终端 2026-11 起迁出
//! 内容区 tab，改内容区下沿的底部停靠面板（`terminal::panel`）；内容区
//! 直通窗口底（statusbar 只在面板段）。

use gpui::{Context, Entity, SharedString, div, prelude::*, px, rgb};

use crate::Chat;
use crate::ContentView;
use crate::appearance::theme::theme as T;
use crate::i18n::tr;
use crate::ui::ScrollAxisExt;

/// 内容主视图：按 content_view 切换（flex-1，占满 topbar-r 以下）。
pub(crate) fn content_main(
    chat: &mut Chat,
    entity: Entity<Chat>,
    weak: &gpui::WeakEntity<Chat>,
    window: &mut gpui::Window,
    cx: &mut Context<Chat>,
) -> gpui::AnyElement {
    let t = T();
    let view: gpui::AnyElement = match chat.layout.content_view {
        ContentView::Chat => {
            crate::session::main_column(chat, entity, weak, window, cx).into_any_element()
        }
        ContentView::File => {
            crate::editor::view::file_view(chat, weak, window, cx).into_any_element()
        }
        // 081 更新日志 tab（新版本首启自动打开）
        ContentView::Changelog => changelog_view(chat).into_any_element(),
        // 040-1 Pi 扩展市场标准页
        ContentView::PiExtMarket => crate::market::pi_ext_market::page(chat, weak, window),
        // 040-2 Pi 原生扩展市场（pi.dev Package Catalog 移植）
        ContentView::PiNativeMarket => crate::market::pi_native_market::page(chat, weak, window),
        ContentView::PfMarket => crate::market::pf_market::page(chat, weak, window),
        // 083 项目搜索（Zed ProjectSearchView 同位：内容区 tab）
        ContentView::Search => crate::search::view(chat, weak, window, cx),
        // 会话查询（同版式，只查询）
        ContentView::SessionSearch => crate::search::session_view(chat, weak, t),
        // 060/002【微信通话】功能页（管理段：绑定 / 设置会话；就绪切回 Chat）
        ContentView::Wx => crate::remote_control::panel::page(chat, weak, window, cx),
        // 090 插件预览页签（契约 v2.4 dock="tab"）：整页声明式树，
        // 按 pkg 现查（重装/重载后 ix 漂移安全）
        ContentView::Plugin => crate::pf_plugin::panel::plugin_tab_page(chat, weak),
        // 微信通信全屏页（060；三横菜单唯一入口）
    };
    div()
        .id("content-main")
        // 082 四期 Zed Pane 模式锚点宿主（zed pane.rs:4433-4434 同款
        // key_context+track_focus）：内容视图容器自身可聚焦且永在树上——
        // 切标签后焦点收回这里（tabs.rs activate_content_tab），派发路径
        // 永不为空且含 "ContentTabs" 上下文 → Ctrl+←/→（main.rs 锚定该
        // 上下文）永远可连续切。模态是对根的子、不在子树内，词移动天然
        // 隔离。**勿上移到 content-col**：topbar_r（窗口控制钮 + 拖拽区）
        // 是 content-col 的子，track_focus 的 MouseDown 监听会 prevent_default
        // 吞掉 Windows NC 按下 → 窗口钮/拖拽全哑（已踩，见 render.rs）
        .key_context("ContentTabs")
        .track_focus(&chat.layout.focus)
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .bg(rgb(t.bg))
        .child(view)
        .into_any_element()
}

pub(crate) fn empty_hint(
    text: &str,
    t: &'static crate::appearance::theme::Theme,
) -> gpui::AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        // §2：空态提示 = 辅助说明档（随界面字号缩放，原 text_xs 裸字号）
        .text_size(crate::appearance::ui_size(11.))
        .text_color(rgb(t.text_dim))
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// 更新日志 tab（081）：markdown 渲染 release notes，阅读列居中
// ---------------------------------------------------------------------------

fn changelog_view(chat: &mut Chat) -> gpui::AnyElement {
    let t = T();
    let Some(page) = &chat.aux.changelog else {
        return empty_hint(tr("about.no_changelog_available"), t);
    };
    let version = page.version.clone();
    let body = page.body.clone();
    let fetching = page.fetching;
    let fetch_failed = page.fetch_failed;
    let mut col = div().flex_1().min_h_0().flex().flex_col();
    // 头：更新日志 + 版本号（滚动跟随外层，不参与内层滚动）
    col = col.child(
        div()
            .px(px(32.))
            .pt(px(20.))
            .flex()
            .items_baseline()
            .gap(px(10.))
            .child(
                div()
                    .text_size(crate::appearance::ui_size(17.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(t.text))
                    .child(tr("about.changelog").to_string()),
            )
            .child(
                div()
                    .font_family(crate::editor::markdown::MONO_FAMILY)
                    .text_size(crate::appearance::ui_size(12.))
                    .text_color(rgb(t.text_muted))
                    .child(SharedString::from(format!("v{version}"))),
            ),
    );
    if let Some(body) = body {
        col = col.child(
            div()
                .id("changelog-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&chat.aux.changelog_scroll)
                .restrict_scroll_to_axis()
                .child(
                    div()
                        .max_w(px(820.))
                        .mx_auto()
                        .px(px(32.))
                        .pb(px(40.))
                        .child(crate::editor::markdown::render(&body, t, false)),
                ),
        );
    } else if fetching {
        col = col.child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .text_size(crate::appearance::ui_size(11.))
                .text_color(rgb(t.text_dim))
                .child(crate::ui::spinner(14., t.text_dim))
                .child(tr("about.fetching_changelog").to_string()),
        );
    } else if fetch_failed {
        // 现拉 15s 超时/失败（被墙、断网）：区别于「暂无更新日志」，回新
        // 会话页再点一次版本号即重试
        col = col.child(empty_hint(tr("about.github_unreachable"), t));
    } else {
        col = col.child(empty_hint(tr("about.no_changelog_available"), t));
    }
    col.into_any_element()
}
