//! `agent24 documents …`: the Documenting OS from the command line, through
//! the same client code the desktop runs (`agent24-documents-client`), so a
//! use case passing here passes for the desktop too (AGENTS.md).

use std::io::Read;
use std::path::PathBuf;

use agent24_documents_client::http::HttpTransport;
use agent24_documents_client::{Progress, Response, call, import_file, refused, route};
use clap::Subcommand;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

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
    /// Show what the Documenting OS can do: storage, engines, operations
    Capabilities {
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
    },
    /// List the documents, a page at a time
    List {
        /// The `next_cursor` of the page before
        #[arg(long)]
        cursor: Option<String>,
        /// Documents per page (1–200)
        #[arg(long)]
        limit: Option<u64>,
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
    },
    /// Show a document and its revisions
    Get {
        /// The document's id (`doc_…`)
        document_id: String,
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
    },
    /// Read the text of a document's revision, a page of blocks at a time
    Text {
        /// The document's id (`doc_…`)
        document_id: String,
        /// The revision (1 is the imported file)
        revision: u64,
        /// The `next_cursor` of the page before
        #[arg(long)]
        cursor: Option<String>,
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
    },
    /// Show a job (an import or an extraction) and how far it got
    Job {
        /// The job's id (`job_…`)
        job_id: String,
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
    },
    /// Cancel a job
    Cancel {
        /// The job's id (`job_…`)
        job_id: String,
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
    },
    /// Run a failed or interrupted job again
    Retry {
        /// The job's id (`job_…`)
        job_id: String,
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
    },
    /// Run one operation given as the desktop's pages send it
    /// (`{"op":"get","documentId":..}`, ...)
    Request {
        /// The operation, as one JSON object
        request: Option<String>,
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
        /// Read `{"base","token","request"}` as one JSON object from stdin
        /// instead of finding the daemon: how the desktop runs it
        #[arg(long, conflicts_with = "request")]
        host_stdin: bool,
    },
}

const NO_DAEMON: &str = "no running daemon: start one with `agent24 daemon start` (a daemon the desktop started is reachable only from the desktop)";
/// Far above what the desktop sends; a bound so stdin cannot grow us.
const MAX_HOST_INPUT: u64 = 64 * 1024;

/// `--host-stdin` for `import`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostImport {
    base: String,
    token: String,
    path: PathBuf,
}

/// `--host-stdin` for `request`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostRequest {
    base: String,
    token: String,
    request: Value,
}

fn host_input<T: DeserializeOwned>(reader: impl Read, shape: &str) -> Result<T, String> {
    let mut text = String::new();
    reader
        .take(MAX_HOST_INPUT + 1)
        .read_to_string(&mut text)
        .map_err(|_| "stdin could not be read".to_owned())?;
    if text.len() as u64 > MAX_HOST_INPUT {
        return Err("stdin is too large".to_owned());
    }
    serde_json::from_str(&text).map_err(|_| format!("stdin must be one JSON object {shape}"))
}

pub(crate) async fn cmd_documents(action: DocumentsAction) -> Result<(), String> {
    match action {
        DocumentsAction::Import {
            path,
            json,
            host_stdin,
        } => import(path, json, host_stdin).await,
        DocumentsAction::Capabilities { json } => {
            operation(json!({"op": "capabilities"}), json).await
        }
        DocumentsAction::List {
            cursor,
            limit,
            json,
        } => {
            let mut req = json!({"op": "list"});
            if let Some(cursor) = cursor {
                req["cursor"] = json!(cursor);
            }
            if let Some(limit) = limit {
                req["limit"] = json!(limit);
            }
            operation(req, json).await
        }
        DocumentsAction::Get { document_id, json } => {
            operation(json!({"op": "get", "documentId": document_id}), json).await
        }
        DocumentsAction::Text {
            document_id,
            revision,
            cursor,
            json,
        } => {
            let mut req =
                json!({"op": "readText", "documentId": document_id, "revision": revision});
            if let Some(cursor) = cursor {
                req["cursor"] = json!(cursor);
            }
            operation(req, json).await
        }
        DocumentsAction::Job { job_id, json } => {
            operation(json!({"op": "job", "jobId": job_id}), json).await
        }
        DocumentsAction::Cancel { job_id, json } => {
            operation(json!({"op": "cancelJob", "jobId": job_id}), json).await
        }
        DocumentsAction::Retry { job_id, json } => {
            operation(json!({"op": "retryJob", "jobId": job_id}), json).await
        }
        DocumentsAction::Request {
            request,
            json,
            host_stdin,
        } => request_op(request, json, host_stdin).await,
    }
}

fn invalid(message: String) -> Response {
    Response::fail(400, "invalid_request", &message)
}

/// The running daemon. A failure to find it, like every failure before
/// anything is sent, is a [`Response`] too, so `--json` still ends with its
/// result line: 400 `invalid_request` for what was given, 503
/// `backend_not_ready` for no daemon (the desktop's code for it).
async fn daemon() -> Result<HttpTransport, Response> {
    let ep = super::attach_only()
        .await
        .ok_or_else(|| Response::fail(503, "backend_not_ready", NO_DAEMON))?;
    HttpTransport::new(&ep.base, Some(ep.token.clone())).map_err(invalid)
}

