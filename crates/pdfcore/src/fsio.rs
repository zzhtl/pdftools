//! 输出文件：原子落盘，以及不覆盖任何东西的命名。
//!
//! 界面层和核心层（图片批量压缩自己写文件）共用这一处，保证两边的规矩一样。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};

/// 原子落盘：先写同目录下的 `.part`，成功后再改名。
///
/// 进程被杀、磁盘写满、用户强退 —— 任何一种情况都不该在用户选定的路径上
/// 留下一个残缺的文件，那比什么都没有更糟，因为用户会以为它是好的。
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    let tmp = path.with_file_name(name);
    std::fs::write(&tmp, data).map_err(|e| CoreError::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        CoreError::io(path, e)
    })
}

/// 批量输出的命名。
///
/// 规则只有一条：**什么都不覆盖** —— 不覆盖任何一个输入（输出目录选成源目录是很自然的事），
/// 不覆盖本批已经写出的文件（不同目录下的同名文件），也不覆盖目录里原有的文件。
/// 冲突时依次改成 `名 (2).ext`、`名 (3).ext`，与系统文件管理器复制同名文件的习惯一致。
pub struct OutputNamer {
    inputs: HashSet<PathBuf>,
    issued: HashSet<PathBuf>,
}

impl OutputNamer {
    pub fn new<P: AsRef<Path>>(inputs: impl IntoIterator<Item = P>) -> Self {
        Self {
            inputs: inputs.into_iter().map(|p| identity(p.as_ref())).collect(),
            issued: HashSet::new(),
        }
    }

    /// 用户在保存对话框里亲自指定的单个输出，是不是某个输入本身。
    /// 那种情况下对话框已经确认过「覆盖」，但覆盖的是原件，必须拦下来。
    pub fn is_input(&self, path: &Path) -> bool {
        self.inputs.contains(&identity(path))
    }

    /// 在 `dir` 里给 `stem.ext` 找一个不会覆盖任何东西的名字。
    pub fn name(&mut self, dir: &Path, stem: &str, ext: &str) -> PathBuf {
        let file = |n: usize| {
            let stem = if n == 1 {
                stem.to_string()
            } else {
                format!("{stem} ({n})")
            };
            if ext.is_empty() {
                stem
            } else {
                format!("{stem}.{ext}")
            }
        };
        (1..)
            .map(|n| dir.join(file(n)))
            .find(|candidate| {
                let key = identity(candidate);
                if candidate.exists() || self.inputs.contains(&key) || self.issued.contains(&key) {
                    return false;
                }
                self.issued.insert(key);
                true
            })
            .expect("总能找到一个没被占用的名字")
    }
}

/// 判断「是不是同一个文件」用的规范形式。
///
/// 存在的文件走 canonicalize（消掉 `..`、符号链接）；还不存在的输出规范化其父目录。
/// Windows 与 macOS 默认的文件系统不区分大小写，`A.jpg` 与 `a.jpg` 是同一个文件。
fn identity(p: &Path) -> PathBuf {
    let canon = std::fs::canonicalize(p)
        .or_else(|_| {
            let parent = p
                .parent()
                .filter(|d| !d.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            std::fs::canonicalize(parent).map(|d| d.join(p.file_name().unwrap_or_default()))
        })
        .unwrap_or_else(|_| p.to_path_buf());
    if cfg!(any(windows, target_os = "macos")) {
        PathBuf::from(canon.to_string_lossy().to_lowercase())
    } else {
        canon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pdfcore-fsio-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names_never_collide_with_inputs_outputs_or_existing_files() {
        let d = dir("names");
        let input = d.join("a.jpg");
        std::fs::write(&input, b"x").unwrap();
        std::fs::write(d.join("b.pdf"), b"old").unwrap();

        let mut namer = OutputNamer::new([&input]);
        // 与输入同名 → 改名
        assert_eq!(namer.name(&d, "a", "jpg"), d.join("a (2).jpg"));
        // 本批里再来一个同名 → 继续往后排
        assert_eq!(namer.name(&d, "a", "jpg"), d.join("a (3).jpg"));
        // 目录里已有的文件也不覆盖
        assert_eq!(namer.name(&d, "b", "pdf"), d.join("b (2).pdf"));
        // 不冲突就用原名
        assert_eq!(namer.name(&d, "c", "png"), d.join("c.png"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn is_input_sees_through_relative_paths() {
        let d = dir("same");
        let input = d.join("x.docx");
        std::fs::write(&input, b"x").unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let namer = OutputNamer::new([&input]);
        assert!(namer.is_input(&d.join("sub/../x.docx")));
        assert!(!namer.is_input(&d.join("x.pdf")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn atomic_write_leaves_no_part_file() {
        let d = dir("atomic");
        let target = d.join("out.pdf");
        write_atomic(&target, b"hello").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
        assert!(!d.join("out.pdf.part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
