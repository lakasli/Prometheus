//! Pure, bounded task trigger parsing and civil-time calculation.
use crate::trigger_calendar::{self, CalendarSet};
use chrono::{
    DateTime, Datelike, LocalResult, NaiveDate, NaiveTime, Offset, TimeZone, Timelike, Utc,
};
use chrono_tz::Tz;
use serde::{
    de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::{Map, Value};
use std::{cell::Cell, collections::BTreeSet};

pub const PROPOSAL_LIMIT: usize = 64 * 1024;
pub const TZDB_VERSION: &str = chrono_tz::IANA_TZDB_VERSION;
pub const MAX_SEARCH_DAYS: usize = 366 * 8 + 8;

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub task_trigger_draft: TaskDefinition,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDefinition {
    pub spec: String,
    pub spec_version: String,
    pub title: String,
    pub time_zone: String,
    pub trigger: Trigger,
    pub action: Action,
    pub delivery: Delivery,
    pub active_window: ActiveWindow,
    pub policy: Policy,
    pub budget: Budget,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    Once(OnceRule),
    Interval(IntervalRule),
    Calendar(CalendarRule),
    Event(EventRule),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnceMode {
    At,
    After,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnceRule {
    pub mode: OnceMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_seconds: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntervalRule {
    pub every_seconds: i64,
    pub anchor: String,
    pub first_fire: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DayClass {
    Any,
    Workday,
    Restday,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MonthDay {
    Day(u32),
    Last(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonthDate {
    pub month: u32,
    pub day: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarRule {
    pub frequency: String,
    pub at_times: Vec<String>,
    pub day_class: DayClass,
    #[serde(deserialize_with = "required_option")]
    pub calendar_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weekdays: Option<Vec<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month_days: Option<Vec<MonthDay>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month_dates: Option<Vec<MonthDate>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dates: Option<Vec<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventRule {
    pub source_id: String,
    pub event_type: String,
    pub filters: Vec<Predicate>,
    pub cooldown_seconds: i64,
    pub day_class: DayClass,
    #[serde(deserialize_with = "required_option")]
    pub calendar_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operator", rename_all = "snake_case", deny_unknown_fields)]
pub enum Predicate {
    Eq { field: String, value: Value },
    Neq { field: String, value: Value },
    In { field: String, values: Vec<Value> },
}
impl Predicate {
    pub fn field(&self) -> &str {
        match self {
            Self::Eq { field, .. } | Self::Neq { field, .. } | Self::In { field, .. } => field,
        }
    }
    fn values(&self) -> Vec<&Value> {
        match self {
            Self::Eq { value, .. } | Self::Neq { value, .. } => vec![value],
            Self::In { values, .. } => values.iter().collect(),
        }
    }
    pub fn matches(&self, data: &Value) -> bool {
        let Some(value) = data.get(self.field().strip_prefix("data.").unwrap_or("")) else {
            return false;
        };
        match self {
            Self::Eq {
                value: expected, ..
            } => value == expected,
            Self::Neq {
                value: expected, ..
            } => value != expected,
            Self::In { values, .. } => values.contains(value),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Notify(NotifyAction),
    AssistantRun(AssistantAction),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifyAction {
    pub content: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantAction {
    pub prompt: String,
    #[serde(deserialize_with = "required_option")]
    pub skill: Option<SkillRef>,
    pub persona: Persona,
    pub output_mode: String,
    pub context_policy: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRef {
    pub id: String,
    pub revision: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Persona {
    InstanceSnapshot,
    CharacterRevision { card_id: String, revision: i64 },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub kind: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveWindow {
    #[serde(deserialize_with = "required_option")]
    pub start_at: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub end_at: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub misfire: String,
    pub grace_seconds: i64,
    pub max_catch_up: i64,
    pub overlap: String,
    pub catch_up_window_seconds: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub daily_occurrences: i64,
    pub daily_model_runs: i64,
    pub max_output_tokens: i64,
    pub run_timeout_seconds: i64,
}

impl TaskDefinition {
    pub fn is_model(&self) -> bool {
        matches!(self.action, Action::AssistantRun(_))
    }
    pub fn needs_calendar(&self) -> bool {
        match &self.trigger {
            Trigger::Calendar(r) => r.day_class != DayClass::Any,
            Trigger::Event(r) => r.day_class != DayClass::Any,
            _ => false,
        }
    }
    pub fn time_zone(&self) -> Result<Tz, String> {
        self.time_zone.parse().map_err(|_| "IANA时区无效".into())
    }
    pub fn window(&self) -> Result<(Option<i64>, Option<i64>), String> {
        Ok((
            self.active_window
                .start_at
                .as_deref()
                .map(parse_instant)
                .transpose()?,
            self.active_window
                .end_at
                .as_deref()
                .map(parse_instant)
                .transpose()?,
        ))
    }
    pub fn within_window(&self, at: i64) -> Result<bool, String> {
        let (start, end) = self.window()?;
        Ok(start.is_none_or(|s| at >= s) && end.is_none_or(|e| at < e))
    }
}

pub fn parse_instant(value: &str) -> Result<i64, String> {
    if !value.is_ascii() || value.len() != 20 && value.len() != 25 {
        return Err("时间须为带秒及明确UTC偏移的RFC3339时刻".into());
    }
    let bytes = value.as_bytes();
    if bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || &value[17..19] == "60"
    {
        return Err("时间格式无效或包含闰秒".into());
    }
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|_| "时间或偏移无效")?;
    if !(1970..=2100).contains(&parsed.year()) {
        return Err("时间须在1970到2100年间".into());
    }
    Ok(parsed.timestamp())
}
pub fn parse_time(value: &str) -> Result<NaiveTime, String> {
    if value.len() != 8
        || value.as_bytes().get(2) != Some(&b':')
        || value.as_bytes().get(5) != Some(&b':')
    {
        return Err("时刻须为HH:mm:ss".into());
    }
    let time = NaiveTime::parse_from_str(value, "%H:%M:%S").map_err(|_| "时刻无效")?;
    if time.nanosecond() != 0 || time.format("%H:%M:%S").to_string() != value {
        return Err("时刻必须为00到59秒，不支持闰秒".into());
    }
    Ok(time)
}
fn nonempty(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.contains('\0')
}
fn unique<T: Serialize>(values: &[T]) -> bool {
    let mut set = BTreeSet::new();
    values
        .iter()
        .all(|v| set.insert(serde_json::to_string(v).unwrap_or_default()))
}
fn day_validate(class: DayClass, id: &Option<String>, zone: &str) -> Result<(), String> {
    if class == DayClass::Any {
        if id.is_some() {
            return Err("不限工作日时calendar_id必须为null".into());
        }
    } else if id.as_deref() != Some(trigger_calendar::CALENDAR_ID)
        || zone != trigger_calendar::CALENDAR_ZONE
    {
        return Err("工作日/休息日须使用cn-official及Asia/Shanghai".into());
    }
    Ok(())
}

pub fn validate_definition(value: &Value, _now: i64) -> Result<TaskDefinition, String> {
    let definition = if value.get("task_trigger_draft").is_some() {
        serde_json::from_value::<Proposal>(value.clone()).map(|p| p.task_trigger_draft)
    } else {
        serde_json::from_value::<TaskDefinition>(value.clone())
    }
    .map_err(|_| "触发任务字段或类型无效（不接受额外字段）")?;
    validate(&definition)?;
    Ok(definition)
}
pub fn validate(def: &TaskDefinition) -> Result<(), String> {
    if def.spec != "dsh.task-trigger"
        || def.spec_version != "1"
        || !nonempty(&def.title, 128)
        || def.title.chars().any(char::is_control)
        || def.time_zone.len() > 80
        || def.delivery.kind != "web_inbox"
    {
        return Err("任务名称、版本、时区或结果去向无效".into());
    }
    let zone = def.time_zone()?;
    if def.time_zone != "UTC" && !def.time_zone.contains('/') {
        return Err("请使用明确的IANA区域时区或UTC".into());
    }
    let (start, end) = def.window()?;
    if start.zip(end).is_some_and(|(a, b)| a >= b) {
        return Err("生效窗口结束必须晚于开始".into());
    }
    if !["skip", "coalesce_one", "bounded_catch_up"].contains(&def.policy.misfire.as_str())
        || !["skip", "coalesce_one"].contains(&def.policy.overlap.as_str())
        || !(1..=86400).contains(&def.policy.grace_seconds)
        || !(1..=3).contains(&def.policy.max_catch_up)
        || !(0..=86400).contains(&def.policy.catch_up_window_seconds)
    {
        return Err("漏触发/重叠策略范围无效".into());
    }
    if def.policy.misfire == "skip" {
        if def.policy.catch_up_window_seconds != 0 || def.policy.max_catch_up != 1 {
            return Err("skip须使用补跑窗口0和max_catch_up=1".into());
        }
    } else if def.policy.catch_up_window_seconds <= def.policy.grace_seconds
        || def.policy.misfire == "coalesce_one" && def.policy.max_catch_up != 1
    {
        return Err("补跑窗口须大于grace，合并补跑最多一次".into());
    }
    let b = &def.budget;
    if !(1..=10000).contains(&b.daily_occurrences)
        || !(0..=100).contains(&b.daily_model_runs)
        || !(128..=4096).contains(&b.max_output_tokens)
        || !(5..=300).contains(&b.run_timeout_seconds)
    {
        return Err("任务预算范围无效".into());
    }
    match &def.action {
        Action::Notify(a) => {
            if !nonempty(&a.content, 16000) || b.daily_model_runs != 0 {
                return Err("固定提醒须为1到16000字节且模型预算为0".into());
            }
        }
        Action::AssistantRun(a) => {
            if !nonempty(&a.prompt, 16000)
                || b.daily_model_runs < 1
                || def.policy.misfire == "bounded_catch_up"
                || a.context_policy != "isolated"
                || !["text", "html_drawing", "native_image"].contains(&a.output_mode.as_str())
            {
                return Err("助手任务内容、隔离策略、输出方式或预算无效".into());
            }
            if let Some(skill) = &a.skill {
                if !nonempty(&skill.id, 64) || skill.revision < 1 {
                    return Err("Skill引用无效".into());
                }
            }
            if a.output_mode != "text" && a.skill.is_none() {
                return Err("绘画输出必须选择已批准的drawing Skill".into());
            }
            if let Persona::CharacterRevision { card_id, revision } = &a.persona {
                if uuid::Uuid::parse_str(card_id).is_err() || *revision < 1 {
                    return Err("角色卡引用无效".into());
                }
            }
        }
    }
    match &def.trigger {
        Trigger::Once(r) => match r.mode {
            OnceMode::At => {
                if r.after_seconds.is_some() {
                    return Err("once.at不能同时设置after_seconds".into());
                }
                let text = r.at.as_deref().ok_or("once.at缺少时刻")?;
                let at = parse_instant(text)?;
                let parsed = DateTime::parse_from_rfc3339(text).map_err(|_| "时刻无效")?;
                let utc = DateTime::<Utc>::from_timestamp(at, 0).ok_or("时刻越界")?;
                if utc.with_timezone(&zone).offset().fix().local_minus_utc()
                    != parsed.offset().local_minus_utc()
                {
                    return Err("时刻UTC偏移与所选IANA时区不一致".into());
                }
                if !def.within_window(at)? {
                    return Err("一次性到期不在生效窗口内".into());
                }
            }
            OnceMode::After => {
                if r.at.is_some() || !r.after_seconds.is_some_and(|s| (1..=31536000).contains(&s)) {
                    return Err("once.after须仅设置1到31536000整数秒".into());
                }
            }
        },
        Trigger::Interval(r) => {
            if !(if def.is_model() { 60 } else { 1 }..=31536000).contains(&r.every_seconds)
                || r.anchor != "first_enable"
                || r.first_fire != "after_interval"
            {
                return Err("间隔须为有效整数秒，模型最少60秒，固定首次启用锚点".into());
            }
        }
        Trigger::Calendar(r) => {
            day_validate(r.day_class, &r.calendar_id, &def.time_zone)?;
            if r.at_times.is_empty() || r.at_times.len() > 24 || !unique(&r.at_times) {
                return Err("日历时刻须为1到24个不重复时刻".into());
            }
            for time in &r.at_times {
                parse_time(time)?;
            }
            let populated = [
                r.weekdays.is_some(),
                r.month_days.is_some(),
                r.month_dates.is_some(),
                r.dates.is_some(),
            ];
            let expected = match r.frequency.as_str() {
                "daily" => [false, false, false, false],
                "weekly" => [true, false, false, false],
                "monthly" => [false, true, false, false],
                "yearly" => [false, false, true, false],
                "dates" => [false, false, false, true],
                _ => return Err("日历频率无效".into()),
            };
            if populated != expected {
                return Err("日历选择器须与frequency一致".into());
            }
            if let Some(days) = &r.weekdays {
                if days.is_empty()
                    || days.len() > 7
                    || !unique(days)
                    || days.iter().any(|d| !(1..=7).contains(d))
                {
                    return Err("星期须为去重的1到7".into());
                }
            }
            if let Some(days) = &r.month_days {
                if days.is_empty()
                    || days.len() > 32
                    || !unique(days)
                    || days.iter().any(|d| match d {
                        MonthDay::Day(n) => !(1..=31).contains(n),
                        MonthDay::Last(s) => s != "last",
                    })
                {
                    return Err("每月日期须为1到31或last".into());
                }
            }
            if let Some(days) = &r.month_dates {
                if days.is_empty()
                    || days.len() > 24
                    || !unique(days)
                    || days
                        .iter()
                        .any(|d| NaiveDate::from_ymd_opt(2000, d.month, d.day).is_none())
                {
                    return Err("每年日期组合无效（允许闰年2月29日）".into());
                }
            }
            if let Some(days) = &r.dates {
                if days.is_empty() || days.len() > 100 || !unique(days) {
                    return Err("指定日期须为1到100个去重日期".into());
                }
                for date in days {
                    trigger_calendar::parse_date(date)?;
                }
            }
        }
        Trigger::Event(r) => {
            day_validate(r.day_class, &r.calendar_id, &def.time_zone)?;
            let allowed = match (r.source_id.as_str(), r.event_type.as_str()) {
                ("dsh.runs", "dsh.run.completed.v1" | "dsh.run.failed.v1") => {
                    &["data.status", "data.run_kind"][..]
                }
                ("dsh.service", "dsh.service.started.v1") => &["data.severity"][..],
                _ => return Err("事件类型或来源尚未注册".into()),
            };
            if r.filters.len() > 8 || !(0..=86400).contains(&r.cooldown_seconds) {
                return Err("事件筛选或冷却超限".into());
            }
            for predicate in &r.filters {
                if !allowed.contains(&predicate.field()) {
                    return Err("此事件不提供该筛选字段".into());
                }
                let values = predicate.values();
                if values.is_empty() || values.len() > 16 {
                    return Err("事件in条件须为1到16个值".into());
                }
                let mut seen = BTreeSet::new();
                for value in values {
                    let text = value.as_str().ok_or("当前事件目录字段仅接受字符串比较")?;
                    if !nonempty(text, 128) || !seen.insert(text) {
                        return Err("事件筛选值无效或重复".into());
                    }
                }
            }
        }
    }
    Ok(())
}

/// First due, not the enable timestamp. Persist it once per schedule epoch.
pub fn activation_anchor(def: &TaskDefinition, now: i64) -> Result<Option<i64>, String> {
    validate(def)?;
    let due = match &def.trigger {
        Trigger::Once(r) => Some(match r.mode {
            OnceMode::At => {
                let at = parse_instant(r.at.as_deref().ok_or("missing at")?)?;
                if at <= now {
                    return Err("一次性时间已过去，请重新安排".into());
                }
                at
            }
            OnceMode::After => now
                .checked_add(r.after_seconds.ok_or("missing seconds")?)
                .ok_or("时间溢出")?,
        }),
        Trigger::Interval(r) => Some(now.checked_add(r.every_seconds).ok_or("时间溢出")?),
        _ => None,
    };
    if matches!(def.trigger, Trigger::Once(_))
        && due.is_some_and(|at| !def.within_window(at).unwrap_or(false))
    {
        return Err("一次性到期不在生效窗口内".into());
    }
    Ok(due)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Due {
    pub at: i64,
    pub key: String,
    pub local: String,
    pub calendar_revision: Option<i64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Preview {
    pub items: Vec<Due>,
    pub blocked_reason: Option<String>,
    pub coverage_end: Option<String>,
    pub tzdb_version: String,
    pub estimated_anchor: bool,
}

fn due(
    def: &TaskDefinition,
    at: i64,
    key: String,
    calendar_revision: Option<i64>,
) -> Result<Due, String> {
    let utc = DateTime::<Utc>::from_timestamp(at, 0).ok_or("时间越界")?;
    Ok(Due {
        at,
        key,
        local: utc.with_timezone(&def.time_zone()?).to_rfc3339(),
        calendar_revision,
    })
}
pub fn day_matches(
    class: DayClass,
    date: NaiveDate,
    calendars: &CalendarSet,
) -> Result<(bool, Option<i64>), String> {
    if class == DayClass::Any {
        return Ok((true, None));
    }
    let (workday, revision) = calendars.workday(date)?;
    Ok((
        if class == DayClass::Workday {
            workday
        } else {
            !workday
        },
        Some(revision),
    ))
}
pub fn event_matches(
    def: &TaskDefinition,
    source: &str,
    event_type: &str,
    data: &Value,
    received_at: i64,
    calendars: &CalendarSet,
) -> Result<bool, String> {
    let Trigger::Event(r) = &def.trigger else {
        return Ok(false);
    };
    if r.source_id != source || r.event_type != event_type || !def.within_window(received_at)? {
        return Ok(false);
    }
    if !r.filters.iter().all(|filter| filter.matches(data)) {
        return Ok(false);
    }
    let date = DateTime::<Utc>::from_timestamp(received_at, 0)
        .ok_or("事件时间越界")?
        .with_timezone(&def.time_zone()?)
        .date_naive();
    Ok(day_matches(r.day_class, date, calendars)?.0)
}
fn calendar_date_matches(r: &CalendarRule, date: NaiveDate) -> bool {
    match r.frequency.as_str() {
        "daily" => true,
        "weekly" => r
            .weekdays
            .as_ref()
            .is_some_and(|days| days.contains(&date.weekday().number_from_monday())),
        "monthly" => r.month_days.as_ref().is_some_and(|days| {
            days.iter().any(|day| match day {
                MonthDay::Day(n) => date.day() == *n,
                MonthDay::Last(_) => date
                    .succ_opt()
                    .is_some_and(|next| next.month() != date.month()),
            })
        }),
        "yearly" => r.month_dates.as_ref().is_some_and(|days| {
            days.iter()
                .any(|d| d.month == date.month() && d.day == date.day())
        }),
        "dates" => r.dates.as_ref().is_some_and(|days| {
            days.iter()
                .any(|s| s == &date.format("%Y-%m-%d").to_string())
        }),
        _ => false,
    }
}

/// Computes the earliest opportunity strictly after `after`. Unknown authority
/// stops the search at that boundary; it must never become a weekend fallback.
pub fn next_due(
    def: &TaskDefinition,
    anchor: Option<i64>,
    after: i64,
    calendars: &CalendarSet,
) -> Result<Option<Due>, String> {
    let (start, end) = def.window()?;
    let lower = after.max(start.unwrap_or(i64::MIN).saturating_sub(1));
    if end.is_some_and(|value| lower >= value.saturating_sub(1)) {
        return Ok(None);
    }
    match &def.trigger {
        Trigger::Once(r) => {
            let at = match r.mode {
                OnceMode::At => parse_instant(r.at.as_deref().ok_or("缺少一次性时间")?)?,
                OnceMode::After => anchor.ok_or("相对任务尚未确定首次启用锚点")?,
            };
            if at > lower && def.within_window(at)? {
                Ok(Some(due(def, at, "once".into(), None)?))
            } else {
                Ok(None)
            }
        }
        Trigger::Interval(r) => {
            let first = anchor.ok_or("间隔任务尚未确定首次启用锚点")?;
            let ordinal = if lower < first {
                0
            } else {
                (lower - first) / r.every_seconds + 1
            };
            let at = ordinal
                .checked_mul(r.every_seconds)
                .and_then(|n| first.checked_add(n))
                .ok_or("间隔时间溢出")?;
            if def.within_window(at)? {
                Ok(Some(due(def, at, format!("interval:{ordinal}"), None)?))
            } else {
                Ok(None)
            }
        }
        Trigger::Event(_) => Ok(None),
        Trigger::Calendar(r) => {
            let zone = def.time_zone()?;
            let lower_date = DateTime::<Utc>::from_timestamp(lower, 0)
                .ok_or("搜索时间越界")?
                .with_timezone(&zone)
                .date_naive();
            let mut date = lower_date;
            let explicit_dates = r
                .dates
                .as_ref()
                .map(|days| {
                    let mut dates = days
                        .iter()
                        .map(|day| trigger_calendar::parse_date(day))
                        .collect::<Result<Vec<_>, _>>()?;
                    dates.sort();
                    Ok::<_, String>(dates)
                })
                .transpose()?;
            let final_date = r
                .dates
                .as_ref()
                .and_then(|days| days.iter().max())
                .and_then(|s| trigger_calendar::parse_date(s).ok());
            let mut times = r
                .at_times
                .iter()
                .map(|s| parse_time(s))
                .collect::<Result<Vec<_>, _>>()?;
            times.sort();
            for _ in 0..MAX_SEARCH_DAYS {
                // A finite, explicitly supplied list can jump to a far future
                // date without enumerating the intervening civil days.
                if let Some(dates) = &explicit_dates {
                    let Some(next) = dates.iter().find(|candidate| **candidate >= date) else {
                        return Ok(None);
                    };
                    date = *next;
                }
                if date.year() > 2100 || final_date.is_some_and(|last| date > last) {
                    return Ok(None);
                }
                if let Some(end) = end {
                    let end_date = DateTime::<Utc>::from_timestamp(end, 0)
                        .ok_or("结束时间越界")?
                        .with_timezone(&zone)
                        .date_naive();
                    if date > end_date {
                        return Ok(None);
                    }
                }
                if calendar_date_matches(r, date) {
                    // A valid date selector only consults authority once there
                    // can be a future time on this date, not for spent history.
                    let candidates = times
                        .iter()
                        .filter_map(
                            |time| match zone.from_local_datetime(&date.and_time(*time)) {
                                LocalResult::None => None,
                                LocalResult::Single(at) => Some(at),
                                LocalResult::Ambiguous(a, b) => {
                                    Some(if a.timestamp() < b.timestamp() { a } else { b })
                                }
                            },
                        )
                        .filter(|at| {
                            at.timestamp() > lower && end.is_none_or(|e| at.timestamp() < e)
                        })
                        .collect::<Vec<_>>();
                    if !candidates.is_empty() {
                        let (matches, revision) = day_matches(r.day_class, date, calendars)?;
                        if matches {
                            let at = candidates
                                .iter()
                                .min_by_key(|at| at.timestamp())
                                .expect("nonempty");
                            return Ok(Some(due(
                                def,
                                at.timestamp(),
                                format!(
                                    "calendar:{}:{}",
                                    date.format("%Y-%m-%d"),
                                    at.format("%H:%M:%S")
                                ),
                                revision,
                            )?));
                        }
                    }
                }
                date = date.succ_opt().ok_or("日期搜索越界")?;
            }
            Err("calendar_search_limit:未来8年内未找到匹配时刻".into())
        }
    }
}

/// Latest eligible nominal opportunities at or before `stamp`, already bounded
/// by the approved grace/catch-up window. This never expands elapsed seconds.
pub fn recent_due(
    def: &TaskDefinition,
    anchor: Option<i64>,
    stamp: i64,
    max_count: usize,
    calendars: &CalendarSet,
) -> Result<Vec<Due>, String> {
    let count = max_count.min(3);
    if count == 0 {
        return Ok(vec![]);
    }
    let window = if def.policy.misfire == "skip" {
        def.policy.grace_seconds
    } else {
        def.policy.catch_up_window_seconds
    };
    let (start, end) = def.window()?;
    let lower = stamp.saturating_sub(window).max(start.unwrap_or(i64::MIN));
    let upper = stamp.min(end.unwrap_or(i64::MAX).saturating_sub(1));
    if lower > upper {
        return Ok(vec![]);
    }
    match &def.trigger {
        Trigger::Once(r) => {
            let at = match r.mode {
                OnceMode::At => parse_instant(r.at.as_deref().ok_or("缺少一次性时刻")?)?,
                OnceMode::After => anchor.ok_or("相对任务缺少已持久锚点")?,
            };
            if (lower..=upper).contains(&at) {
                Ok(vec![due(def, at, "once".into(), None)?])
            } else {
                Ok(vec![])
            }
        }
        Trigger::Interval(r) => {
            let first = anchor.ok_or("间隔任务缺少已持久锚点")?;
            if upper < first {
                return Ok(vec![]);
            }
            let latest = (upper - first) / r.every_seconds;
            let mut values = Vec::new();
            for offset in 0..count as i64 {
                let ordinal = latest - offset;
                if ordinal < 0 {
                    break;
                }
                let at = ordinal
                    .checked_mul(r.every_seconds)
                    .and_then(|v| first.checked_add(v))
                    .ok_or("间隔时间溢出")?;
                if at < lower {
                    break;
                }
                values.push(due(def, at, format!("interval:{ordinal}"), None)?);
            }
            values.reverse();
            Ok(values)
        }
        Trigger::Calendar(r) => {
            let zone = def.time_zone()?;
            let mut date = DateTime::<Utc>::from_timestamp(upper, 0)
                .ok_or("搜索时间越界")?
                .with_timezone(&zone)
                .date_naive();
            let first_date = DateTime::<Utc>::from_timestamp(lower, 0)
                .ok_or("搜索时间越界")?
                .with_timezone(&zone)
                .date_naive();
            let mut times = r
                .at_times
                .iter()
                .map(|s| parse_time(s))
                .collect::<Result<Vec<_>, _>>()?;
            times.sort_by(|a, b| b.cmp(a));
            let mut values = Vec::new();
            // UTC window <=24h can touch only a bounded number of local days,
            // including timezone civil-day jumps. Exact UTC bounds remain final.
            for _ in 0..4 {
                if date < first_date {
                    break;
                }
                if calendar_date_matches(r, date) {
                    let candidates = times
                        .iter()
                        .filter_map(
                            |time| match zone.from_local_datetime(&date.and_time(*time)) {
                                LocalResult::None => None,
                                LocalResult::Single(at) => Some(at),
                                LocalResult::Ambiguous(a, b) => {
                                    Some(if a.timestamp() < b.timestamp() { a } else { b })
                                }
                            },
                        )
                        .filter(|at| (lower..=upper).contains(&at.timestamp()))
                        .collect::<Vec<_>>();
                    if !candidates.is_empty() {
                        let (matches, revision) = day_matches(r.day_class, date, calendars)?;
                        if matches {
                            for at in candidates {
                                values.push(due(
                                    def,
                                    at.timestamp(),
                                    format!(
                                        "calendar:{}:{}",
                                        date.format("%Y-%m-%d"),
                                        at.format("%H:%M:%S")
                                    ),
                                    revision,
                                )?);
                                if values.len() >= count {
                                    values.sort_by_key(|d| d.at);
                                    return Ok(values);
                                }
                            }
                        }
                    }
                }
                date = date.pred_opt().ok_or("日期搜索越界")?;
            }
            values.sort_by_key(|d| d.at);
            Ok(values)
        }
        Trigger::Event(_) => Ok(vec![]),
    }
}

pub fn preview(
    def: &TaskDefinition,
    anchor: Option<i64>,
    after: i64,
    count: usize,
    calendars: &CalendarSet,
) -> Result<Preview, String> {
    validate(def)?;
    let estimated_anchor = anchor.is_none()
        && matches!(
            def.trigger,
            Trigger::Interval(_)
                | Trigger::Once(OnceRule {
                    mode: OnceMode::After,
                    ..
                })
        );
    let actual_anchor = if estimated_anchor {
        activation_anchor(def, after)?
    } else {
        anchor
    };
    let year = DateTime::<Utc>::from_timestamp(after, 0)
        .ok_or("预演时间越界")?
        .with_timezone(&def.time_zone()?)
        .year();
    let mut result = Preview {
        items: vec![],
        blocked_reason: None,
        coverage_end: if def.needs_calendar() {
            calendars.coverage_end(year)
        } else {
            None
        },
        tzdb_version: TZDB_VERSION.into(),
        estimated_anchor,
    };
    let mut cursor = after;
    for _ in 0..count.min(10) {
        match next_due(def, actual_anchor, cursor, calendars) {
            Ok(Some(item)) => {
                cursor = item.at;
                result.items.push(item)
            }
            Ok(None) => break,
            Err(error)
                if error.starts_with("calendar_unknown")
                    || error.starts_with("calendar_search_limit") =>
            {
                result.blocked_reason = Some(error);
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(result)
}

/// Bounded JSON pre-parser: rejects duplicate keys before conversion to Value.
struct Seed<'a> {
    depth: usize,
    nodes: &'a Cell<usize>,
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        let n = self.nodes.get() + 1;
        self.nodes.set(n);
        if self.depth > 16 || n > 2048 {
            return Err(D::Error::custom("JSON深度或节点超限"));
        }
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_bool<E: Error>(self, v: bool) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_i64<E: Error>(self, v: i64) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_u64<E: Error>(self, v: u64) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_f64<E: Error>(self, v: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("非有限数值"))
    }
    fn visit_str<E: Error>(self, v: &str) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_string<E: Error>(self, v: String) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_unit<E: Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut values = vec![];
        while let Some(value) = access.next_element_seed(Seed {
            depth: self.depth + 1,
            nodes: self.nodes,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(A::Error::custom("重复JSON键"));
            }
            values.insert(
                key,
                access.next_value_seed(Seed {
                    depth: self.depth + 1,
                    nodes: self.nodes,
                })?,
            );
        }
        Ok(Value::Object(values))
    }
}
pub fn strict_value(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > PROPOSAL_LIMIT {
        return Err("任务草稿超过64KiB".into());
    }
    let nodes = Cell::new(0);
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    let value = Seed {
        depth: 0,
        nodes: &nodes,
    }
    .deserialize(&mut parser)
    .map_err(|_| "JSON重复键、深度、节点或格式无效")?;
    parser.end().map_err(|_| "JSON尾部有多余内容")?;
    Ok(value)
}
pub fn parse_proposal(bytes: &[u8]) -> Result<TaskDefinition, String> {
    let value = strict_value(bytes)?;
    let proposal: Proposal =
        serde_json::from_value(value).map_err(|_| "须为task_trigger_draft包裹的严格任务草稿")?;
    validate(&proposal.task_trigger_draft)?;
    Ok(proposal.task_trigger_draft)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn definition(name: &str) -> TaskDefinition {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/examples/task-triggers")
            .join(format!("{name}.json"));
        parse_proposal(&std::fs::read(path).unwrap()).unwrap()
    }
    fn set() -> CalendarSet {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT);")
            .unwrap();
        trigger_calendar::initialize(&db).unwrap();
        trigger_calendar::load(&db).unwrap()
    }
    fn at(value: &str) -> i64 {
        parse_instant(value).unwrap()
    }
    fn mutate(
        def: &TaskDefinition,
        change: impl FnOnce(&mut Value),
    ) -> Result<TaskDefinition, String> {
        let mut value = serde_json::to_value(def).unwrap();
        change(&mut value);
        validate_definition(&value, 0)
    }

    #[test]
    fn design_examples_are_strictly_parsed_and_unknown_duplicate_or_deep_json_rejected() {
        for name in [
            "once-at",
            "once-after",
            "interval",
            "workday",
            "restday",
            "weekdays",
            "month-end",
            "leap-day",
            "date-list",
            "event",
            "assistant-workday",
        ] {
            validate(&definition(name)).unwrap();
        }
        assert!(strict_value(br#"{"a":1,"a":2}"#).is_err());
        assert!(strict_value(br#"{"a":{"b":1,"b":2}}"#).is_err());
        assert!(strict_value("[".repeat(18).as_bytes()).is_err());
        let nodes = format!("[{}]", (0..2050).map(|_| "1").collect::<Vec<_>>().join(","));
        assert!(strict_value(nodes.as_bytes()).is_err());
        assert!(strict_value(&vec![b' '; PROPOSAL_LIMIT + 1]).is_err());
        assert!(mutate(&definition("once-at"), |d| d["owner_id"] = json!("forged")).is_err());
        assert!(mutate(&definition("once-at"), |d| d["enabled"] = json!(true)).is_err());
        assert!(mutate(&definition("once-at"), |d| d["title"] = json!("   ")).is_err());
        assert!(mutate(&definition("once-at"), |d| d["title"] =
            json!("汉".repeat(43)))
        .is_err());
        assert!(mutate(&definition("assistant-workday"), |d| {
            d["action"].as_object_mut().unwrap().remove("skill");
        })
        .is_err());
    }

    #[test]
    fn typed_rules_enforce_selectors_budget_offsets_and_model_frequency() {
        assert!(mutate(&definition("once-at"), |d| d["trigger"]["at"] =
            json!("2026-11-01T09:00:00Z"))
        .is_err());
        assert!(mutate(&definition("once-at"), |d| d["trigger"]["at"] =
            json!("2026-02-30T09:00:00+08:00"))
        .is_err());
        assert!(parse_instant("2026-11-01T09:00:60Z").is_err());
        assert!(parse_instant("2026-11-01T09:00:汉Z").is_err());
        assert!(
            mutate(&definition("month-end"), |d| d["trigger"]["weekdays"] =
                json!([1]))
            .is_err()
        );
        assert!(
            mutate(&definition("leap-day"), |d| d["trigger"]["month_dates"] =
                json!([{"month":2,"day":30}]))
            .is_err()
        );
        assert!(mutate(&definition("workday"), |d| d["time_zone"] =
            json!("America/New_York"))
        .is_err());
        assert!(
            mutate(&definition("workday"), |d| d["trigger"]["calendar_id"] =
                Value::Null)
            .is_err()
        );
        assert!(
            mutate(&definition("interval"), |d| d["trigger"]["every_seconds"] =
                json!(0))
            .is_err()
        );
        assert!(mutate(&definition("assistant-workday"), |d|d["trigger"]=json!({"type":"interval","every_seconds":59,"anchor":"first_enable","first_fire":"after_interval"})).is_err());
        assert!(mutate(&definition("once-at"), |d| {
            d["policy"]["misfire"] = json!("coalesce_one");
            d["policy"]["catch_up_window_seconds"] = json!(60)
        })
        .is_err());
        assert!(mutate(&definition("assistant-workday"), |d| d["action"]
            ["output_mode"] =
            json!("native_image"))
        .is_err());
        assert!(mutate(&definition("once-at"), |d| d["budget"]
            ["daily_model_runs"] =
            json!(1))
        .is_err());
    }

    #[test]
    fn relative_once_is_persistent_and_interval_jumps_to_original_grid_in_constant_work() {
        let def = definition("once-after");
        let now = at("2026-10-25T10:00:00+08:00");
        let anchor = activation_anchor(&def, now).unwrap();
        assert_eq!(anchor, Some(now + 30));
        assert_eq!(
            next_due(&def, anchor, now + 10, &set())
                .unwrap()
                .unwrap()
                .at,
            now + 30
        );
        assert!(next_due(&def, anchor, now + 30, &set()).unwrap().is_none());
        let def = definition("interval");
        let first = now + 30;
        let later = next_due(&def, Some(first), now + 86400, &set())
            .unwrap()
            .unwrap();
        assert_eq!(later.at, now + 86430);
        assert_eq!(later.key, "interval:2880");
        let window = mutate(&def, |d| {
            d["active_window"]["start_at"] = json!("2026-10-25T10:01:45+08:00");
            d["active_window"]["end_at"] = json!("2026-10-25T10:02:30+08:00");
        })
        .unwrap();
        assert_eq!(
            next_due(&window, Some(first), now, &set())
                .unwrap()
                .unwrap()
                .at,
            now + 120
        );
        assert!(next_due(&window, Some(first), now + 120, &set())
            .unwrap()
            .is_none());
        let once_window = mutate(&definition("once-after"), |d| {
            d["active_window"]["end_at"] = json!("2026-10-25T10:00:20+08:00")
        })
        .unwrap();
        assert!(activation_anchor(&once_window, now).is_err());
    }

    #[test]
    fn calendar_supports_month_end_missing_days_leap_dates_and_deduplicated_last() {
        let empty = CalendarSet::default();
        let last = definition("month-end");
        assert!(
            next_due(&last, None, at("2027-02-01T00:00:00+08:00"), &empty)
                .unwrap()
                .unwrap()
                .local
                .starts_with("2027-02-28T20:00:00")
        );
        let day31 = mutate(&last, |d| d["trigger"]["month_days"] = json!([31])).unwrap();
        assert!(
            next_due(&day31, None, at("2027-02-01T00:00:00+08:00"), &empty)
                .unwrap()
                .unwrap()
                .local
                .starts_with("2027-03-31T20:00:00")
        );
        let both = mutate(&last, |d| d["trigger"]["month_days"] = json!([31, "last"])).unwrap();
        let items = preview(&both, None, at("2027-03-01T00:00:00+08:00"), 2, &empty)
            .unwrap()
            .items;
        assert!(items[0].local.starts_with("2027-03-31"));
        assert!(items[1].local.starts_with("2027-04-30"));
        let leap = definition("leap-day");
        assert!(
            next_due(&leap, None, at("2026-03-01T00:00:00+08:00"), &empty)
                .unwrap()
                .unwrap()
                .local
                .starts_with("2028-02-29")
        );
        let far = mutate(&definition("date-list"), |d| {
            d["trigger"]["dates"] = json!(["2099-11-01"])
        })
        .unwrap();
        assert!(
            next_due(&far, None, at("2026-11-01T00:00:00+08:00"), &empty)
                .unwrap()
                .unwrap()
                .local
                .starts_with("2099-11-01")
        );
    }

    #[test]
    fn authority_unknown_is_reported_with_partial_preview_and_never_weekend_fallback() {
        let calendar = set();
        let work = definition("workday");
        assert!(
            next_due(&work, None, at("2026-10-09T23:00:00+08:00"), &calendar)
                .unwrap()
                .unwrap()
                .local
                .starts_with("2026-10-10T09:00:00")
        );
        let rest = definition("restday");
        assert!(
            next_due(&rest, None, at("2026-09-30T23:00:00+08:00"), &calendar)
                .unwrap()
                .unwrap()
                .local
                .starts_with("2026-10-01T10:00:00")
        );
        let preview = preview(&work, None, at("2026-12-30T23:00:00+08:00"), 10, &calendar).unwrap();
        assert_eq!(preview.items.len(), 1);
        assert!(preview
            .blocked_reason
            .unwrap()
            .starts_with("calendar_unknown:2027"));
        assert_eq!(preview.coverage_end.as_deref(), Some("2026-12-31"));
        let mut weekdays = definition("weekdays");
        weekdays.trigger = Trigger::Calendar(CalendarRule {
            frequency: "weekly".into(),
            at_times: vec!["09:00:00".into()],
            day_class: DayClass::Any,
            calendar_id: None,
            weekdays: Some(vec![6, 7]),
            month_days: None,
            month_dates: None,
            dates: None,
        });
        assert!(next_due(
            &weekdays,
            None,
            at("2026-10-09T23:00:00+08:00"),
            &CalendarSet::default()
        )
        .unwrap()
        .unwrap()
        .local
        .starts_with("2026-10-10"));
    }

    #[test]
    fn daylight_saving_gap_skips_and_fold_only_uses_first_utc_instant() {
        let def=mutate(&definition("month-end"),|d|{d["time_zone"]=json!("America/New_York");d["trigger"]=json!({"type":"calendar","frequency":"daily","at_times":["02:30:00"],"day_class":"any","calendar_id":null});}).unwrap();
        let next = next_due(
            &def,
            None,
            at("2027-03-14T00:00:00-05:00"),
            &CalendarSet::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(next.local, "2027-03-15T02:30:00-04:00");
        let def = mutate(&def, |d| d["trigger"]["at_times"] = json!(["01:30:00"])).unwrap();
        let first = next_due(
            &def,
            None,
            at("2026-11-01T00:00:00-04:00"),
            &CalendarSet::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(first.at, at("2026-11-01T01:30:00-04:00"));
        let next = next_due(&def, None, first.at + 1, &CalendarSet::default())
            .unwrap()
            .unwrap();
        assert!(next.local.starts_with("2026-11-02T01:30:00"));
    }

    #[test]
    fn event_catalog_is_typed_missing_fields_never_match_and_calendar_uses_receipt_date() {
        let event = definition("event");
        assert!(event_matches(
            &event,
            "dsh.runs",
            "dsh.run.failed.v1",
            &json!({"run_kind":"model","status":"failed"}),
            at("2026-10-25T00:00:00+08:00"),
            &set()
        )
        .unwrap());
        assert!(!event_matches(
            &event,
            "dsh.runs",
            "dsh.run.failed.v1",
            &json!({"status":"failed"}),
            at("2026-10-25T00:00:00+08:00"),
            &set()
        )
        .unwrap());
        assert!(mutate(&event, |d| d["trigger"]["filters"] =
            json!([{"field":"data.channel","operator":"eq","value":"qq"}]))
        .is_err());
        assert!(mutate(&event, |d| d["trigger"]["filters"] =
            json!([{"field":"data.status","operator":"eq","value":4}]))
        .is_err());
        let work = mutate(&event, |d| {
            d["trigger"]["day_class"] = json!("workday");
            d["trigger"]["calendar_id"] = json!("cn-official");
        })
        .unwrap();
        assert!(event_matches(
            &work,
            "dsh.runs",
            "dsh.run.failed.v1",
            &json!({"run_kind":"model"}),
            at("2027-01-04T00:00:00+08:00"),
            &set()
        )
        .is_err());
        let missing = Predicate::Neq {
            field: "data.status".into(),
            value: json!("completed"),
        };
        assert!(!missing.matches(&json!({})));
    }

    #[test]
    fn recent_nominals_bound_seconds_backfill_to_approved_window_and_choose_latest_calendar() {
        let now = at("2026-10-25T10:00:00+08:00");
        let interval = mutate(&definition("interval"), |d| {
            d["trigger"]["every_seconds"] = json!(1);
            d["policy"]["misfire"] = json!("bounded_catch_up");
            d["policy"]["catch_up_window_seconds"] = json!(3600);
            d["policy"]["max_catch_up"] = json!(3);
        })
        .unwrap();
        let recent = recent_due(
            &interval,
            Some(now - 86400),
            now,
            3,
            &CalendarSet::default(),
        )
        .unwrap();
        assert_eq!(
            recent.iter().map(|d| d.at).collect::<Vec<_>>(),
            vec![now - 2, now - 1, now]
        );
        assert_eq!(recent[2].key, "interval:86400");
        let expired = definition("once-after");
        assert!(
            recent_due(&expired, Some(now - 301), now, 1, &CalendarSet::default())
                .unwrap()
                .is_empty()
        );
        let daily=mutate(&definition("month-end"),|d|{d["trigger"]=json!({"type":"calendar","frequency":"daily","at_times":["09:00:00","09:30:00"],"day_class":"any","calendar_id":null});d["policy"]["misfire"]=json!("coalesce_one");d["policy"]["catch_up_window_seconds"]=json!(3600);}).unwrap();
        let values = recent_due(&daily, None, now, 1, &CalendarSet::default()).unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].at, now - 1800);
    }
}
