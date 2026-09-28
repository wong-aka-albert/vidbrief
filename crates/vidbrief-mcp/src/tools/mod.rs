//! §11 的 A–G 组工具,一个模块一组 —— 外加 `guide`(说明书,不属于 A–G,
//! 是「怎么把说明递给没有 prompt 能力的宿主」的那个出口)。
//!
//! 每个工具函数都是**薄薄一层**:把 MCP 的 JSON 参数翻译成 `vidbrief-ops` 的调用,
//! 再把结果翻回 JSON。判断、流程、文件格式一概不在这里 ——
//! 那三样分别属于 agent、`vidbrief-ops`、`docs/schema/`。

pub mod conclusions;
pub mod guide;
pub mod materials;
pub mod preprocess;
pub mod project;
pub mod report;
pub mod shots;
pub mod skill;
pub mod view;

use rmcp::model::{CallToolResult, ContentBlock};
use tokio::sync::OwnedMutexGuard;
use vidbrief_ops::OpenProject;

use crate::json::blocking;
use crate::server::VidbriefServer;

/// 一个项目上的写操作名额。
///
/// **必须持有到操作结束** —— 它管两件事:
///
/// 1. **排队**:同一项目上的两个写工具不会交叉执行(都是"读 JSON → 改 → 原子写",
///    交叉就是后写的赢,前一个的结论凭空消失);
/// 2. **按次持锁**:项目写锁活到这次调用结束就放(`WriteHold` 的 Drop)——
///    空转的会话不再把项目占死。
///
/// `_` 前缀是提醒:**别 drop 早了**。
pub(crate) struct WriteSlot {
    /// 名额本身。
    _guard: OwnedMutexGuard<()>,
    /// 这一趟的写锁凭证。
    _hold: crate::state::WriteHold,
    /// 写锁没锁上时的说明,**只在同一项目第一次写操作时给一次**。
    warning: Option<String>,
}

impl WriteSlot {
    /// 该不该在结果里带一句警告。带过的调用方必须把它贴进结果(见 [`attach_warning`])。
    pub(crate) fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }
}

/// 把警告贴在工具结果的最前面。
///
/// 贴在**第一个文本块的开头**,而不是另起一块:agent 逐块读,另起一块容易被当成
/// "附带信息"跳过 —— 而这句话的全部意义就是被读到。
pub(crate) fn attach_warning(
    mut result: CallToolResult,
    warning: Option<&str>,
) -> CallToolResult {
    let Some(warning) = warning else {
        return result;
    };
    match result.content.first_mut() {
        Some(ContentBlock::Text(text)) => {
            text.text = format!("⚠️ {warning}\n\n{}", text.text);
        }
        // 结果里没有文本块(理论上不会发生)时,自己开一块也比把话丢了强
        _ => result
            .content
            .insert(0, ContentBlock::text(format!("⚠️ {warning}"))),
    }
    result
}

impl VidbriefServer {
    /// 只读打开一个项目(材料查询、视图渲染)。
    pub(crate) async fn open_read(&self, project_id: &str) -> Result<OpenProject, CallToolResult> {
        let state = self.state.clone();
        let id = project_id.to_string();
        blocking(move || {
            let opened = OpenProject::open(&id)?;
            state.note(opened.dir(), opened.id());
            Ok(opened)
        })
        .await
    }

