//! Video in and out through ffmpeg / ffprobe subprocesses.

use crate::engine::signal::{H, W};
use anyhow::{anyhow, bail, Context, Result};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub const FRAME_BYTES: usize = W * H * 3;

#[derive(Clone, Debug)]
pub struct Probe {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub duration: f64,
    pub has_audio: bool,
}

impl Probe {
    pub fn fps(&self) -> f64 {
        self.fps_num as f64 / self.fps_den as f64
    }
    pub fn frame_count(&self) -> i64 {
        (self.duration * self.fps()).floor() as i64
    }
}

pub fn probe(path: &Path) -> Result<Probe> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(path)
        .output()
        .context("ffprobe not found: install ffmpeg")?;
    if !out.status.success() {
        bail!("ffprobe could not read {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or_else(|| anyhow!("{} has no video stream", path.display()))?;
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");
    let (fps_num, fps_den) = parse_rate(video["avg_frame_rate"].as_str())
        .or_else(|| parse_rate(video["r_frame_rate"].as_str()))
        .unwrap_or((30000, 1001));
    let duration = v["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| video["duration"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0);
    Ok(Probe {
        path: path.to_path_buf(),
        width: video["width"].as_u64().unwrap_or(768) as u32,
        height: video["height"].as_u64().unwrap_or(480) as u32,
        fps_num,
        fps_den,
        duration,
        has_audio,
    })
}

fn parse_rate(s: Option<&str>) -> Option<(u32, u32)> {
    let (n, d) = s?.split_once('/')?;
    let (n, d): (u32, u32) = (n.parse().ok()?, d.parse().ok()?);
    (n > 0 && d > 0).then_some((n, d))
}

/// Source frames, squeezed to the box's 768 × 480 picture, from `start` seconds.
pub struct Decoder {
    child: Child,
    out: BufReader<ChildStdout>,
}

impl Decoder {
    pub fn open(p: &Probe, start: f64) -> Result<Self> {
        let mut child = Command::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-ss", &format!("{:.6}", start.max(0.0))])
            .arg("-i")
            .arg(&p.path)
            .args([
                "-an",
                "-sn",
                "-vf",
                &format!("fps={}/{},scale={W}:{H}:flags=area,format=rgb24", p.fps_num, p.fps_den),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("ffmpeg not found: install ffmpeg")?;
        let out = BufReader::with_capacity(FRAME_BYTES * 2, child.stdout.take().unwrap());
        Ok(Self { child, out })
    }

    /// Reads the next frame; false at the end of the video.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<bool> {
        let mut got = 0;
        while got < buf.len() {
            let n = self.out.read(&mut buf[got..])?;
            if n == 0 {
                return Ok(false);
            }
            got += n;
        }
        Ok(true)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// H.264 output at the source's size and frame rate, with the source's sound from `audio_start`.
pub struct Encoder {
    child: Child,
    stdin: Option<ChildStdin>,
    pub path: PathBuf,
}

impl Encoder {
    pub fn open(p: &Probe, out: &Path, audio_start: Option<f64>) -> Result<Self> {
        let w = p.width.max(2) & !1;
        let h = p.height.max(2) & !1;
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-nostdin", "-y", "-loglevel", "error"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &format!("{W}x{H}")])
            .args(["-framerate", &format!("{}/{}", p.fps_num, p.fps_den), "-i", "-"]);
        if let (Some(t), true) = (audio_start, p.has_audio) {
            cmd.args(["-ss", &format!("{:.6}", t.max(0.0)), "-i"]).arg(&p.path);
            cmd.args(["-map", "0:v:0", "-map", "1:a:0", "-c:a", "aac", "-b:a", "192k", "-shortest"]);
        }
        cmd.args([
            "-vf",
            &format!("scale={w}:{h}:flags=bicubic,setsar=1,format=yuv420p"),
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "16",
            "-movflags",
            "+faststart",
        ])
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("ffmpeg not found: install ffmpeg")?;
        let stdin = child.stdin.take();
        Ok(Self { child, stdin, path: out.to_path_buf() })
    }

    pub fn write(&mut self, frame: &[u8]) -> Result<()> {
        self.stdin
            .as_mut()
            .ok_or_else(|| anyhow!("encoder closed"))?
            .write_all(frame)
            .context("ffmpeg stopped while writing the video")
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        drop(self.stdin.take());
        let out = self.child.wait_with_output()?;
        if !out.status.success() {
            bail!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(self.path.clone())
    }
}
