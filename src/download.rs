//! Fetches a video from a link (YouTube and others) through a yt-dlp subprocess.

use crate::media::{lock, StderrTail};
use anyhow::{anyhow, bail, Context, Result};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const INSTALL_HINT: &str = "yt-dlp n'est pas installé. Dans un terminal : uv tool install yt-dlp";

/// The link being typed in an overlay slot, and its download if one is running.
#[derive(Default)]
pub struct DownloadSlot {
    pub url: String,
    pub job: Option<Arc<Download>>,
}

#[derive(Default)]
pub struct Download {
    /// 0..1, once the size is known.
    pub progress: Mutex<Option<f32>>,
    result: Mutex<Option<Result<PathBuf, String>>>,
    child: Mutex<Option<Child>>,
    cancelled: AtomicBool,
}

impl Download {
    pub fn start(url: &str) -> Arc<Self> {
        let job = Arc::new(Self::default());
        let (url, worker) = (url.trim().to_string(), job.clone());
        std::thread::spawn(move || {
            let r = worker.run(&url).map_err(|e| format!("{e:#}"));
            let r = if worker.cancelled.load(Ordering::Relaxed) { Err("Téléchargement annulé.".to_string()) } else { r };
            *lock(&worker.result) = Some(r);
        });
        job
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(c) = lock(&self.child).as_mut() {
            let _ = c.kill();
        }
    }

    /// The outcome, once, when the download has ended.
    pub fn take_result(&self) -> Option<Result<PathBuf, String>> {
        lock(&self.result).take()
    }

    fn run(&self, url: &str) -> Result<PathBuf> {
        check_url(url)?;
        let dir = videos_dir()?.join("peugeot300mille");
        std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let mut child = Command::new("yt-dlp")
            .args(["--no-playlist", "--newline", "--no-warnings", "--progress"])
            // Overlays are at most 384 pixels: 480p is plenty, and H.264 decodes fastest.
            .args(["-f", "bv*/b", "-S", "res:480,vcodec:h264"])
            .args(["--progress-template", "download:PCT %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s"])
            .args(["--print", "after_move:filepath"])
            .arg("-o")
            .arg(output_template(&dir))
            .arg("--")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { anyhow!(INSTALL_HINT) } else { e.into() })?;
        let out = child.stdout.take().unwrap();
        // In the background so a chatty stderr can never block yt-dlp.
        let errors = StderrTail::capture(&mut child);
        *lock(&self.child) = Some(child);
        // Cancelled while it was starting: `cancel` had no process to kill yet.
        if self.cancelled.load(Ordering::Relaxed) {
            self.cancel();
        }
        let mut path = None;
        // Bytes, not text: a title in another encoding must not abort the download.
        for line in BufReader::new(out).split(b'\n') {
            let Ok(line) = line else { break };
            let line = line.trim_ascii();
            match line.strip_prefix(b"PCT ") {
                Some(rest) => {
                    if let Some(f) = parse_progress(&String::from_utf8_lossy(rest)) {
                        *lock(&self.progress) = Some(f);
                    }
                }
                None if !line.is_empty() => path = Some(PathBuf::from(OsStr::from_bytes(line))),
                None => {}
            }
        }
        let status = self.wait_child();
        let err = errors.finish();
        if !status?.success() {
            let why = err.lines().rev().find(|l| l.contains("ERROR")).unwrap_or("échec du téléchargement");
            bail!("{}", why.trim_start_matches("ERROR:").trim());
        }
        path.filter(|p| p.exists()).ok_or_else(|| anyhow!("yt-dlp n'a pas indiqué de fichier"))
    }

    /// Waits without holding the lock, so that `cancel` still answers (and still kills).
    fn wait_child(&self) -> Result<std::process::ExitStatus> {
        loop {
            {
                let mut slot = lock(&self.child);
                let child = slot.as_mut().context("yt-dlp n'est plus suivi")?;
                if let Some(status) = child.try_wait()? {
                    *slot = None;
                    return Ok(status);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// yt-dlp's output pattern in `dir`, whose own `%` must not read as template fields.
fn output_template(dir: &Path) -> PathBuf {
    let mut escaped = Vec::new();
    for &b in dir.as_os_str().as_bytes() {
        escaped.push(b);
        if b == b'%' {
            escaped.push(b'%');
        }
    }
    PathBuf::from(OsString::from_vec(escaped)).join("%(title).80B [%(id)s].%(ext)s")
}

/// Only web links: anything else could be read by yt-dlp as a local file or an option.
fn check_url(url: &str) -> Result<()> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
    let has_host = rest.is_some_and(|r| !r.is_empty() && !r.starts_with('/'));
    if !has_host || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("Ce n'est pas un lien web : {url}");
    }
    Ok(())
}

/// "downloaded total estimate": the total can be "NA" while the other is known.
fn parse_progress(s: &str) -> Option<f32> {
    let mut it = s.split_whitespace().map(|v| v.parse::<f64>().ok().filter(|x| x.is_finite()));
    let done = it.next()??;
    let total = it.next().flatten().or_else(|| it.next().flatten())?;
    (total > 0.0).then(|| (done / total).clamp(0.0, 1.0) as f32)
}

fn videos_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("XDG_VIDEOS_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d));
    }
    if let Ok(o) = Command::new("xdg-user-dir").arg("VIDEOS").output() {
        let d = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if o.status.success() && !d.is_empty() {
            return Ok(PathBuf::from(d));
        }
    }
    let home = std::env::var_os("HOME").context("no home directory")?;
    Ok(PathBuf::from(home).join("Videos"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_links_are_accepted() {
        assert!(check_url("https://www.youtube.com/watch?v=abc").is_ok());
        for bad in ["--exec=rm", "/etc/passwd", "file:///etc/passwd", "https://a b", "", "https://", "http:///x", "https://a\u{7}b", "-https://a.b"] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn progress_uses_the_estimate_when_the_size_is_unknown() {
        assert_eq!(parse_progress("50 200 NA"), Some(0.25));
        assert_eq!(parse_progress("50 NA 100"), Some(0.5));
        assert_eq!(parse_progress("50 NA NA"), None);
        assert_eq!(parse_progress("nan 100 NA"), None);
        assert_eq!(parse_progress("50 inf 100"), Some(0.5));
    }

    #[test]
    fn a_percent_in_the_folder_is_not_a_template() {
        let t = output_template(Path::new("/v/100%/x"));
        assert_eq!(t, Path::new("/v/100%%/x/%(title).80B [%(id)s].%(ext)s"));
    }

    /// Needs yt-dlp: PEUGEOT300MILLE_TEST_URL=http://… XDG_VIDEOS_DIR=/tmp/x cargo test --release
    #[test]
    fn downloads_a_link() {
        let Ok(url) = std::env::var("PEUGEOT300MILLE_TEST_URL") else { return };
        let job = Download::start(&url);
        let t0 = std::time::Instant::now();
        let r = loop {
            if let Some(r) = job.take_result() {
                break r;
            }
            assert!(t0.elapsed().as_secs() < 120, "timeout");
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let p = r.expect("downloaded");
        assert!(std::fs::metadata(&p).unwrap().len() > 0);
        eprintln!("downloaded {}", p.display());
    }
}
