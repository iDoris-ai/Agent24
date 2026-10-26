//! `agent24 documents …`: the Documenting OS from the command line, through
//! the same client code the desktop runs (`agent24-documents-client`), so a
//! use case passing here passes for the desktop's import too (AGENTS.md).

use std::io::Read;
use std::path::PathBuf;

use agent24_documents_client::http::HttpTransport;
use agent24_documents_client::{Progress, Response, import_file};
use clap::Subcommand;
use serde::Deserialize;
use serde_json::json;

#[derive(Subcommand)]
pub(crate) enum DocumentsAction {
    /// Import a file (PDF, JPG or PNG) into the Documenting OS and print the
    /// import job. Needs a running daemon (`agent24 daemon start`): the job
    /// goes on in it after this returns. Only the file's base name is sent.
    Import {
        /// The file to import
        path: Option<PathBuf>,
        /// Print JSON lines on stdout: `{"type":"progress",..}` while
        /// sending, then one `{"type":"result",..}`
        #[arg(long)]
        json: bool,
        /// Read `{"base","token","path"}` as one JSON object from stdin
        /// instead of finding the daemon: how the desktop runs it, so the
        /// token and the path stay out of the process list
        #[arg(long, conflicts_with = "path")]
        host_stdin: bool,
    },
}

const NO_DAEMON: &str = "no running daemon to import into: start one with `agent24 daemon start` (a daemon the desktop started is reachable only from the desktop)";
/// Far above `{"base","token","path"}`; a bound so stdin cannot grow us.
const MAX_HOST_INPUT: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostInput {
    base: String,
    token: String,
    path: PathBuf,
}

fn host_input(reader: impl Read) -> Result<HostInput, String> {
    let mut text = String::new();
    reader
        .take(MAX_HOST_INPUT + 1)
        .read_to_string(&mut text)
        .map_err(|_| "stdin could not be read".to_owned())?;
    if text.len() as u64 > MAX_HOST_INPUT {
        return Err("stdin is too large".to_owned());
    }
    serde_json::from_str(&text)
        .map_err(|_| r#"stdin must be one JSON object {"base","token","path"}"#.to_owned())
}

pub(crate) async fn cmd_documents(action: DocumentsAction) -> Result<(), String> {
    match action {
        DocumentsAction::Import {
            path,
            json,
            host_stdin,
        } => import(path, json, host_stdin).await,
    }
}

async fn import(path: Option<PathBuf>, json: bool, host_stdin: bool) -> Result<(), String> {
    let (transport, path) = if host_stdin {
        let input = host_input(std::io::stdin().lock())?;
        (
            HttpTransport::new(&input.base, Some(input.token))?,
            input.path,
        )
    } else {
        let path = path.ok_or("a file to import is required")?;
        let ep = super::attach_only().await.ok_or(NO_DAEMON)?;
        (HttpTransport::new(&ep.base, Some(ep.token.clone()))?, path)
    };
    let mut shown = false;
    let mut on_progress = |p: Progress| {
        if json {
            println!(
                "{}",
                json!({"type": "progress", "sent": p.sent, "total": p.total})
            );
        } else {
            eprint!("\rsent {} of {} bytes", p.sent, p.total);
            shown = true;
        }
    };
    let res = import_file(&path, &transport, &mut on_progress).await;
    if shown {
        eprintln!();
    }
    report(&res, json)
}

/// The result as one JSON line, or as a line for a person; an error is an
/// `Err` either way, so the exit code is 1.
fn report(res: &Response, json: bool) -> Result<(), String> {
    if json {
        let mut line = res.to_json();
        line["type"] = json!("result");
        println!("{line}");
    }
    match res {
        Response::Ok { data, .. } => {
            if !json {
                let field = |k: &str| data.get(k).and_then(|v| v.as_str()).unwrap_or("?");
                println!("import job {} ({})", field("job_id"), field("status"));
            }
            Ok(())
        }
        Response::Err { error, .. } => Err(match &error.hint {
            Some(hint) => format!("{}: {} ({hint})", error.code, error.message),
            None => format!("{}: {}", error.code, error.message),
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn host_input_is_one_object_with_base_token_and_path() {
        let ok = host_input(&br#"{"base":"http://127.0.0.1:1","token":"t","path":"/a b.pdf"}"#[..])
            .unwrap();
        assert_eq!(
            (ok.base.as_str(), ok.token.as_str()),
            ("http://127.0.0.1:1", "t")
        );
        assert_eq!(ok.path, PathBuf::from("/a b.pdf"));
        for bad in [
            &br#"{"base":"http://127.0.0.1:1","path":"/a"}"#[..],
            &br#"{"base":"http://127.0.0.1:1","token":"t","path":"/a","extra":1}"#[..],
            &b"not json"[..],
        ] {
            assert!(host_input(bad).is_err());
        }
        let huge = format!(
            r#"{{"base":"{}","token":"t","path":"/a"}}"#,
            "x".repeat(70_000)
        );
        assert_eq!(
            host_input(huge.as_bytes()).unwrap_err(),
            "stdin is too large"
        );
    }

    #[test]
    fn an_error_names_its_code_message_and_hint_and_never_succeeds() {
        let mut err = Response::fail(409, "conflict", "busy");
        assert_eq!(report(&err, false).unwrap_err(), "conflict: busy");
        if let Response::Err { error, .. } = &mut err {
            error.hint = Some("try again".to_owned());
        }
        assert_eq!(
            report(&err, true).unwrap_err(),
            "conflict: busy (try again)"
        );
        let ok = Response::Ok {
            status: 202,
            data: json!({"job_id": "job_1", "status": "queued"}),
        };
        assert!(report(&ok, true).is_ok());
    }
}
