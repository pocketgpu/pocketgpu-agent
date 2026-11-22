//! The computer's own day, for a share's daily count: it starts again at local midnight.

use jiff::Timestamp;
use jiff::tz::TimeZone;

/// The local date of `now_ms`, as `YYYY-MM-DD`.
pub fn local_day(now_ms: i64) -> String {
    zoned(now_ms)
        .map(|z| z.date().to_string())
        .unwrap_or_else(|| (now_ms / 86_400_000).to_string())
}

/// When the next local day starts, in Unix ms.
pub fn next_local_midnight_ms(now_ms: i64) -> i64 {
    zoned(now_ms)
        .and_then(|z| {
            let tz = z.time_zone().clone();
            z.date().tomorrow().ok()?.to_zoned(tz).ok()
        })
        .map(|z| z.timestamp().as_millisecond())
        .unwrap_or_else(|| (now_ms / 86_400_000 + 1) * 86_400_000)
}

fn zoned(now_ms: i64) -> Option<jiff::Zoned> {
    Some(
        Timestamp::from_millisecond(now_ms)
            .ok()?
            .to_zoned(TimeZone::system()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_day_starts_after_now_and_within_a_day() {
        let now = 1_795_000_000_000;
        let next = next_local_midnight_ms(now);
        assert!(next > now && next - now <= 25 * 3_600_000, "{}", next - now);
        assert_ne!(local_day(now), local_day(next));
        assert_eq!(local_day(next), local_day(next + 1000));
        assert_eq!(local_day(now).len(), 10);
    }
}
