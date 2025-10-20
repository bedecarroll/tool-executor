use std::time::SystemTime;

pub fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub fn format_timestamp(ts: Option<i64>) -> String {
    match ts {
        Some(value) => {
            let datetime = time::OffsetDateTime::from_unix_timestamp(value)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
            datetime
                .format(&time::macros::format_description!("%Y-%m-%d %H:%M:%S"))
                .unwrap()
        }
        None => "-".to_string(),
    }
}
