//! 工具返回值的小工具。
//!
//! MCP 把失败分成两类,这个区分很要紧(`docs/DESIGN.md` §6.1 的"工具不判断对错"):
//!
//! | 类别 | 怎么返回 | agent 看到什么 | 谁产生 |
//! |------|---------|---------------|--------|
//! | **协议错** | JSON-RPC `error` | 调用本身没成立:参数不合 schema、方法不存在 | `rmcp` 框架 + [`crate::server`] |
//! | **工具错** | `isError: true` 的结果 | 调用成立了,但事情没办成 —— 项目不存在、转录还没做 | 本模块的 [`failed`] |
//!
//! 本项目绝大多数失败属于第二类,而且**消息本身就是给 agent 的下一步指引**
//! (「步 1 还没跑,先调 preprocess」)。所以工具函数的签名统一写成
//! `Result<CallToolResult, CallToolResult>` —— 错误类型也是 `CallToolResult`,
//! `?` 就能直接用,不需要来回包装。

use rmcp::model::{CallToolResult, ContentBlock};

/// 把阻塞的重活挪到线程池。
///
/// 本项目的每个动作都是同步的(读文件、起 ffmpeg、起 whisper)。直接在 `async` 上下文里跑,
/// 会把 tokio 的工作线程占住 —— 抽一次帧几秒、转一次录像几分钟,
/// 期间连 `ping` 都回不了,客户端可能判定服务已死。
pub async fn blocking<T, F>(work: F) -> Result<T, CallToolResult>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Ok(value),
        // `{:#}` 打整条因果链:"为什么会失败"比"失败了"有用得多
        Ok(Err(e)) => Err(failed(format!("{e:#}"))),
        Err(e) => Err(failed(format!("后台任务崩了:{e}"))),
    }
}

/// 工具跑完了,结果是这个 JSON(缩进过,给模型读)。
pub fn json<T: serde::Serialize>(value: &T) -> Result<CallToolResult, CallToolResult> {
    let text = serde_json::to_string_pretty(value).map_err(|e| failed(format!("序列化失败:{e}")))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// 同上,但不缩进。
///
/// 给差帧图这种"点数上千、结构扁平"的产物用:缩进会让 token 翻好几倍,
/// 而它本来就是一串 `{t, score}`,缩进与否对人眼没区别。
pub fn json_compact<T: serde::Serialize>(value: &T) -> Result<CallToolResult, CallToolResult> {
    let text = serde_json::to_string(value).map_err(|e| failed(format!("序列化失败:{e}")))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// 一段人读的话(不是 JSON)。
pub fn text(message: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(message.into())])
}

/// **工具错**:调用成立了,但事情没办成。agent 能读到消息并自己决定下一步。
pub fn failed(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

/// 把 `anyhow` 的错误链拍成一段给 agent 看的文字。
pub fn failed_from(error: &anyhow::Error) -> CallToolResult {
    failed(format!("{error:#}"))
}

/// 把**工具错**转成**协议错**。
///
/// 资源读写没有 `isError` 这种余地(结果类型是 `ResourceContents`,不是工具结果),
/// 所以在那条路上必须转换一次。
pub fn to_error_data(result: CallToolResult) -> rmcp::ErrorData {
    let message = result
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "工具失败(而且没给出原因)".to_string());
    rmcp::ErrorData::internal_error(message, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_marks_the_result_as_an_error() {
        let result = failed("项目不存在");
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content.len(), 1);
    }

    #[test]
    fn success_results_are_not_errors() {
        assert_ne!(text("好了").is_error, Some(true));
    }

    #[test]
    fn json_is_pretty_and_compact_is_not() {
        let value = serde_json::json!({"a": 1, "b": [1, 2]});
        let text_of = |r: CallToolResult| match &r.content[0] {
            ContentBlock::Text(t) => t.text.clone(),
            other => panic!("应该是文本:{other:?}"),
        };
        assert!(text_of(json(&value).unwrap()).contains('\n'));
        assert!(!text_of(json_compact(&value).unwrap()).contains('\n'));
    }

    #[tokio::test]
    async fn blocking_moves_the_work_off_the_async_thread() {
        // 拿线程名当证据:活必须在别的线程上跑完
        let caller = std::thread::current().name().map(str::to_string);
        let worker = blocking(|| Ok(std::thread::current().name().map(str::to_string))).await;
        assert_ne!(worker.unwrap(), caller);
    }

    #[tokio::test]
    async fn blocking_turns_errors_into_tool_errors() {
        let result: Result<(), CallToolResult> =
            blocking(|| anyhow::bail!("转录还没做")).await;
        let error = result.unwrap_err();
        assert_eq!(error.is_error, Some(true));
    }
}
