//! `agent24 documents …`: the Documenting OS from the command line, through
//! the same client code the desktop runs (`agent24-documents-client`), so a
//! use case passing here passes for the desktop too (AGENTS.md).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

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
        /// Start at this block (`p<page>/b<n>`, a block's `block_id`); not
        /// with --cursor, which already holds the position
        #[arg(long)]
        block: Option<String>,
        /// Blocks per page (1–200)
        #[arg(long)]
        limit: Option<u64>,
        /// Fail with `partial_parse` rather than read a partly parsed file
        #[arg(long)]
        require_complete: bool,
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
    },
    /// Find text in a document's revision; every match is an anchor
    Find {
        /// The document's id (`doc_…`)
        document_id: String,
        /// The revision (1 is the imported file)
        revision: u64,
        /// What to look for (1–500 characters)
        query: String,
        /// The `next_cursor` of the page before
        #[arg(long)]
        cursor: Option<String>,
        /// Matches per page (1–200)
        #[arg(long)]
        limit: Option<u64>,
        /// Fail with `partial_parse` rather than search a partly parsed file
        #[arg(long)]
        require_complete: bool,
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
    },
    /// Extract fields from a document's revision with the local model; prints
    /// the extraction job (`documents job` follows it, `documents extraction`
    /// reads its values)
    Extract {
        /// The document's id (`doc_…`)
        document_id: String,
        /// The revision (1 is the imported file)
        revision: u64,
        /// A JSON file with the fields to extract (`{"fields":[{"key",
        /// "description","type"},..]}`), or `-` for stdin
        #[arg(long)]
        schema: PathBuf,
        /// Extract again although this schema was extracted before; needs --key
        #[arg(long)]
        rerun: bool,
        /// Names a rerun: the same key again replays it, a new one runs again.
        /// Only with --rerun (the OS reads it for nothing else)
        #[arg(long)]
        key: Option<String>,
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
    },
    /// Show an extraction's values, a page at a time
    Extraction {
        /// The extraction's id (`ext_…`)
        extraction_id: String,
        /// The `next_cursor` of the page before
        #[arg(long)]
        cursor: Option<String>,
        /// Values per page (1–200)
        #[arg(long)]
        limit: Option<u64>,
        /// Print the result as one JSON line
        #[arg(long)]
        json: bool,
    },
    /// Render a page of a document's revision as PNG
    Render {
        /// The document's id (`doc_…`)
        document_id: String,
        /// The revision (1 is the imported file)
        revision: u64,
        /// The page, from 1
        page: u64,
        /// Scale (0.25–4, default 1); the OS lowers it until the PNG fits 1 MiB
        #[arg(long)]
        scale: Option<f64>,
        /// Only this area: `x0,y0,x1,y1` in points from the page's top left
        #[arg(long)]
        region: Option<String>,
        /// Write the PNG to this file: into a new file beside it, then renamed
        /// over it. So not to a device or a pipe: in a directory that cannot
        /// be written (`/dev`) it fails, and a named pipe is replaced by a
        /// file (use --json and decode `png` instead)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Print the result as one JSON line, the PNG in base64 (`png`)
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
/// A request body reaches the OS only up to the kernel's 1 MiB
/// (`agent24-domain` `MAX_BODY_BYTES`), an extract's schema near that at
/// most; with room for the base and token, a bound so stdin cannot grow us.
const MAX_HOST_INPUT: u64 = 1024 * 1024 + 64 * 1024;

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
            block,
            limit,
            require_complete,
            json,
        } => {
            let mut req =
                json!({"op": "readText", "documentId": document_id, "revision": revision});
            put(&mut req, "cursor", cursor);
            put(&mut req, "block", block);
            put(&mut req, "limit", limit);
            put(
                &mut req,
                "requireComplete",
                require_complete.then_some(true),
            );
            operation(req, json).await
        }
        DocumentsAction::Find {
            document_id,
            revision,
            query,
            cursor,
            limit,
            require_complete,
            json,
        } => {
            let mut req = json!({"op": "find", "documentId": document_id, "revision": revision, "query": query});
            put(&mut req, "cursor", cursor);
            put(&mut req, "limit", limit);
            put(
                &mut req,
                "requireComplete",
                require_complete.then_some(true),
            );
            operation(req, json).await
        }
        DocumentsAction::Extract {
            document_id,
            revision,
            schema,
            rerun,
            key,
            json,
        } => match extract_request(document_id, revision, &schema, rerun, key) {
            Ok(req) => operation(req, json).await,
            Err(res) => report(&res, json),
        },
        DocumentsAction::Extraction {
            extraction_id,
            cursor,
            limit,
            json,
        } => {
            let mut req = json!({"op": "getExtraction", "extractionId": extraction_id});
            put(&mut req, "cursor", cursor);
            put(&mut req, "limit", limit);
            operation(req, json).await
        }
        DocumentsAction::Render {
            document_id,
            revision,
            page,
            scale,
            region,
            out,
            json,
        } => {
            let mut req = json!({"op": "renderPage", "documentId": document_id, "revision": revision, "page": page});
            put(&mut req, "scale", scale);
            put(&mut req, "region", region);
            render(req, out, json).await
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

/// Sets `req[key]` when there is a value.
fn put<T: serde::Serialize>(req: &mut Value, key: &str, value: Option<T>) {
    if let Some(value) = value {
        req[key] = json!(value);
    }
}

/// Far above a hundred fields (`DocumentsExtractRequest`); a bound so a
/// wrong file is not read whole.
const MAX_SCHEMA: u64 = 1024 * 1024;

/// The fields to extract, from a file or stdin (`-`): any JSON value; the OS
/// checks its shape. Not read, or not JSON: 400, before the daemon is asked.
fn read_schema(path: &Path) -> Result<Value, Response> {
    let mut text = String::new();
    let read = if path == Path::new("-") {
        std::io::stdin()
            .lock()
            .take(MAX_SCHEMA + 1)
            .read_to_string(&mut text)
    } else {
        std::fs::File::open(path).and_then(|f| f.take(MAX_SCHEMA + 1).read_to_string(&mut text))
    };
    if read.is_err() || text.len() as u64 > MAX_SCHEMA {
        return Err(Response::fail(
            400,
            "file_unreadable",
            "the schema could not be read",
        ));
    }
    serde_json::from_str(&text).map_err(|_| invalid("the schema is not JSON".to_owned()))
}

/// `render`: the page as the shared operation returns it; with `--out` its
/// PNG written there first, so a page that could not be written is the
/// result (`file_unwritable`). A person is shown the page without its base64.
async fn render(req: Value, out: Option<PathBuf>, json: bool) -> Result<(), String> {
    let mut res = against_daemon(&req).await;
    if let (Some(path), Response::Ok { data, .. }) = (&out, &res)
        && let Err(failed) = write_page(data, path)
    {
        res = failed;
    }
    if let (false, Response::Ok { data, .. }) = (json, &mut res)
        && let Some(shown) = data.as_object_mut()
    {
        shown.remove("png");
        if let Some(path) = &out {
            shown.insert("written_to".to_owned(), json!(path));
        }
    }
    report(&res, json)
}

/// The `extract` operation. A `--key` without `--rerun` is refused here: the
/// OS reads the key only for a rerun, so it would otherwise answer with the
/// earlier extraction as if the key had been used.
fn extract_request(
    document_id: String,
    revision: u64,
    schema: &Path,
    rerun: bool,
    key: Option<String>,
) -> Result<Value, Response> {
    if key.is_some() && !rerun {
        return Err(invalid(
            "--key names a rerun; give --rerun with it".to_owned(),
        ));
    }
    let schema = read_schema(schema)?;
    let mut req =
        json!({"op": "extract", "documentId": document_id, "revision": revision, "schema": schema});
    put(&mut req, "rerun", rerun.then_some(true));
    put(&mut req, "idempotencyKey", key);
    Ok(req)
}

/// Writes the page's PNG to `path` whole or not at all: into a new file
/// beside it first (a name of its own), renamed over `path` once written, so
/// a failure midway (a full disk) leaves a file already there as it was. In
/// place of a file, the new one is readable only by its owner until it is
/// written and then takes that file's permissions; with no file there, it is
/// made as the umask allows from the start, as writing in place would. A
/// link is written through to its target. A page that is not base64 is
/// `invalid_response`; any step of writing it that fails is
/// `file_unwritable`; among them, the new file cannot be made beside it (a
/// directory that cannot be written, as `/dev` for `/dev/null`), a link leads
/// nowhere (it cannot be resolved), or the rename fails (a directory at
/// `path`). A file there is replaced, a named pipe too; another hard link
/// keeps the old page.
fn write_page(data: &Value, path: &Path) -> Result<(), Response> {
    use base64::Engine as _;
    let png = data
        .get("png")
        .and_then(Value::as_str)
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .ok_or_else(|| Response::fail(502, "invalid_response", "the page is not base64"))?;
    fn unwritable<E>(_: E) -> Response {
        Response::fail(400, "file_unwritable", "the page could not be written")
    }
    let target = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            std::fs::canonicalize(path).map_err(unwritable)?
        }
        _ => path.to_path_buf(),
    };
    target.file_name().ok_or(()).map_err(unwritable)?;
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let was = std::fs::metadata(&target).ok().map(|m| m.permissions());
    let mut builder = tempfile::Builder::new();
    builder.prefix(".agent24-page-").suffix(".part");
    if was.is_none() {
        as_new(&mut builder);
    }
    let mut part = builder.tempfile_in(dir).map_err(unwritable)?;
    part.write_all(&png)
        .and_then(|()| match was {
            Some(was) => part.as_file().set_permissions(was),
            None => Ok(()),
        })
        .and_then(|()| part.as_file().sync_all())
        .map_err(unwritable)?;
    // A failure drops the part file, which removes it.
    part.persist(&target).map(drop).map_err(unwritable)
}

