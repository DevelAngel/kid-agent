use anyhow::{Context, Result};
use matrix_sdk::Client;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events::room::message::{
    FormattedBody, MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
    TextMessageEventContent,
};
use matrix_sdk::ruma::{OwnedRoomId, RoomId, RoomOrAliasId};
use std::future::Future;
use std::sync::Arc;
use strum::{EnumIter, EnumString, IntoEnumIterator};

/// Chat commands recognized via a `!`-prefix (e.g. `!help`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, EnumIter, derive_more::Display)]
#[strum(ascii_case_insensitive)]
enum ChatCommand {
    #[display("- `!help` — show this list")]
    Help,
    #[display("- `!tools` — list available MCP tools")]
    Tools,
    #[display("- `!resources` — list available MCP resources")]
    Resources,
}

impl ChatCommand {
    /// Parses a `!`-prefixed, case-insensitive room message body into a
    /// [`ChatCommand`].
    fn from_message(body: &str) -> Option<Self> {
        body.trim().strip_prefix('!')?.parse().ok()
    }
}

/// Lists all commands via their [`ChatCommand`] `Display` impl - grows
/// automatically as variants are added.
fn help_text() -> String {
    let commands = ChatCommand::iter().map(|command| command.to_string()).collect::<Vec<_>>();
    format!("**Available commands**\n\n{}\n", commands.join("\n"))
}

/// Extension trait adding chat-command dispatch and message sending to a
/// Matrix [`Client`].
pub(crate) trait ChatCommandLoop {
    /// Runs an indefinite sync loop, dispatching chat commands. `list_tools`
    /// backs `!tools`, `list_resources` backs `!resources`; both kept
    /// pluggable so this crate stays MCP-agnostic.
    async fn run_sync_loop<F1, Fut1, F2, Fut2>(
        &self,
        list_tools: F1,
        list_resources: F2,
    ) -> Result<()>
    where
        F1: Fn() -> Fut1 + Send + Sync + 'static,
        Fut1: Future<Output = Result<String>> + Send + 'static,
        F2: Fn() -> Fut2 + Send + Sync + 'static,
        Fut2: Future<Output = Result<String>> + Send + 'static;

    /// Sends `text` (Markdown) as a single message to `room_id_or_alias`.
    async fn send_message(&self, room_id_or_alias: &str, text: &str) -> Result<()>;
}

impl ChatCommandLoop for Client {
    async fn run_sync_loop<F1, Fut1, F2, Fut2>(
        &self,
        list_tools: F1,
        list_resources: F2,
    ) -> Result<()>
    where
        F1: Fn() -> Fut1 + Send + Sync + 'static,
        Fut1: Future<Output = Result<String>> + Send + 'static,
        F2: Fn() -> Fut2 + Send + Sync + 'static,
        Fut2: Future<Output = Result<String>> + Send + 'static,
    {
        let list_tools = Arc::new(list_tools);
        let list_resources = Arc::new(list_resources);
        self.add_event_handler(move |event: OriginalSyncRoomMessageEvent, room: Room| {
            let list_tools = Arc::clone(&list_tools);
            let list_resources = Arc::clone(&list_resources);
            async move { on_room_message(event, room, list_tools, list_resources).await }
        });
        self.sync(SyncSettings::default())
            .await
            .context("matrix sync loop terminated")
    }



    async fn send_message(&self, room_id_or_alias: &str, text: &str) -> Result<()> {
        let room_or_alias_id = RoomOrAliasId::parse(room_id_or_alias)
            .with_context(|| format!("'{room_id_or_alias}' is neither a valid room ID nor alias"))?;

        self.sync_once(SyncSettings::default())
            .await
            .context("failed to sync")?;

        // A room alias (e.g. "#foo:example.com") isn't a room ID and can't
        // be looked up with `get_room` directly - it has to be resolved to
        // the actual room ID via the server first.
        let room_id: OwnedRoomId = match <&RoomId>::try_from(&*room_or_alias_id) {
            Ok(room_id) => room_id.to_owned(),
            Err(alias) => {
                self.resolve_room_alias(alias)
                    .await
                    .with_context(|| format!("failed to resolve room alias {alias}"))?
                    .room_id
            }
        };

        let room = self
            .get_room(&room_id)
            .with_context(|| format!("not a member of room {room_id}, or room is unknown"))?;

        room.send(RoomMessageEventContent::markdown(text))
            .await
            .context("failed to send message")?;
        tracing::info!("message sent to {room_id}");

        Ok(())
    }
}

