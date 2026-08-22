pub use clap::Parser;
use clap::Subcommand;
use clap_verbosity_flag::{InfoLevel, Verbosity};
use secrecy::SecretString;
use std::path::PathBuf;

/// Matrix relay: either runs as a long-lived daemon (chat sync + control
/// socket) or, as a lightweight client, triggers a report on an already
/// running daemon over that control socket.
#[derive(Debug, Parser)]
#[command(author, version, about)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[command(flatten)]
    pub global: GlobalArgs,

    // verbose and quiet flag handling
    #[command(flatten)]
    pub verbosity: Verbosity<InfoLevel>,
}

/// Args accepted regardless of subcommand, since both `serve` and `trigger`
/// need to agree on the same control socket path.
#[derive(Debug, clap::Args)]
pub(crate) struct GlobalArgs {
    /// Path of the Unix control socket used to trigger reports on a running
    /// daemon. For `serve`, ignored if a socket has already been passed in
    /// by systemd socket activation (LISTEN_FDS) - that path always takes
    /// precedence, since systemd then owns the socket's lifetime and
    /// permissions.
    #[arg(
        long,
        env = "KID_AGENT_CONTROL_SOCKET",
        default_value = "/run/kid-agent/control.sock",
        global = true
    )]
    pub control_socket: PathBuf,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Runs continuously: syncs Matrix chat events and listens on a Unix
    /// control socket for report triggers, keeping a single logged-in
    /// Matrix device for both.
    Serve(Box<DaemonArgs>),

    /// Sends a report-trigger request to an already running daemon's
    /// control socket, then exits. Meant to be invoked by a systemd
    /// oneshot service on a timer.
    Trigger(TriggerArgs),
}

#[derive(Debug, clap::Args)]
pub(crate) struct DaemonArgs {
    /// Matrix Homeserver, e.g. "matrix.example.com".
    #[arg(long, env = "KID_AGENT_HOMESERVER")]
    pub homeserver: String,

    /// Device name
    #[arg(long, env = "KID_AGENT_DEVICE_NAME")]
    pub devicename: String,

    /// User name
    #[arg(long, env = "KID_AGENT_USERNAME")]
    pub username: String,

    /// Password of user
    #[arg(long, env = "KID_AGENT_PASSWORD", hide_env_values(true))]
    pub password: SecretString,

    /// Recovery key used to recover secrets (and thereby cross-sign this
    /// device) after login, so the bot's device is trusted without manual
    /// verification.
    #[arg(long, env = "KID_AGENT_RECOVERY_KEY", hide_env_values(true))]
    pub recovery_key: SecretString,

    /// Directory used to persist the Matrix state/crypto store and the login
    /// session across restarts, so this client keeps reusing the same device
    /// instead of accumulating a new one on every run.
    #[arg(long, env = "KID_AGENT_STATE_DIR", default_value = "./matrix-state")]
    pub state_dir: PathBuf,

    /// Room to send to: either a room ID (e.g. "!abcdef:example.com") or a
    /// room alias (e.g. "#room:example.com").
    #[arg(long, env = "KID_AGENT_ROOM_ID")]
    pub room_id: String,

    /// URL of the "generate message" MCP server's Streamable HTTP endpoint
    /// (e.g. "http://127.0.0.1:8001/mcp"), used to fetch report text.
    #[arg(long, env = "KID_AGENT_GENERATE_URL")]
    pub generate_url: String,

    /// OAuth 2.1 client ID used to authenticate with the "generate message"
    /// MCP server via the client credentials grant.
    #[arg(long, env = "KID_AGENT_GENERATE_CLIENT_ID")]
    pub generate_client_id: String,

    /// OAuth 2.1 client secret used to authenticate with the "generate
    /// message" MCP server via the client credentials grant.
    #[arg(long, env = "KID_AGENT_GENERATE_CLIENT_SECRET", hide_env_values(true))]
    pub generate_client_secret: SecretString,

    /// OpenAI-compatible API Base URL (e.g. "http://localhost:8000/v1")
    #[arg(long, env = "KID_AGENT_LLM_API_BASE_URL")]
    pub llm_api_base_url: String,
}

#[derive(Debug, clap::Args)]
pub(crate) struct TriggerArgs {
    /// URI of the resource to read on the "generate message" MCP server,
    /// e.g. "kid://report/daily". Not fixed yet, hence configurable rather
    /// than hardcoded.
    #[arg(long, env = "KID_AGENT_GENERATE_RESOURCE")]
    pub resource: String,

    /// Room to send this report to, overriding the daemon's default room
    /// for this trigger only: either a room ID or a room alias.
    #[arg(long, env = "KID_AGENT_TRIGGER_ROOM_ID")]
    pub room_id: Option<String>,
}