/// A page with no file before it: its part file is made as a file made in
/// place would be, readable as the umask allows (tempfile's own default is
/// owner-only, which a page replacing a file keeps until it is written).
#[cfg(unix)]
fn as_new(builder: &mut tempfile::Builder<'_, '_>) {
    use std::os::unix::fs::PermissionsExt as _;
    builder.permissions(std::fs::Permissions::from_mode(0o666));
}

#[cfg(not(unix))]
fn as_new(_: &mut tempfile::Builder<'_, '_>) {}

fn invalid(message: String) -> Response {
    Response::fail(400, "invalid_request", &message)
}

/// The running daemon. A failure to find it, like every failure before
/// anything is sent, is a [`Response`] too, so `--json` still ends with its
/// result line: 400 `invalid_request` for what was given, 503
/// `backend_not_ready` for no daemon (the desktop's code for it), 503
/// `backend_incompatible` for a daemon on another protocol (CORE-4): nothing
/// is sent to it.
async fn daemon() -> Result<HttpTransport, Response> {
    let ep = super::attach_only()
        .await
        .map_err(|why| Response::fail(503, "backend_incompatible", &why))?
        .ok_or_else(|| Response::fail(503, "backend_not_ready", NO_DAEMON))?;
    HttpTransport::new(&ep.base, Some(ep.token().to_owned())).map_err(invalid)
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
            // A progress line that cannot be written is not the result.
            let _ = say(json!({"type": "progress", "sent": p.sent, "total": p.total}));
        } else {
            let _ = write!(std::io::stderr(), "\rsent {} of {} bytes", p.sent, p.total);
            shown = true;
        }
    };
    let res = import_file(&path, &transport, &mut on_progress).await;
    if shown {
        let _ = writeln!(std::io::stderr());
    }
    report(&res, json)
}

