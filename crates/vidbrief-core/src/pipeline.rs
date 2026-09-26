//! 步 1 的"视觉"部分编排:抽帧 → 差帧图 → 密集区 → 代表帧截图 → 落盘。
//!
//! **放在 `core` 而不是 CLI**,是因为这是业务逻辑不是参数解析;CLI 只该负责
//! "把命令行的东西翻译成调用"。进度通过回调传出去,库自己不打印。
//!
//! 转录(`vidbrief-asr`)不在这里 —— 它不在同一个 crate,由 CLI 串起来。

use std::path::Path;

use anyhow::{Context, Result, bail};
use vidbrief_project::{
    ChangeCurve, DenseRegions, Layout, RegionParams, Shot, ShotId, Shots, ShotStatus, VideoMeta,
};

use crate::curve::{self, CurveOptions};
use crate::extract::{self, ShotQuality};
use crate::regions;
use crate::shots::{self, PlannedShot};

/// 步 1 视觉部分的产出。
pub struct VisualReport {
    pub curve: ChangeCurve,
    pub regions: DenseRegions,
    pub shots: Shots,
    /// 抽帧失败的时刻与原因。**不致命** —— 36 张里坏一张不该让整轮白跑。
    pub skipped: Vec<(f64, String)>,
}

impl VisualReport {
    pub fn shot_count(&self) -> usize {
        self.shots.shots.len()
    }
}

/// 分析(不落盘):抽帧算曲线、找密集区。
pub fn analyze(
    video: &Path,
    meta: &VideoMeta,
    opts: CurveOptions,
    params: RegionParams,
) -> Result<(ChangeCurve, DenseRegions)> {
    let curve = curve::build_curve(video, meta.path.clone(), meta.duration, opts)?;
    let regions = regions::detect(&curve, &params);
    Ok((curve, regions))
}

/// 按计划抽帧,登记成 [`Shots`]。
///
/// `on_progress(已完成, 总数)` 每抽完一张调一次 —— 让调用方决定怎么显示,
/// 库自己不往 stdout 写东西。
pub fn extract_shots(
    video: &Path,
    layout: &Layout,
    duration: f64,
    planned: &[PlannedShot],
    quality: ShotQuality,
    on_progress: &mut dyn FnMut(usize, usize),
) -> Result<(Shots, Vec<(f64, String)>)> {
    let mut shots = Shots::new();
    let mut skipped = Vec::new();
    let total = planned.len();

    for (done, item) in planned.iter().enumerate() {
        let id = shots.next_id()?;
        // 末尾几帧可能刚好越过视频结尾,夹一下免得 ffmpeg 抽不到
        let t = clamp_time(item.t, duration);

        // 先体检:纯色帧(切黑、淡出、纯色占位)**照样截下来**,但**预标成弃用**。
        //
        // 为什么不干脆不截(这是实测改的口径):`fixtures/test_sample.mp4` 的三个切换点
        // **就是纯色帧**(合成素材:画面 → 单色 → 单色)。不截的话,"切换点必须命中"
        // 这条 M1 验收标准当场作废 —— **一次画面切换是事件,黑屏也是那个事件的证据**,
        // 把图丢掉等于把事件丢掉。所以材料的完整性优先,省的是**注意力**:
        // 标了弃用它就不进"待校准"那一批(门不拦它),agent 不必为它花一次看图调用;
        // 而它仍在清单与时间轴上,谁想看一眼都还在,`update_shot(keep)` 一句话就能翻过来。
        let flat = match extract::flatness_at(video, t) {
            Ok(f) if f.flat => Some(format!(
                "纯色帧(亮度标准差 {:.2}、单色占比 {:.0}%):像是切黑 / 淡出 / 纯色占位,\
                 默认弃用 —— 不用为它花一次看图;要留证据就 `update_shot(keep)` 改回来",
                f.luma_stddev,
                f.dominant_share * 100.0
            )),
            // 体检失败不当作抽帧失败再报一次:真抽不出来,下面那次会说话
            _ => None,
        };

        let file = Shot::default_rel_path(item.origin, id, t)?;
        let path = layout.rel(file.as_str());

        match extract::extract_frame(video, t, &path, quality) {
            Ok(()) => {
                let status = if flat.is_some() {
                    ShotStatus::Drop
                } else {
                    ShotStatus::Keep
                };
                shots.shots.push(Shot {
                    id,
                    t,
                    file,
                    origin: item.origin,
                    region_id: item.region_id,
                    status,
                    reason: flat,
                    created_at: vidbrief_project::now(),
                    check: None,
                });
            }
            Err(e) => {
                // 错误链要带上根因,否则只看到"抽帧失败"没法排查
                skipped.push((t, format!("{e:#}")));
            }
        }
        on_progress(done + 1, total);
    }

    // 一张都没留下 = 抽不出来(纯色帧现在也会留下,见上)
    if total > 0 && shots.shots.is_empty() {
        let detail = skipped
            .first()
            .map(|(t, why)| format!("(第一张 t={t:.1}s:{why})"))
            .unwrap_or_default();
        bail!("{total} 张截图全部抽取失败{detail}");
    }
    Ok((shots, skipped))
}

/// 把时刻夹进 `[0, duration)`。视频最后一帧之后是抽不到的。
fn clamp_time(t: f64, duration: f64) -> f64 {
    let upper = (duration - 0.05).max(0.0);
    t.clamp(0.0, upper)
}

/// 一次跑完视觉部分并落盘(曲线、密集区、截图、`shots.json`)。
pub fn run_visual(
    video: &Path,
    layout: &Layout,
    meta: &VideoMeta,
    opts: CurveOptions,
    params: RegionParams,
    on_progress: &mut dyn FnMut(usize, usize),
) -> Result<VisualReport> {
    let (curve, regions) = analyze(video, meta, opts, params)?;
    curve
        .save(&layout.change_curve())
        .context("写入 change_curve.json 失败")?;
    regions
        .save(&layout.dense_regions())
        .context("写入 dense_regions.json 失败")?;

    let planned = shots::plan(&regions, &curve);
    let (shots, skipped) = extract_shots(video, layout, meta.duration, &planned, ShotQuality::default(), on_progress)?;
    shots
        .save(&layout.shots_json())
        .context("写入 shots.json 失败")?;

    Ok(VisualReport {
        curve,
        regions,
        shots,
        skipped,
    })
}

/// 供测试与 CLI 复用:密集区里到底排了几帧。
pub fn plan_shots(regions: &DenseRegions, curve: &ChangeCurve) -> Vec<PlannedShot> {
    shots::plan(regions, curve)
}

/// 给一个从 1 开始的 shot id(测试与补图用)。
pub fn first_shot_id() -> ShotId {
    ShotId::FIRST
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_times_inside_the_video() {
        assert_eq!(clamp_time(-5.0, 60.0), 0.0);
        assert_eq!(clamp_time(30.0, 60.0), 30.0);
        assert!(clamp_time(60.0, 60.0) < 60.0, "末尾要留一点余量");
        assert_eq!(clamp_time(1.0, 0.0), 0.0, "零长视频不该算出负数");
    }
}
