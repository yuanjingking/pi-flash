//! shell —— 应用壳域（P3 自 main.rs 拆出）：状态类型、Chat 实体与
//! 渲染根。域方法按各自领域住 session/、function_panel/、settings/ 等。

pub(crate) mod chat;
pub(crate) mod render;
pub(crate) mod types;

pub(crate) use chat::PendingJump;