    /// 为**写**打开:确保本进程持有该项目的写锁,并排队。
    ///
    /// 拿不到锁是很正常的情况(另一个 agent 会话正开着同一个项目),
    /// 报错要**说清是哪个会话占着**,并告诉调用方"查看不受影响"——
    /// 否则 agent 会以为整个项目都废了。
    pub(crate) async fn open_write(
        &self,
        project_id: &str,
    ) -> Result<(OpenProject, WriteSlot), CallToolResult> {
        let state = self.state.clone();
        let id = project_id.to_string();
        // 凭证要和 opened 一起送出这个闭包:它一 drop 就把锁放了
        let (opened, hold) = blocking(move || {
            let opened = OpenProject::open(&id)?;
            let hold = state.enter_write(&opened).map_err(|e| {
                anyhow::anyhow!(
                    "{e:#}
                     
                     这个项目正被另一个会话编辑。**只读的活不受影响**                     (list_shots / get_shot / get_transcript / render_workbench 都能照用);                     要改材料和结论,请让对方先收工,或让用户关掉那个会话。"
                )
            })?;
            Ok((opened, hold))
        })
        .await?;

        // 写锁要是没锁上(文件系统不支持),得让用户知道 —— 但只在他这次会话
        // 第一次动这个项目时说一遍,别每个工具调用都刷一遍
        let warning = self.state.take_lock_warning(opened.dir());

        let slot = self.state.serialize(opened.dir()).await;
        Ok((
            opened,
            WriteSlot {
                _guard: slot,
                _hold: hold,
                warning,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::attach_warning;
    use crate::server::VidbriefServer;
    use rmcp::model::ContentBlock;

    /// 警告要贴在**正文最前面**,而且**不能动正文**。
    ///
    /// "贴进第一块"而不是"另起一块"是有意的:agent 逐块读,另起一块容易被当成
    /// 附带信息跳过 —— 而这句话的全部意义就是被读到。
    #[test]
    fn a_warning_goes_in_front_of_the_payload() {
        let result = attach_warning(
            crate::json::text("{\"a\":1}"),
            Some("这个文件系统不支持文件锁,已跳过互斥"),
        );
        assert_eq!(result.content.len(), 1, "贴在正文开头,不是另起一块");
        let ContentBlock::Text(text) = &result.content[0] else {
            panic!("应该是文本块");
        };
        assert!(text.text.starts_with("⚠️ 这个文件系统"), "{}", text.text);
        assert!(
            text.text.ends_with("{\"a\":1}"),
            "正文一个字都不能动(agent 要解析它):{}",
            text.text
        );
        assert_ne!(result.is_error, Some(true), "这是提醒,不是失败");
    }

    #[test]
    fn no_warning_leaves_the_result_alone() {
        let result = attach_warning(crate::json::text("原样"), None);
        let ContentBlock::Text(text) = &result.content[0] else {
            panic!("应该是文本块");
        };
        assert_eq!(text.text, "原样");
        assert_eq!(result.content.len(), 1);
    }

    /// **`skill/tools.md` 不许过期** —— 它是活的工具表的投影。
    ///
    /// 为什么用"比对"而不是"手写一份":手写的清单一定会烂(参数加一个、说明改一句,
    /// 文档不知道),而模型照着过期的清单调工具只会拿到一堆「missing field」。
    /// 这里直接拿当前工具表渲染一遍,和文件比对;不一致就把文件重写一遍提交。
    ///
    /// 要同步:`VIDBRIEF_REGEN_SKILL=1 cargo test -p vidbrief-mcp skill_tools_index`。
    #[test]
    fn skill_tools_index_is_a_projection_of_the_live_tool_table() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../skill/tools.md");
        let rendered = crate::tools::skill::render_tools_index(&VidbriefServer::new().tools_for_test());

        if std::env::var_os("VIDBRIEF_REGEN_SKILL").is_some() {
            std::fs::write(&path, &rendered).expect("写 skill/tools.md 失败");
            eprintln!("已按当前工具表重写 {}", path.display());
            return;
        }

        let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("读不到 {}:{e}
先跑一次 VIDBRIEF_REGEN_SKILL=1 cargo test -p vidbrief-mcp", path.display())
        });
        assert_eq!(
            on_disk, rendered,
            "skill/tools.md 与当前工具表对不上了 ——              `VIDBRIEF_REGEN_SKILL=1 cargo test -p vidbrief-mcp skill_tools_index` 重写它,然后一起提交"
        );
    }

    /// 每个工具名都必须落在 MCP 认的形状里:`[a-zA-Z0-9_-]{1,64}`。
    #[test]
    fn tool_names_are_mcp_legal() {
        let names = VidbriefServer::new().tool_names_for_test();
        assert!(names.len() >= 15, "工具太少,是不是漏接了某组:{names:?}");

        for name in &names {
            assert!(!name.is_empty() && name.len() <= 64, "名字长度不合法:{name}");
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
                "工具名只能含 ASCII 字母数字与 `_`/`-`:{}",
                name
            );
        }

        // 顺手钉住几个关键工具在不在(漏了它们,MCP 就是残的)
        for expected in [
            "create_project",
            "preprocess",
            "get_shot",
            "record_shot_check",
            "apply_corrections",
            "render_workbench",
        ] {
            assert!(names.iter().any(|n| n == expected), "缺工具 {expected}");
        }
    }

    /// 收集一个 schema 里所有 `$ref`(带路径,报错时指得出是哪个参数)。
    fn refs_in(node: &serde_json::Value, path: &str, out: &mut Vec<String>) {
        match node {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    let here = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    if key == "$ref" {
                        out.push(format!("{here} = {value}"));
                    }
                    refs_in(value, &here, out);
                }
            }
            serde_json::Value::Array(items) => {
                for (index, value) in items.iter().enumerate() {
                    refs_in(value, &format!("{path}[{index}]"), out);
                }
            }
            _ => {}
        }
    }

    /// **每件工具的入参 schema 都要自足:不许出现 `$ref` / `$defs`。**
    ///
    /// 这条判据是被一次真 agent 实测逼出来的(2026-09-27,agnes 3.0 flash):
    /// `build_report.style` 的 schema 是 `{"$ref": "#/$defs/StyleParam"}` ——
    /// **一个 `type` 都没有**。模型没有可依据的类型,就把那个对象写成了 JSON
    /// 字符串,连试五次全废(`invalid type: string …, expected struct StyleParam`)。
    ///
    /// 当时全表 41 件工具 / 110 个属性里,只有**两个**属性是这样裸 `$ref`:
    /// `style`(指向对象 → 猜错)和 `record_shot_check.confidence`
    /// (指向字符串枚举 → 猜"string"猜对了,纯属运气)。带 `type` 的属性一个没错。
    ///
    /// 规矩取的是宽的那一头:**不假设宿主会解引用 `$ref`**(OpenAI 的 strict 模式
    /// 干脆不收),更不假设模型会。内联的做法是 `#[schemars(inline)]` ——
    /// 标在**被引用的类型**上(类型定义仍只有一处),生成的 schema 就不再长 `$defs`。
    #[test]
    fn tool_schemas_are_self_contained() {
        let tools = VidbriefServer::new().tools_for_test();
        assert!(tools.len() >= 15, "工具太少,是不是漏接了某组");

        let mut offenders: Vec<String> = Vec::new();
        let mut properties = 0usize;
        for tool in &tools {
            let schema = serde_json::Value::Object((*tool.input_schema).clone());
            properties += schema
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|p| p.len())
                .unwrap_or(0);
            let mut refs = Vec::new();
            refs_in(&schema, &tool.name, &mut refs);
            offenders.extend(refs);
        }

        assert!(properties > 50, "属性太少({properties}),是不是工具表没接全");
        assert!(
            offenders.is_empty(),
            "这些入参是裸 `$ref`(模型看不到类型,会照最省事的猜):\n{}\n\
             修法:给被引用的类型标 `#[schemars(inline)]`。",
            offenders.join("\n")
        );
    }

    /// **`apply_corrections` 的口径要投放到 agent 真正读得到的地方。**
    ///
    /// 2026-09-28 实测(样本 D):agent 要把自己前一版改错的字退回原文,
    /// 两条判据都拒它,于是判定「没有对应档位」—— 靠猜试出 `kept`。
    /// 当时只补了工具 description 里的那张表和两条拒词,漏了 `outcome` 的
    /// **枚举变体注释**(它生成的 `oneOf[].description` 才是模型选值时看的),
    /// 一验 MCP 投放文本才发现还是旧的「看了,决定保留原句」。
    ///
    /// 所以这条判据钉的是**投放面**:description 与入参 schema 两处都要说得出
    /// 「这一段按原文算」和「退回原文」—— 改一处漏一处,这里就红。
    /// (同一条毛病在这个仓里出过不止一次:见 `group_holds_quote` 那条注释。)
    #[test]
    fn the_revert_wording_reaches_the_agent() {
        let tools = VidbriefServer::new().tools_for_test();
        let tool = tools
            .iter()
            .find(|t| t.name == "apply_corrections")
            .expect("apply_corrections 不在工具表里");
        let description = tool.description.as_deref().unwrap_or("").to_string();
        let schema = serde_json::to_string(&serde_json::Value::Object((*tool.input_schema).clone()))
            .expect("schema 序列化");

        for (what, blob) in [("description", description.as_str()), ("入参 schema", schema.as_str())] {
            for needle in ["这一段按原文算", "退回原文"] {
                assert!(
                    blob.contains(needle),
                    "{what} 里没有「{needle}」—— 退回原文这条口径没投放到 agent 读得到的地方"
                );
            }
        }
    }
}
