//! 编辑器「跳转到定义」（Ctrl+Click / F12 / 右键菜单，2026-10-22）。
//!
//! **不走真 LSP**：PF 不做语言服务器进程（零配置、零启动成本是第一卖点；
//! 且 Settings 模块禁止改动，LSP server 路径无处可配）。这里做的是
//! **语言感知的符号解析**——与 Zed 的 `goto_definition` 同入口语义
//! （ctrl+click 跳定义），实现是「词法提取 + 声明模式正则 + 项目文件扫描」：
//!
//! 1. 光标处取标识符（[`word_at`]：字母/数字/下划线/非 ASCII，跳过关键字）
//! 2. 按后缀定语言（[`lang_of`]），取该语言的**声明模式表**（[`patterns`]，
//!    优先级即数组顺序：`fn name` > `struct name` > `let name` …）
//! 3. 先搜当前文档（跳过光标处那个词，避免原地跳），再扫项目文件
//!    （[`crate::search::engine::candidate_paths`] 同一事实源，天然尊重
//!    .gitignore）
//! 4. 命中 → 同文件移光标；跨文件走 083 的 `pending_jump` 行跳转通道
//!
//! 全流程跑 `background_spawn`（铁律：热路径禁止同步重活——项目扫描要读
//! 千级文件，绝不能在按键帧里做）。结果只回传目标文件 + 字节区间，
//! 前台只做「移光标 / 开 tab」。
//!
//! 边界：纯文本/配置类后缀（md/json/toml/yaml/html/css…）不做跳转
//! （[`lang_of`] 返回 None）；模式表命中不到项目内定义时静默（状态栏
//! toast 一条 `editor.def_not_found`）。

use std::ops::Range;
use std::path::{Path, PathBuf};

use gpui::{Context, prelude::*};
use regex::Regex;

use crate::Chat;
use crate::i18n::tr;

/// 项目扫描的文件数上限（防巨型仓库一次点击扫到天荒地老）
const MAX_SCAN_FILES: usize = 4_000;
/// 单文件扫描上限（超过不读：大文件多非手写源码）
const MAX_FILE_BYTES: u64 = 512 * 1024;
/// 整次扫描的读盘总预算
const MAX_SCAN_BYTES: u64 = 48 * 1024 * 1024;

/// 跳转目标：目标文件 + 定义符号在文件内的位置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DefTarget {
    pub(crate) path: PathBuf,
    /// 定义名首字节（文档字节偏移）
    pub(crate) offset: usize,
    /// 定义名字节长度
    pub(crate) len: usize,
    /// 1-based 行号（跨文件跳转用）
    pub(crate) line: u32,
    /// 行内字节列（0-based，跨文件跳转用）
    pub(crate) col: u32,
}

/// 语言族：只影响「声明长什么样」的模式表，不做语法树解析。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Lang {
    Rust,
    Python,
    Js,
    Go,
    CStyle,
    /// 关键字族兜底（swift/zig/scala/kotlin/elixir/ruby/lua/proto/shell…）
    Generic,
}

/// 后缀 → 语言族；None = 该类型不做符号跳转（md/json/toml/yaml/html/css…）。
pub(crate) fn lang_of(path: &Path) -> Option<Lang> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => Lang::Rust,
        "py" | "pyw" | "pyi" => Lang::Python,
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "tsx" => Lang::Js,
        "go" => Lang::Go,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "cs" | "java" | "m" | "mm" => {
            Lang::CStyle
        }
        "swift" | "zig" | "scala" | "kt" | "kts" | "ex" | "exs" | "rb" | "lua" | "proto" | "sh"
        | "bash" | "zsh" | "ps1" | "php" | "dart" | "r" | "jl" | "hs" | "ml" | "vim" | "sql" => {
            Lang::Generic
        }
        _ => return None,
    })
}