async fn on_room_message<F1, Fut1, F2, Fut2>(
    event: OriginalSyncRoomMessageEvent,
    room: Room,
    list_tools: Arc<F1>,
    list_resources: Arc<F2>,
) where
    F1: Fn() -> Fut1,
    Fut1: Future<Output = Result<String>>,
    F2: Fn() -> Fut2,
    Fut2: Future<Output = Result<String>>,
{
    let MessageType::Text(text) = &event.content.msgtype else {
        return;
    };
    tracing::info!(
        room_id = %room.room_id(),
        sender = %event.sender,
        body = %text.body,
        "received room message",
    );

    let Some(command) = ChatCommand::from_message(&text.body) else {
        return;
    };

    match command {
        ChatCommand::Help => send_markdown(&room, &help_text()).await,
        ChatCommand::Tools => match list_tools().await {
            Ok(text) => send_markdown(&room, &text).await,
            Err(err) => tracing::error!(?err, room_id = %room.room_id(), "failed to list MCP tools"),
        },
        ChatCommand::Resources => match list_resources().await {
            Ok(text) => send_markdown(&room, &text).await,
            Err(err) => {
                tracing::error!(?err, room_id = %room.room_id(), "failed to list MCP resources")
            }
        },
    }
}



/// Sends `text` as Markdown to `room`; logs rather than propagates send
/// failures, so a broken reply can't take down the sync loop.
async fn send_markdown(room: &Room, text: &str) {
    let content = RoomMessageEventContent::markdown(text);
    if let Err(err) = room.send(content).await {
        tracing::error!(?err, room_id = %room.room_id(), "failed to send message");
    }
}

/// Extension trait constructing message content from Markdown, rendering it
/// into an HTML `formatted_body` alongside the plain-text fallback. If the
/// source contains no Markdown formatting, `formatted` stays `None` and
/// clients just show the plain text.
trait MarkdownContent {
    fn markdown(text: &str) -> Self;
}

impl MarkdownContent for RoomMessageEventContent {
    fn markdown(text: &str) -> Self {
        let mut text_content = TextMessageEventContent::plain(text);
        text_content.formatted = FormattedBody::markdown(text);
        Self::new(MessageType::Text(text_content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_message_recognizes_help() {
        assert_eq!(ChatCommand::from_message("!help"), Some(ChatCommand::Help));
    }

    #[test]
    fn from_message_recognizes_tools() {
        assert_eq!(ChatCommand::from_message("!tools"), Some(ChatCommand::Tools));
    }

    #[test]
    fn from_message_recognizes_resources() {
        assert_eq!(ChatCommand::from_message("!resources"), Some(ChatCommand::Resources));
    }

    #[test]
    fn from_message_is_case_insensitive() {
        assert_eq!(ChatCommand::from_message("!HeLp"), Some(ChatCommand::Help));
    }

    #[test]
    fn from_message_ignores_plain_chat() {
        assert_eq!(ChatCommand::from_message("hello there"), None);
    }

    #[test]
    fn from_message_requires_the_prefix() {
        assert_eq!(ChatCommand::from_message("help"), None);
    }

    #[test]
    fn from_message_rejects_unknown_commands() {
        assert_eq!(ChatCommand::from_message("!report daily"), None);
    }

    #[test]
    fn from_message_trims_surrounding_whitespace() {
        assert_eq!(ChatCommand::from_message("  !help  "), Some(ChatCommand::Help));
    }
    fn as_text(content: &RoomMessageEventContent) -> &TextMessageEventContent {
        match &content.msgtype {
            MessageType::Text(text) => text,
            other => panic!("expected a text message, got {other:?}"),
        }
    }

    #[test]
    fn markdown_content_keeps_plain_text_as_body() {
        let content = RoomMessageEventContent::markdown("just plain text, no markdown");
        assert_eq!(as_text(&content).body, "just plain text, no markdown");
    }

    #[test]
    fn markdown_content_without_markdown_has_no_formatted_body() {
        let content = RoomMessageEventContent::markdown("just plain text, no markdown");
        assert!(as_text(&content).formatted.is_none());
    }

    #[test]
    fn markdown_content_with_markdown_renders_html() {
        let content = RoomMessageEventContent::markdown("**bold** and _italic_");
        let formatted = as_text(&content)
            .formatted
            .as_ref()
            .expect("markdown should produce a formatted body");
        assert!(formatted.body.contains("<strong>bold</strong>"));
        assert!(formatted.body.contains("<em>italic</em>"));
    }
}
