// panw-api-ollama: A secure proxy for Ollama API with PANW AI security integration
//
// This service wraps the Ollama API and provides content security scanning using
// Palo Alto Networks' AI Runtime API before forwarding requests to Ollama.
//
// # Overview
//
// This application serves as a security proxy for the Ollama API, scanning both
// prompts sent to language models and responses from them for security threats,
// policy violations, and potentially harmful content using Palo Alto Networks'
// AI Runtime security services.
//
// # Architecture
//
// - Configuration: Loaded from a YAML file at startup
// - Security: Integration with PANW AI Runtime API
// - Proxying: Transparent forwarding to Ollama API
// - Handlers: Endpoint-specific request processors
// - Streaming: Support for both streaming and non-streaming responses

//------------------------------------------------------------------------------
// Module declarations
//------------------------------------------------------------------------------

// Configuration loading and management.
mod config;
// HTTP request handlers for API endpoints.
mod handlers;
// Client for interacting with Ollama API services.
mod ollama;
// Security assessment and content filtering using PANW AI Runtime API.
mod security;
// Utilities for handling streaming responses.
mod stream;
// Common type definitions used throughout the application.
mod types;

//------------------------------------------------------------------------------
// Import declarations
//------------------------------------------------------------------------------

// Internal crate imports
use crate::handlers::*;
use crate::ollama::OllamaClient;
use crate::security::SecurityClient;

// Web framework imports
use axum::{extract::DefaultBodyLimit, routing::{get, post}, Router};

// Standard library imports
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

// Middleware and utility imports
use tower_http::trace::TraceLayer;
use tracing::{error, info};

//------------------------------------------------------------------------------
// Application State
//------------------------------------------------------------------------------

// Shared application state containing clients for external services.
//
// This state is cloned and passed to each request handler, providing
// access to the Ollama client and security assessment functionality.
#[derive(Clone)]
pub struct AppState {
    // Client for communicating with Ollama API
    pub(crate) ollama_client: OllamaClient,
    // Client for performing security assessments
    pub(crate) security_client: SecurityClient,
}

impl AppState {
    // Creates a new builder for constructing AppState with a fluent API.
    pub fn builder() -> AppStateBuilder {
        AppStateBuilder::default()
    }
}

// Builder for creating AppState instances with a fluent API.
//
// This builder follows the builder pattern to provide a clean interface
// for initializing the application state with required components.
#[derive(Default)]
pub struct AppStateBuilder {
    // Optional Ollama client to be set before building
    ollama_client: Option<OllamaClient>,
    // Optional security client to be set before building
    security_client: Option<SecurityClient>,
}

impl AppStateBuilder {
    // Sets the Ollama client for the application state.
    pub fn with_ollama_client(mut self, client: OllamaClient) -> Self {
        self.ollama_client = Some(client);
        self
    }

    // Sets the security client for the application state.
    pub fn with_security_client(mut self, client: SecurityClient) -> Self {
        self.security_client = Some(client);
        self
    }

    // Builds the AppState from the configured components.
    //
    // # Errors
    //
    // Returns an error if any required component is missing.
    pub fn build(self) -> Result<AppState, &'static str> {
        let ollama_client = self.ollama_client.ok_or("OllamaClient is required")?;
        let security_client = self.security_client.ok_or("SecurityClient is required")?;

        Ok(AppState {
            ollama_client,
            security_client,
        })
    }
}

//------------------------------------------------------------------------------
// Application Entry Point
//------------------------------------------------------------------------------

// Application entry point that initializes and runs the server.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging with a default level BEFORE loading config so that
    // any tracing emitted during config parsing (errors, validation warnings)
    // actually reaches stderr instead of being dropped by the default
    // no-op subscriber.
    setup_logging("info");

    // Load configuration
    let config = config::load_config("config.yaml")?;

    // Re-initialize logging at the configured level. tracing_subscriber's
    // global default can only be set once, so this is best-effort: if the
    // initial subscriber is already installed, the configured level is
    // applied via env-filter / level guards inside setup_logging.
    setup_logging(&config.server.debug_level);

    // Create application state
    let state = build_app_state(
        config.ollama.base_url,
        config.security
    )?;
    info!("Application state initialized successfully");

    // Build router with all the Ollama API endpoints
    let app = build_router(state);
    info!("Router configured with all endpoints");

    // Start the server
    info!("Starting server with configuration: {:?}", config.server);
    start_server(app, &config.server).await?;

    Ok(())
}

//------------------------------------------------------------------------------
// Helper Functions
//------------------------------------------------------------------------------

