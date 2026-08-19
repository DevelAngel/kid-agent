//! Unix-socket control channel used to trigger a report on an already
//! running daemon without starting a second Matrix client/device.
//!
//! Protocol: one JSON object per connection, request then response, both
//! newline-terminated.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// A request sent over the control socket. `command` is a vestige of the
/// eventual chat-command dispatch sharing this same channel; only
/// `"report"` is handled today.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Request {
    pub command: String,
    pub resource: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub(crate) enum Response {
    Ok,
    Error { message: String },
}

/// Binds the control socket, preferring a systemd socket-activation FD
/// (`LISTEN_FDS`/`LISTEN_PID`) if one was passed in - systemd then owns the
/// socket file's creation, permissions, and cleanup. Falls back to binding
/// `path` directly, e.g. for local runs without systemd.
pub(crate) fn bind(path: &Path) -> Result<UnixListener> {
    let fds = sd_listen_fds::get().context("failed to inspect systemd LISTEN_FDS")?;
    if let Some((_name, fd)) = fds.into_iter().next() {
        let std_listener = std::os::unix::net::UnixListener::from(fd);
        std_listener
            .set_nonblocking(true)
            .context("failed to set socket-activated listener non-blocking")?;
        return UnixListener::from_std(std_listener)
            .context("failed to adopt systemd-provided control socket");
    }

    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove stale socket at {}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    UnixListener::bind(path)
        .with_context(|| format!("failed to bind control socket at {}", path.display()))
}

/// Reads one request, invokes `handle_report`, writes back the response.
/// Connection errors are logged and dropped rather than propagated, so one
/// bad client can't take down the daemon's control loop.
pub(crate) async fn handle_connection<F, Fut>(stream: UnixStream, handle_report: F)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    if let Err(err) = handle_connection_inner(stream, handle_report).await {
        tracing::error!(?err, "control connection failed");
    }
}

async fn handle_connection_inner<F, Fut>(stream: UnixStream, handle_report: F) -> Result<()>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .context("failed to read control request")?;

    let request: Request =
        serde_json::from_str(line.trim_end()).context("failed to parse control request")?;

    let response = if request.command == "report" {
        match handle_report(request.resource).await {
            Ok(()) => Response::Ok,
            Err(err) => Response::Error {
                message: err.to_string(),
            },
        }
    } else {
        Response::Error {
            message: format!("unknown command '{}'", request.command),
        }
    };

    let mut payload = serde_json::to_string(&response).context("failed to encode response")?;
    payload.push('\n');
    write_half
        .write_all(payload.as_bytes())
        .await
        .context("failed to write control response")?;

    Ok(())
}

/// Connects to the daemon's control socket at `path`, sends a report
/// trigger for `resource`, and returns once the daemon confirms success or
/// reports an error.
pub(crate) async fn trigger_report(path: &Path, resource: &str) -> Result<()> {
    let stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("failed to connect to control socket at {}", path.display()))?;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let request = Request {
        command: "report".to_owned(),
        resource: resource.to_owned(),
    };
    let mut payload = serde_json::to_string(&request).context("failed to encode request")?;
    payload.push('\n');
    write_half
        .write_all(payload.as_bytes())
        .await
        .context("failed to send control request")?;

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .context("failed to read control response")?;

    match serde_json::from_str::<Response>(line.trim_end())
        .context("failed to parse control response")?
    {
        Response::Ok => Ok(()),
        Response::Error { message } => bail!("daemon reported an error: {message}"),
    }
}