/// A line on stdout.
fn say(line: impl std::fmt::Display) -> Result<(), String> {
    say_to(std::io::stdout().lock(), line)
}

/// A reader that went away (the desktop closing the pipe, `| head`) is no
/// reason to panic or to fail: the exit code still says how it went, and an
/// import already sent goes on in the daemon. Any other failure to write (a
/// full disk) is an error, so a result nobody got never exits 0.
fn say_to(mut out: impl Write, line: impl std::fmt::Display) -> Result<(), String> {
    match writeln!(out, "{line}") {
        Err(err) if err.kind() != std::io::ErrorKind::BrokenPipe => {
            Err("stdout could not be written".to_owned())
        }
        _ => Ok(()),
    }
}

/// The result as one JSON line, or for a person (an import job as one
/// line, anything else as indented JSON); an error is an `Err` either way,
/// so the exit code is 1.
fn report(res: &Response, json: bool) -> Result<(), String> {
    if json {
        let mut line = res.to_json();
        line["type"] = json!("result");
        say(line)?;
    }
    match res {
        Response::Ok { data, .. } => {
            if !json {
                say(for_a_person(data))?;
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
            "x".repeat(MAX_HOST_INPUT as usize)
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
        // An extract with the most fields the OS takes, each described at
        // length (100 × 500 characters): far past the old 64 KiB.
        let field = json!({"key": "k", "description": "说明".repeat(250)});
        let request = json!({"op": "extract", "schema": {"fields": vec![field; 100]}});
        let big = json!({"base": "http://127.0.0.1:1", "token": "t", "request": request});
        let big = big.to_string();
        assert!(big.len() > 150_000);
        let ok: HostRequest = host_input(big.as_bytes(), SHAPE).unwrap();
        assert_eq!(ok.request, request);
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

    struct Failing(std::io::ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn only_a_reader_that_went_away_is_not_a_failure_to_write() {
        assert_eq!(say_to(Failing(std::io::ErrorKind::BrokenPipe), "x"), Ok(()));
        assert_eq!(
            say_to(Failing(std::io::ErrorKind::StorageFull), "x").unwrap_err(),
            "stdout could not be written"
        );
        let mut buf = Vec::new();
        assert_eq!(say_to(&mut buf, "line"), Ok(()));
        assert_eq!(buf, b"line\n");
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
    fn a_schema_is_any_json_from_its_file_and_anything_else_is_refused_unsent() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("ask.json");
        std::fs::write(&ok, r#"{"fields":[{"key":"date"}]}"#).unwrap();
        assert_eq!(
            read_schema(&ok).unwrap(),
            json!({"fields": [{"key": "date"}]})
        );
        let text = dir.path().join("ask.txt");
        std::fs::write(&text, "fields: date").unwrap();
        let not_json = Response::fail(400, "invalid_request", "the schema is not JSON");
        assert_eq!(read_schema(&text).unwrap_err(), not_json);
        // Refused by extract_request, so there is no request to send.
        assert_eq!(
            extract_request("doc_x".into(), 1, &text, false, None).unwrap_err(),
            not_json
        );
        let big = dir.path().join("big.json");
        std::fs::write(&big, format!("\"{}\"", "x".repeat(1024 * 1024))).unwrap();
        let unreadable = Response::fail(400, "file_unreadable", "the schema could not be read");
        assert_eq!(read_schema(&big).unwrap_err(), unreadable);
        assert_eq!(
            read_schema(&dir.path().join("none.json")).unwrap_err(),
            unreadable
        );
    }

    #[test]
    fn a_page_is_written_as_its_png_bytes_or_the_failure_is_the_result() {
        let dir = tempfile::tempdir().unwrap();
        let page = json!({"media_type": "image/png", "bytes": 4, "scale": 1.0, "png": "iVBORw=="});
        let out = dir.path().join("p1.png");
        write_page(&page, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"\x89PNG");
        assert_eq!(
            write_page(&page, &dir.path().join("no/such/dir/p1.png")).unwrap_err(),
            Response::fail(400, "file_unwritable", "the page could not be written")
        );
        assert_eq!(
            write_page(&json!({"png": "not base64!"}), &out)
                .unwrap_err()
                .code(),
            Some("invalid_response")
        );
        // A failure leaves the file there as it was, and no part file.
        let taken = dir.path().join("taken");
        std::fs::create_dir(&taken).unwrap();
        std::fs::write(taken.join("x"), b"x").unwrap();
        assert_eq!(
            write_page(&page, &taken).unwrap_err().code(),
            Some("file_unwritable")
        );
        assert!(taken.join("x").exists());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        // Over an existing file: replaced whole.
        std::fs::write(&out, b"old page").unwrap();
        write_page(&page, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"\x89PNG");
        // The longest name the disk takes is still a name to write to.
        let long = dir.path().join(format!("{}.png", "p".repeat(251)));
        write_page(&page, &long).unwrap();
        assert_eq!(std::fs::read(&long).unwrap(), b"\x89PNG");
    }

    #[cfg(unix)]
    #[test]
    fn a_page_keeps_the_permissions_and_the_link_of_what_it_replaces() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let page = json!({"png": "iVBORw=="});
        let private = dir.path().join("private.png");
        std::fs::write(&private, b"old").unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_page(&page, &private).unwrap();
        assert_eq!(mode(&private), 0o600);
        let shared = dir.path().join("shared.png");
        std::fs::write(&shared, b"old").unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o664)).unwrap();
        write_page(&page, &shared).unwrap();
        assert_eq!(mode(&shared), 0o664);
        // A new page is as readable as a file made in place.
        let made = dir.path().join("made");
        std::fs::write(&made, b"").unwrap();
        let new = dir.path().join("new.png");
        write_page(&page, &new).unwrap();
        assert_eq!(mode(&new), mode(&made));
        // Through a link: the link stays, its target is the new page.
        let kept = dir.path().join("archive.png");
        std::fs::write(&kept, b"old").unwrap();
        let link = dir.path().join("link.png");
        std::os::unix::fs::symlink(&kept, &link).unwrap();
        write_page(&page, &link).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&kept).unwrap(), b"\x89PNG");
    }

    #[test]
    fn a_key_without_a_rerun_is_refused_unsent() {
        let dir = tempfile::tempdir().unwrap();
        let schema = dir.path().join("ask.json");
        std::fs::write(&schema, r#"{"fields":[]}"#).unwrap();
        let ask = |rerun, key: Option<&str>| {
            extract_request("doc_x".into(), 1, &schema, rerun, key.map(Into::into))
        };
        assert_eq!(
            ask(false, Some("k")).unwrap_err(),
            Response::fail(
                400,
                "invalid_request",
                "--key names a rerun; give --rerun with it"
            )
        );
        assert_eq!(
            ask(true, Some("k")).unwrap(),
            json!({"op": "extract", "documentId": "doc_x", "revision": 1,
                "schema": {"fields": []}, "rerun": true, "idempotencyKey": "k"})
        );
        assert_eq!(
            ask(false, None).unwrap(),
            json!({"op": "extract", "documentId": "doc_x", "revision": 1, "schema": {"fields": []}})
        );
    }

    #[test]
    fn the_os_not_clap_judges_a_rerun_without_its_key_and_a_block_with_a_cursor() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            action: DocumentsAction,
        }
        let parse =
            |args: &[&str]| Cli::try_parse_from(["documents"].iter().chain(args)).map(|_| ());
        assert!(
            parse(&[
                "extract", "doc_x", "1", "--schema", "a.json", "--rerun", "--key", "k"
            ])
            .is_ok()
        );
        // Refused by clap there is no result line for --json (exit 2): these
        // are refused with one, by the OS or before it (a key with no rerun).
        assert!(parse(&["extract", "doc_x", "1", "--schema", "a.json", "--rerun"]).is_ok());
        assert!(parse(&["extract", "doc_x", "1", "--schema", "a.json", "--key", "k"]).is_ok());
        assert!(parse(&["text", "doc_x", "1", "--block", "p1/b1", "--cursor", "ab"]).is_ok());
        assert!(parse(&["extract", "doc_x", "1"]).is_err());
        assert!(parse(&["text", "doc_x", "1", "--block", "p1/b1", "--limit", "5"]).is_ok());
        assert!(
            parse(&[
                "render", "doc_x", "1", "2", "--scale", "0.5", "--out", "p.png"
            ])
            .is_ok()
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
