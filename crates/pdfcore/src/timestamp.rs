//! 时间点。
//!
//! 对取证类场景，「这张照片是什么时候拍的」往往比照片本身还关键，
//! 所以时间必须一路从 EXIF 带到 PDF 的文档属性里，并且要说清楚它的来源 ——
//! 拍摄时间和文件修改时间不是一回事，后者随便复制一下就变了。

use std::time::SystemTime;

/// 带可选时区偏移的时间点。刻意不用 `chrono::DateTime<Tz>`：
/// EXIF 里的时间经常不带时区，硬套一个时区反而是在编造信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 相对 UTC 的分钟偏移。EXIF 里没有 `OffsetTimeOriginal` 时为 None。
    pub utc_offset_minutes: Option<i16>,
}

/// 时间是从哪来的。界面上要显示出来 —— 拍摄时间可信，文件时间只是参考。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSource {
    /// EXIF `DateTimeOriginal` —— 相机按下快门的时刻。最可信。
    Exif,
    /// XMP `xmp:CreateDate` / `photoshop:DateCreated`。
    /// EXIF 被剥掉之后，这里常常还留着。
    Xmp,
    /// IPTC `DateCreated` + `TimeCreated`。
    Iptc,
    /// 用户手动录入。文件里没有拍摄时间时，这是唯一能得到正确时间的途径。
    Manual,
    /// 文件系统时间。**不是拍摄时间**，仅作为参考显示。
    FileSystem,
}

impl TimeSource {
    pub fn label(self) -> &'static str {
        match self {
            TimeSource::Exif => "EXIF 拍摄时间",
            TimeSource::Xmp => "XMP 拍摄时间",
            TimeSource::Iptc => "IPTC 拍摄时间",
            TimeSource::Manual => "手动录入",
            TimeSource::FileSystem => "文件时间·非拍摄时间",
        }
    }

    /// 是不是可以当作「拍摄时间」使用的值。
    ///
    /// 文件系统时间不算：复制一次就被刷成当前时刻，把它写进 PDF 的
    /// `/CreationDate` 等于伪造一个拍摄时间。
    pub fn is_capture_time(self) -> bool {
        !matches!(self, TimeSource::FileSystem)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DatedFile {
    pub when: Timestamp,
    pub source: TimeSource,
}

impl Timestamp {
    pub fn from_exif(dt: &exif::DateTime) -> Self {
        Self {
            year: dt.year,
            month: dt.month,
            day: dt.day,
            hour: dt.hour,
            minute: dt.minute,
            second: dt.second,
            utc_offset_minutes: dt.offset,
        }
    }

    pub fn from_system_time(t: SystemTime) -> Self {
        let dt: chrono::DateTime<chrono::Local> = t.into();
        Self::from_chrono(&dt)
    }

    pub fn now() -> Self {
        Self::from_chrono(&chrono::Local::now())
    }

    fn from_chrono(dt: &chrono::DateTime<chrono::Local>) -> Self {
        use chrono::{Datelike, Offset, Timelike};
        let offset = dt.offset().fix().local_minus_utc() / 60;
        Self {
            year: dt.year().clamp(0, 9999) as u16,
            month: dt.month() as u8,
            day: dt.day() as u8,
            hour: dt.hour() as u8,
            minute: dt.minute() as u8,
            second: dt.second() as u8,
            utc_offset_minutes: Some(offset as i16),
        }
    }

    /// 给人看的形式。
    pub fn display(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// 解析 ISO 8601（`2024-03-15T14:30:22+08:00`、`2024-03-15 14:30:22Z`…）。
    /// XMP 与 docx 的核心属性都用这个格式。
    pub fn parse_iso8601(s: &str) -> Option<Self> {
        let s = s.trim();
        let num = |a: usize, b: usize| s.get(a..b)?.parse::<u32>().ok();
        if s.len() < 19 {
            return None;
        }
        let offset = if s.ends_with('Z') || s.ends_with('z') {
            Some(0i16)
        } else {
            // 尾部形如 +08:00 / -05:30
            let tail = &s[s.len().saturating_sub(6)..];
            let bytes = tail.as_bytes();
            match (bytes.first(), tail.len()) {
                (Some(b'+'), 6) | (Some(b'-'), 6) if bytes[3] == b':' => {
                    let h: i16 = tail[1..3].parse().ok()?;
                    let m: i16 = tail[4..6].parse().ok()?;
                    let v = h * 60 + m;
                    Some(if bytes[0] == b'-' { -v } else { v })
                }
                _ => None,
            }
        };
        Some(Self {
            year: num(0, 4)? as u16,
            month: num(5, 7)? as u8,
            day: num(8, 10)? as u8,
            hour: num(11, 13)? as u8,
            minute: num(14, 16)? as u8,
            second: num(17, 19)? as u8,
            utc_offset_minutes: offset,
        })
    }

    /// 解析用户手动录入的时间。接受 `2024-03-15 14:30`、`2024-03-15 14:30:22`、
    /// `2024/03/15 14:30` 这几种常见写法，日期和时间之间可以是空格或 T。
    pub fn parse_user_input(s: &str) -> Option<Self> {
        let s = s.trim();
        let digits: Vec<u32> = s
            .split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .map(|p| p.parse::<u32>().ok())
            .collect::<Option<Vec<_>>>()?;
        if digits.len() < 5 {
            return None;
        }
        let (y, mo, d, h, mi) = (digits[0], digits[1], digits[2], digits[3], digits[4]);
        let sec = digits.get(5).copied().unwrap_or(0);
        if !(1900..=9999).contains(&y)
            || !(1..=12).contains(&mo)
            || !(1..=31).contains(&d)
            || h > 23
            || mi > 59
            || sec > 59
        {
            return None;
        }
        Some(Self {
            year: y as u16,
            month: mo as u8,
            day: d as u8,
            hour: h as u8,
            minute: mi as u8,
            second: sec as u8,
            // 手动录入按本地时间理解，取当前时区偏移。
            utc_offset_minutes: Self::now().utc_offset_minutes,
        })
    }

    /// PDF 的日期字符串形式：`D:YYYYMMDDHHmmSS+HH'mm'`。
    /// 直接改写已有 PDF 的字典时用它（`pdf_writer::Date` 只能用于新建文档）。
    pub fn to_pdf_string(&self) -> String {
        let mut out = format!(
            "D:{:04}{:02}{:02}{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        );
        match self.utc_offset_minutes {
            Some(0) => out.push('Z'),
            Some(off) => {
                let sign = if off < 0 { '-' } else { '+' };
                let abs = off.unsigned_abs();
                out.push_str(&format!("{sign}{:02}'{:02}'", abs / 60, abs % 60));
            }
            None => {}
        }
        out
    }

    /// 转成 PDF 的日期对象。
    pub fn to_pdf_date(&self) -> pdf_writer::Date {
        let mut d = pdf_writer::Date::new(self.year)
            .month(self.month.clamp(1, 12))
            .day(self.day.clamp(1, 31))
            .hour(self.hour.min(23))
            .minute(self.minute.min(59))
            .second(self.second.min(59));
        if let Some(off) = self.utc_offset_minutes {
            let hours = (off / 60).clamp(-23, 23) as i8;
            let minutes = (off % 60).unsigned_abs().min(59) as u8;
            d = d.utc_offset_hour(hours).utc_offset_minute(minutes);
        }
        d
    }
}
