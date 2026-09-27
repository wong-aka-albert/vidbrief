//! `ui/*.html` 的**模板体检**:内联脚本里不许有跨行的普通字符串。
//!
//! 为什么值得单开一条:这类错**没有任何运行时报错能看出来**。
//! 2026-09-27 实测(`ui/report.html`,`清账(二)` 带进来的):
//!
//! ```js
//! single.title = "这一份的图是内嵌的,文件可以单独发给别人。
//! " +
//! ```
//!
//! 普通字符串在 JS 里不能跨行 —— 整段 `<script>` 编译不过,页面就成了**空壳**:
//! HTML 在、CSS 在、数据在(15 条知识点一个不少地躺在页面源码里),只有渲染全没了。
//! 浏览器只报一句 `Invalid or unexpected token`,而且**不告诉你是哪一页坏了**:
//! 用户看到的是"报告是空的",追查得从头捋。
//!
//! 测试里不背一个 JS 引擎(那要 node),就手写一个小状态机盯这一条:
//! 认得出 `"…"` / `'…'` / `` `…` `` / 行注释 / 块注释 / **正则字面量**
//! (不认正则就会把 `/[&<>"]/g` 里的引号当成字符串开头 —— 假阳比漏报更坏,
//! 没人会看一条永远红的测试)。只有"进到 `"` 或 `'` 里、到行尾还没闭合"才报。

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("无法定位仓库根目录")
}

/// 词法状态。`Line`/`Block`/`Regex`/`Tpl` 跨行是合法的,`Dq`/`Sq` 不是。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lex {
    Code,
    DoubleQuote,
    SingleQuote,
    Template,
    LineComment,
    BlockComment,
    Regex,
}

/// 正则字面量的判定:**看它前面那个有效字符**(标准启发式)——
/// 前置是运算符/分隔符/开头,`/` 就是正则的开始;前置是标识符或 `)`、`]`,
/// 那就是除号。
const REGEX_PREV: &str = "(,=:[!&|?{};+-*%~^<>";

/// 扫一段 JS,返回「第几行、行里是什么」的违规清单(空 = 干净)。
fn line_breaking_strings(script: &str) -> Vec<(usize, String)> {
    let mut bad = Vec::new();
    let mut state = Lex::Code;
    let mut prev = '\0';
    for (index, raw) in script.lines().enumerate() {
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            let next = chars.get(i + 1).copied().unwrap_or('\0');
            match state {
                Lex::Code => {
                    if ch == '/' && next == '/' {
                        state = Lex::LineComment;
                        i += 2;
                        continue;
                    }
                    if ch == '/' && next == '*' {
                        state = Lex::BlockComment;
                        i += 2;
                        continue;
                    }
                    if ch == '/' && (prev == '\0' || REGEX_PREV.contains(prev)) {
                        state = Lex::Regex;
                        i += 1;
                        continue;
                    }
                    match ch {
                        '"' => state = Lex::DoubleQuote,
                        '\'' => state = Lex::SingleQuote,
                        '`' => state = Lex::Template,
                        _ => {}
                    }
                    if !ch.is_whitespace() {
                        prev = ch;
                    }
                }
                Lex::DoubleQuote => {
                    if ch == '\\' {
                        i += 2;
                        continue;
                    }
                    if ch == '"' {
                        state = Lex::Code;
                        prev = '"';
                    }
                }
                Lex::SingleQuote => {
                    if ch == '\\' {
                        i += 2;
                        continue;
                    }
                    if ch == '\'' {
                        state = Lex::Code;
                        prev = '\'';
                    }
                }
                Lex::Template => {
                    if ch == '\\' {
                        i += 2;
                        continue;
                    }
                    if ch == '`' {
                        state = Lex::Code;
                        prev = '`';
                    }
                }
                Lex::Regex => {
                    if ch == '\\' {
                        i += 2;
                        continue;
                    }
                    if ch == '[' {
                        // 字符组里的 `/` 不结束正则。
                        i += 1;
                        while i < chars.len() && chars[i] != ']' {
                            i += if chars[i] == '\\' { 2 } else { 1 };
                        }
                    } else if ch == '/' {
                        state = Lex::Code;
                        prev = '/';
                    }
                }
                Lex::BlockComment => {
                    if ch == '*' && next == '/' {
                        state = Lex::Code;
                        i += 2;
                        continue;
                    }
                }
                Lex::LineComment => {}
            }
            i += 1;
        }
        match state {
            Lex::LineComment => state = Lex::Code, // 到行尾自解
            Lex::DoubleQuote | Lex::SingleQuote => {
                bad.push((index + 1, raw.trim().chars().take(90).collect::<String>()));
                state = Lex::Code; // 复位,后面接着看
            }
            _ => {}
        }
    }
    bad
}