/// 关键字白名单（光标落在这些词上不跳：它们不是用户符号）。
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "case", "catch", "class", "const", "continue", "crate",
    "default", "def", "defer", "delete", "do", "else", "enum", "extern", "false", "final", "fn",
    "for", "func", "function", "go", "if", "impl", "import", "in", "interface", "is", "lambda",
    "let", "loop", "match", "mod", "move", "mut", "new", "nil", "none", "not", "null", "or",
    "package", "pass", "pub", "raise", "ref", "return", "self", "Self", "static", "struct",
    "super", "switch", "this", "throw", "trait", "true", "try", "type", "typeof", "union",
    "unsafe", "use", "var", "void", "where", "while", "with", "yield",
];

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// 光标处的标识符（`(词, 文档字节区间)`）。
///
/// 光标落在词尾也算命中（Zed 语义：光标紧跟 `foo|` 时仍视作 `foo`）；
/// 纯数字、语言关键字、以及 UTF-8 边界不齐的切片一律 None。
pub(crate) fn word_at(text: &str, offset: usize) -> Option<(String, Range<usize>)> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut off = offset.min(bytes.len());
    if off == bytes.len() || !is_ident_byte(bytes[off]) {
        // 光标在词尾/空白处：左探一格
        if off > 0 && is_ident_byte(bytes[off - 1]) {
            off -= 1;
        } else {
            return None;
        }
    }
    let mut start = off;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = off;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    let word = text.get(start..end)?;
    if word.is_empty() || word.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    if KEYWORDS.contains(&word) {
        return None;
    }
    Some((word.to_string(), start..end))
}

/// 语言族的声明模式表（优先级 = 数组顺序）。`({s})` 捕获组 1 = 定义名。
fn patterns(lang: Lang, sym: &str) -> Vec<Regex> {
    let s = regex::escape(sym);
    let raws: Vec<String> = match lang {
        Lang::Rust => vec![
            format!(
                r#"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:default[ \t]+)?(?:async[ \t]+)?(?:unsafe[ \t]+)?(?:const[ \t]+)?(?:extern[ \t]+"[^"]*"[ \t]+)?fn[ \t]+({s})\b"#
            ),
            format!(
                r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:struct|enum|trait|union)[ \t]+({s})\b"
            ),
            format!(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:type|mod)[ \t]+({s})\b"),
            format!(
                r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:const|static)[ \t]+(?:mut[ \t]+)?({s})\b"
            ),
            format!(r"(?m)^[ \t]*impl(?:<[^>]*>)?[ \t]+({s})\b"),
            format!(r"macro_rules![ \t]*({s})\b"),
        ],
        Lang::Python => vec![
            format!(r"(?m)^[ \t]*(?:async[ \t]+)?def[ \t]+({s})\b"),
            format!(r"(?m)^[ \t]*class[ \t]+({s})\b"),
            // 模块级/类级赋值（大写常量、单例）
            format!(r"(?m)^({s})[ \t]*[:=]"),
        ],
        Lang::Js => vec![
            format!(
                r"(?m)^[ \t]*(?:export[ \t]+)?(?:default[ \t]+)?(?:async[ \t]+)?function[ \t]*\*?[ \t]*({s})\b"
            ),
            format!(
                r"(?m)^[ \t]*(?:export[ \t]+)?(?:default[ \t]+)?(?:abstract[ \t]+)?(?:declare[ \t]+)?(?:class|interface|type|enum)[ \t]+({s})\b"
            ),
            format!(r"(?m)^[ \t]*(?:export[ \t]+)?(?:const|let|var)[ \t]+({s})\b"),
            // 类方法 / 对象方法简写
            format!(r"(?m)^[ \t]*(?:public |private |protected |static |async |get |set )*({s})[ \t]*\([^)]*\)[ \t]*\{{"),
            // 箭头函数 / 函数表达式赋值
            format!(r"(?m)^[ \t]*(?:export[ \t]+)?(?:const|let|var)[ \t]+{s}[ \t]*[:=][ \t]*(?:async[ \t]*)?(?:function|\()"),
        ],
        Lang::Go => vec![
            format!(r"(?m)^[ \t]*func[ \t]+(?:\([^)]*\)[ \t]*)?({s})[ \t]*\("),
            format!(r"(?m)^[ \t]*type[ \t]+({s})\b"),
            format!(r"(?m)^[ \t]*(?:var|const)[ \t]+({s})\b"),
        ],
        Lang::CStyle => vec![
            format!(
                r"(?m)^[ \t]*(?:static[ \t]+)?(?:inline[ \t]+)?(?:virtual[ \t]+)?(?:extern[ \t]+)?(?:const[ \t]+)?[A-Za-z_][\w:<>\*&,\[\] \t]*[ \t\*&]({s})[ \t]*\("
            ),
            format!(
                r"(?m)^[ \t]*(?:typedef[ \t]+)?(?:struct|class|enum|union|interface)[ \t]+({s})\b"
            ),
            format!(r"(?m)^[ \t]*#define[ \t]+({s})\b"),
        ],
        Lang::Generic => vec![
            format!(
                r"(?m)^[ \t]*(?:pub[ \t]+)?(?:export[ \t]+)?(?:async[ \t]+)?(?:fn|func|def|function|proc|sub|method|subroutine)[ \t]+({s})\b"
            ),
            format!(
                r"(?m)^[ \t]*(?:pub[ \t]+)?(?:export[ \t]+)?(?:class|struct|interface|trait|enum|type|typedef|object|module|mod|record|protocol)[ \t]+({s})\b"
            ),
            format!(r"(?m)^[ \t]*(?:const|let|var|static|val|final)[ \t]+({s})\b"),
            format!(r"(?m)^[ \t]*({s})[ \t]*\("),
        ],
    };
    // 模式写错了也不能让跳转炸掉：编译失败的那条直接丢
    raws.iter().filter_map(|r| Regex::new(r).ok()).collect()
}

