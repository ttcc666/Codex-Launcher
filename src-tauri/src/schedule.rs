use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_TRIGGER_DELAY_SECONDS: u32 = 9_999 * 60 + 59;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    fn schtasks_name(self) -> &'static str {
        match self {
            Self::Monday => "MON",
            Self::Tuesday => "TUE",
            Self::Wednesday => "WED",
            Self::Thursday => "THU",
            Self::Friday => "FRI",
            Self::Saturday => "SAT",
            Self::Sunday => "SUN",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Monday => "周一",
            Self::Tuesday => "周二",
            Self::Wednesday => "周三",
            Self::Thursday => "周四",
            Self::Friday => "周五",
            Self::Saturday => "周六",
            Self::Sunday => "周日",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Month {
    January,
    February,
    March,
    April,
    May,
    June,
    July,
    August,
    September,
    October,
    November,
    December,
}

impl Month {
    fn schtasks_name(self) -> &'static str {
        match self {
            Self::January => "JAN",
            Self::February => "FEB",
            Self::March => "MAR",
            Self::April => "APR",
            Self::May => "MAY",
            Self::June => "JUN",
            Self::July => "JUL",
            Self::August => "AUG",
            Self::September => "SEP",
            Self::October => "OCT",
            Self::November => "NOV",
            Self::December => "DEC",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::January => "1月",
            Self::February => "2月",
            Self::March => "3月",
            Self::April => "4月",
            Self::May => "5月",
            Self::June => "6月",
            Self::July => "7月",
            Self::August => "8月",
            Self::September => "9月",
            Self::October => "10月",
            Self::November => "11月",
            Self::December => "12月",
        }
    }

    fn maximum_day(self) -> u8 {
        match self {
            Self::February => 29,
            Self::April | Self::June | Self::September | Self::November => 30,
            _ => 31,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MonthlyDay {
    Day { day: u8 },
    LastDay,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum IntervalUnit {
    Minutes,
    Hours,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ScheduleConfig {
    Daily {
        time: String,
        every_days: u16,
    },
    Weekly {
        time: String,
        every_weeks: u8,
        days: Vec<Weekday>,
    },
    Monthly {
        time: String,
        day: MonthlyDay,
        months: Vec<Month>,
    },
    Interval {
        unit: IntervalUnit,
        every: u16,
        start_time: String,
    },
    AtLogon {
        delay_seconds: u32,
    },
    AtStartup {
        delay_seconds: u32,
    },
    Cron {
        expression: String,
    },
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self::Daily {
            time: "08:40".to_string(),
            every_days: 1,
        }
    }
}

impl ScheduleConfig {
    pub fn validate(&self) -> Result<(), String> {
        compile_schedule(self).map(|_| ())
    }

    pub fn summary(&self) -> Result<String, String> {
        compile_schedule(self).map(|compiled| compiled.summary)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledSchedule {
    pub schtasks_args: Vec<String>,
    pub summary: String,
}

pub fn validate_time(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.len() != 5 || value.as_bytes().get(2) != Some(&b':') {
        return Err("计划时间必须使用严格 HH:mm 格式".to_string());
    }
    chrono::NaiveTime::parse_from_str(value, "%H:%M")
        .map(|_| ())
        .map_err(|_| "计划时间必须是 00:00 到 23:59 的有效时间".to_string())
}

pub fn compile_schedule(schedule: &ScheduleConfig) -> Result<CompiledSchedule, String> {
    match schedule {
        ScheduleConfig::Daily { time, every_days } => {
            validate_time(time)?;
            if !(1..=365).contains(every_days) {
                return Err("每日触发间隔必须在 1..=365 天之间".to_string());
            }
            Ok(CompiledSchedule {
                schtasks_args: strings([
                    "/SC",
                    "DAILY",
                    "/MO",
                    &every_days.to_string(),
                    "/ST",
                    time.trim(),
                ]),
                summary: if *every_days == 1 {
                    format!("每天 {}", time.trim())
                } else {
                    format!("每 {} 天 {}", every_days, time.trim())
                },
            })
        }
        ScheduleConfig::Weekly {
            time,
            every_weeks,
            days,
        } => {
            validate_time(time)?;
            if !(1..=52).contains(every_weeks) {
                return Err("每周触发间隔必须在 1..=52 周之间".to_string());
            }
            let days = normalized_weekdays(days)?;
            let day_argument = days
                .iter()
                .map(|day| day.schtasks_name())
                .collect::<Vec<_>>()
                .join(",");
            let day_summary = days
                .iter()
                .map(|day| day.label())
                .collect::<Vec<_>>()
                .join("、");
            Ok(CompiledSchedule {
                schtasks_args: strings([
                    "/SC",
                    "WEEKLY",
                    "/MO",
                    &every_weeks.to_string(),
                    "/D",
                    &day_argument,
                    "/ST",
                    time.trim(),
                ]),
                summary: if *every_weeks == 1 {
                    format!("每周 {} {}", day_summary, time.trim())
                } else {
                    format!("每 {} 周 {} {}", every_weeks, day_summary, time.trim())
                },
            })
        }
        ScheduleConfig::Monthly { time, day, months } => {
            validate_time(time)?;
            let months = normalized_months(months);
            if let MonthlyDay::Day { day } = day {
                if !(1..=31).contains(day) {
                    return Err("每月触发日期必须在 1..=31 之间".to_string());
                }
                if let Some(month) = months.iter().find(|month| *day > month.maximum_day()) {
                    return Err(format!("{} 不存在第 {} 天", month.label(), day));
                }
            }
            let month_argument = if months.is_empty() {
                "*".to_string()
            } else {
                months
                    .iter()
                    .map(|month| month.schtasks_name())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let month_summary = if months.is_empty() {
                "每月".to_string()
            } else {
                months
                    .iter()
                    .map(|month| month.label())
                    .collect::<Vec<_>>()
                    .join("、")
            };
            let (day_args, day_summary) = match day {
                MonthlyDay::Day { day } => {
                    (strings(["/D", &day.to_string()]), format!("{}日", day))
                }
                MonthlyDay::LastDay => (strings(["/MO", "LASTDAY"]), "最后一天".to_string()),
            };
            let mut schtasks_args = strings(["/SC", "MONTHLY"]);
            schtasks_args.extend(day_args);
            schtasks_args.extend(strings(["/M", &month_argument, "/ST", time.trim()]));
            Ok(CompiledSchedule {
                schtasks_args,
                summary: format!("{}{} {}", month_summary, day_summary, time.trim()),
            })
        }
        ScheduleConfig::Interval {
            unit,
            every,
            start_time,
        } => {
            validate_time(start_time)?;
            let (schedule_type, max, unit_label) = match unit {
                IntervalUnit::Minutes => ("MINUTE", 1_439, "分钟"),
                IntervalUnit::Hours => ("HOURLY", 23, "小时"),
            };
            if *every == 0 || u32::from(*every) > max {
                return Err(format!("{}间隔必须在 1..={} 之间", unit_label, max));
            }
            Ok(CompiledSchedule {
                schtasks_args: strings([
                    "/SC",
                    schedule_type,
                    "/MO",
                    &every.to_string(),
                    "/ST",
                    start_time.trim(),
                ]),
                summary: format!("从 {} 起每 {} {}", start_time.trim(), every, unit_label),
            })
        }
        ScheduleConfig::AtLogon { delay_seconds } => {
            compile_event_schedule("ONLOGON", "用户登录时", *delay_seconds)
        }
        ScheduleConfig::AtStartup { delay_seconds } => {
            compile_event_schedule("ONSTART", "Windows 启动时", *delay_seconds)
        }
        ScheduleConfig::Cron { expression } => compile_cron(expression),
    }
}

fn compile_event_schedule(
    schedule_type: &str,
    label: &str,
    delay_seconds: u32,
) -> Result<CompiledSchedule, String> {
    if delay_seconds > MAX_TRIGGER_DELAY_SECONDS {
        return Err(format!("触发延迟不能超过 {} 秒", MAX_TRIGGER_DELAY_SECONDS));
    }
    let mut schtasks_args = strings(["/SC", schedule_type]);
    let summary = if delay_seconds == 0 {
        label.to_string()
    } else {
        let minutes = delay_seconds / 60;
        let seconds = delay_seconds % 60;
        schtasks_args.extend(strings(["/DELAY", &format!("{minutes:04}:{seconds:02}")]));
        format!("{}延迟 {} 秒", label, delay_seconds)
    };
    Ok(CompiledSchedule {
        schtasks_args,
        summary,
    })
}

fn compile_cron(expression: &str) -> Result<CompiledSchedule, String> {
    let fields = expression.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return Err("Cron 必须是五字段：minute hour day-of-month month day-of-week".to_string());
    }
    let [minute, hour, day_of_month, month, day_of_week] =
        <[&str; 5]>::try_from(fields).expect("length checked");

    if let Some(step) = parse_wildcard_step(minute, 1, 59, "minute")? {
        require_wildcards([hour, day_of_month, month, day_of_week])?;
        return Ok(CompiledSchedule {
            schtasks_args: strings(["/SC", "MINUTE", "/MO", &step.to_string(), "/ST", "00:00"]),
            summary: format!("Cron：每 {} 分钟", step),
        });
    }

    let exact_minute = parse_exact(minute, 0, 59, "minute")?;
    if let Some((start_hour, step)) = parse_hour_step(hour)? {
        require_wildcards([day_of_month, month, day_of_week])?;
        return Ok(CompiledSchedule {
            schtasks_args: strings([
                "/SC",
                "HOURLY",
                "/MO",
                &step.to_string(),
                "/ST",
                &format!("{start_hour:02}:{exact_minute:02}"),
            ]),
            summary: format!(
                "Cron：从 {start_hour:02}:{exact_minute:02} 起每 {} 小时",
                step
            ),
        });
    }

    let exact_hour = parse_exact(hour, 0, 23, "hour")?;
    let time = format!("{exact_hour:02}:{exact_minute:02}");
    if [day_of_month, month, day_of_week]
        .iter()
        .all(|field| *field == "*")
    {
        let mut compiled = compile_schedule(&ScheduleConfig::Daily {
            time,
            every_days: 1,
        })?;
        compiled.summary = format!("Cron：{}", compiled.summary);
        return Ok(compiled);
    }

    if day_of_month == "*" && month == "*" && day_of_week != "*" {
        let days = parse_weekday_set(day_of_week)?;
        let mut compiled = compile_schedule(&ScheduleConfig::Weekly {
            time,
            every_weeks: 1,
            days,
        })?;
        compiled.summary = format!("Cron：{}", compiled.summary);
        return Ok(compiled);
    }

    if day_of_month != "*" && day_of_week == "*" {
        let day = parse_exact(day_of_month, 1, 31, "day-of-month")? as u8;
        let months = if month == "*" {
            Vec::new()
        } else {
            parse_month_set(month)?
        };
        let mut compiled = compile_schedule(&ScheduleConfig::Monthly {
            time,
            day: MonthlyDay::Day { day },
            months,
        })?;
        compiled.summary = format!("Cron：{}", compiled.summary);
        return Ok(compiled);
    }

    Err(
        "该 Cron 无法无损映射为单个 Windows 触发器；请改用可视化 Weekly/Monthly 类型或简化表达式"
            .to_string(),
    )
}

fn parse_hour_step(value: &str) -> Result<Option<(u32, u32)>, String> {
    let Some((start, step)) = value.split_once('/') else {
        return Ok(None);
    };
    let start = if start == "*" {
        0
    } else {
        parse_exact(start, 0, 23, "hour step start")?
    };
    let step = parse_exact(step, 1, 23, "hour step")?;
    Ok(Some((start, step)))
}

fn parse_wildcard_step(
    value: &str,
    min: u32,
    max: u32,
    label: &str,
) -> Result<Option<u32>, String> {
    let Some(step) = value.strip_prefix("*/") else {
        return Ok(None);
    };
    parse_exact(step, min, max, label).map(Some)
}

fn parse_exact(value: &str, min: u32, max: u32, label: &str) -> Result<u32, String> {
    if value.contains([',', '-', '/', '*']) {
        return Err(format!("Cron {label} 必须是单一数值"));
    }
    let parsed = value
        .parse::<u32>()
        .map_err(|_| format!("Cron {label} 不是有效整数: {value}"))?;
    if !(min..=max).contains(&parsed) {
        return Err(format!("Cron {label} 必须在 {min}..={max} 之间"));
    }
    Ok(parsed)
}

fn require_wildcards<const N: usize>(fields: [&str; N]) -> Result<(), String> {
    if fields.into_iter().all(|field| field == "*") {
        Ok(())
    } else {
        Err("该 Cron step 与日期限制组合无法映射为单个 Windows 触发器".to_string())
    }
}

fn parse_weekday_set(value: &str) -> Result<Vec<Weekday>, String> {
    let numbers = parse_named_set(
        value,
        0,
        7,
        &[
            ("SUN", 0),
            ("MON", 1),
            ("TUE", 2),
            ("WED", 3),
            ("THU", 4),
            ("FRI", 5),
            ("SAT", 6),
        ],
        "day-of-week",
    )?;
    let mut days = BTreeSet::new();
    for number in numbers {
        days.insert(match number {
            0 | 7 => Weekday::Sunday,
            1 => Weekday::Monday,
            2 => Weekday::Tuesday,
            3 => Weekday::Wednesday,
            4 => Weekday::Thursday,
            5 => Weekday::Friday,
            6 => Weekday::Saturday,
            _ => unreachable!("validated weekday"),
        });
    }
    Ok(days.into_iter().collect())
}

fn parse_month_set(value: &str) -> Result<Vec<Month>, String> {
    let numbers = parse_named_set(
        value,
        1,
        12,
        &[
            ("JAN", 1),
            ("FEB", 2),
            ("MAR", 3),
            ("APR", 4),
            ("MAY", 5),
            ("JUN", 6),
            ("JUL", 7),
            ("AUG", 8),
            ("SEP", 9),
            ("OCT", 10),
            ("NOV", 11),
            ("DEC", 12),
        ],
        "month",
    )?;
    Ok(numbers
        .into_iter()
        .map(|number| match number {
            1 => Month::January,
            2 => Month::February,
            3 => Month::March,
            4 => Month::April,
            5 => Month::May,
            6 => Month::June,
            7 => Month::July,
            8 => Month::August,
            9 => Month::September,
            10 => Month::October,
            11 => Month::November,
            12 => Month::December,
            _ => unreachable!("validated month"),
        })
        .collect())
}

fn parse_named_set(
    value: &str,
    min: u32,
    max: u32,
    names: &[(&str, u32)],
    label: &str,
) -> Result<BTreeSet<u32>, String> {
    let mut result = BTreeSet::new();
    for part in value.split(',') {
        if part.is_empty() || part.contains(['*', '/']) {
            return Err(format!("Cron {label} 集合不支持该语法: {part}"));
        }
        if let Some((start, end)) = part.split_once('-') {
            let start = parse_named_value(start, min, max, names, label)?;
            let end = parse_named_value(end, min, max, names, label)?;
            if start > end {
                return Err(format!("Cron {label} 范围起点不能大于终点"));
            }
            result.extend(start..=end);
        } else {
            result.insert(parse_named_value(part, min, max, names, label)?);
        }
    }
    if result.is_empty() {
        return Err(format!("Cron {label} 不能为空"));
    }
    Ok(result)
}

fn parse_named_value(
    value: &str,
    min: u32,
    max: u32,
    names: &[(&str, u32)],
    label: &str,
) -> Result<u32, String> {
    let upper = value.to_ascii_uppercase();
    if let Some((_, number)) = names.iter().find(|(name, _)| *name == upper) {
        return Ok(*number);
    }
    parse_exact(value, min, max, label)
}

fn normalized_weekdays(days: &[Weekday]) -> Result<Vec<Weekday>, String> {
    let days = days.iter().copied().collect::<BTreeSet<_>>();
    if days.is_empty() {
        return Err("每周触发至少选择一个星期".to_string());
    }
    Ok(days.into_iter().collect())
}

fn normalized_months(months: &[Month]) -> Vec<Month> {
    months
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_schedules_compile_to_expected_schtasks_arguments() {
        assert_eq!(
            compile_schedule(&ScheduleConfig::Daily {
                time: "08:40".to_string(),
                every_days: 2,
            })
            .expect("daily")
            .schtasks_args,
            strings(["/SC", "DAILY", "/MO", "2", "/ST", "08:40"])
        );
        assert_eq!(
            compile_schedule(&ScheduleConfig::Weekly {
                time: "09:15".to_string(),
                every_weeks: 1,
                days: vec![Weekday::Friday, Weekday::Monday],
            })
            .expect("weekly")
            .schtasks_args,
            strings(["/SC", "WEEKLY", "/MO", "1", "/D", "MON,FRI", "/ST", "09:15"])
        );
        assert_eq!(
            compile_schedule(&ScheduleConfig::AtStartup { delay_seconds: 65 })
                .expect("startup")
                .schtasks_args,
            strings(["/SC", "ONSTART", "/DELAY", "0001:05"])
        );
    }

    #[test]
    fn cron_subset_maps_to_native_schedules() {
        let minute = compile_schedule(&ScheduleConfig::Cron {
            expression: "*/15 * * * *".to_string(),
        })
        .expect("minute cron");
        assert_eq!(
            minute.schtasks_args,
            strings(["/SC", "MINUTE", "/MO", "15", "/ST", "00:00"])
        );

        let weekly = compile_schedule(&ScheduleConfig::Cron {
            expression: "30 9 * * MON-FRI".to_string(),
        })
        .expect("weekly cron");
        assert_eq!(
            weekly.schtasks_args,
            strings([
                "/SC",
                "WEEKLY",
                "/MO",
                "1",
                "/D",
                "MON,TUE,WED,THU,FRI",
                "/ST",
                "09:30"
            ])
        );
    }

    #[test]
    fn unsupported_cron_is_rejected_without_approximation() {
        for expression in [
            "0,30 9 * * *",
            "0 9 1 * MON",
            "0 9 * JAN MON",
            "0 0 9 * * *",
        ] {
            assert!(compile_schedule(&ScheduleConfig::Cron {
                expression: expression.to_string(),
            })
            .is_err());
        }
    }

    #[test]
    fn monthly_explicit_impossible_date_is_rejected() {
        let error = compile_schedule(&ScheduleConfig::Monthly {
            time: "08:00".to_string(),
            day: MonthlyDay::Day { day: 31 },
            months: vec![Month::February],
        })
        .expect_err("February 31 must fail");
        assert!(error.contains("2月"));
    }
}
