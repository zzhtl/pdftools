//! LibreOffice 参照：只在开发机上用，CI 与发布包都不依赖它。
//!
//! - 用独立的 profile（`-env:UserInstallation`），不碰开发者自己的 LibreOffice 配置；
//! - profile 的界面语言固定下来：空段落用哪个字体的行高这类行为可能随 locale 变，
//!   基线里记下是哪个 locale 跑出来的，换 locale 时才知道差异从哪来；
//! - 一次调用转一批，soffice 冷启动要好几秒；
//! - 结果按 docx 内容哈希缓存，没改过的用例不重复转换。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 同一个 profile 不能被两个 soffice 进程同时使用。测试线程并发调用时在这里排队。
static LOCK: Mutex<()> = Mutex::new(());

pub struct Lo {
    exe: PathBuf,
    locale: String,
}

impl Lo {
    /// 找本机的 soffice。`PDFTOOLS_SOFFICE` 可以指定路径。找不到返回 None（调用方应当跳过）。
    pub fn find() -> Option<Self> {
        Self::find_with_locale("en-US")
    }

    pub fn find_with_locale(locale: &str) -> Option<Self> {
        let exe = std::env::var_os("PDFTOOLS_SOFFICE")
            .map(PathBuf::from)
            .or_else(|| which("soffice"))?;
        Some(Self {
            exe,
            locale: locale.to_string(),
        })
    }

    pub fn locale(&self) -> &str {
        &self.locale
    }

    /// 把一批 docx 转成 PDF，返回与输入一一对应的 PDF 路径。
    pub fn convert(&self, docs: &[PathBuf]) -> std::io::Result<Vec<PathBuf>> {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let root = super::tmp(&format!("lo-{}", self.locale));
        let cache = root.join("cache");
        let stage = root.join("stage");
        std::fs::create_dir_all(&cache)?;

        let mut out = Vec::with_capacity(docs.len());
        let mut todo: Vec<(PathBuf, String)> = Vec::new();
        for d in docs {
            let bytes = std::fs::read(d)?;
            let key = format!("{:016x}", fnv1a(&bytes));
            let pdf = cache.join(format!("{key}.pdf"));
            if !pdf.exists() && !todo.iter().any(|(_, k)| *k == key) {
                todo.push((d.clone(), key));
            }
            out.push(pdf);
        }
        if todo.is_empty() {
            return Ok(out);
        }

        // 输出文件名取自输入的文件名。不同目录下的同名 docx 会互相覆盖，
        // 所以先按哈希改名放进暂存目录。
        let _ = std::fs::remove_dir_all(&stage);
        std::fs::create_dir_all(&stage)?;
        let mut staged = Vec::new();
        for (src, key) in &todo {
            let dst = stage.join(format!("{key}.docx"));
            std::fs::copy(src, &dst)?;
            staged.push(dst);
        }

        let profile = self.profile(&root)?;
        let mut cmd = Command::new(&self.exe);
        cmd.arg(format!("-env:UserInstallation={}", file_url(&profile)))
            .args([
                "--headless",
                "--norestore",
                "--convert-to",
                "pdf",
                "--outdir",
            ])
            .arg(&cache)
            .args(&staged)
            .env("LC_ALL", "C.UTF-8")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        run_with_timeout(cmd, Duration::from_secs(60 + 10 * todo.len() as u64))?;

        for (_, key) in &todo {
            let pdf = cache.join(format!("{key}.pdf"));
            if !pdf.exists() {
                return Err(std::io::Error::other(format!(
                    "LibreOffice 没有产出 {key}.pdf（输入可能打不开）"
                )));
            }
        }
        Ok(out)
    }

    /// 独立 profile，界面语言与系统 locale 都钉成 `self.locale`。
    fn profile(&self, root: &Path) -> std::io::Result<PathBuf> {
        let profile = root.join("profile");
        let user = profile.join("user");
        std::fs::create_dir_all(&user)?;
        let xcu = user.join("registrymodifications.xcu");
        if !xcu.exists() {
            let l = &self.locale;
            std::fs::write(
                &xcu,
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<item oor:path="/org.openoffice.Setup/L10N"><prop oor:name="ooLocale" oor:op="fuse"><value>{l}</value></prop></item>
<item oor:path="/org.openoffice.Setup/L10N"><prop oor:name="ooSetupSystemLocale" oor:op="fuse"><value>{l}</value></prop></item>
</oor:items>
"#
                ),
            )?;
        }
        Ok(profile)
    }
}

fn run_with_timeout(mut cmd: Command, limit: Duration) -> std::io::Result<()> {
    let mut child = cmd.spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other(format!("soffice 退出码 {status}")))
            };
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other(format!(
                "soffice 超过 {limit:?} 没有结束，已终止"
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn file_url(p: &Path) -> String {
    let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = abs.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// FNV-1a 64。缓存键只需要稳定，不需要抗碰撞。
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
