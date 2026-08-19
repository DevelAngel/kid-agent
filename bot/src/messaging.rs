use anyhow::{Context, Result};
use matrix_sdk::Client;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events::room::message::{
    FormattedBody, MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
    TextMessageEventContent,
};
use matrix_sdk::ruma::{OwnedRoomId, RoomId, RoomOrAliasId};
use strum::EnumString;

/// Chat commands recognized via a `!`-prefix (e.g. `!help`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString)]
#[strum(ascii_case_insensitive)]
enum ChatCommand {
    Help,
}

/// Parses a `!`-prefixed, case-insensitive room message body into a
/// [`ChatCommand`].
fn parse_chat_command(body: &str) -> Option<ChatCommand> {
    body.trim().strip_prefix('!')?.parse().ok()
}

/// The text sent back for `!help`. Grows as more commands are added.
fn help_text() -> String {
    indoc::indoc! {"
        **Available commands**

        - `!help` — show this list
    "}
    .to_owned()
}

/// Runs an indefinite Matrix sync loop, dispatching chat commands. Returns
/// only on an unrecoverable sync error.
pub async fn run_sync_loop(client: &Client) -> Result<()> {
    client.add_event_handler(on_room_message);
    client
        .sync(SyncSettings::default())
        .await
        .context("matrix sync loop terminated")
}

async fn on_room_message(event: OriginalSyncRoomMessageEvent, room: Room) {
    let MessageType::Text(text) = &event.content.msgtype else {
        return;
    };
    tracing::info!(
        room_id = %room.room_id(),
        sender = %event.sender,
        body = %text.body,
        "received room message",
    );

    let Some(command) = parse_chat_command(&text.body) else {
        return;
    };

    match command {
        ChatCommand::Help => {
            if let Err(err) = room.send(build_content(&help_text())).await {
                tracing::error!(?err, room_id = %room.room_id(), "failed to send help text");
            }
        }
    }
}

/// Sends `text` (interpreted as Markdown) as a single message to
/// `room_id_or_alias`, then returns. Callers that only need to send one-off
/// messages don't need a continuous sync loop or event handler - just enough
/// of a sync to have the room and device state needed to send (encrypted)
/// messages.
pub async fn send_message(client: &Client, room_id_or_alias: &str, text: &str) -> Result<()> {
    let room_or_alias_id = RoomOrAliasId::parse(room_id_or_alias)
        .with_context(|| format!("'{room_id_or_alias}' is neither a valid room ID nor alias"))?;

    client
        .sync_once(SyncSettings::default())
        .await
        .context("failed to sync")?;

    // A room alias (e.g. "#foo:example.com") isn't a room ID and can't be
    // looked up with `get_room` directly - it has to be resolved to the
    // actual room ID via the server first.
    let room_id: OwnedRoomId = match <&RoomId>::try_from(&*room_or_alias_id) {
        Ok(room_id) => room_id.to_owned(),
        Err(alias) => {
            client
                .resolve_room_alias(alias)
                .await
                .with_context(|| format!("failed to resolve room alias {alias}"))?
                .room_id
        }
    };

    let room = client
        .get_room(&room_id)
        .with_context(|| format!("not a member of room {room_id}, or room is unknown"))?;

    room.send(build_content(text))
        .await
        .context("failed to send message")?;
    tracing::info!("message sent to {room_id}");

    Ok(())
}

/// Builds the message content for `text`, rendering it as Markdown into an
/// HTML `formatted_body` alongside the plain-text fallback. If `text`
/// contains no Markdown formatting, `formatted` stays `None` and clients
/// just show the plain text.
fn build_content(text: &str) -> RoomMessageEventContent {
    let mut text_content = TextMessageEventContent::plain(text);
    text_content.formatted = FormattedBody::markdown(text);
    RoomMessageEventContent::new(MessageType::Text(text_content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chat_command_recognizes_help() {
        assert_eq!(parse_chat_command("!help"), Some(ChatCommand::Help));
    }

    #[test]
    fn parse_chat_command_is_case_insensitive() {
        assert_eq!(parse_chat_command("!HeLp"), Some(ChatCommand::Help));
    }

    #[test]
    fn parse_chat_command_ignores_plain_chat() {
        assert_eq!(parse_chat_command("hello there"), None);
    }

    #[test]
    fn parse_chat_command_requires_the_prefix() {
        assert_eq!(parse_chat_command("help"), None);
    }

    #[test]
    fn parse_chat_command_rejects_unknown_commands() {
        assert_eq!(parse_chat_command("!report daily"), None);
    }

    #[test]
    fn parse_chat_command_trims_surrounding_whitespace() {
        assert_eq!(parse_chat_command("  !help  "), Some(ChatCommand::Help));
    }


    fn as_text(content: &RoomMessageEventContent) -> &TextMessageEventContent {
        match &content.msgtype {
            MessageType::Text(text) => text,
            other => panic!("expected a text message, got {other:?}"),
        }
    }

    #[test]
    fn build_content_keeps_plain_text_as_body() {
        let content = build_content("just plain text, no markdown");
        assert_eq!(as_text(&content).body, "just plain text, no markdown");
    }

    #[test]
    fn build_content_without_markdown_has_no_formatted_body() {
        let content = build_content("just plain text, no markdown");
        assert!(as_text(&content).formatted.is_none());
    }

    #[test]
    fn build_content_with_markdown_renders_html() {
        let content = build_content("**bold** and _italic_");
        let formatted = as_text(&content)
            .formatted
            .as_ref()
            .expect("markdown should produce a formatted body");
        assert!(formatted.body.contains("<strong>bold</strong>"));
        assert!(formatted.body.contains("<em>italic</em>"));
    }
}
