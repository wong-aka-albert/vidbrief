//! 打开默认浏览器。
//!
//! 设计见 `docs/DESIGN.md` §9.1 的"弹出 UI"。这一层薄得几乎不值得单独放一个模块,
//! 但它的**失败不该让渲染失败** —— 无头机器、SSH、容器里都开不了浏览器,
//! 而 HTML 已经渲染好了,用户自己去看就行。

/// 打开 URL。失败时返回错误,由调用方决定是警告还是忽略。
pub fn open(url: &str) -> anyhow::Result<()> {
    open::that_detached(url).map_err(open_failed)
}

/// 错误文案单独拎出来,好测 —— 而且要保证它**指出退路**。
///
/// (不能靠"传一个坏 URL 看它失败"来测:`open::that_detached("")` 在 Windows 上
/// 会成功地起一个空命令,那个方向根本测不出错误。)
fn open_failed(e: std::io::Error) -> anyhow::Error {
    anyhow::anyhow!("打不开默认浏览器:{e}(HTML 已经渲染好了,可以手动打开)")
}

#[cfg(test)]
mod tests {
    use std::io;

    #[test]
    fn the_error_message_points_at_a_way_out() {
        let msg = super::open_failed(io::Error::other("没有关联的浏览器")).to_string();
        assert!(msg.contains("没有关联的浏览器"), "要带上底层原因:{msg}");
        assert!(
            msg.contains("手动打开"),
            "无头机器上开不了浏览器是常态,必须告诉用户还有退路:{msg}"
        );
    }
}
