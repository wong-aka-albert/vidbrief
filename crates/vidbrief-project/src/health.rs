//! 项目健康检查:源视频还在不在、链接还可不可用。
//!
//! 存在的理由:`init` 时对"这个项目不自包含"警告过一次,**之后再也没有任何地方会提醒你**。
//! 软链接的项目可能已经悄悄坏了几周 —— 直到有人想看视频才发现。
//!
//! 三种引入方式要查的东西完全不同(见 `docs/DESIGN.md` §4.8):
//!
//! | `source.link` | 会怎么坏 | 检查什么 |
//! |---------------|---------|---------|
//! | `hardlink` | **不会坏**(数据在项目里),只是可能与源文件不再同源 | 两边是否仍是同一份数据 |
//! | `symlink` | **会坏**:源文件一移动/删除,项目立刻不可用 | 链接目标是否还在 |
//! | `copy` | 不会坏 | 无需检查 |
//!
//! 措辞放在本模块而不是 CLI,是为了能**不起进程就单测** —— 上一轮
//! "硬链接被念成复制"的教训就是文案没人测。

use std::path::Path;

use crate::layout::Layout;
use crate::meta::VideoMeta;
use crate::project::Project;
use crate::source::{self, SourceLink};

/// 检查结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// 一切正常。
    Ok(String),
    /// 值得知道,但不影响使用。
    Note(String),
    /// **项目已不可用**,必须处理。
    Problem(String),
}

impl Health {
    pub fn message(&self) -> &str {
        match self {
            Self::Ok(m) | Self::Note(m) | Self::Problem(m) => m,
        }
    }

    /// 项目是不是已经不能用了。
    pub fn is_problem(&self) -> bool {
        matches!(self, Self::Problem(_))
    }

    /// 给终端用的一行前缀。
    pub fn marker(&self) -> &'static str {
        match self {
            Self::Ok(_) => "✓",
            Self::Note(_) => "·",
            Self::Problem(_) => "✗",
        }
    }
}

/// 源视频在项目里的绝对路径。
fn source_path(project: &Project, layout: &Layout) -> std::path::PathBuf {
    layout.rel(project.source.path.as_str())
}

/// 检查源视频的链接状态。
pub fn check_source_link(project: &Project, layout: &Layout) -> Health {
    let path = source_path(project, layout);

    // 用 symlink_metadata:悬空的软链接**存在**(目录项在)但指向的文件不在,
    // `metadata` 会失败而 `symlink_metadata` 成功 —— 正好用来区分这两种情况
    if std::fs::symlink_metadata(&path).is_err() {
        return Health::Problem(format!("项目里的源视频不见了:{}", project.source.path));
    }

    match project.source.link {
        SourceLink::Copy => Health::Note(format!(
            "独立副本,与原文件无关,无需检查({})",
            crate::human_size(file_len(&path))
        )),

        SourceLink::Symlink => {
            if std::fs::metadata(&path).is_ok() {
                Health::Ok("软链接目标可达".to_string())
            } else {
                // **这条文案里不能出现"删"字。**
                //
                // 原来写的是"请恢复源文件到 X,或删掉项目用复制重建" —— 那是全仓最危险的一句:
                // 项目里装着转录、截图和 agent 的全部校准结论,"重建"等于把它们全扔掉;
                // 而读这句话的常常是**模型**,你写"删",它就会删。
                //
                // 坏掉的只有"视频读不了"这一件事,材料与结论都完好 ——
                // 该说的永远是**怎么修好**,不是**怎么清掉**。
                Health::Problem(format!(
                    "软链接已失效:{} 指向的文件不存在 —— 视频读不了,\
                     **但项目里的材料与结论都还在**。\n\
                     把源文件放回 {} 就能恢复。\n\
                     放不回去的话,可以直接拿视频文件**替换**那个软链接。\n\
                     **不要删这个项目** —— 转录、截图与校准结论都在里面,删了就没了",
                    project.source.path, project.source.original
                ))
            }
        }

        SourceLink::Hardlink => {
            let original = Path::new(&project.source.original);
            if source::same_file(&path, original) {
                Health::Ok("与源文件同源(两边是同一份数据)".to_string())
            } else if std::fs::symlink_metadata(original).is_err() {
                Health::Note(format!(
                    "源文件 {} 已不在原路径(移动或删除)。**本项目数据完整,不受影响**",
                    project.source.original
                ))
            } else {
                Health::Note(format!(
                    "源文件路径 {} 现在指向**另一份**文件(被替换过)。\
                     本项目仍持有原来那一份,不受影响",
                    project.source.original
                ))
            }
        }
    }
}

