//! `OpenCode`: `~/.local/share/opencode/opencode.db`, v1 and v2 alike.
//!
//! The only source with a real index. One indexed query yields every list
//! column — title, directory, model, tokens, cost, archived state — for all
//! ~1900 sessions in tens of milliseconds, so tier 1 never touches a body.
//!
//! `session.directory` and `project.worktree` store the path verbatim, so
//! there is nothing to decode.
//!
//! The database is 4.9 GB with a live WAL and is written while we browse; the
//! session count moved during a single research pass. Reading goes through
//! `storage::sqlite::open`, which is read-only with an `immutable=1` fallback.
//!
//! `OpenCode` v2 (2.0.x) keeps the same file and binary name but writes new
//! `session_v2`/`session_message` tables beside the v1 ones; one source reads
//! both, so a migrated session is listed once rather than per generation.
//! Kilo ships the v1 Drizzle schema, so the decoding lives in the shared
//! `opencode_schema` module used by both sources.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::model::{ArchiveOutcome, Locator, SessionSummary, SourceId, Transcript};
use crate::source::{RemoteLayout, Source, opencode_schema, row_locator};

#[derive(Debug)]
pub struct OpenCode {
    db: PathBuf,
}

impl OpenCode {
    pub fn new(home: &Path) -> Self {
        Self {
            db: home.join(".local/share/opencode/opencode.db"),
        }
    }
}

impl Source for OpenCode {
    fn id(&self) -> SourceId {
        SourceId::OpenCode
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![self.db.clone()]
    }

    fn list(&self) -> Result<Vec<SessionSummary>> {
        opencode_schema::list(&self.db, SourceId::OpenCode)
    }

    fn open(&self, locator: &Locator) -> Result<Transcript> {
        let (db, id) = row_locator(SourceId::OpenCode, locator)?;
        opencode_schema::open(db, SourceId::OpenCode, id)
    }

    /// `OpenCode` models archiving on `session.time_archived`, so the state
    /// belongs in its own store rather than in our overlay.
    fn archive(&self, locator: &Locator, archived: bool) -> Result<ArchiveOutcome> {
        let (db, id) = row_locator(SourceId::OpenCode, locator)?;
        opencode_schema::set_archived(db, id, archived)
    }

    fn resume_command(&self, id: &str) -> Option<Vec<String>> {
        Some(vec!["opencode".into(), "--session".into(), id.into()])
    }

    /// `opencode run` sends one message and exits. It goes through the
    /// shared background service (`opencode serve --service`, started on
    /// demand), the same server a TUI uses unless it was launched with
    /// `--standalone` — so the service, not us, is the session's one writer.
    fn insert_command(&self, id: &str, message: &str) -> Option<Vec<String>> {
        Some(run_command(id, message, None))
    }

    /// A running client is reached through the server it talks to: the shared
    /// service by default, or its `--server <url>`. A `--standalone` client's
    /// private server announces no address, so it cannot be reached.
    fn live_insert_command(
        &self,
        id: &str,
        message: &str,
        owner: &[String],
    ) -> Option<Vec<String>> {
        if owner.iter().any(|arg| arg == "--standalone") {
            return None;
        }
        let server = owner.iter().enumerate().find_map(|(i, arg)| {
            arg.strip_prefix("--server=")
                .or_else(|| (arg == "--server").then(|| owner.get(i + 1).map(String::as_str))?)
        });
        Some(run_command(id, message, server))
    }

    /// One remote `sqlite3` query answers tier 1, so there is nothing to glob.
    fn remote_layout(&self) -> RemoteLayout {
        RemoteLayout {
            files: &[],
            sql: Some((
                ".local/share/opencode/opencode.db",
                opencode_schema::REMOTE_LIST_SQL,
            )),
        }
    }
}

/// `opencode run --session <id> [--server <url>] -- <message>`; `--` ends
/// its flags, so a message starting with `-` stays a message.
fn run_command(id: &str, message: &str, server: Option<&str>) -> Vec<String> {
    let mut command = vec![
        "opencode".to_owned(),
        "run".to_owned(),
        "--session".to_owned(),
        id.to_owned(),
    ];
    if let Some(server) = server {
        command.extend(["--server".to_owned(), server.to_owned()]);
    }
    command.extend(["--".to_owned(), message.to_owned()]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|&arg| arg.to_owned()).collect()
    }

    /// The message must reach the server the running client talks to; a
    /// different one would be a second writer on the same session.
    #[test]
    fn a_live_client_is_reached_through_its_own_server() {
        let opencode = OpenCode::new(Path::new("/home/u"));
        let send = |owner: &[&str]| opencode.live_insert_command("ses_1", "hi", &argv(owner));
        let shared = argv(&["opencode", "run", "--session", "ses_1", "--", "hi"]);
        assert_eq!(send(&["/usr/bin/opencode"]), Some(shared.clone()));
        assert_eq!(send(&["opencode", "-s", "ses_1"]), Some(shared));
        let pinned = argv(&[
            "opencode",
            "run",
            "--session",
            "ses_1",
            "--server",
            "http://h:1",
            "--",
            "hi",
        ]);
        assert_eq!(
            send(&["opencode", "--server", "http://h:1"]),
            Some(pinned.clone())
        );
        assert_eq!(send(&["opencode", "--server=http://h:1"]), Some(pinned));
        assert_eq!(send(&["opencode", "--standalone"]), None);
    }
}