/// 模板里每一段 `<script>…</script>` 的正文。
fn script_bodies(html: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for piece in html.split("<script").skip(1) {
        let Some((_, rest)) = piece.split_once('>') else {
            continue;
        };
        let body = rest.split("</script>").next().unwrap_or(rest);
        out.push(body);
    }
    out
}

/// **`ui/` 下每个模板的内联脚本都要过词法这一关。**
///
/// 扫的是目录不是清单:以后加一个视图,它自动被这条测试罩住 ——
/// 靠人记得往清单里补名字,总有一天会漏。
#[test]
fn inline_scripts_have_no_line_breaking_strings() {
    let dir = repo_root().join("ui");
    let mut checked = 0;
    let mut problems: Vec<String> = Vec::new();

    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("读不到模板目录 {}:{e}", dir.display()));
    for entry in entries {
        let path = entry.expect("目录项").path();
        if path.extension().and_then(|e| e.to_str()) != Some("html") {
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let html = std::fs::read_to_string(&path).expect("读模板");
        checked += 1;
        for body in script_bodies(&html) {
            for (line, text) in line_breaking_strings(body) {
                problems.push(format!("{name}:{line} {text}"));
            }
        }
    }

    assert!(checked >= 3, "只扫到 {checked} 个模板,`ui/` 是不是搬走了?");
    assert!(
        problems.is_empty(),
        "内联脚本里有**跨行字符串** —— 整段 JS 会编译不过,页面变成空壳:\n{}\n\
         (普通字符串在 JS 里不能跨行;要换行就拆成两个字符串用 `+` 接起来。)",
        problems.join("\n")
    );
}

/// 判据自己得先站得住:坏样本必须报,好样本必须不报。
///
/// 不钉这一条,`line_breaking_strings` 写歪了(比如整个状态机不动)
/// 会让上面那条测试永远绿 —— 而它守的恰恰是"页面全空"这种没人预料的故障。
#[test]
fn the_scanner_catches_the_bug_that_emptied_the_report_page() {
    // 事故现场(逐字来自 `ui/report.html` 修之前那一版)
    let broken = "if (DATA.self_contained) {\n  single.title = \"这一份的图是内嵌的,文件可以单独发给别人。\n\" +\n    \"视频不在里面。\";\n}\n";
    let hits = line_breaking_strings(broken);
    assert_eq!(hits.len(), 2, "跨行的那两行都该报:{hits:?}");
    assert!(hits[0].1.contains("可以单独发给别人"), "{hits:?}");

    // 修好之后:同一句拆成两个字符串用 `+` 接
    let fixed = "if (DATA.self_contained) {\n  single.title = \"这一份的图是内嵌的,文件可以单独发给别人。\" +\n    \"视频不在里面。\";\n}\n";
    assert!(line_breaking_strings(fixed).is_empty(), "修好的不该报");

    // 不该被当成字符串的三处:转义引号、模板串、正则里的引号。
    let tricky = "const a = \"他说 \\\"你好\\\"\";\nconst b = `跨\n行没关系`;\nconst esc = String(s).replace(/[&<>\"]/g, (c) => c);\n// 注释里的 \" 不算\n";
    assert!(
        line_breaking_strings(tricky).is_empty(),
        "转义/模板串/正则/注释都不该报:{:?}",
        line_breaking_strings(tricky)
    );
}