/// Where to send and what: from stdin (the desktop) or from the argument and
/// the running daemon.
async fn setup(
    path: Option<PathBuf>,
    host_stdin: bool,
) -> Result<(HttpTransport, PathBuf), Response> {
    if host_stdin {
        let input: HostImport =
            host_input(std::io::stdin().lock(), r#"{"base","token","path"}"#).map_err(invalid)?;
        let transport = HttpTransport::new(&input.base, Some(input.token)).map_err(invalid)?;
        return Ok((transport, input.path));
    }
    let path = path.ok_or_else(|| invalid("a file to import is required".to_owned()))?;
    Ok((daemon().await?, path))
}

/// `request`: the operation from stdin with the daemon (the desktop), or
/// from the argument.
async fn request_op(request: Option<String>, json: bool, host_stdin: bool) -> Result<(), String> {
    let found = if host_stdin {
        host_input::<HostRequest>(std::io::stdin().lock(), r#"{"base","token","request"}"#)
            .map_err(invalid)
            .and_then(|input| {
                let transport =
                    HttpTransport::new(&input.base, Some(input.token)).map_err(invalid)?;
                Ok((transport, input.request))
            })
    } else {
        match request.map(|r| serde_json::from_str::<Value>(&r)) {
            Some(Ok(req)) => return operation(req, json).await,
            Some(Err(_)) => Err(invalid("the operation must be one JSON object".to_owned())),
            None => Err(invalid("an operation is required".to_owned())),
        }
    };
    match found {
        Ok((transport, req)) => report(&call(&req, &transport).await, json),
        Err(res) => report(&res, json),
    }
}

/// One operation against the running daemon.
async fn operation(req: Value, json: bool) -> Result<(), String> {
    report(&against_daemon(&req).await, json)
}

/// A request that is not one is [`refused`] before the daemon is looked
/// for, so it is 400 with or without one, as on the desktop.
async fn against_daemon(req: &Value) -> Response {
    if route(req).is_none() {
        return refused();
    }
    match daemon().await {
        Ok(transport) => call(req, &transport).await,
        Err(res) => res,
    }
}

async fn import(path: Option<PathBuf>, json: bool, host_stdin: bool) -> Result<(), String> {
    let (transport, path) = match setup(path, host_stdin).await {
        Ok(found) => found,
        Err(res) => return report(&res, json),
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

/// The result as one JSON line, or for a person (an import job as one
/// line, anything else as indented JSON); an error is an `Err` either way,
/// so the exit code is 1.
fn report(res: &Response, json: bool) -> Result<(), String> {
    if json {
        let mut line = res.to_json();
        line["type"] = json!("result");
        println!("{line}");
    }
    match res {
        Response::Ok { data, .. } => {
            if !json {
                println!("{}", for_a_person(data));
            }
            Ok(())
        }
        Response::Err { error, .. } => Err(match &error.hint {
            Some(hint) => format!("{}: {} ({hint})", error.code, error.message),
            None => format!("{}: {}", error.code, error.message),
        }),
    }
}

/// What a person sees for a resource: a job (it has a `job_id` and a
/// `kind`) as `<kind> job <id> (<status>)`, anything else as indented JSON.
fn for_a_person(data: &Value) -> String {
    let field = |k: &str| data.get(k).and_then(Value::as_str);
    match (field("job_id"), field("kind")) {
        (Some(id), Some(kind)) => format!("{kind} job {id} ({})", field("status").unwrap_or("?")),
        _ => serde_json::to_string_pretty(data).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn host_input_is_one_object_with_base_token_and_path() {
        const SHAPE: &str = r#"{"base","token","path"}"#;
        let ok: HostImport = host_input(
            &br#"{"base":"http://127.0.0.1:1","token":"t","path":"/a b.pdf"}"#[..],
            SHAPE,
        )
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
            assert!(host_input::<HostImport>(bad, SHAPE).is_err());
        }
        let huge = format!(
            r#"{{"base":"{}","token":"t","path":"/a"}}"#,
            "x".repeat(70_000)
        );
        assert_eq!(
            host_input::<HostImport>(huge.as_bytes(), SHAPE).unwrap_err(),
            "stdin is too large"
        );
    }

    #[test]
    fn host_input_for_a_request_carries_the_operation_as_it_is() {
        const SHAPE: &str = r#"{"base","token","request"}"#;
        let ok: HostRequest = host_input(
            &br#"{"base":"http://127.0.0.1:1","token":"t","request":{"op":"job","jobId":"j"}}"#[..],
            SHAPE,
        )
        .unwrap();
        assert_eq!(ok.request, json!({"op": "job", "jobId": "j"}));
        for bad in [
            &br#"{"base":"http://127.0.0.1:1","token":"t"}"#[..],
            &br#"{"base":"http://127.0.0.1:1","token":"t","request":{},"path":"/a"}"#[..],
        ] {
            assert_eq!(
                host_input::<HostRequest>(bad, SHAPE).unwrap_err(),
                r#"stdin must be one JSON object {"base","token","request"}"#
            );
        }
    }

    #[tokio::test]
    async fn a_malformed_request_is_refused_before_any_daemon_is_looked_for() {
        for req in [
            json!({"op": "list", "limit": 0}),
            json!({"op": "get", "documentId": "bad"}),
            json!({"op": "proxy"}),
        ] {
            assert_eq!(against_daemon(&req).await, refused(), "{req}");
        }
    }

    #[test]
    fn a_job_reads_as_one_line_and_anything_else_as_json() {
        let job = json!({"job_id": "job_1", "kind": "extract", "status": "running"});
        assert_eq!(for_a_person(&job), "extract job job_1 (running)");
        let list = json!({"documents": [], "next_cursor": null});
        assert_eq!(
            for_a_person(&list),
            "{\n  \"documents\": [],\n  \"next_cursor\": null\n}"
        );
    }

    #[tokio::test]
    async fn no_file_given_is_an_invalid_request_before_any_daemon_is_looked_for() {
        let Err(res) = setup(None, false).await else {
            panic!("expected an error")
        };
        assert_eq!(
            res,
            Response::fail(400, "invalid_request", "a file to import is required")
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
