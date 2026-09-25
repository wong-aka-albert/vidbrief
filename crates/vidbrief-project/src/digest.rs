//! 内容摘要 —— 判"**材料**变没变"用。
//!
//! # 为什么不数字节、也不用文件哈希
//!
//! 要回答的是"用户签过的那份材料还在不在",不是"文件变没变"。
//! agent 刷新一次时间线视图、往笔记里多写一段,文件都变了,可那些不是他签的东西 ——
//! 拿文件哈希当判据,人会被无谓地拦下来重签。
//!
//! 所以摘要算在**挑出来的字段**上。挑哪些是政策,不在这一层
//! (见 `vidbrief_ops::review`:它知道用户是照着什么点头的)。
//!
//! # 为什么 8 位十六进制够
//!
//! 它防的是"**悄悄改了一处**",不是防有人去撞哈希。真想去撞的人,
//! 早就能直接改文件了 —— 这道门拦不住,也不需要拦(见 `vidbrief_ops::review`
//! 那句"防漏掉,不是防伪造")。位数短一点,是为了能直接写进 `project.json`
//! 还让人读得下去。

use sha2::{Digest, Sha256};

/// 把几段文本按顺序拼起来算摘要,取前 4 字节的十六进制(8 个字符)。
///
/// 拼接处**必须加分隔符**:不加的话 `["ab", "c"]` 与 `["a", "bc"]` 摘要相同 ——
/// 而 `type_read` 与 `issues` 这种相邻字段,正好是能被这样拼错的形状。
pub fn short(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        // ASCII 的 US(单元分隔符)。转录正文与 LLM 写的字里都不会出现它,
        // 拿它当边界不会被内容本身伪造出来。
        hasher.update([0x1f]);
    }
    let sum = hasher.finalize();
    // 按字节切,不是按字符串切 —— 这里没有字符边界问题(`sum` 是 `[u8]`)。
    let mut out = String::with_capacity(8);
    for byte in &sum[..4] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_content_hashes_the_same_and_a_different_one_does_not() {
        assert_eq!(short(&["abc", "def"]), short(&["abc", "def"]));
        assert_ne!(short(&["abc", "def"]), short(&["abc", "deg"]));
    }

    /// 分隔符不能省 —— 拼起来的两个片段不能跟另一种切法撞上。
    #[test]
    fn field_boundaries_are_part_of_the_digest() {
        assert_ne!(short(&["ab", "c"]), short(&["a", "bc"]));
    }

    #[test]
    fn the_digest_is_eight_hex_chars() {
        let d = short(&["随便什么中文"]);
        assert_eq!(d.len(), 8, "{d}");
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()), "{d}");
    }
}
