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
    /// EXIF 的 DateTimeOriginal，相机按下快门的时刻。
    Captured,
    /// 文件系统的修改时间。复制、导出都会改变它。
    FileModified,
}

impl TimeSource {
    pub fn label(self) -> &'static str {
        match self {
            TimeSource::Captured => "拍摄",
            TimeSource::FileModified => "文件时间",
        }
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
