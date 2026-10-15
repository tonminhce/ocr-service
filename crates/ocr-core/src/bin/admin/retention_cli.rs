//! Bounded operational CLI. No document contents or caller-supplied actor output.
use std::io::Write;

use chrono::{DateTime, Utc};
use ocr_core::{
    retention::{
        MAX_PLAN_ROWS, RetentionAction, RetentionCursor, RetentionError, RetentionHoldRequest,
        RetentionPlanRequest,
    },
    secrets,
    store::Store,
};
use serde::Deserialize;
use uuid::Uuid;

pub const HELP: &str = "ocr-admin retention plan [--limit 1..100] [--after-created-at RFC3339 --after-id UUID]\nocr-admin retention hold|release < request.json\nocr-admin retention apply\n\nRetention commands check exact schema; hold/release use the maintenance login in\nthe restricted deployment. JSON stdin fields: job_id, request_id, hold_id,\nexpected_revision, reason. Action comes from the command; actor fields are rejected.\nInput is bounded at 64KiB with a 10-second total read deadline. Keep reasons out\nof argv/logs; redirect protected request files through stdin. Replay returns the\nrecorded mutation snapshot. Plan counts describe one page/snapshot, not a DB total.\napply always fails before credential resolution or connection: explicit policy\nand backup/restore gates remain open. No hidden enable flag or deletion path.";

const MAX_REQUEST_BYTES: usize = 64 * 1024;

enum Command {
    Plan(RetentionPlanRequest),
    Change(RetentionAction),
    Apply,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HoldInput {
    job_id: Uuid,
    request_id: Uuid,
    expected_revision: i64,
    hold_id: Uuid,
    reason: String,
}

fn parse_args(args: &[String]) -> Result<Command, &'static str> {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    match args.as_slice() {
        ["hold"] => Ok(Command::Change(RetentionAction::Hold)),
        ["release"] => Ok(Command::Change(RetentionAction::Release)),
        ["apply"] => Ok(Command::Apply),
        ["plan", rest @ ..] => {
            let (mut limit, mut created_at, mut job_id) = (None, None, None);
            for pair in rest.chunks(2) {
                match pair {
                    ["--limit", value] if limit.is_none() => {
                        let parsed: usize = value.parse().map_err(|_| "invalid plan limit")?;
                        if !(1..=MAX_PLAN_ROWS).contains(&parsed) {
                            return Err("plan limit must be within 1..=100");
                        }
                        limit = Some(parsed);
                    }
                    ["--after-created-at", value] if created_at.is_none() => {
                        created_at = Some(
                            DateTime::parse_from_rfc3339(value)
                                .map_err(|_| "invalid plan cursor timestamp")?
                                .with_timezone(&Utc),
                        );
                    }
                    ["--after-id", value] if job_id.is_none() => {
                        let parsed =
                            Uuid::parse_str(value).map_err(|_| "invalid plan cursor ID")?;
                        if parsed.is_nil() {
                            return Err("plan cursor ID must be non-nil");
                        }
                        job_id = Some(parsed);
                    }
                    _ => return Err("unsupported, duplicate or incomplete plan argument"),
                }
            }
            let cursor = match (created_at, job_id) {
                (Some(created_at), Some(job_id)) => Some(RetentionCursor { created_at, job_id }),
                (None, None) => None,
                _ => return Err("plan cursor requires both --after-created-at and --after-id"),
            };
            Ok(Command::Plan(RetentionPlanRequest {
                limit: limit.unwrap_or(50),
                cursor,
            }))
        }
        _ => Err("unsupported retention command; use --help"),
    }
}

fn decode_input(bytes: &[u8]) -> Result<HoldInput, &'static str> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("retention request exceeds 65536 bytes");
    }
    // Deserialization diagnostics can contain input. Expose fixed text only.
    serde_json::from_slice(bytes).map_err(|_| "invalid retention request JSON (details redacted)")
}

#[cfg(unix)]
fn read_bounded_fd(
    fd: std::os::fd::RawFd,
    budget: std::time::Duration,
) -> Result<Vec<u8>, &'static str> {
    use std::time::Instant;
    let deadline = Instant::now() + budget;
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("retention request stdin deadline elapsed");
        }
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
        // The CLI owns its only stdin reader. No uncancellable input thread or
        // mutation of the inherited open-file-description flags is needed.
        let ready = unsafe { libc::poll(&raw mut descriptor, 1, millis) };
        if ready == 0 {
            return Err("retention request stdin deadline elapsed");
        }
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err("retention request stdin could not be read");
        }
        let mut chunk = [0_u8; 4096];
        let count = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count < 0 {
            if matches!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err("retention request stdin could not be read");
        }
        if count == 0 {
            return Ok(bytes);
        }
        let count =
            usize::try_from(count).map_err(|_| "retention request stdin could not be read")?;
        if bytes.len() + count > MAX_REQUEST_BYTES {
            return Err("retention request exceeds 65536 bytes");
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

#[cfg(unix)]
fn read_input() -> Result<HoldInput, &'static str> {
    use std::os::fd::AsRawFd;
    decode_input(&read_bounded_fd(
        std::io::stdin().as_raw_fd(),
        std::time::Duration::from_secs(10),
    )?)
}

