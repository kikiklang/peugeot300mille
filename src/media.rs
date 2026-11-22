//! Video in and out through ffmpeg / ffprobe subprocesses.

use crate::engine::signal::{H, W};
use anyhow::{anyhow, bail, Context, Result};
use std::borrow::Cow;
use std::io::{BufReader, ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

pub const FRAME_BYTES: usize = W * H * 3;
/// Above this a "frame rate" is a container's time base, not a real rate.
const MAX_FPS: f64 = 1000.0;
/// How much of a subprocess's error output is kept for the message.
const STDERR_TAIL: usize = 4096;

/// Takes a lock even if a thread panicked while holding it: everything guarded here stays valid
/// between two writes, and one failed frame must not take the whole app down.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A file name as ffmpeg must see it: a relative `-x.mp4` would read as an option, and an existing
/// `a:b.mp4` as the protocol `a`. Links and absolute paths go through untouched.
pub fn local_arg(path: &Path) -> Cow<'_, Path> {
    let name = path.as_os_str().as_bytes();
    if path.is_relative() && (name.starts_with(b"-") || (name.contains(&b':') && path.exists())) {
        Cow::Owned(Path::new(".").join(path))
    } else {
        Cow::Borrowed(path)
    }
}

/// Keeps reading a subprocess's stderr in the background, so that it can never fill the pipe and
/// block the process, and remembers the end of it for the error message.
pub struct StderrTail(Option<JoinHandle<String>>);

impl StderrTail {
    /// The child must have been spawned with a piped stderr.
    pub fn capture(child: &mut Child) -> Self {
        Self(child.stderr.take().map(|mut e| std::thread::spawn(move || read_tail(&mut e, STDERR_TAIL))))
    }

    /// What the process said. Call it once the process has ended.
    pub fn finish(mut self) -> String {
        self.0.take().and_then(|t| t.join().ok()).unwrap_or_default()
    }
}