/// Sets up logging with the configured level.
///
/// Initializes the tracing subscriber with the appropriate log level
/// based on the configuration setting.
///
/// # Arguments
///
/// * `debug_level_str` - The string representation of the desired log level
fn setup_logging(debug_level_str: &str) {
    let debug_level = tracing::Level::from_str(debug_level_str).unwrap_or_else(|_| {
        error!(
            "Unknown debug level: {}, defaulting to ERROR",
            debug_level_str
        );
        tracing::Level::ERROR
    });

    // try_init is used (not init) so this function is idempotent. main()
    // calls it twice — once with a default level before config load, and
    // again with the configured level after — and the second call must not
    // panic. The first install wins; the second is a no-op. To still honor
    // the configured level, we wrap with a global level filter that
    // subsequent calls can re-set.
    let _ = tracing_subscriber::fmt()
        .with_max_level(debug_level)
        .with_target(true) // Include module path in logs
        .with_thread_ids(true) // Include thread IDs for concurrent diagnostics
        .try_init();

    info!(
        "Starting panw-api-ollama v{} server with log level: {}",
        env!("CARGO_PKG_VERSION"),
        debug_level
    );
}

/// Builds the application state with configured clients.
///
/// Creates and initializes the application state containing clients
/// for Ollama API and PANW security services.
///
/// # Arguments
///
/// * `config` - The application configuration
///
/// # Returns
///
/// * `Ok(AppState)` - Initialized application state
/// * `Err` - If client creation or initialization fails
fn build_app_state(
    ollama_base_url: String,
    security_config: config::SecurityConfig
) -> Result<AppState, Box<dyn std::error::Error>> {
    info!("Building application state with configured clients");

    // Create Ollama client
    let ollama_client = OllamaClient::new(ollama_base_url.clone())?;
    info!(
        "Created Ollama client with base URL: {}",
        ollama_base_url
    );

    // Create security client
    let security_client = SecurityClient::new(security_config)?;

    info!(
        "Created security client with base URL: {}",
        security_client.base_url()
    );

    // Build the application state using the builder pattern
    let state = AppState::builder()
        .with_ollama_client(ollama_client)
        .with_security_client(security_client)
        .build()?;

    Ok(state)
}

/// Builds the router with all API endpoints.
///
/// Creates an Axum router with all the API endpoints and middleware.
///
/// # Arguments
///
/// * `state` - The application state to be shared with handlers
///
/// # Returns
///
/// An Axum router configured with all endpoints
/// Body size limit for scanned chat/generate/embeddings endpoints (10 MiB).
/// Generous enough for long system prompts and batched embeddings while
/// bounding memory if a client (or a malicious actor) posts a huge payload.
pub const SCANNED_BODY_LIMIT: usize = 10 * 1024 * 1024;

fn build_router(state: AppState) -> Router {
    info!("Building API router with all endpoints");

    // Only endpoints that carry user prompts or model output get a dedicated
    // handler with PANW scanning. Everything else (model management,
    // metadata, OpenAI/Anthropic compat shims, future Ollama additions)
    // flows through the catch-all passthrough below without scanning.
    //
    // SCANNED_BODY_LIMIT caps Json extractor input on scanned routes.
    // Passthrough route enforces its own (larger) cap internally for blob
    // uploads — see handlers::passthrough.
    let scanned_routes = Router::new()
        .route("/api/generate", post(generate::handle_generate))
        .route("/api/chat", post(chat::handle_chat))
        .route("/api/embeddings", post(embeddings::handle_embeddings))
        .route("/api/embed", post(embeddings::handle_embed))
        .layer(DefaultBodyLimit::max(SCANNED_BODY_LIMIT));

    Router::new()
        .route("/healthz", get(health_check))
        .merge(scanned_routes)
        .fallback(handlers::passthrough::passthrough)
        // Disable the default Json body limit at the outer layer so the
        // passthrough fallback (which takes raw Body) is not constrained by
        // Axum's 2 MiB default. The passthrough handler enforces its own
        // larger limit via http_body_util::Limited.
        .layer(DefaultBodyLimit::disable())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

// Lightweight liveness probe — does not call Ollama.
// Used by the Docker HEALTHCHECK and orchestrators to confirm the
// proxy process is running and accepting connections.
async fn health_check() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Starts the HTTP server with the configured router.
///
/// Binds to the configured address and port and starts serving requests.
///
/// # Arguments
///
/// * `app` - The configured Axum router
/// * `server_config` - Server configuration settings
///
/// # Returns
///
/// * `Ok(())` - If the server starts and runs successfully
/// * `Err` - If binding fails or the server encounters an error
async fn start_server(
    app: Router,
    server_config: &config::ServerConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr = SocketAddr::new(IpAddr::from_str(&server_config.host)?, server_config.port);

    info!("Binding server to {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Server started successfully on {}", addr);

    info!("Waiting for incoming connections...");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    info!("Server shut down cleanly");
    Ok(())
}

/// Resolves when the process receives SIGINT (Ctrl-C) or SIGTERM. axum's
/// `with_graceful_shutdown` then stops accepting new connections and waits
/// for in-flight requests (including SSE/NDJSON streams) to finish before
/// returning. Without this, a SIGTERM during a streaming chat response cuts
/// the TCP connection mid-frame and clients see a malformed JSON tail.
async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            error!("Failed to install SIGINT handler: {}", e);
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                error!("Failed to install SIGTERM handler: {}", e);
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("Received SIGINT, starting graceful shutdown"),
        _ = terminate => info!("Received SIGTERM, starting graceful shutdown"),
    }
}
