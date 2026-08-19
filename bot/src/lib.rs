mod messaging;
mod recovery;
mod session;

use anyhow::Result;
use matrix_sdk::Client;
use std::path::Path;

/// A logged-in, cross-signed Matrix client.
///
/// This is the entry point for anything that needs to talk to Matrix - the
/// MCP server exposing `send_message` as a tool. Cheap to clone: like
/// [`Client`] itself, it just clones a handle to the shared connection
/// state, which the streamable HTTP transport needs to do per session.
#[derive(Clone)]
pub struct Bot {
    client: Client,
}

impl Bot {
    /// Logs in (or restores a persisted session), then cross-signs the
    /// device using `recovery_key` so other clients trust it. See
    /// `session::login` and `recovery::recover_device` for details.
    pub async fn connect(
        homeserver: &str,
        devicename: &str,
        username: &str,
        password: &str,
        recovery_key: &str,
        state_dir: &Path,
    ) -> Result<Self> {
        let client = session::login(homeserver, devicename, username, password, state_dir).await?;
        recovery::recover_device(&client, recovery_key).await?;
        Ok(Self { client })
    }

    /// Sends `text` (Markdown) to the given room ID or alias.
    pub async fn send_message(&self, room_id_or_alias: &str, text: &str) -> Result<()> {
        messaging::send_message(&self.client, room_id_or_alias, text).await
    }

    /// Runs an indefinite Matrix sync loop, handling incoming chat events.
    pub async fn run_sync_loop(&self) -> Result<()> {
        messaging::run_sync_loop(&self.client).await
    }
}
