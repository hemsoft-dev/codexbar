//! Rolling local Grok CLI turn metadata. No prompts, token counts, auth files or inferred quota percentages.
use chrono::{DateTime, Duration, Utc};
use codexbar_core::Metric;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read as _,
    path::Path,
};
const MAX_FILES: usize = 10_000;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    session_id: String,
    turns: Vec<Turn>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Turn {
    turn_number: u64,
    ended_at: DateTime<Utc>,
}
#[derive(Default)]
pub struct Activity {
    pub metric: Option<Metric>,
    pub messages: Vec<String>,
}
pub fn read(home: &Path, now: DateTime<Utc>) -> Activity {
    let scope = "Tracked activity covers this local Grok CLI home only; web, T3 and other machines are outside its coverage. No quota percentage or SUS is inferred from turns.";
    let Ok(roots) = fs::read_dir(home.join("sessions")) else {
        return Activity {
            metric: None,
            messages: vec!["Local Grok CLI activity is unavailable; SUS n/a.".into()],
        };
    };
    let mut events: HashMap<String, HashSet<u64>> = HashMap::new();
    let mut partial = false;
    let mut files = 0usize;
    let mut valid = 0usize;
    let mut bytes = 0u64;
    for (root_index, root) in roots.take(MAX_FILES + 1).enumerate() {
        if root_index == MAX_FILES || bytes >= MAX_TOTAL_BYTES {
            partial = true;
            break;
        }
        let Ok(root) = root else {
            partial = true;
            continue;
        };
        if !root.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(children) = fs::read_dir(root.path()) else {
            partial = true;
            continue;
        };
        for child in children.take(MAX_FILES + 1) {
            files += 1;
            if files > MAX_FILES || bytes >= MAX_TOTAL_BYTES {
                partial = true;
                break;
            }
            let Ok(child) = child else {
                partial = true;
                continue;
            };
            if !child.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = child.path().join("usage.json");
            if !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
                continue;
            }
            let result = (|| {
                let file = fs::File::open(path).ok()?;
                let mut text = Vec::new();
                let limit = MAX_BYTES.min(MAX_TOTAL_BYTES - bytes);
                let result = file.take(limit + 1).read_to_end(&mut text);
                bytes += text.len() as u64;
                if result.is_err() || text.len() as u64 > MAX_BYTES || bytes > MAX_TOTAL_BYTES {
                    return None;
                }
                serde_json::from_slice::<Usage>(&text).ok()
            })();
            let Some(usage) = result else {
                partial = true;
                continue;
            };
            if usage.session_id.trim().is_empty() {
                partial = true;
                continue;
            }
            valid += 1;
            let turns = events.entry(usage.session_id).or_default();
            for turn in usage.turns {
                if turn.ended_at >= now - Duration::days(7) && turn.ended_at <= now {
                    turns.insert(turn.turn_number);
                }
            }
        }
        if files > MAX_FILES {
            break;
        }
    }
    let mut messages = vec![scope.into()];
    if partial {
        messages.push("Some local activity files could not be read; the tracked count is incomplete.".into());
    }
    if valid == 0 {
        messages.push("No readable local activity source was found; activity is unavailable, SUS n/a.".into());
    }
    let metric = (valid > 0).then(|| Metric::Activity {
        label: "Turns/7d".into(),
        count: events.values().map(|turns| turns.len() as u64).sum(),
    });
    Activity { metric, messages }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_empty_layout_is_unavailable_but_a_parsed_zero_is_measured() {
        let dir = std::env::temp_dir().join(format!("codexbar-grok-empty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sessions/day/no-usage-file")).unwrap();
        assert!(read(&dir, Utc::now()).metric.is_none());
        assert!(
            read(&dir, Utc::now())
                .messages
                .iter()
                .any(|m| m.contains("unavailable"))
        );
        fs::write(
            dir.join("sessions/day/no-usage-file/usage.json"),
            r#"{"sessionId":"s","turns":[]}"#,
        )
        .unwrap();
        assert_eq!(
            read(&dir, Utc::now()).metric,
            Some(Metric::Activity {
                label: "Turns/7d".into(),
                count: 0
            })
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn local_metadata_deduplicates_and_keeps_exact_seven_day_bounds() {
        let dir = std::env::temp_dir().join(format!("codexbar-grok-activity-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let now: DateTime<Utc> = "2026-10-23T14:00:00Z".parse().unwrap();
        for leaf in ["a", "duplicate"] {
            let p = dir.join("sessions/day").join(leaf);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("usage.json"),r#"{"sessionId":"s","turns":[{"turnNumber":1,"endedAt":"2026-10-16T14:00:00Z"},{"turnNumber":2,"endedAt":"2026-10-16T13:59:59Z"},{"turnNumber":3,"endedAt":"2026-10-23T14:00:01Z"}]}"#).unwrap();
        }
        assert_eq!(
            read(&dir, now).metric,
            Some(Metric::Activity {
                label: "Turns/7d".into(),
                count: 1
            })
        );
        assert!(read(&dir.join("missing"), now).metric.is_none());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn unreadable_metadata_does_not_invent_zero_activity() {
        let dir = std::env::temp_dir().join(format!("codexbar-grok-bad-{}", std::process::id()));
        let p = dir.join("sessions/day/bad");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("usage.json"), "broken").unwrap();
        assert!(read(&dir, Utc::now()).metric.is_none());
        fs::remove_dir_all(dir).unwrap();
    }
}