/// The last `max` bytes of a stream, as text whatever their encoding.
fn read_tail(r: &mut impl Read, max: usize) -> String {
    let (mut tail, mut buf) = (Vec::new(), [0u8; 1024]);
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > max * 2 {
                    tail.drain(..tail.len() - max);
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let from = tail.len().saturating_sub(max);
    String::from_utf8_lossy(&tail[from..]).trim().to_string()
}

#[derive(Clone, Debug)]
pub struct Probe {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub duration: f64,
    pub has_audio: bool,
    /// A music file: the picture is a black background lasting as long as the sound.
    pub audio_only: bool,
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
        .arg(local_arg(path).as_os_str())
        .stdin(Stdio::null())
        .output()
        .context("ffprobe not found: install ffmpeg")?;
    if !out.status.success() {
        bail!("ffprobe could not read {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");
    // The cover art of a music file is stored as a one-image "video" stream: it doesn't count.
    let video = streams.iter().find(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1);
    let Some(video) = video else {
        if !has_audio {
            bail!("{} has neither picture nor sound", path.display());
        }
        let audio = streams.iter().find(|s| s["codec_type"] == "audio");
        // The black picture runs until this time: without a real one it would never end.
        let Some(duration) = seconds(&v["format"]["duration"]).or_else(|| audio.and_then(|a| seconds(&a["duration"]))) else {
            bail!("{}: unknown sound duration", path.display());
        };
        return Ok(Probe {
            path: path.to_path_buf(),
            width: W as u32,
            height: H as u32,
            fps_num: 30000,
            fps_den: 1001,
            duration,
            has_audio,
            audio_only: true,
        });
    };
    let (fps_num, fps_den) = parse_rate(video["avg_frame_rate"].as_str())
        .or_else(|| parse_rate(video["r_frame_rate"].as_str()))
        .unwrap_or((30000, 1001));
    let duration = seconds(&v["format"]["duration"]).or_else(|| seconds(&video["duration"])).unwrap_or(0.0);
    Ok(Probe {
        path: path.to_path_buf(),
        width: side(&video["width"], W as u32),
        height: side(&video["height"], H as u32),
        fps_num,
        fps_den,
        duration,
        has_audio,
        audio_only: false,
    })
}

/// A duration as ffprobe prints it. "nan" and "inf" do parse as numbers, but aren't durations.
fn seconds(v: &serde_json::Value) -> Option<f64> {
    v.as_str()?.trim().parse::<f64>().ok().filter(|d| d.is_finite() && *d > 0.0)
}

fn side(v: &serde_json::Value, default: u32) -> u32 {
    v.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0).unwrap_or(default)
}

fn parse_rate(s: Option<&str>) -> Option<(u32, u32)> {
    let (n, d) = s?.split_once('/')?;
    let (n, d): (u32, u32) = (n.parse().ok()?, d.parse().ok()?);
    (n > 0 && d > 0 && n as f64 / d as f64 <= MAX_FPS).then_some((n, d))
}

/// Source frames, squeezed to the box's 768 × 480 picture, from `start` seconds.
pub struct Decoder {
    child: Child,
    out: BufReader<ChildStdout>,
    errors: Option<StderrTail>,
    /// Still true when nothing came out of a file read from its start: that is a failure, where
    /// nothing after a seek can simply be the end of the file.
    expect_picture: bool,
    name: PathBuf,
}

impl Decoder {
    pub fn open(p: &Probe, start: f64) -> Result<Self> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-nostdin", "-loglevel", "error"]);
        if p.audio_only {
            let black = format!("color=c=black:s={W}x{H}:r={}/{}", p.fps_num, p.fps_den);
            cmd.args(["-f", "lavfi", "-i", &black]);
        } else {
            cmd.args(["-ss", &format!("{:.6}", start.max(0.0))]).arg("-i").arg(local_arg(&p.path).as_os_str());
        }
        let mut child = cmd
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
            .stderr(Stdio::piped())
            .spawn()
            .context("ffmpeg not found: install ffmpeg")?;
        let errors = Some(StderrTail::capture(&mut child));
        let out = BufReader::with_capacity(FRAME_BYTES * 2, child.stdout.take().unwrap());
        Ok(Self { child, out, errors, expect_picture: start <= 0.0 && !p.audio_only, name: p.path.clone() })
    }

    /// Reads the next frame; false at the end of the video.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<bool> {
        let mut got = 0;
        while got < buf.len() {
            match self.out.read(&mut buf[got..]) {
                Ok(0) => return self.ended().map(|()| false),
                Ok(n) => got += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        self.expect_picture = false;
        Ok(true)
    }

    /// The stream is over: an error only if ffmpeg failed before giving a single picture.
    fn ended(&mut self) -> Result<()> {
        if !std::mem::take(&mut self.expect_picture) {
            return Ok(());
        }
        let status = self.child.wait()?;
        let why = self.errors.take().map(StderrTail::finish).unwrap_or_default();
        if !status.success() {
            bail!("Impossible de lire l'image de {} : {why}", self.name.display());
        }
        Ok(())
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
    /// Taken by `finish`; otherwise reaped on drop.
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    errors: Option<StderrTail>,
    pub path: PathBuf,
}

impl Encoder {
    pub fn open(p: &Probe, out: &Path, audio_start: Option<f64>) -> Result<Self> {
        // ffmpeg would truncate the source it is still reading (-y), through a link as well.
        if let (Ok(a), Ok(b)) = (out.canonicalize(), p.path.canonicalize()) {
            if a == b {
                bail!("Le fichier de sortie est la vidéo source : {}", out.display());
            }
        }
        let w = p.width.max(2) & !1;
        let h = p.height.max(2) & !1;
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-nostdin", "-y", "-loglevel", "error"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &format!("{W}x{H}")])
            .args(["-framerate", &format!("{}/{}", p.fps_num, p.fps_den), "-i", "-"]);
        if let (Some(t), true) = (audio_start, p.has_audio) {
            cmd.args(["-ss", &format!("{:.6}", t.max(0.0)), "-i"]).arg(local_arg(&p.path).as_os_str());
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
        // Always a file: "./" keeps a name like "-a.mp4" or "a:b.mp4" from being read otherwise.
        .arg(if out.is_relative() { Path::new(".").join(out) } else { out.to_path_buf() })
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("ffmpeg not found: install ffmpeg")?;
        let stdin = child.stdin.take();
        let errors = Some(StderrTail::capture(&mut child));
        Ok(Self { child: Some(child), stdin, errors, path: out.to_path_buf() })
    }

    pub fn write(&mut self, frame: &[u8]) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| anyhow!("encoder closed"))?;
        if stdin.write_all(frame).is_ok() {
            return Ok(());
        }
        // ffmpeg has gone: its own last words say why, a broken pipe doesn't.
        drop(self.stdin.take());
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        let why = self.errors.take().map(StderrTail::finish).unwrap_or_default();
        bail!("ffmpeg stopped while writing the video: {why}");
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        drop(self.stdin.take());
        let status = self.child.take().ok_or_else(|| anyhow!("encoder closed"))?.wait()?;
        let why = self.errors.take().map(StderrTail::finish).unwrap_or_default();
        if !status.success() {
            bail!("ffmpeg failed: {why}");
        }
        Ok(self.path.clone())
    }
}

impl Drop for Encoder {
    /// Abandoned (an error along the way): closing stdin lets ffmpeg end a playable file, and it
    /// is waited for elsewhere so that it doesn't stay a zombie nor hold up the caller.
    fn drop(&mut self) {
        drop(self.stdin.take());
        if let Some(mut child) = self.child.take() {
            let errors = self.errors.take();
            std::thread::spawn(move || {
                let _ = child.wait();
                drop(errors);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn durations_must_be_real() {
        assert_eq!(seconds(&json!("12.5")), Some(12.5));
        for bad in [json!("nan"), json!("inf"), json!("-3"), json!("0"), json!("N/A"), json!(null)] {
            assert_eq!(seconds(&bad), None, "{bad}");
        }
    }

    #[test]
    fn odd_frame_rates_are_refused() {
        assert_eq!(parse_rate(Some("30000/1001")), Some((30000, 1001)));
        for bad in ["0/0", "25/0", "0/1", "90000/1", "abc", "25", ""] {
            assert_eq!(parse_rate(Some(bad)), None, "{bad}");
        }
        assert_eq!(parse_rate(None), None);
    }

    #[test]
    fn sides_fall_back_when_absurd() {
        assert_eq!(side(&json!(1920), 768), 1920);
        for bad in [json!(0), json!(-1), json!(u64::MAX), json!("x"), json!(null)] {
            assert_eq!(side(&bad, 768), 768, "{bad}");
        }
    }

    #[test]
    fn file_names_cannot_become_options() {
        assert_eq!(local_arg(Path::new("-y.mp4")).as_ref(), Path::new("./-y.mp4"));
        assert_eq!(local_arg(Path::new("/a/-y.mp4")).as_ref(), Path::new("/a/-y.mp4"));
        assert_eq!(local_arg(Path::new("clip.mp4")).as_ref(), Path::new("clip.mp4"));
        // Not a local file: stays a link for ffmpeg.
        assert_eq!(local_arg(Path::new("https://a/b.mp4")).as_ref(), Path::new("https://a/b.mp4"));
    }

    #[test]
    fn only_the_end_of_stderr_is_kept() {
        let mut data: &[u8] = &[b'x'; 100_000];
        assert_eq!(read_tail(&mut data, 100).len(), 100);
        let mut bad: &[u8] = b"caf\xe9 error\n";
        assert!(read_tail(&mut bad, 100).ends_with("error"));
    }
}