/// 重算源视频哈希,与 `video.meta.json` 里记的比对。
///
/// 这是唯一能发现"源文件被**就地修改**"的手段 —— 硬链接与软链接都会跟着变,
/// 而链接关系看起来毫无异常。几百 MB 的视频要读一遍,所以是显式动作(CLI 的 `--verify`)。
pub fn verify_content(project: &Project, meta: &VideoMeta, layout: &Layout) -> anyhow::Result<Health> {
    let path = source_path(project, layout);
    let actual = source::sha256_of(&path)?;
    if actual == meta.sha256 {
        // 只取前 16 位做个短摘要。用 `get` 而不是 `[..16]` —— 即使 sha256 被人手改成
        // 很短的字符串,这里也只是少显示几位,不会 panic
        let short = meta.sha256.get(..16).unwrap_or(&meta.sha256);
        Ok(Health::Ok(format!("内容与记录一致(sha256 {short}…)")))
    } else {
        Ok(Health::Problem(format!(
            "内容与记录**不一致** —— 源视频被改过。\n\
             \x20           记录:{}\n\
             \x20           实际:{}",
            meta.sha256, actual
        )))
    }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProjectId;

    fn setup(tag: &str) -> (std::path::PathBuf, Layout, Project, VideoMeta) {
        let root = std::env::temp_dir().join(format!("vidbrief-health-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layout = Layout::new(&root);
        layout.create_dirs().unwrap();

        let rel = crate::source::video_rel_for(Path::new("orig.mp4"));
        let mut project = Project::new(
            ProjectId::new("t").unwrap(),
            "t",
            rel.clone(),
            root.join("orig.mp4").display().to_string(),
            SourceLink::Copy,
        );
        let meta = VideoMeta {
            path: rel,
            duration: 1.0,
            width: None,
            height: None,
            fps: None,
            codec: None,
            size_bytes: 4,
            sha256: source::sha256_of(&write(root.join("orig.mp4"), b"data")).unwrap(),
            probed_at: crate::now(),
            prober: "test".into(),
        };
        project.progress.preprocess.mark_done();
        (root, layout, project, meta)
    }

    fn write(path: std::path::PathBuf, bytes: &[u8]) -> std::path::PathBuf {
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn missing_source_in_project_is_a_problem() {
        let (root, layout, project, _meta) = setup("missing");
        let h = check_source_link(&project, &layout);
        assert!(h.is_problem(), "{h:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn copy_reports_nothing_to_check() {
        let (root, layout, mut project, _meta) = setup("copy");
        let dst = layout.rel(project.source.path.as_str());
        std::fs::copy(root.join("orig.mp4"), &dst).unwrap();
        project.source.link = SourceLink::Copy;

        let h = check_source_link(&project, &layout);
        assert!(matches!(h, Health::Note(_)), "{h:?}");
        assert!(h.message().contains("无需检查"), "{h:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hardlink_reports_intact_when_both_paths_present() {
        let (root, layout, mut project, _meta) = setup("hl-ok");
        let dst = layout.rel(project.source.path.as_str());
        if std::fs::hard_link(root.join("orig.mp4"), &dst).is_err() {
            std::fs::remove_dir_all(&root).unwrap();
            return; // 文件系统不支持硬链接
        }
        project.source.link = SourceLink::Hardlink;

        let h = check_source_link(&project, &layout);
        assert!(matches!(h, Health::Ok(_)), "{h:?}");
        assert!(h.message().contains("同源"), "{h:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn deleted_original_is_only_a_note_for_hardlinks() {
        // 删掉源文件**不影响**项目 —— 数据在项目里,这不是问题,只是值得知道
        let (root, layout, mut project, _meta) = setup("hl-gone");
        let dst = layout.rel(project.source.path.as_str());
        if std::fs::hard_link(root.join("orig.mp4"), &dst).is_err() {
            std::fs::remove_dir_all(&root).unwrap();
            return;
        }
        project.source.link = SourceLink::Hardlink;
        std::fs::remove_file(root.join("orig.mp4")).unwrap();

        let h = check_source_link(&project, &layout);
        assert!(matches!(h, Health::Note(_)), "不该报成问题:{h:?}");
        assert!(!h.is_problem());
        assert!(h.message().contains("数据完整"), "{h:?}");
        // 项目里那份仍然读得出来
        assert_eq!(std::fs::read(&dst).unwrap(), b"data");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn replaced_original_is_only_a_note_for_hardlinks() {
        let (root, layout, mut project, _meta) = setup("hl-replaced");
        let dst = layout.rel(project.source.path.as_str());
        if std::fs::hard_link(root.join("orig.mp4"), &dst).is_err() {
            std::fs::remove_dir_all(&root).unwrap();
            return;
        }
        project.source.link = SourceLink::Hardlink;
        // 原地替换成另一份内容(新 inode)
        std::fs::remove_file(root.join("orig.mp4")).unwrap();
        std::fs::write(root.join("orig.mp4"), b"other").unwrap();

        let h = check_source_link(&project, &layout);
        assert!(matches!(h, Health::Note(_)), "{h:?}");
        assert!(h.message().contains("另一份"), "{h:?}");
        // 项目里那份还是旧的
        assert_eq!(std::fs::read(&dst).unwrap(), b"data");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn verify_content_catches_in_place_edit() {
        let (root, layout, mut project, meta) = setup("verify");
        let dst = layout.rel(project.source.path.as_str());
        std::fs::copy(root.join("orig.mp4"), &dst).unwrap();
        project.source.link = SourceLink::Copy;

        let ok = verify_content(&project, &meta, &layout).unwrap();
        assert!(matches!(ok, Health::Ok(_)), "{ok:?}");

        // 就地改内容(大小不变,只有哈希能发现)
        std::fs::write(&dst, b"daTa").unwrap();
        let bad = verify_content(&project, &meta, &layout).unwrap();
        assert!(bad.is_problem(), "{bad:?}");
        assert!(bad.message().contains("被改过"), "{bad:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
