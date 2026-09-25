//! 测试夹具:一个**不用 ffmpeg** 就能造出来的完整项目目录。
//!
//! 为什么值得单独写一个:本层的大部分函数只跟项目目录里的 JSON 打交道。
//! 如果每条测试都要先跑一遍 `ffprobe` 和抽帧,反馈环就从毫秒变成几十秒 ——
//! 而这一层根本没有算法,不该被算法的代价拖住。
//!
//! 造出来的东西**形状与真项目一致**(同样的文件、同样的字段),
//! 只是内容是人手填的:源视频是个几字节的占位文件,`video.meta.json` 直接写。
//! 凡是要"看画面"的地方(抽帧、转录)才必须用真素材,那些测试在别的 crate。

use std::sync::atomic::{AtomicUsize, Ordering};

use vidbrief_project::{
    ChangeCurve, ChangePoint, Layout, Project, ProjectId, RelPath, Shot, ShotId, ShotOrigin,
    Shots, ShotStatus, SourceLink, Transcript, VideoMeta, video_rel_for,
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// 一个临时的假项目。**Drop 时删掉整个目录。**
pub struct Fake {
    layout: Layout,
}

impl Fake {
    /// `tag` 必须在同一进程里唯一(测试是并行跑的,重名会互相踩)。
    pub fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "vidbrief-ops-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let layout = Layout::new(&dir);
        layout.create_dirs().expect("建目录骨架");

        let rel = video_rel_for(std::path::Path::new("video.mp4"));
        std::fs::write(layout.rel(rel.as_str()), b"not a real video").expect("占位源视频");

        let mut project = Project::new(
            ProjectId::new("demo").expect("demo 是合法 id"),
            "测试项目",
            rel.clone(),
            "E:/fake/video.mp4",
            SourceLink::Copy,
        );
        project.save(&layout.project_json()).expect("写 project.json");

        let meta = VideoMeta {
            path: rel,
            duration: 60.0,
            width: Some(640),
            height: Some(360),
            fps: Some(25.0),
            codec: Some("h264".into()),
            size_bytes: 16,
            sha256: "0".repeat(64),
            probed_at: vidbrief_project::now(),
            prober: "fixture".into(),
        };
        meta.save(&layout.video_meta()).expect("写 video.meta.json");

        Self { layout }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn open(&self) -> crate::project::OpenProject {
        crate::project::OpenProject::from_layout(self.layout.clone()).expect("打开假项目")
    }

    /// 写一份转录。`(起, 止, 文本)`。
    pub fn with_transcript(self, segs: &[(f64, f64, &str)]) -> Self {
        let transcript = Transcript {
            source: RelPath::new(video_rel_for(std::path::Path::new("video.mp4")).as_str())
                .expect("合法相对路径"),
            model: "fixture".into(),
            language: "zh".into(),
            duration: 60.0,
            generated_at: vidbrief_project::now(),
            segments: segs
                .iter()
                .enumerate()
                .map(|(i, (start, end, text))| vidbrief_project::Segment {
                    id: i as u32 + 1,
                    start: *start,
                    end: *end,
                    text: (*text).to_string(),
                    corrected_text: None,
                    correction: None,
                })
                .collect(),
        };
        transcript
            .save(&self.layout.transcript())
            .expect("写 transcript.json");
        self
    }

    /// 写一份差帧图,点落在整秒上(`t = 0..n`)。
    ///
    /// **连带写 `dense_regions.json`** —— 步 1 的这两样产出总是一起来的,
    /// 只造一半会让视图渲染这类下游直接报"缺文件",测出来的就不是你要测的东西。
    pub fn with_curve(self, n: usize) -> Self {
        let curve = ChangeCurve {
            source: RelPath::new(video_rel_for(std::path::Path::new("video.mp4")).as_str())
                .expect("合法相对路径"),
            duration: 60.0,
            fps_sampled: 1.0,
            width: 480,
            height: 270,
            metric: vidbrief_project::curve::DEFAULT_METRIC.to_string(),
            generated_at: vidbrief_project::now(),
            changes: (0..n)
                .map(|i| ChangePoint {
                    t: i as f64,
                    score: if i % 5 == 0 { 0.3 } else { 0.01 },
                })
                .collect(),
        };
        curve.save(&self.layout.change_curve()).expect("写差频图");

        let params = vidbrief_project::RegionParams::default();
        let regions = vidbrief_project::DenseRegions::new(
            params,
            vidbrief_project::RegionStats::new(n as u32, 0.0, &params),
            Vec::new(),
        );
        regions
            .save(&self.layout.dense_regions())
            .expect("写 dense_regions.json");
        self
    }

    /// 加一张截图(连带造出那个 jpg 占位文件)。
    pub fn with_shot(self, id: u32, t: f64) -> Self {
        self.add_shot(id, t, ShotOrigin::DenseRegion);
        self
    }

    fn add_shot(&self, id: u32, t: f64, origin: ShotOrigin) {
        let sid = ShotId::new(id).expect("合法 shot id");
        let file = Shot::default_rel_path(origin, sid, t).expect("合法截图路径");
        std::fs::write(self.layout.rel(file.as_str()), b"\xff\xd8\xff\xd9").expect("占位 jpg");

        let mut shots = Shots::load(&self.layout.shots_json()).unwrap_or_else(|_| Shots::new());
        shots.shots.push(Shot {
            id: sid,
            t,
            file,
            origin,
            region_id: None,
            status: ShotStatus::Keep,
            reason: None,
            created_at: vidbrief_project::now(),
            check: None,
        });
        shots.sort_by_time();
        shots.save(&self.layout.shots_json()).expect("写 shots.json");
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.layout.root());
    }
}