#[cfg(not(unix))]
fn read_input() -> Result<HoldInput, &'static str> {
    Err("bounded retention stdin is supported on Unix only")
}

fn print_json(value: &impl serde::Serialize) -> Result<(), &'static str> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, value)
        .map_err(|_| "operation output could not be written")?;
    output
        .write_all(b"\n")
        .map_err(|_| "operation output could not be written")
}

pub async fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let command = parse_args(args)?;
    if matches!(command, Command::Apply) {
        return Err(RetentionError::ApplyDisabled.into());
    }
    // Operator reasons never enter argv, fixed diagnostics or normal stdout.
    let change = if let Command::Change(action) = command {
        let input = read_input()?;
        let request = RetentionHoldRequest {
            job_id: input.job_id,
            request_id: input.request_id,
            expected_revision: input.expected_revision,
            hold_id: input.hold_id,
            action,
            reason: input.reason,
        };
        request.validate()?;
        Some(request)
    } else {
        None
    };
    let store = Store::connect(&secrets::required("DATABASE_URL")?).await?;
    store.check_schema().await?;
    if let Command::Plan(request) = command {
        print_json(&store.retention_plan(request).await?)?;
    } else if let Some(request) = change {
        print_json(&store.change_retention_hold(request).await?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caller_actor_action_unknown_fields_and_malformed_input_are_redacted() {
        for field in ["actor", "principal", "action", "unknown"] {
            let request = serde_json::json!({"job_id":Uuid::nil(),"request_id":Uuid::nil(),
                "hold_id":Uuid::nil(),"expected_revision":0,"reason":"CLI_REASON_SENTINEL",(field):"CLI_VALUE_SENTINEL"});
            let error = decode_input(&serde_json::to_vec(&request).unwrap())
                .err()
                .unwrap();
            assert!(!error.contains("SENTINEL"));
        }
        assert!(
            !decode_input(b"CLI_BAD_JSON_SENTINEL")
                .err()
                .unwrap()
                .contains("SENTINEL")
        );
        assert!(decode_input(&vec![b' '; MAX_REQUEST_BYTES + 1]).is_err());
    }
    #[test]
    fn cursor_requires_both_keys_and_rejects_unbounded_or_duplicate_options() {
        for args in [
            vec!["--limit", "0"],
            vec!["--limit", "101"],
            vec!["--limit", "50", "--limit", "50"],
            vec!["--after-id", "00000000-0000-0000-0000-000000000000"],
            vec!["--after-created-at", "2026-10-15T00:00:00Z"],
            vec!["--unexpected", "CLI_ARGUMENT_SENTINEL"],
        ] {
            let args = [vec!["plan"], args]
                .concat()
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>();
            let error = parse_args(&args).err().unwrap();
            assert!(!error.contains("SENTINEL"));
        }
    }
    #[cfg(unix)]
    #[test]
    fn stdin_bounds_idle_wait_and_bytes_without_an_orphan_input_thread() {
        use std::{
            os::{fd::AsRawFd, unix::net::UnixStream},
            time::{Duration, Instant},
        };
        let (reader, _held_writer) = UnixStream::pair().unwrap();
        let started = Instant::now();
        assert_eq!(
            read_bounded_fd(reader.as_raw_fd(), Duration::from_millis(25)).unwrap_err(),
            "retention request stdin deadline elapsed"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let writer_task =
            std::thread::spawn(move || writer.write_all(&vec![b'x'; MAX_REQUEST_BYTES + 1]));
        assert_eq!(
            read_bounded_fd(reader.as_raw_fd(), Duration::from_secs(1)).unwrap_err(),
            "retention request exceeds 65536 bytes"
        );
        drop(reader);
        writer_task.join().unwrap().unwrap();
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"synthetic").unwrap();
        drop(writer);
        assert_eq!(
            read_bounded_fd(reader.as_raw_fd(), Duration::from_secs(1)).unwrap(),
            b"synthetic"
        );
    }
}
