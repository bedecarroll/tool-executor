use std::time::SystemTime;

pub fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub fn format_timestamp(ts: Option<i64>) -> String {
    match ts {
        Some(value) => {
            let datetime = time::OffsetDateTime::from_unix_timestamp(value as i64)
                .unwrap_or_else(|_| time::OffsetDateTime::UNIX_EPOCH);
            datetime
                .format(&time::macros::format_description!("%Y-%m-%d %H:%M:%S"))
                .unwrap()
        }
        None => "-".to_string(),
    }
}