/// 该位置是否落在注释里（行注释起步 / 块注释未闭合 / doc 注释星号行）。
///
/// 粗判但零成本：只为挡掉「注释里写了 `fn foo`」这类假命中，不追求
/// 词法精确（真精确要 tree-sitter，收益不抵成本）。
fn in_comment(text: &str, pos: usize) -> bool {
    let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let prefix = &text[line_start..pos];
    let head = prefix.trim_start();
    if head.starts_with("//") || head.starts_with('*') || head.starts_with("--") {
        return true;
    }
    // `#` 注释：C 的 #define/#include/#[attr]、Rust 的 #[attr] 是代码
    if head.starts_with('#')
        && !head.starts_with("#define")
        && !head.starts_with("#include")
        && !head.starts_with("#[")
        && !head.starts_with("#!")
    {
        return true;
    }
    // 同一行内 `/*` 之后且没有 `*/`
    match prefix.rfind("/*") {
        Some(ix) => !prefix[ix..].contains("*/"),
        None => false,
    }
}

/// 在文本里按模式优先级找定义，返回 `(字节起点, 字节长度)`。
///
/// `skip`：光标处那个词的区间（同文件搜索时跳过它，否则「跳到定义」会
/// 原地打转——但若定义确实在光标处，跳过它就无命中，此时静默）。
fn find_in_text(text: &str, pats: &[Regex], skip: Option<Range<usize>>) -> Option<(usize, usize)> {
    for re in pats {
        for caps in re.captures_iter(text) {
            let Some(m) = caps.get(1) else { continue };
            let (start, end) = (m.start(), m.end());
            if let Some(sk) = &skip
                && start < sk.end
                && sk.start < end
            {
                continue;
            }
            if in_comment(text, start) {
                continue;
            }
            return Some((start, end - start));
        }
    }
    None
}

/// 字节偏移 → `(1-based 行号, 0-based 行内字节列)`。
pub(crate) fn line_col(text: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = text[..line_start].bytes().filter(|b| *b == b'\n').count() as u32 + 1;
    (line, (offset - line_start) as u32)
}

