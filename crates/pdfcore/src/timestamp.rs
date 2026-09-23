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

    /// 各字段是否落在合法范围内。
    ///
    /// 相机没设时间时会在 EXIF 里写「0000:00:00 00:00:00」这样的占位值，
    /// 它能被解析，却不是时间。当成拍摄时间的话，它会是全批里最早的一个，
    /// 被写进 PDF 的 /CreationDate —— 这正是取证材料里最不能出现的伪造日期。
    pub fn is_plausible(&self) -> bool {
        (1000..=9999).contains(&self.year)
            && (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour <= 23
            && self.minute <= 59
            && self.second <= 60
            && self
                .utc_offset_minutes
                .is_none_or(|o| (-14 * 60..=14 * 60).contains(&o))
    }

    /// 解析 ISO 8601。XMP 与 docx 的核心属性都用这个格式。
    ///
    /// 接受 `2024-03-15`、`2024-03-15T14:30`、`2024-03-15T14:30:22.123`，
    /// 时区可以是 `Z`、`+08:00`、`+0800`、`+08` 或缺省。只有日期时时刻按 00:00:00 记。
    /// photoshop:DateCreated 常常只写日期，要求带秒的话这类照片就读不出拍摄时间了。
    pub fn parse_iso8601(s: &str) -> Option<Self> {
        let s = s.trim();
        let num = |t: &str| -> Option<u32> {
            (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
                .then(|| t.parse().ok())
                .flatten()
        };
        let date = s.get(..10)?;
        if date.as_bytes().get(4) != Some(&b'-') || date.as_bytes().get(7) != Some(&b'-') {
            return None;
        }
        let (year, month, day) = (num(&date[..4])?, num(&date[5..7])?, num(&date[8..10])?);

        let mut rest = &s[10..];
        let (mut hour, mut minute, mut second) = (0, 0, 0);
        if !rest.is_empty() {
            rest = rest.strip_prefix(['T', 't', ' '])?;
            hour = num(rest.get(..2)?)?;
            if rest.as_bytes().get(2) != Some(&b':') {
                return None;
            }
            minute = num(rest.get(3..5)?)?;
            rest = &rest[5..];
            if let Some(r) = rest.strip_prefix(':') {
                second = num(r.get(..2)?)?;
                rest = &r[2..];
                // 小数秒只影响亚秒精度，PDF 的日期格式也放不下，丢掉。
                if let Some(r) = rest.strip_prefix(['.', ',']) {
                    rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
                }
            }
        }

        let utc_offset_minutes = match rest {
            "" => None,
            "Z" | "z" => Some(0),
            _ => {
                let sign: i16 = match rest.as_bytes()[0] {
                    b'+' => 1,
                    b'-' => -1,
                    _ => return None,
                };
                let digits: String = rest[1..].chars().filter(|c| *c != ':').collect();
                if digits.len() != 2 && digits.len() != 4 {
                    return None;
                }
                let h = num(&digits[..2])? as i16;
                let m = if digits.len() == 4 {
                    num(&digits[2..])? as i16
                } else {
                    0
                };
                Some(sign * (h * 60 + m))
            }
        };

        let t = Self {
            year: u16::try_from(year).ok()?,
            month: u8::try_from(month).ok()?,
            day: u8::try_from(day).ok()?,
            hour: u8::try_from(hour).ok()?,
            minute: u8::try_from(minute).ok()?,
            second: u8::try_from(second).ok()?,
            utc_offset_minutes,
        };
        t.is_plausible().then_some(t)
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
    ///
    /// 新建和改写 PDF 都用它，不用 `pdf_writer::Date`：后者把时区拆成带符号的小时
    /// 和无符号的分钟，-00:30 这种不到一小时的负时区会丢掉负号。
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
}
