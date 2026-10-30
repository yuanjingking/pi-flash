//! 文件编辑器模块域：文件编辑视图 + Markdown 渲染与所见即所得。
//!
//! 子模块：
//! - kind：FileTypeHandler 注册表（按后缀名注册 handler，未注册走默认
//!   文本 kind；打开路由/编辑器配置/主体渲染/工具栏/右键菜单全由 handler
//!   决定，2026-10-12 拍板）
//! - view：023 fileView 视图（自 content.rs 拆入：file_view/
//!   file_editor_body/面包屑/冲突横幅）；文件打开/缓冲区/file_cache
//!   编排状态仍在 Chat（后置收拢）
//! - markdown：Markdown 渲染器（聊天正文 + md 预览），含 render/
//!   {html,math,mermaid} 富渲染组件
//!   └─ wysiwyg：Markdown 所见即所得（Live Preview，024 设计）
//! - definition：跳转到定义（Ctrl+Click / F12 / 右键菜单）——语言感知的
//!   符号解析，不走 LSP 进程（零配置，2026-10-22）
//! - file_content：文件字节 ↔ 文本解码/编码（Zed file_content.rs 逐字
//!   移植：BOM/二进制判定/chardetng 嗅探），打开与保存共用
//!
//! vendor 编辑底座见 `vendor/gpui-component/src/input/`
//! （rope/光标/undo/IME），本域不碰内核。

pub(crate) mod definition;
pub(crate) mod file_content;
pub(crate) mod kind;
pub mod markdown;
pub(crate) mod tabs;
pub mod view;