/// `(1-based 行号, 0-based 行内字节列)` → 字节偏移（clamp 到行尾）。
pub(crate) fn offset_of_line_col(text: &str, line: u32, col: u32) -> usize {
    let want = line.max(1) as usize;
    let mut offset = 0usize;
    for (ix, raw) in text.split_inclusive('\n').enumerate() {
        if ix + 1 >= want {
            let line_len = raw.trim_end_matches('\n').len();
            return offset + (col as usize).min(line_len);
        }
        offset += raw.len();
    }
    text.len()
}

/// 解析光标处的跳转目标。`cwd` = 项目根（None = 无项目，只有同文件搜索）。
///
/// 跑在后台线程（读盘 + 正则），返回轻量结果。
pub(crate) fn resolve(
    cwd: Option<&Path>,
    current: &Path,
    doc: &str,
    offset: usize,
) -> Option<DefTarget> {
    let lang = lang_of(current)?;
    let (sym, sym_range) = word_at(doc, offset)?;
    let pats = patterns(lang, &sym);
    if pats.is_empty() {
        return None;
    }
    if let Some((start, len)) = find_in_text(doc, &pats, Some(sym_range)) {
        let (line, col) = line_col(doc, start);
        return Some(DefTarget {
            path: current.to_path_buf(),
            offset: start,
            len,
            line,
            col,
        });
    }
    let cwd = cwd?;
    let rels = crate::search::engine::candidate_paths(cwd, false);
    scan_files(cwd, &rels, current, lang, &sym, &pats)
}

/// 项目内扫描（候选清单来自 file_index，已排序）。
fn scan_files(
    cwd: &Path,
    rels: &[String],
    current: &Path,
    lang: Lang,
    sym: &str,
    pats: &[Regex],
) -> Option<DefTarget> {
    let mut scanned = 0usize;
    let mut bytes_budget = MAX_SCAN_BYTES;
    for rel in rels {
        let abs = cwd.join(rel);
        if abs == current || lang_of(&abs) != Some(lang) {
            continue;
        }
        scanned += 1;
        if scanned > MAX_SCAN_FILES || bytes_budget == 0 {
            break;
        }
        let Ok(meta) = std::fs::metadata(&abs) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        bytes_budget = bytes_budget.saturating_sub(meta.len());
        let Ok(bytes) = std::fs::read(&abs) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        // 廉价前置过滤：整个文件不含符号名，正则就不必跑
        if !text.contains(sym) {
            continue;
        }
        if let Some((start, len)) = find_in_text(&text, pats, None) {
            let (line, col) = line_col(&text, start);
            return Some(DefTarget {
                path: abs,
                offset: start,
                len,
                line,
                col,
            });
        }
    }
    None
}

