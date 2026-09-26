//! `skill/tools.md` —— **自动生成**,别手改。
//!
//! ```text
//! 唯一事实源:活的工具表(名字 / 说明 / 必填参数,都从代码里的工具定义读)
//! 这份文件:    它的投影(给装了技能目录的宿主当一张地图)
//! ```
//!
//! 为什么要生成:**手写的工具清单一定会烂**。参数加一个、说明改一句,文档不知道;
//! 而模型照着过期的清单去调工具,只会拿到一堆「missing field」——
//! 那是工具的错还是文档的错,谁也说不清。
//!
//! 过期了怎么办:`VIDBRIEF_REGEN_SKILL=1 cargo test -p vidbrief-mcp skill_tools_index`
//! 会按当前工具表重写这份文件(改完记得把它一起提交)。
//!
//! **参数怎么填、门的规矩**仍然以 `get_guide(step=…)` 为准 —— 那份会展开讲;
//! 这里只有"有哪些工具、干什么、必须给什么"。

use rmcp::model::Tool;

/// 把活的工具表渲染成 `skill/tools.md` 的正文。
pub fn render_tools_index(tools: &[Tool]) -> String {
    let mut out = String::new();
    out.push_str("# 工具速查(**自动生成**,别手改)\n\n");
    out.push_str(
        "> **唯一事实源是工具自己**:名字、说明、必填参数都从活的工具表里读出来,\n\
         > 这份文件是它的投影。改了工具的说明或参数,\n\
         > `cargo test -p vidbrief-mcp skill_tools_index` 会红;\n\
         > 按提示重跑一次就把这份同步了。\n\
         >\n\
         > 参数怎么填、门的规矩、报错怎么解,**以 `get_guide(step=…)` 为准** ——\n\
         > 那份会展开讲;这里只有「有哪些工具、干什么、必须给什么」。\n\n",
    );

    out.push_str(&format!("一共 {} 件工具。\n\n", tools.len()));
    for tool in tools {
        let about = first_sentence(tool.description.as_deref().unwrap_or("(没写说明)"));
        out.push_str(&format!("- **`{}`** — {about}
", tool.name));
        let required = required_params(tool);
        if !required.is_empty() {
            out.push_str(&format!(
                "  必填:{}\n",
                required
                    .iter()
                    .map(|p| format!("`{p}`"))
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        }
    }
    out
}

/// 说明的第一句 —— 清单要短,细节留给 `get_guide` 与工具自己的 description。
fn first_sentence(description: &str) -> String {
    let flat = description.replace('\n', " ");
    let flat = flat.trim();
    // 断句只认中文句号与"句点 + 空格" —— 英文句点是路径和文件名的一部分
    // (`02_calibrate/transcript.fixed.json`),在那儿断会把句子切成半截。
    let cut = flat
        .find('。')
        .map(|i| i + '。'.len_utf8())
        .or_else(|| flat.find(". ").map(|i| i + 1))
        .unwrap_or(flat.len());
    let head = flat.get(..cut).unwrap_or(flat).trim();
    if head.chars().count() > 90 {
        let short: String = head.chars().take(90).collect();
        format!("{short}…")
    } else {
        head.to_string()
    }
}

/// 入参 schema 里的必填项(没有 schema 或没有 required 就是空)。
fn required_params(tool: &Tool) -> Vec<String> {
    let mut names: Vec<String> = tool
        .input_schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}
