//! `pgpu audit [--since 1d]`: what phones (and the command line) changed on this machine.

use anyhow::{Result, bail};
use pocketgpu_proto::local::{AuditEntry, Request, Response};

use crate::ipc::client;
use crate::store::db::now_ms;
use crate::store::paths::Paths;

/// `30m`, `6h`, `1d` or `2w` as milliseconds.
pub fn parse_since(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num
        .parse()
        .map_err(|_| format!("{s:?} is not a time like 30m, 6h, 1d or 2w"))?;
    let unit_ms = match unit {
        "m" => 60_000,
        "h" => 3_600_000,
        "d" | "" => 86_400_000,
        "w" => 7 * 86_400_000,
        _ => return Err(format!("{s:?} is not a time like 30m, 6h, 1d or 2w")),
    };
    Ok(n * unit_ms)
}

pub async fn run(paths: &Paths, json: bool, since_ms: i64) -> Result<()> {
    let now = now_ms();
    let req = Request::Audit {
        since_ms: now - since_ms,
    };
    let entries = match client::call(paths, req).await? {
        Response::Audit(e) => e,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        let out: Vec<_> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "at_ms": e.at_ms, "device": e.device, "event": e.event,
                    "target": e.target, "result": e.result,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", render(&entries, now));
    }
    Ok(())
}

pub fn render(entries: &[AuditEntry], now_ms: i64) -> String {
    if entries.is_empty() {
        return "Nothing in the audit log for this period.\n".into();
    }
    let mut out = format!(
        "{:>8}  {:<16} {:<17} {:<24} {}\n",
        "WHEN", "DEVICE", "EVENT", "TARGET", "RESULT"
    );
    for e in entries {
        // Cleaned again: entries written by older agents weren't.
        let device: String = crate::audit::clean(&e.device).chars().take(16).collect();
        let target: String = crate::audit::clean(&e.target).chars().take(24).collect();
        let result = crate::audit::clean(&e.result);
        out += &format!(
            "{:>8}  {:<16} {:<17} {:<24} {}\n",
            ago(now_ms - e.at_ms),
            device,
            e.event,
            target,
            result
        );
    }
    out
}

fn ago(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86_400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_periods() {
        assert_eq!(parse_since("30m"), Ok(30 * 60_000));
        assert_eq!(parse_since("1d"), Ok(86_400_000));
        assert_eq!(parse_since("2"), Ok(2 * 86_400_000));
        assert!(parse_since("soon").is_err());
        assert!(parse_since("3y").is_err());
    }
}