impl Chat {
    /// 跳转到定义（Ctrl+Click / F12 / 右键菜单三条入口共用）。
    ///
    /// `offset` = 编辑器文档字节偏移（调用方从光标取）。解析在后台线程，
    /// 前台收尾只有两种动作：同文件移光标 / 跨文件开 tab + `pending_jump`。
    pub(crate) fn file_goto_definition(
        &mut self,
        path: PathBuf,
        offset: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(ed) = self.ws.file_cache.get(&path).and_then(|f| f.editor.clone()) else {
            return;
        };
        // Rope 克隆是 Arc 计数（不复制文本）；真正的 to_string 在后台做
        let text = ed.read(cx).text().clone();
        let cwd = self.ws.cwd.clone();
        cx.spawn(async move |weak, cx| {
            let probe = path.clone();
            let target = cx
                .background_spawn(async move {
                    resolve(cwd.as_deref(), &probe, &text.to_string(), offset)
                })
                .await;
            let Some(t) = target else {
                let _ = weak.update(cx, |c, cx| {
                    c.set_status(tr("editor.def_not_found").to_string(), cx);
                    cx.notify();
                });
                return;
            };
            let src = path.clone();
            let _ = weak.update(cx, |c, cx| {
                if t.path == src {
                    if let Some(ed) = c.ws.file_cache.get(&src).and_then(|f| f.editor.clone()) {
                        ed.update(cx, |st, cx| st.set_cursor_offset(t.offset, cx));
                    }
                } else {
                    // 跨文件：复用 083 搜索跳行的通道（文件 tab 渲染帧消费）
                    c.ws.pending_jump = Some(crate::shell::PendingJump {
                        path: t.path.clone(),
                        line: t.line,
                        col: t.col,
                    });
                    c.open_file_tab(t.path.clone(), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 指定文件「光标处跳定义」（右键菜单挂在某个 file tab 上，不一定是
    /// 当前活跃 tab；F12 走 [`Self::goto_definition_at_cursor`]）。
    pub(crate) fn goto_definition_in(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let offset = self
            .ws
            .file_cache
            .get(&path)
            .and_then(|f| f.editor.clone())
            .map(|ed| ed.read(cx).cursor());
        if let Some(offset) = offset {
            self.file_goto_definition(path, offset, cx);
        }
    }

    /// 当前活跃文件「光标处跳定义」的薄封装（F12 / 右键菜单用）。
    pub(crate) fn goto_definition_at_cursor(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.active_file_path() {
            self.goto_definition_in(path, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用 `|` 标记光标位（测例里不出现裸管符），拆成 `(正文, 偏移)`。
    fn marked(src: &str) -> (String, usize) {
        let off = src.find('|').expect("cursor marker");
        (src.replacen('|', "", 1), off)
    }

    /// 光标位标在引用/调用处，期望跳到后面的定义。
    fn resolve_marked(src: &str, ext: &str) -> Option<DefTarget> {
        let (doc, off) = marked(src);
        let path = std::path::PathBuf::from(format!("a.{ext}"));
        resolve(None, &path, &doc, off)
    }

    fn rust_targets(src: &str) -> Option<DefTarget> {
        resolve_marked(src, "rs")
    }

    /// 光标处取词：词中、词尾、关键字、数字都不成符号。
    #[test]
    fn word_at_basics() {
        let t = "fn helper(x: u32) { helper(x) }";
        let (w, r) = word_at(t, 5).expect("inside helper");
        assert_eq!(w, "helper");
        assert_eq!(&t[r], "helper");
        // 词尾（光标紧跟词后的空格位）仍视作命中该词
        let (w, _) = word_at(t, 9).expect("word end");
        assert_eq!(w, "helper");
        // 关键字 / 空白 / 数字
        assert!(word_at(t, 1).is_none(), "fn is a keyword");
        assert!(word_at("   ", 1).is_none());
        assert!(word_at("let x = 42;", 9).is_none(), "digits are not symbols");
    }

    /// Rust：同文件 fn 定义优先于引用（引用在前也要跳到后面的定义）。
    #[test]
    fn rust_same_file_fn() {
        let doc = "fn main() {\n    helper(1);\n}\n\npub fn helper(x: i32) {}\n";
        let t = rust_targets("fn main() {\n    hel|per(1);\n}\n\npub fn helper(x: i32) {}\n")
            .expect("found");
        assert_eq!(t.path, Path::new("a.rs"));
        assert_eq!(t.line, 5);
        assert_eq!(&doc[t.offset..t.offset + t.len], "helper");
    }

    /// 注释里的 `fn foo` 不算定义（光标在调用处）。
    #[test]
    fn skips_comments() {
        let t = rust_targets(
            "// fn helper by accident\nfn main() {\n    hel|per(1);\n}\n\npub fn helper() {}\n",
        )
        .expect("found");
        assert_eq!(t.line, 6);
    }

    /// 结构体/枚举/trait 同样可跳（光标在返回位置的使用处）。
    #[test]
    fn rust_struct_and_trait() {
        let t = rust_targets("struct Wid|get { a: u8 }\nfn build() -> Widget { todo!() }\n");
        // 光标已在定义上，无其他候选 → 静默 None（不原地打转）
        assert!(t.is_none());
        let t = rust_targets("struct Widget { a: u8 }\nfn build() -> Wid|get { todo!() }\n")
            .expect("found");
        assert_eq!(t.line, 1);
    }

    /// Python：`def` / `class` / 模块级常量。
    #[test]
    fn python_defs() {
        let py = Path::new("m.py");
        let doc = "def run():\n    return helper()\n\n\ndef helper():\n    pass\n";
        let off = doc.find("helper()").unwrap() + 1;
        let t = resolve(None, py, doc, off).expect("found");
        assert_eq!(t.line, 5);

        let doc2 = "MAX = 3\n\ndef f():\n    return MAX\n";
        let off2 = doc2.rfind("MAX").unwrap();
        let t2 = resolve(None, py, doc2, off2).expect("found");
        assert_eq!(t2.line, 1);
    }

    /// C 风格：函数定义/原型 + struct。
    #[test]
    fn c_functions() {
        let p = Path::new("m.c");
        let doc = "struct point { int x; };\n\nint add(int a, int b) {\n    return a + b;\n}\n\nint use(void) { return add(1, 2); }\n";
        let off = doc.rfind("add(1").unwrap() + 1;
        let t = resolve(None, p, doc, off).expect("found");
        assert_eq!(t.line, 3);
    }

    /// Go：方法（带接收者）也能跳。
    #[test]
    fn go_methods() {
        let p = Path::new("m.go");
        let doc = "package m\n\nfunc (s *Server) handle() {}\n\nfunc run(s *Server) { s.handle() }\n";
        let off = doc.rfind("handle()").unwrap() + 1;
        let t = resolve(None, p, doc, off).expect("found");
        assert_eq!(t.line, 3);
    }

    /// 非代码后缀不做跳转（配置/文档没有「定义」语义）。
    #[test]
    fn non_code_suffix_has_no_target() {
        let doc = "a = 1\n";
        assert!(resolve(None, Path::new("x.md"), doc, 0).is_none());
        assert!(resolve(None, Path::new("x.json"), doc, 0).is_none());
        assert!(lang_of(Path::new("x.toml")).is_none());
    }

    /// 行/列互转（跨文件跳转的坐标换算，两边必须一致）。
    #[test]
    fn line_col_roundtrip() {
        let doc = "aaa\nbbb\nccc";
        assert_eq!(line_col(doc, 0), (1, 0));
        assert_eq!(line_col(doc, 5), (2, 1));
        assert_eq!(line_col(doc, doc.len()), (3, 3));
        assert_eq!(offset_of_line_col(doc, 2, 1), 5);
        assert_eq!(offset_of_line_col(doc, 3, 99), doc.len());
        assert_eq!(offset_of_line_col(doc, 1, 0), 0);
    }

    /// 跨文件：候选清单扫描命中另一个文件（不碰 git / 真实仓库）。
    #[test]
    fn scans_other_files() {
        let dir = std::env::temp_dir().join("pf-def-scan-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.rs");
        let b = dir.join("b.rs");
        std::fs::write(&a, "fn main() { helper(); }\n").unwrap();
        std::fs::write(&b, "pub fn helper() {}\n").unwrap();
        let doc = std::fs::read_to_string(&a).unwrap();
        let off = doc.find("helper()").unwrap() + 1;
        let rels = vec!["a.rs".to_string(), "b.rs".to_string()];
        let pats = patterns(Lang::Rust, "helper");
        let t = scan_files(&dir, &rels, &a, Lang::Rust, "helper", &pats).expect("found");
        assert_eq!(t.path, b);
        assert_eq!(t.line, 1);
        // 端到端：resolve 的 cwd 分支（非 git 目录走 BFS 回退）
        let t2 = resolve(Some(&dir), &a, &doc, off).expect("found via cwd");
        assert_eq!(t2.path, b);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
