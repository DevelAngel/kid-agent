mod cli;
mod control;

use crate::cli::{Cli, Command, DaemonArgs};
use crate::control::ControlServer;
use matrix_bot::Bot;
use matrix_sampling::{LLM, Message as LLMMessage};

use anyhow::{Context, Result};
use clap::Parser;
use reqwest::Client;
use reqwest::header::HeaderMap;
use rmcp::model::{
    ClientCapabilities, ClientInfo, Implementation, ReadResourceRequestParams, ReadResourceResult,
    ResourceContents,
};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::auth::{AuthClient, ClientCredentialsConfig, OAuthState};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{ErrorData as McpError, ServiceExt};
use secrecy::ExposeSecret;
use serde::Deserialize;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

struct Agent {
    llm: LLM,
}

impl Agent {
    fn new(api_base_url: &str, api_key: Option<&str>) -> Self {
        let llm = LLM::connect(api_base_url, api_key);
        Self { llm }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_max_level(cli.verbosity)
        .with_writer(io::stderr)
        .init();

    let control_socket = cli.global.control_socket;
    match cli.command {
        Command::Serve(args) => run_daemon(*args, control_socket).await,
        Command::Trigger(args) => control::trigger_report(&control_socket, &args.resource).await,
    }
}

/// Runs the daemon: connects once as the Matrix bot, then drives the chat
/// sync loop and the control-socket accept loop concurrently until either
/// one fails. Both share the same `Bot`/`Agent`, so only one Matrix device
/// is ever in use.
async fn run_daemon(args: DaemonArgs, control_socket: PathBuf) -> Result<()> {
    let api_key = None; // unsupported yet
    let agent = Agent::new(&args.llm_api_base_url, api_key);

    tracing::info!("connect to matrix bot account");
    let bot = Bot::connect(
        &args.homeserver,
        &args.devicename,
        &args.username,
        args.password.expose_secret(),
        args.recovery_key.expose_secret(),
        &args.state_dir,
    )
    .await?;

    let server = ControlServer::bind(&control_socket)?;
    tracing::info!(path = %control_socket.display(), "control socket ready");

    let control_loop = async {
        loop {
            let stream = server.accept().await?;
            let bot = bot.clone();
            let agent = &agent;
            let args = &args;
            control::handle_connection(stream, |resource| async move {
                dispatch_report(&bot, agent, args, &resource).await
            })
            .await;
        }
    };

    tokio::select! {
        result = bot.run_sync_loop() => result.context("chat sync loop ended"),
        result = control_loop => result,
    }
}

/// Generates the report text for `resource` and sends it to the configured
/// room. Shared by every control-socket "report" request.
async fn dispatch_report(
    bot: &Bot,
    agent: &Agent,
    args: &DaemonArgs,
    resource: &str,
) -> Result<()> {
    tracing::info!(resource, "fetch resource");
    let text = generate_message(
        &args.generate_url,
        resource,
        &args.generate_client_id,
        args.generate_client_secret.expose_secret(),
    )
    .await?;

    let text = render_report(agent, resource, &text).await;

    tracing::info!("send matrix bot message");
    bot.send_message(&args.room_id, &text).await
}

/// Adds the report's Zelda-voiced framing on top of the raw `text` fetched
/// from the "generate message" MCP resource, picking a title/intro/outro
/// style based on `resource`'s name.
async fn render_report(agent: &Agent, resource: &str, text: &str) -> String {
    if resource.contains("weekly") {
        tracing::info!("request zelda review");
        let title = "Weekly Review";
        match ZeldaReview::request(agent, title, text).await {
            Ok(ZeldaReview(review)) => {
                format!("**{title}**\n\n{review}\n\n**Changelog**\n\n{text}\n")
            }
            Err(err) => {
                tracing::error!(?err, "failed to get a Zelda review from LLM");
                format!("**{title}** — **Changelog**\n\n{text}\n")
            }
        }
    } else if resource.contains("daily") {
        tracing::info!("request Zelda commentary from LLM");
        let title = "Daily Report";
        match ZeldaCommentary::request(agent, title, text).await {
            Ok(ZeldaCommentary { intro, outro }) => {
                format!("**{title}**\n\n{intro}\n\n{text}\n\n{outro}\n")
            }
            Err(err) => {
                tracing::error!(?err, "failed to get a Zelda commentar from LLM");
                let intro = select_random_text(&DAILY_REPORT_INTROS);
                format!("**{title}**\n\n{intro}\n\n{text}\n")
            }
        }
    } else if resource.contains("quick") {
        let title = "Quick Wins";
        match ZeldaCommentary::request(agent, title, text).await {
            Ok(ZeldaCommentary { intro, outro }) => {
                format!("**{title}**\n\n{intro}\n\n{text}\n\n{outro}\n")
            }
            Err(err) => {
                tracing::error!(?err, "failed to get a Zelda commentar from LLM");
                let intro = select_random_text(&QUICK_WINS_INTROS);
                format!("**{title}**\n\n{intro}\n\n{text}\n")
            }
        }
    } else {
        let title = "Backlog";
        match ZeldaCommentary::request(agent, title, text).await {
            Ok(ZeldaCommentary { intro, outro }) => {
                format!("**{title}**\n\n{intro}\n\n{text}\n\n{outro}\n")
            }
            Err(err) => {
                tracing::error!(?err, "failed to get a Zelda commentar from LLM");
                let intro = select_random_text(&BACKLOG_INTROS);
                format!("**{title}**\n\n{intro}\n\n{text}\n")
            }
        }
    }
}

/// Connects as an MCP client to the "generate message" server at
/// `generate_url` (Streamable HTTP), authenticating via the OAuth 2.1
/// client credentials grant, reads its `generate_resource` resource, and
/// returns the generated message text.
async fn generate_message(
    generate_url: &str,
    generate_resource: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<String> {
    let oauth_http_client = Client::builder()
        .timeout(Duration::from_secs(60))
        .default_headers(HeaderMap::new())
        .build()
        .context("failed to create http client for OAuth communication")?;
    let mut oauth_state = OAuthState::new(generate_url, Some(oauth_http_client))
        .await
        .with_context(|| format!("failed to initialize OAuth state for {generate_url}"))?;
    oauth_state
        .authenticate_client_credentials(ClientCredentialsConfig::ClientSecret {
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
            scopes: vec![],
            resource: Some(generate_url.to_owned()),
        })
        .await
        .with_context(|| {
            format!("OAuth client credentials authentication failed for {generate_url}")
        })?;

    let auth_manager = oauth_state
        .into_authorization_manager()
        .context("failed to get OAuth authorization manager")?;
    let auth_client = AuthClient::new(Client::default(), auth_manager);
    let transport = StreamableHttpClientTransport::with_client(
        auth_client,
        StreamableHttpClientTransportConfig::with_uri(generate_url),
    );

    let client_info = ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("matrix-relay", env!("CARGO_PKG_VERSION")),
    );
    let client = client_info
        .serve(transport)
        .await
        .with_context(|| format!("failed to connect to generate server at {generate_url}"))?;

    let result = client
        .read_resource(ReadResourceRequestParams::new(generate_resource.to_owned()))
        .await
        .with_context(|| format!("failed to read resource {generate_resource}"))?;

    let _ = client.cancel().await;

    extract_text(&result).with_context(|| format!("{generate_resource} has no text contents"))
}

/// Concatenates all text contents of a resource read result. Resources are
/// meant to return exactly one content item, but joining multiple (e.g. one
/// per sub-resource) is harmless and doesn't need special-casing.
fn extract_text(result: &ReadResourceResult) -> Option<String> {
    let text: String = result
        .contents
        .iter()
        .filter_map(|content| match content {
            ResourceContents::TextResourceContents { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    if text.is_empty() { None } else { Some(text) }
}

#[derive(Debug)]
struct ZeldaReview(String);

impl ZeldaReview {
    async fn request(agent: &Agent, report_title: &str, report_markdown: &str) -> Result<Self> {
        use indoc::indoc;
        const SYSTEM_PROMPT: &str = indoc! {r#"
            You narrate a weekly review of a household task list in the voice of
            a Zelda game — courage, quests, the Triforce, Navi-esque
            encouragement, emojis like 🧚 🌳 🍃 🗡️ ✨ 🐎 🧝 🧙 🐲 — based on a raw
            list of tasks created, changed, or completed in the last 8 days.

            The Zelda voice is decoration. These six rules govern the substance
            and override the voice whenever they conflict:

            1. Describe behavior, not performance. Never evaluate, rate, or
               praise the person themselves — only what was done and how.
               "You cleared the westward road before the second moon" not "You
               did great this week."
            2. No scores. No percentages, no "X of Y done", no streaks, no
               points, no rank — not even reskinned as hearts, rupees, or
               levels.
            3. No comparisons to other weeks or to some ideal pace. Each week
               stands alone.
            4. Open or postponed tasks are never failure, never framed with
               urgency or guilt. Frame them as a strategy choice: "This quest
               still awaits — worth splitting into smaller trials, or naming a
               day next week to take it on?"
            5. Keep tone constant regardless of how much or little happened.
               No extra triumph for a busy week, no dimmer tone for a quiet one.
            6. When something was clearly hard or got pushed more than once,
               treat it as a question of strategy, not resolve. Never imply the
               person lacked courage, effort, or discipline — only that the
               approach might want adjusting.

            Structure the review in three parts, told in-voice:
            - What realms/quest-lines the week's tasks clustered around (name
              the actual categories/themes — don't just say "you were busy").
            - What happened, narrated as a chronicle of actions and their order,
              not a checklist recap.
            - What quests remain open, each framed per rule 4.

            Respond in the same language as the task titles in the list. If
            tasks are in German, respond in German. If in English, respond in
            English. The Zelda flavor words (Triforce, quest, etc.) may stay
            in their conventional form even when translated loosely.
        "#};

        let user_msg = format!("Recent activity for: {report_title}\n\n{report_markdown}");
        let user_msg = LLMMessage::User(&user_msg);

        let response = agent
            .llm
            .send(Some(SYSTEM_PROMPT), &[user_msg], 0.9)
            .await?;

        Ok(Self(response))
    }
}

#[derive(Debug, Deserialize)]
struct ZeldaCommentary {
    intro: String,
    outro: String,
}

impl ZeldaCommentary {
    async fn request(agent: &Agent, report_title: &str, report_markdown: &str) -> Result<Self> {
        use indoc::indoc;
        const SYSTEM_PROMPT: &str = indoc! {r#"
            You narrate household task reports in the voice of a Zelda game —
            courage, quests, the Triforce, Navi-esque encouragement —
            while staying grounded in the actual report content you are given.
            Using emojis like 🧚 🌳 🍃 🗡️ ✨ 🐎 🧝 🧙 🐲 is your spirit.

            You always respond with ONLY a JSON object.
            No markdown code fences.
            No prose before or after the JSON.
            The object has exactly two fields:
            - "intro" (one short motivating sentence, placed before the task list) and
            - "outro" (a longer closing passage, 3-5 sentences, task related, placed after the list).

            Respond in the same language as the task summaries in the report.
            If tasks are in German, respond in German.
            If tasks are in English, respond in English.
            Match whichever language dominates the report.
        "#};

        let user_msg = format!("Report: {report_title}\n\n{report_markdown}");
        let user_msg = LLMMessage::User(&user_msg);

        let response = agent
            .llm
            .send(Some(SYSTEM_PROMPT), &[user_msg], 0.7)
            .await
            .map_err(|e| {
                McpError::internal_error(
                    "failed to retrieve a motivating sentence from LLM",
                    Some(e.to_string().into()),
                )
            })?;

        let response = serde_json::from_str::<Self>(&response)?;
        Ok(response)
    }
}

const DAILY_REPORT_INTROS: [&str; 10] = [
    "Hey! Listen! 🧚 New quests have appeared - time to set out!",
    "Wake up, young hero. 🧚 The tasks of this day await your courage. 🌳",
    "Hey! Hey! 🧚 Today's trials are ready for you!",
    "The path ahead is clear, adventurer. 🧙 Let's clear these quests! 🗡️",
    "Fairy's honor: today's objectives won't complete themselves. Onward! ✨",
    "Hey! 🧚 Over here! Your quest log has refreshed for today!",
    "It is time, hero. 🧝 Destiny - or at least today's to-do list - awaits.",
    "Hyah! 🐲 A new day dawns over Hyrule, and with it, new quests. ☀️",
    "The Great Deku Tree has watched over these tasks. Now they're yours. 🌳",
    "Hey! Listen! Don't let the day slip by like a Skulltula in the dark. ✨",
];

const BACKLOG_INTROS: [&str; 10] = [
    "Deep in the quest log, these tasks slumber - no deadline binds them yet. 🌳",
    "Side quests, patiently waiting in the shadow of the Great Tree. 🍃",
    "Hey! 🧚 These ones don't have a due date, but they haven't been forgotten!",
    "The undated scrolls of your journey rest here, hero. 🧙",
    "No urgency, no timer - just quests waiting for a worthy moment. 🧙",
    "Hey! Listen! 🧚 These are the quests that time forgot - for now.",
    "The Great Deku Tree has seen many ages pass, and these tasks with them. 🌳",
    "A hero's journey is long. 🧝 These quests will keep for when you're ready.",
    "Hey! 🧚 No rush on these ones - the forest keeps its secrets patiently. 🍃",
    "Old quests, older courage. 🧙 They'll be here when you return. ✨",
];

const QUICK_WINS_INTROS: [&str; 10] = [
    "Hey! Listen! 🧚 These quests are quick - a true hero clears them in a flash! ✨",
    "Small trials, swiftly won. Even a Kokiri could finish these. 🍃",
    "Hey! 🧚 No need for the Master Sword here - just a few minutes!",
    "Short quests, sorted by how fast courage can conquer them. 🗡️",
    "The Great Deku Tree smiles upon these easy victories. 🌳",
    "Hey! Listen! 🧚 Quick as a Deku Nut - these won't take long!",
    "Small deeds, hero, but every Triforce piece starts somewhere. ✨",
    "Hey! 🧚 Over here! Easy quests, ripe for the taking!",
    "Not every hero's journey needs an Epona 🐎 - these are a short walk. 🍃",
    "Fast quests for a fast hero. Go get 'em! 🗡️",
];

fn select_random_text(sentences: &[&'static str]) -> &'static str {
    use rand::seq::IndexedRandom;
    sentences.choose(&mut rand::rng()).unwrap()
}
