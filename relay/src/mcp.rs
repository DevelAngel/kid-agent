//! MCP client connection helper shared by report generation and tool
//! discovery, both talking to the "generate message" MCP server.

use anyhow::{Context, Result};
use reqwest::Client;
use reqwest::header::HeaderMap;
use rmcp::RoleClient;
use rmcp::model::{ClientCapabilities, ClientInfo, Implementation};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::auth::{AuthClient, ClientCredentialsConfig, OAuthState};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::ServiceExt;
use std::time::Duration;

/// Connects as an MCP client to `generate_url`. Caller must cancel the
/// returned session when done.
pub(crate) async fn connect(
    generate_url: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<RunningService<RoleClient, ClientInfo>> {
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
    client_info
        .serve(transport)
        .await
        .with_context(|| format!("failed to connect to generate server at {generate_url}"))
}

/// Lists the tools exposed by the "generate message" MCP server, formatted
/// as Markdown ready to send as a chat reply.
pub(crate) async fn list_tools_markdown(
    generate_url: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<String> {
    let client = connect(generate_url, client_id, client_secret).await?;
    let result = client.list_tools(None).await.context("failed to list MCP tools");
    let _ = client.cancel().await;
    let result = result?;

    if result.tools.is_empty() {
        return Ok("**Available tools**\n\nno tools available\n".to_owned());
    }

    let tools = result
        .tools
        .iter()
        .map(|tool| match &tool.description {
            Some(description) => format!("- `{}` — {description}", tool.name),
            None => format!("- `{}`", tool.name),
        })
        .collect::<Vec<_>>();
    Ok(format!("**Available tools**\n\n{}\n", tools.join("\n")))
}
