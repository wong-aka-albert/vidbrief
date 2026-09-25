//! §11 的 A–F 组工具,一个模块一组。
//!
//! 每个工具函数都是**薄薄一层**:把 MCP 的 JSON 参数翻译成 `vidbrief-ops` 的调用,
//! 再把结果翻回 JSON。判断、流程、文件格式一概不在这里 ——
//! 那三样分别属于 agent、`vidbrief-ops`、`docs/schema/`。

pub mod conclusions;
pub mod materials;
pub mod preprocess;
pub mod project;
pub mod shots;
pub mod view;

use rmcp::model::CallToolResult;
use tokio::sync::OwnedMutexGuard;
use vidbrief_ops::OpenProject;

use crate::json::blocking;
use crate::server::VidbriefServer;

/// 一个项目上的写操作名额。
///
/// **必须持有到操作结束** —— 它保证同一项目上的两个写工具不会交叉执行
/// (都是"读 JSON → 改 → 原子写",交叉就是后写的赢,前一个的结论凭空消失)。
pub(crate) type WriteSlot = OwnedMutexGuard<()>;

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
        let opened = blocking(move || {
            let opened = OpenProject::open(&id)?;
            state.ensure_write_lock(&opened).map_err(|e| {
                anyhow::anyhow!(
                    "{e:#}\n\
                     \n\
                     这个项目正被另一个会话编辑。**只读的活不受影响**\
                     (list_shots / get_shot / get_transcript / render_timeline 都能照用);\
                     要改材料和结论,请让对方先收工,或让用户关掉那个会话。"
                )
            })?;
            state.note(opened.dir(), opened.id());
            Ok(opened)
        })
        .await?;

        let slot = self.state.serialize(opened.dir()).await;
        Ok((opened, slot))
    }
}

#[cfg(test)]
mod tests {
    use crate::server::VidbriefServer;

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
            "render_timeline",
        ] {
            assert!(names.iter().any(|n| n == expected), "缺工具 {expected}");
        }
    }
}
