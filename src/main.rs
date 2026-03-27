mod config;
mod output;
mod resolve;
mod slack;
mod socket_mode;

use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use reqwest::Method;
use rpassword::prompt_password;
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

use crate::config::{
    AuthType, ConfigStore, ProfileMeta, RuntimeAppSession, RuntimeSession, SLACK_APP_TOKEN_ENV,
    SLACKCLI_APP_TOKEN_ENV, SessionSource, StoredAppSecret, StoredSecret, read_text_file,
    slugify_profile_name,
};
use crate::output::OutputFormat;
use crate::resolve::SlackResolver;
use crate::slack::{SlackApiError, SlackAuthTest, SlackClient, merge_object_body};
use crate::socket_mode::ListenOptions;

#[derive(Parser)]
#[command(
    name = "slackcli",
    version,
    about = "Fast, local-first Rust CLI for the Slack Web API",
    long_about = "Fast, local-first Rust CLI for the Slack Web API.\n\nRun `slackcli auth login` once to store your Slack token locally. Future commands reuse the active stored profile automatically. Set `SLACK_TOKEN` for one-shot use without saving."
)]
struct Cli {
    /// Use a specific saved profile instead of the active profile.
    #[arg(long, global = true)]
    profile: Option<String>,
    /// Output format for command results. The default is human-readable.
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Human)]
    output: OutputFormat,
    /// Increase log verbosity. Repeat for more detail.
    #[arg(long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Clone)]
struct GlobalOptions {
    profile: Option<String>,
    output: OutputFormat,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Manage saved credentials and inspect authentication state")]
    Auth(AuthArgs),
    #[command(about = "List, create, archive, and inspect channels, DMs, and threads")]
    #[command(alias = "convo")]
    #[command(alias = "conversations")]
    Conversation(ConversationArgs),
    #[command(about = "Add, remove, and inspect reactions")]
    #[command(alias = "reactions")]
    Reaction(ReactionArgs),
    #[command(about = "List, inspect, upload, and delete files")]
    #[command(alias = "files")]
    File(FileArgs),
    #[command(about = "Inspect Slack users")]
    User(UserArgs),
    #[command(about = "Inspect the current workspace")]
    Team(TeamArgs),
    #[command(about = "Resolve human-friendly refs to Slack IDs without mutating Slack state")]
    Resolve(ResolveArgs),
    #[command(about = "Search messages")]
    Search(SearchArgs),
    #[command(about = "Post, update, delete, and permalink messages")]
    Message(MessageArgs),
    #[command(about = "Listen for Slack events over Socket Mode")]
    Listen(ListenArgs),
    #[command(about = "Call any Slack Web API method directly")]
    Api(ApiArgs),
}

#[derive(Args)]
#[command(about = "Authentication and profile management")]
struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Subcommand)]
enum AuthCommand {
    Login(AuthLoginArgs),
    #[command(about = "Validate and store an app-level xapp token for Socket Mode")]
    AppLogin(AuthAppLoginArgs),
    #[command(about = "List saved profiles and show which one is active")]
    List,
    #[command(about = "Check token reachability and current auth metadata")]
    Doctor,
    Use(AuthUseArgs),
    #[command(about = "Fetch the live auth.test response for the active profile")]
    Whoami,
    Logout(AuthLogoutArgs),
}

#[derive(Args)]
#[command(
    about = "Validate and store a Slack token",
    long_about = "Validate and store a Slack token.\n\nIf `--token` is omitted, the command prompts once with hidden input, validates the token with `auth.test`, stores it in the local credentials file, and marks the resulting profile as active. In interactive use, the command also prompts for the Socket Mode app token unless `--app-token` or `SLACK_APP_TOKEN` is already set. Prefer the hidden prompt for interactive use because shell history and process lists can expose command-line arguments. Set `SLACK_TOKEN` for one-shot use without saving."
)]
struct AuthLoginArgs {
    /// Optional profile name to store. If omitted, the CLI derives one automatically.
    #[arg(long)]
    profile_name: Option<String>,
    /// Slack token for automation-only flows. Prefer the hidden prompt for interactive use.
    #[arg(long)]
    token: Option<String>,
    /// Optional app-level xapp token to store for Socket Mode.
    #[arg(long)]
    app_token: Option<String>,
}

#[derive(Args)]
#[command(
    about = "Validate and store an app-level xapp token for Socket Mode",
    long_about = "Validate and store an app-level xapp token for Socket Mode.\n\nIf `--token` is omitted, the command prompts once with hidden input, validates the token with `apps.connections.open`, and stores it against an existing saved profile. Use this when `slackcli listen` should work without exporting `SLACK_APP_TOKEN` every time."
)]
struct AuthAppLoginArgs {
    /// Existing profile name to attach the app token to. Defaults to the active profile.
    #[arg(long)]
    profile_name: Option<String>,
    /// App-level token for automation-only flows. Prefer the hidden prompt for interactive use.
    #[arg(long)]
    token: Option<String>,
}

#[derive(Args)]
#[command(about = "Switch the active saved profile")]
struct AuthUseArgs {
    /// Profile name to activate.
    profile_name: String,
}

#[derive(Args)]
#[command(about = "Delete a saved profile and remove its secret from the local credentials file")]
struct AuthLogoutArgs {
    /// Profile name to remove. Defaults to the active profile.
    profile_name: Option<String>,
}

#[derive(Args)]
#[command(about = "Conversation operations")]
struct ConversationArgs {
    #[command(subcommand)]
    command: ConversationCommand,
}

#[derive(Subcommand)]
enum ConversationCommand {
    List(ConversationListArgs),
    Open(ConversationOpenArgs),
    Create(ConversationCreateArgs),
    Archive(ConversationArchiveArgs),
    Info(ConversationInfoArgs),
    History(ConversationHistoryArgs),
    Replies(ConversationRepliesArgs),
    Members(ConversationMembersArgs),
}

#[derive(Args)]
#[command(
    about = "List conversations for the current user",
    after_help = "Examples:\n  slackcli conversation list\n  slackcli conversation list --types im,mpim\n  slackcli conversation list --types public_channel,private_channel --exclude-archived"
)]
struct ConversationListArgs {
    /// Conversation types to include.
    #[arg(long, default_value = "public_channel,private_channel,im,mpim")]
    types: String,
    /// Exclude archived channels.
    #[arg(long)]
    exclude_archived: bool,
    /// Override the user whose memberships are listed. Accepts a user ID, @name, display name, or email.
    #[arg(long)]
    user: Option<String>,
    #[command(flatten)]
    pagination: PaginationArgs,
}

#[derive(Args)]
#[command(
    about = "Open or resume a DM or group DM",
    after_help = "Examples:\n  slackcli conversation open @alice\n  slackcli conversation open @alice,@bob"
)]
struct ConversationOpenArgs {
    /// Slack users to include. Accepts user IDs, @names, display names, and emails.
    #[arg(value_delimiter = ',', num_args = 1.., required = true)]
    users: Vec<String>,
    /// Ask Slack to return the full IM conversation object when possible.
    #[arg(long)]
    return_im: bool,
    /// Do not create a conversation if one does not already exist.
    #[arg(long)]
    prevent_creation: bool,
}

#[derive(Args)]
#[command(
    about = "Create a public or private channel",
    after_help = "Examples:\n  slackcli conversation create slackcli-smoke\n  slackcli conversation create slackcli-smoke --private"
)]
struct ConversationCreateArgs {
    /// Channel name. Must use lowercase letters, numbers, hyphens, or underscores.
    name: String,
    /// Create a private channel instead of a public one.
    #[arg(long)]
    private: bool,
}

#[derive(Args)]
#[command(about = "Archive a conversation when the Slack API supports it")]
struct ConversationArchiveArgs {
    /// Channel or conversation reference. Accepts IDs, #channel names, and existing @user DMs.
    channel: String,
}

#[derive(Args)]
#[command(about = "Fetch metadata for a conversation")]
struct ConversationInfoArgs {
    /// Channel or conversation reference. Accepts IDs, #channel names, and existing @user DMs.
    /// This command does not open DMs. Use `conversation open` first when needed.
    channel: String,
    /// Ask Slack to compute and return member count when supported.
    #[arg(long)]
    include_num_members: bool,
}

#[derive(Args)]
#[command(
    about = "Fetch messages from a conversation",
    after_help = "Examples:\n  slackcli conversation history C12345\n  slackcli conversation history C12345 --limit 50\n  slackcli conversation history C12345 --oldest 1710000000.000100"
)]
struct ConversationHistoryArgs {
    /// Channel or conversation reference. Accepts IDs, #channel names, and existing @user DMs.
    /// This command does not open DMs. Use `conversation open` first when needed.
    channel: String,
    #[command(flatten)]
    pagination: PaginationArgs,
    #[command(flatten)]
    window: TimelineArgs,
}

#[derive(Args)]
#[command(
    about = "Fetch replies for a thread",
    after_help = "Example:\n  slackcli conversation replies C12345 1710000000.000100 --limit 100"
)]
struct ConversationRepliesArgs {
    /// Channel or conversation reference. Accepts IDs, #channel names, and existing @user DMs.
    /// This command does not open DMs. Use `conversation open` first when needed.
    channel: String,
    /// Thread timestamp.
    thread_ts: String,
    #[command(flatten)]
    pagination: PaginationArgs,
    #[command(flatten)]
    window: TimelineArgs,
}

#[derive(Args)]
#[command(about = "List members of a conversation")]
struct ConversationMembersArgs {
    /// Channel or conversation reference. Accepts IDs, #channel names, and existing @user DMs.
    /// This command does not open DMs. Use `conversation open` first when needed.
    channel: String,
    #[command(flatten)]
    pagination: PaginationArgs,
}

#[derive(Args)]
#[command(about = "Reaction operations")]
struct ReactionArgs {
    #[command(subcommand)]
    command: ReactionCommand,
}

#[derive(Subcommand)]
enum ReactionCommand {
    Add(ReactionAddArgs),
    Remove(ReactionRemoveArgs),
    List(ReactionListArgs),
}

#[derive(Args)]
#[command(
    about = "Add a reaction to a message",
    after_help = "Example:\n  slackcli reaction add '#general' 1710000000.000100 thumbsup"
)]
struct ReactionAddArgs {
    /// Channel or conversation reference. Accepts IDs, #channel names, and @user DM targets.
    channel: String,
    /// Message timestamp.
    ts: String,
    /// Reaction name, with or without surrounding colons.
    name: String,
}

#[derive(Args)]
#[command(about = "Remove a reaction from a message")]
struct ReactionRemoveArgs {
    /// Channel or conversation reference. Accepts IDs, #channel names, and @user DM targets.
    channel: String,
    /// Message timestamp.
    ts: String,
    /// Reaction name, with or without surrounding colons.
    name: String,
}

#[derive(Args)]
#[command(about = "List reactions, optionally filtered by user")]
struct ReactionListArgs {
    /// Restrict results to a specific user. Accepts a user ID, @name, display name, or email.
    #[arg(long)]
    user: Option<String>,
    /// Include the full parent items in the response when supported.
    #[arg(long)]
    full: bool,
    /// Number of results per page.
    #[arg(long, default_value_t = 20)]
    count: usize,
    /// Page number.
    #[arg(long, default_value_t = 1)]
    page: usize,
}

#[derive(Args)]
#[command(about = "File operations")]
struct FileArgs {
    #[command(subcommand)]
    command: FileCommand,
}

#[derive(Subcommand)]
enum FileCommand {
    List(FileListArgs),
    Get(FileGetArgs),
    Delete(FileDeleteArgs),
    Upload(FileUploadArgs),
}

#[derive(Args)]
#[command(
    about = "List visible files",
    after_help = "Examples:\n  slackcli file list\n  slackcli file list --channel '#general' --count 50\n  slackcli file list --user @alice"
)]
struct FileListArgs {
    /// Restrict results to a user. Accepts a user ID, @name, display name, or email.
    #[arg(long)]
    user: Option<String>,
    /// Restrict results to a channel or DM. Accepts IDs, #channel names, and existing @user DMs.
    /// This command does not open DMs. Use `conversation open` first when needed.
    #[arg(long)]
    channel: Option<String>,
    /// File types filter passed through to Slack.
    #[arg(long)]
    types: Option<String>,
    /// Number of results per page.
    #[arg(long, default_value_t = 20)]
    count: usize,
    /// Page number.
    #[arg(long, default_value_t = 1)]
    page: usize,
    /// Lower bound Unix timestamp for file creation.
    #[arg(long = "ts-from")]
    ts_from: Option<String>,
    /// Upper bound Unix timestamp for file creation.
    #[arg(long = "ts-to")]
    ts_to: Option<String>,
    /// Include hidden files that Slack would normally omit.
    #[arg(long)]
    show_hidden: bool,
}

#[derive(Args)]
#[command(about = "Fetch a single file object")]
struct FileGetArgs {
    /// File ID.
    file_id: String,
}

#[derive(Args)]
#[command(about = "Delete a file")]
struct FileDeleteArgs {
    /// File ID.
    file_id: String,
}

#[derive(Args)]
#[command(
    about = "Upload a local file using Slack's external upload flow",
    after_help = "Examples:\n  slackcli file upload ./report.pdf --channel '#general'\n  slackcli file upload ./image.png --channel @alice --initial-comment \"latest\""
)]
struct FileUploadArgs {
    /// Local file to upload.
    file: PathBuf,
    /// Optional share target. Accepts IDs, #channel names, and @user DM targets.
    /// When given an @user ref, Slack may create or resume a DM share target.
    #[arg(long)]
    channel: Option<String>,
    /// Optional title for the uploaded file.
    #[arg(long)]
    title: Option<String>,
    /// Override the filename reported to Slack.
    #[arg(long)]
    filename: Option<String>,
    /// Optional message text to include when sharing into a channel.
    #[arg(long)]
    initial_comment: Option<String>,
    /// Optional thread timestamp to share into.
    #[arg(long)]
    thread_ts: Option<String>,
}

#[derive(Args)]
#[command(about = "User operations")]
struct UserArgs {
    #[command(subcommand)]
    command: UserCommand,
}

#[derive(Subcommand)]
enum UserCommand {
    #[command(about = "Fetch the current Slack user")]
    Me,
    #[command(about = "List users in the workspace")]
    List(UserListArgs),
    #[command(about = "Fetch a single user")]
    Get(UserGetArgs),
}

#[derive(Args)]
#[command(about = "List users in the workspace")]
struct UserListArgs {
    #[command(flatten)]
    pagination: PaginationArgs,
}

#[derive(Args)]
#[command(about = "Fetch a single user")]
struct UserGetArgs {
    /// User reference. Accepts a user ID, @name, display name, or email.
    user_id: String,
}

#[derive(Args)]
#[command(about = "Workspace operations")]
struct TeamArgs {
    #[command(subcommand)]
    command: TeamCommand,
}

#[derive(Subcommand)]
enum TeamCommand {
    #[command(about = "Fetch workspace metadata")]
    Info,
}

#[derive(Args)]
#[command(about = "Resolve refs to canonical Slack IDs without mutating Slack state")]
struct ResolveArgs {
    #[command(subcommand)]
    command: ResolveCommand,
}

#[derive(Subcommand)]
enum ResolveCommand {
    #[command(about = "Resolve a user ref to a user ID")]
    User(ResolveUserArgs),
    #[command(about = "Resolve a conversation ref to a conversation ID without opening a DM")]
    Conversation(ResolveConversationArgs),
}

#[derive(Args)]
#[command(
    about = "Resolve a user ref",
    after_help = "Examples:\n  slackcli resolve user @alice\n  slackcli resolve user alice@example.com"
)]
struct ResolveUserArgs {
    /// User reference. Accepts a user ID, @name, display name, or email.
    user: String,
}

#[derive(Args)]
#[command(
    about = "Resolve a conversation ref without mutating Slack state",
    after_help = "Examples:\n  slackcli resolve conversation '#general'\n  slackcli resolve conversation D12345\n  slackcli resolve conversation @alice"
)]
struct ResolveConversationArgs {
    /// Conversation reference. Accepts IDs, #channel names, and existing @user DMs.
    /// This command never opens a DM; use `conversation open` when creation/resume is desired.
    conversation: String,
}

#[derive(Args)]
#[command(about = "Search operations")]
struct SearchArgs {
    #[command(subcommand)]
    command: SearchCommand,
}

#[derive(Subcommand)]
enum SearchCommand {
    Messages(SearchMessagesArgs),
}

#[derive(Args)]
#[command(
    about = "Search messages",
    after_help = "Examples:\n  slackcli search messages \"deploy failed\"\n  slackcli search messages \"from:alice in:#engineering\" --count 50 --sort timestamp\n\nThe query string is passed to Slack unchanged. Use Slack's own search syntax and prefer explicit names when deterministic behavior matters."
)]
struct SearchMessagesArgs {
    /// Search query.
    query: String,
    /// Number of matches per page.
    #[arg(long, default_value_t = 20)]
    count: usize,
    /// Search result page number.
    #[arg(long, default_value_t = 1)]
    page: usize,
    /// Optional sort key.
    #[arg(long, value_enum)]
    sort: Option<SearchSort>,
    /// Optional sort direction.
    #[arg(long = "sort-dir", value_enum)]
    sort_dir: Option<SortDirection>,
    /// Ask Slack to highlight matching terms when supported.
    #[arg(long)]
    highlight: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SearchSort {
    Score,
    Timestamp,
}

impl SearchSort {
    fn as_str(self) -> &'static str {
        match self {
            Self::Score => "score",
            Self::Timestamp => "timestamp",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

#[derive(Args)]
#[command(about = "Message operations")]
struct MessageArgs {
    #[command(subcommand)]
    command: MessageCommand,
}

#[derive(Subcommand)]
enum MessageCommand {
    Send(MessageSendArgs),
    Update(MessageUpdateArgs),
    Delete(MessageDeleteArgs),
    Permalink(MessagePermalinkArgs),
}

#[derive(Args)]
#[command(
    about = "Send a message",
    after_help = "Examples:\n  slackcli message send C12345 --text \"hello\"\n  slackcli message send D12345 --text \"reply\" --thread-ts 1710000000.000100\n  slackcli message send C12345 --text \"plain link\" --no-unfurl-links --no-unfurl-media"
)]
struct MessageSendArgs {
    /// Channel or conversation reference. Accepts IDs, #channel names, and @user DM targets.
    /// When given an @user ref, Slack may create or resume a DM before sending.
    channel: String,
    /// Message text.
    #[arg(long)]
    text: String,
    /// Optional thread timestamp to reply into.
    #[arg(long)]
    thread_ts: Option<String>,
    /// Broadcast a thread reply into the parent channel.
    #[arg(long)]
    reply_broadcast: bool,
    /// Disable link unfurl previews for links in the message body.
    #[arg(long)]
    no_unfurl_links: bool,
    /// Disable media unfurl previews for links in the message body.
    #[arg(long)]
    no_unfurl_media: bool,
}

#[derive(Args)]
#[command(about = "Update a message")]
struct MessageUpdateArgs {
    /// Channel or conversation reference. Accepts IDs, #channel names, and existing @user DMs.
    /// This command does not open DMs. Use `conversation open` first when needed.
    channel: String,
    /// Message timestamp.
    ts: String,
    /// Replacement text.
    #[arg(long)]
    text: String,
}

#[derive(Args)]
#[command(about = "Delete a message")]
struct MessageDeleteArgs {
    /// Channel or conversation reference. Accepts IDs, #channel names, and existing @user DMs.
    /// This command does not open DMs. Use `conversation open` first when needed.
    channel: String,
    /// Message timestamp.
    ts: String,
}

#[derive(Args)]
#[command(about = "Fetch a message permalink")]
struct MessagePermalinkArgs {
    /// Channel or conversation reference. Accepts IDs, #channel names, and existing @user DMs.
    /// This command does not open DMs. Use `conversation open` first when needed.
    channel: String,
    /// Message timestamp.
    ts: String,
}

#[derive(Args)]
#[command(
    about = "Listen for Slack events over Socket Mode",
    after_help = "Examples:\n  slackcli listen\n  slackcli --profile work listen\n  slackcli listen --app-token \"$SLACK_APP_TOKEN\" --output json\n  slackcli listen --debug-reconnects"
)]
struct ListenArgs {
    /// App-level xapp token. Defaults to SLACK_APP_TOKEN or the saved token for the selected profile.
    #[arg(long)]
    app_token: Option<String>,
    /// Ask Slack to shorten the connection lifetime for reconnect testing.
    #[arg(long)]
    debug_reconnects: bool,
    /// Delay between reconnect attempts after a socket refresh or disconnect.
    #[arg(long, default_value_t = 1)]
    reconnect_delay_secs: u64,
}

#[derive(Args)]
#[command(about = "Call Slack API methods directly")]
struct ApiArgs {
    #[command(subcommand)]
    command: ApiCommand,
}

#[derive(Subcommand)]
enum ApiCommand {
    Call(ApiCallArgs),
}

#[derive(Args)]
#[command(
    about = "Call a Slack API method directly",
    after_help = "Examples:\n  slackcli api call conversations.list --http get --param types=public_channel\n  slackcli api call chat.postMessage --param channel=C123 --param text=hello\n  slackcli api call views.publish --body-json '{\"user_id\":\"U123\",\"view\":{...}}'"
)]
struct ApiCallArgs {
    /// Slack API method name, for example conversations.list.
    method: String,
    /// HTTP method to use.
    #[arg(long, value_enum, default_value_t = HttpMethodArg::Post)]
    http: HttpMethodArg,
    /// Repeated key=value parameters. For GET they become query params; for POST they merge into the JSON object body.
    #[arg(long = "param", value_parser = parse_key_value_pair)]
    params: Vec<(String, String)>,
    #[command(flatten)]
    body: JsonInputArgs,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum HttpMethodArg {
    Get,
    Post,
}

impl HttpMethodArg {
    fn into_reqwest(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
        }
    }
}

#[derive(Debug, Clone, Default, Args)]
struct PaginationArgs {
    /// Maximum number of results to request.
    #[arg(long)]
    limit: Option<usize>,
    /// Pagination cursor returned by Slack.
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Args)]
struct TimelineArgs {
    /// Oldest message timestamp to include.
    #[arg(long)]
    oldest: Option<String>,
    /// Latest message timestamp to include.
    #[arg(long)]
    latest: Option<String>,
    /// Include boundary timestamps when oldest/latest are provided.
    #[arg(long)]
    inclusive: bool,
}

#[derive(Debug, Clone, Default, Args)]
struct JsonInputArgs {
    /// Inline JSON request body.
    #[arg(long)]
    body_json: Option<String>,
    /// Read a JSON request body from a file.
    #[arg(long)]
    from_file: Option<PathBuf>,
    /// Read a JSON request body from stdin.
    #[arg(long)]
    stdin: bool,
}

#[tokio::main]
async fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            ErrorKind::DisplayHelp
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            | ErrorKind::DisplayVersion => {
                let _ = error.print();
                process::exit(0);
            }
            _ => {
                let output = requested_output_format_from_argv();
                let _ = output.print_error(400, "invalid_request", &error.to_string());
                process::exit(2);
            }
        },
    };

    let output = cli.output;
    if let Err(error) = run(cli).await {
        let classified = classify_error(&error);
        let _ = output.print_error(classified.status, &classified.code, &error.to_string());
        process::exit(classified.exit_code);
    }
}

fn requested_output_format_from_argv() -> OutputFormat {
    let args: Vec<OsString> = std::env::args_os().collect();
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--output" {
            if let Some(value) = iter.next()
                && let Some(format) = parse_output_format(value.to_string_lossy().as_ref())
            {
                return format;
            }
            continue;
        }

        if let Some(value) = arg.to_string_lossy().strip_prefix("--output=")
            && let Some(format) = parse_output_format(value)
        {
            return format;
        }
    }

    OutputFormat::Human
}

fn parse_output_format(value: &str) -> Option<OutputFormat> {
    match value {
        "human" => Some(OutputFormat::Human),
        "json" => Some(OutputFormat::Json),
        "yaml" => Some(OutputFormat::Yaml),
        _ => None,
    }
}

async fn run(cli: Cli) -> Result<()> {
    init_tracing(cli.verbose)?;

    let mut store = ConfigStore::load()?;
    let client = SlackClient::from_config(&store)?;

    let globals = GlobalOptions {
        profile: cli.profile.clone(),
        output: cli.output,
    };

    match cli.command {
        Commands::Auth(args) => handle_auth(args, &globals, &client, &mut store).await,
        Commands::Conversation(args) => {
            handle_conversation(args, &globals, &client, &mut store).await
        }
        Commands::Reaction(args) => handle_reaction(args, &globals, &client, &mut store).await,
        Commands::File(args) => handle_file(args, &globals, &client, &mut store).await,
        Commands::User(args) => handle_user(args, &globals, &client, &mut store).await,
        Commands::Team(args) => handle_team(args, &globals, &client, &mut store).await,
        Commands::Resolve(args) => handle_resolve(args, &globals, &client, &mut store).await,
        Commands::Search(args) => handle_search(args, &globals, &client, &mut store).await,
        Commands::Message(args) => handle_message(args, &globals, &client, &mut store).await,
        Commands::Listen(args) => handle_listen(args, &globals, &client, &store).await,
        Commands::Api(args) => handle_api(args, &globals, &client, &mut store).await,
    }
}

fn init_tracing(verbose: u8) -> Result<()> {
    let filter = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_target(false)
        .try_init()
        .map_err(|error| anyhow!("failed to initialize logging: {error}"))
}

async fn handle_auth(
    args: AuthArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    match args.command {
        AuthCommand::Login(args) => handle_auth_login(args, globals, client, store).await,
        AuthCommand::AppLogin(args) => handle_auth_app_login(args, globals, client, store).await,
        AuthCommand::List => {
            let profiles = store
                .profiles()
                .iter()
                .map(|(name, meta)| {
                    json!({
                        "name": name,
                        "is_active": store.active_profile() == Some(name.as_str()),
                        "has_secret": store.has_persisted_secret(name),
                        "has_app_token": store.has_persisted_app_secret(name),
                        "meta": meta,
                    })
                })
                .collect::<Vec<_>>();

            globals.output.print_success(&json!({
                "object": "list",
                "active_profile": store.active_profile(),
                "profiles": profiles,
            }))
        }
        AuthCommand::Doctor => {
            let session = resolve_session(store, globals)?;
            let auth = client.auth_test(&session).await?;
            let profile = session
                .profile_name
                .as_deref()
                .and_then(|name| store.get_profile(name));
            let app_session = store.resolve_app_session(globals.profile.as_deref()).ok();

            globals.output.print_success(&json!({
                "object": "auth_state",
                "active_profile": store.active_profile(),
                "selected_profile": session.profile_name,
                "session_source": session.source,
                "config_dir": store.paths().config_dir,
                "config_file": store.paths().config_file,
                "credentials_file": store.paths().credentials_file,
                "stored_profile": profile,
                "has_app_token": app_session.is_some(),
                "app_token_profile": app_session.as_ref().and_then(|session| session.profile_name.as_deref()),
                "app_token_source": app_session.as_ref().map(|session| session.source),
                "auth_test": auth,
            }))
        }
        AuthCommand::Use(args) => {
            store.set_active_profile(&args.profile_name)?;
            globals.output.print_success(&json!({
                "object": "auth_profile",
                "active_profile": args.profile_name,
            }))
        }
        AuthCommand::Whoami => {
            let session = resolve_session(store, globals)?;
            let auth = client.auth_test(&session).await?;
            globals.output.print_success(&json!({
                "object": "auth_identity",
                "profile": session.profile_name,
                "session_source": session.source,
                "auth_test": auth,
            }))
        }
        AuthCommand::Logout(args) => {
            let name = match args.profile_name {
                Some(name) => name,
                None => store
                    .active_profile()
                    .map(str::to_string)
                    .ok_or_else(|| anyhow!("no active profile configured"))?,
            };
            store.remove_profile(&name)?;
            globals.output.print_success(&json!({
                "object": "auth_profile",
                "removed_profile": name,
                "active_profile": store.active_profile(),
            }))
        }
    }
}

async fn handle_auth_login(
    args: AuthLoginArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let token = match args.token {
        Some(token) => token.trim().to_string(),
        None => prompt_for_token()?,
    };

    if token.is_empty() {
        bail!("provide a Slack token")
    }

    let auth = client.auth_test_for_token(&token).await?;
    let meta = profile_meta_from_auth(&token, &auth);
    let profile_name = args
        .profile_name
        .unwrap_or_else(|| derive_profile_name(&meta, store.active_profile()));
    let secret = StoredSecret::Token { token };
    let app_token = resolve_auth_login_app_token(args.app_token)?;

    store.put_profile(profile_name.clone(), meta.clone(), &secret)?;
    if let Some(app_token) = app_token {
        client.apps_connections_open_for_token(&app_token).await?;
        let app_secret = StoredAppSecret::Token { token: app_token };
        store.put_app_secret(&profile_name, &app_secret)?;
    }
    globals.output.print_success(&json!({
        "object": "auth_profile",
        "profile_name": profile_name,
        "active_profile": store.active_profile(),
        "has_app_token": store.has_persisted_app_secret(&profile_name),
        "meta": meta,
        "auth_test": auth,
    }))
}

async fn handle_auth_app_login(
    args: AuthAppLoginArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let token = match args.token {
        Some(token) => token.trim().to_string(),
        None => prompt_for_app_token()?,
    };

    if token.is_empty() {
        bail!("provide a Slack app token")
    }

    let profile_name = args
        .profile_name
        .or_else(|| globals.profile.clone())
        .or_else(|| store.active_profile().map(str::to_string))
        .ok_or_else(|| anyhow!("no target profile configured; run `slackcli auth login` first or pass --profile-name"))?;

    client.apps_connections_open_for_token(&token).await?;

    let secret = StoredAppSecret::Token { token };
    store.put_app_secret(&profile_name, &secret)?;
    globals.output.print_success(&json!({
        "object": "app_auth_profile",
        "profile_name": profile_name,
        "active_profile": store.active_profile(),
        "session_source": "persisted_profile",
        "has_app_token": true,
    }))
}

async fn handle_conversation(
    args: ConversationArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let session = resolve_session(store, globals)?;
    let mut resolver = SlackResolver::new();

    let response = match args.command {
        ConversationCommand::List(args) => {
            let mut query = vec![("types".into(), args.types)];
            if args.exclude_archived {
                query.push(("exclude_archived".into(), "true".into()));
            }
            if let Some(user) = args.user {
                let user_id = resolver.resolve_user_id(client, &session, &user).await?;
                query.push(("user".into(), user_id));
            }
            push_limit_and_cursor(&mut query, &args.pagination);
            client.users_conversations(&session, query).await?
        }
        ConversationCommand::Open(args) => {
            let current_user_id = resolver.auth_user_id(client, &session).await?;
            let mut user_ids = Vec::new();

            for user in args.users {
                let user_id = resolver.resolve_user_id(client, &session, &user).await?;
                if user_id != current_user_id && !user_ids.contains(&user_id) {
                    user_ids.push(user_id);
                }
            }

            if user_ids.is_empty() {
                bail!("provide at least one user other than yourself")
            }

            client
                .conversations_open(&session, &user_ids, args.return_im, args.prevent_creation)
                .await?
        }
        ConversationCommand::Create(args) => {
            let name = normalize_conversation_name(&args.name)?;
            client
                .conversations_create(&session, &name, args.private)
                .await?
        }
        ConversationCommand::Archive(args) => {
            let channel_id = resolver
                .resolve_conversation_id(client, &session, &args.channel)
                .await?;
            client.conversations_archive(&session, &channel_id).await?
        }
        ConversationCommand::Info(args) => {
            let channel_id = resolver
                .resolve_conversation_id(client, &session, &args.channel)
                .await?;
            client
                .conversations_info(&session, &channel_id, args.include_num_members)
                .await?
        }
        ConversationCommand::History(args) => {
            let channel_id = resolver
                .resolve_conversation_id(client, &session, &args.channel)
                .await?;
            let mut query = vec![("channel".into(), channel_id)];
            push_limit_and_cursor(&mut query, &args.pagination);
            push_timeline(&mut query, &args.window);
            client.conversations_history(&session, query).await?
        }
        ConversationCommand::Replies(args) => {
            let channel_id = resolver
                .resolve_conversation_id(client, &session, &args.channel)
                .await?;
            let mut query = vec![
                ("channel".into(), channel_id),
                ("ts".into(), args.thread_ts),
            ];
            push_limit_and_cursor(&mut query, &args.pagination);
            push_timeline(&mut query, &args.window);
            client.conversations_replies(&session, query).await?
        }
        ConversationCommand::Members(args) => {
            let channel_id = resolver
                .resolve_conversation_id(client, &session, &args.channel)
                .await?;
            client
                .conversations_members(
                    &session,
                    &channel_id,
                    args.pagination.cursor,
                    args.pagination.limit,
                )
                .await?
        }
    };

    globals.output.print_success(&response)
}

async fn handle_reaction(
    args: ReactionArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let session = resolve_session(store, globals)?;
    let mut resolver = SlackResolver::new();

    let response = match args.command {
        ReactionCommand::Add(args) => {
            let channel_id = resolver
                .resolve_conversation_id(client, &session, &args.channel)
                .await?;
            let name = normalize_reaction_name(&args.name)?;
            client
                .reactions_add(&session, &channel_id, &args.ts, &name)
                .await?
        }
        ReactionCommand::Remove(args) => {
            let channel_id = resolver
                .resolve_conversation_id(client, &session, &args.channel)
                .await?;
            let name = normalize_reaction_name(&args.name)?;
            client
                .reactions_remove(&session, &channel_id, &args.ts, &name)
                .await?
        }
        ReactionCommand::List(args) => {
            let mut query = vec![
                ("count".into(), args.count.to_string()),
                ("page".into(), args.page.to_string()),
            ];
            if args.full {
                query.push(("full".into(), "true".into()));
            }
            if let Some(user) = args.user {
                let user_id = resolver.resolve_user_id(client, &session, &user).await?;
                query.push(("user".into(), user_id));
            }
            client.reactions_list(&session, query).await?
        }
    };

    globals.output.print_success(&response)
}

async fn handle_file(
    args: FileArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let session = resolve_session(store, globals)?;
    let mut resolver = SlackResolver::new();

    let response = match args.command {
        FileCommand::List(args) => {
            let mut query = vec![
                ("count".into(), args.count.to_string()),
                ("page".into(), args.page.to_string()),
            ];
            if let Some(user) = args.user {
                let user_id = resolver.resolve_user_id(client, &session, &user).await?;
                query.push(("user".into(), user_id));
            }
            if let Some(channel) = args.channel {
                let channel_id = resolver
                    .resolve_conversation_id(client, &session, &channel)
                    .await?;
                query.push(("channel".into(), channel_id));
            }
            if let Some(types) = args.types {
                query.push(("types".into(), types));
            }
            if let Some(ts_from) = args.ts_from {
                query.push(("ts_from".into(), ts_from));
            }
            if let Some(ts_to) = args.ts_to {
                query.push(("ts_to".into(), ts_to));
            }
            if args.show_hidden {
                query.push(("show_files_hidden_by_limit".into(), "true".into()));
            }
            client.files_list(&session, query).await?
        }
        FileCommand::Get(args) => client.files_info(&session, &args.file_id).await?,
        FileCommand::Delete(args) => client.files_delete(&session, &args.file_id).await?,
        FileCommand::Upload(args) => {
            if args.channel.is_none()
                && (args.initial_comment.is_some() || args.thread_ts.is_some())
            {
                bail!("--channel is required when using --initial-comment or --thread-ts")
            }

            let file_bytes = read_file_bytes(&args.file)?;
            let filename = resolved_upload_filename(&args.file, args.filename.as_deref())?;
            let channel_id = match args.channel {
                Some(channel) => Some(
                    resolver
                        .resolve_conversation_id_for_write(client, &session, &channel)
                        .await?,
                ),
                None => None,
            };

            let ticket = client
                .files_get_upload_url_external(&session, &filename, file_bytes.len() as u64)
                .await?;
            client
                .upload_external_file(&ticket.upload_url, file_bytes)
                .await?;
            client
                .files_complete_upload_external(
                    &session,
                    &ticket.file_id,
                    args.title.as_deref(),
                    channel_id.as_deref(),
                    args.initial_comment.as_deref(),
                    args.thread_ts.as_deref(),
                )
                .await?
        }
    };

    globals.output.print_success(&response)
}

async fn handle_user(
    args: UserArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let session = resolve_session(store, globals)?;
    let mut resolver = SlackResolver::new();

    let response = match args.command {
        UserCommand::Me => {
            let auth = client.auth_test(&session).await?;
            let user_id = auth
                .user_id
                .clone()
                .ok_or_else(|| anyhow!("Slack auth response did not include a user_id"))?;
            let user = client.users_info(&session, &user_id).await?;
            json!({
                "object": "slack_identity",
                "profile": session.profile_name,
                "auth_test": auth,
                "user": user,
            })
        }
        UserCommand::List(args) => {
            client
                .users_list(&session, args.pagination.cursor, args.pagination.limit)
                .await?
        }
        UserCommand::Get(args) => {
            let user_id = resolver
                .resolve_user_id(client, &session, &args.user_id)
                .await?;
            client.users_info(&session, &user_id).await?
        }
    };

    globals.output.print_success(&response)
}

async fn handle_team(
    args: TeamArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let session = resolve_session(store, globals)?;
    let response = match args.command {
        TeamCommand::Info => client.team_info(&session).await?,
    };
    globals.output.print_success(&response)
}

async fn handle_resolve(
    args: ResolveArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let session = resolve_session(store, globals)?;
    let mut resolver = SlackResolver::new();

    let response = match args.command {
        ResolveCommand::User(args) => {
            let user_id = resolver
                .resolve_user_id(client, &session, &args.user)
                .await?;
            let user = client.users_info(&session, &user_id).await?;
            json!({
                "object": "resolved_user",
                "input": args.user,
                "user_id": user_id,
                "response": user,
            })
        }
        ResolveCommand::Conversation(args) => {
            let conversation_id = resolver
                .resolve_conversation_id(client, &session, &args.conversation)
                .await?;
            let conversation = client
                .conversations_info(&session, &conversation_id, false)
                .await?;
            json!({
                "object": "resolved_conversation",
                "input": args.conversation,
                "conversation_id": conversation_id,
                "response": conversation,
            })
        }
    };

    globals.output.print_success(&response)
}

async fn handle_search(
    args: SearchArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let session = resolve_session(store, globals)?;
    let response = match args.command {
        SearchCommand::Messages(args) => {
            let mut query = vec![
                ("query".into(), args.query),
                ("count".into(), args.count.to_string()),
                ("page".into(), args.page.to_string()),
            ];
            if let Some(sort) = args.sort {
                query.push(("sort".into(), sort.as_str().into()));
            }
            if let Some(sort_dir) = args.sort_dir {
                query.push(("sort_dir".into(), sort_dir.as_str().into()));
            }
            if args.highlight {
                query.push(("highlight".into(), "true".into()));
            }
            client.search_messages(&session, query).await?
        }
    };
    globals.output.print_success(&response)
}

async fn handle_message(
    args: MessageArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let session = resolve_session(store, globals)?;
    let mut resolver = SlackResolver::new();
    let response = match args.command {
        MessageCommand::Send(args) => {
            let channel_id = resolver
                .resolve_conversation_id_for_write(client, &session, &args.channel)
                .await?;
            let mut body = json!({
                "channel": channel_id,
                "text": args.text,
                "unfurl_links": !args.no_unfurl_links,
                "unfurl_media": !args.no_unfurl_media,
            });

            if let Some(thread_ts) = args.thread_ts
                && let Some(object) = body.as_object_mut()
            {
                object.insert("thread_ts".into(), Value::String(thread_ts));
            }
            if args.reply_broadcast
                && let Some(object) = body.as_object_mut()
            {
                object.insert("reply_broadcast".into(), Value::Bool(true));
            }

            client.chat_post_message(&session, body).await?
        }
        MessageCommand::Update(args) => {
            let channel_id = resolver
                .resolve_conversation_id(client, &session, &args.channel)
                .await?;
            client
                .chat_update(
                    &session,
                    json!({
                        "channel": channel_id,
                        "ts": args.ts,
                        "text": args.text,
                    }),
                )
                .await?
        }
        MessageCommand::Delete(args) => {
            let channel_id = resolver
                .resolve_conversation_id(client, &session, &args.channel)
                .await?;
            client.chat_delete(&session, &channel_id, &args.ts).await?
        }
        MessageCommand::Permalink(args) => {
            let channel_id = resolver
                .resolve_conversation_id(client, &session, &args.channel)
                .await?;
            client
                .chat_get_permalink(&session, &channel_id, &args.ts)
                .await?
        }
    };

    globals.output.print_success(&response)
}

async fn handle_listen(
    args: ListenArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &ConfigStore,
) -> Result<()> {
    let session = resolve_app_session(store, globals, args.app_token)?;

    let options = ListenOptions {
        output: globals.output,
        debug_reconnects: args.debug_reconnects,
        reconnect_delay: Duration::from_secs(args.reconnect_delay_secs),
    };

    socket_mode::listen(client, session.secret.app_token(), options).await
}

async fn handle_api(
    args: ApiArgs,
    globals: &GlobalOptions,
    client: &SlackClient,
    store: &mut ConfigStore,
) -> Result<()> {
    let session = resolve_session(store, globals)?;
    let response = match args.command {
        ApiCommand::Call(args) => {
            let http_method = args.http.into_reqwest();
            let body = read_optional_json_input(&args.body)?;

            if http_method == Method::GET {
                if body.is_some() {
                    bail!("GET requests do not support --body-json, --from-file, or --stdin")
                }

                client
                    .api_call(
                        &session,
                        http_method,
                        &args.method,
                        (!args.params.is_empty()).then_some(args.params),
                        None,
                    )
                    .await?
            } else {
                let body = merge_object_body(body, &args.params)?;
                client
                    .api_call(&session, http_method, &args.method, None, body)
                    .await?
            }
        }
    };

    globals.output.print_success(&response)
}

fn resolve_session(store: &ConfigStore, globals: &GlobalOptions) -> Result<RuntimeSession> {
    store.resolve_session(globals.profile.as_deref())
}

fn resolve_app_session(
    store: &ConfigStore,
    globals: &GlobalOptions,
    explicit_token: Option<String>,
) -> Result<RuntimeAppSession> {
    match explicit_token {
        Some(token) => {
            let token = token.trim().to_string();
            if token.is_empty() {
                bail!("provide a Slack app token")
            }
            Ok(RuntimeAppSession {
                profile_name: globals
                    .profile
                    .clone()
                    .or_else(|| store.active_profile().map(str::to_string)),
                secret: StoredAppSecret::Token { token },
                source: SessionSource::Environment,
            })
        }
        None => store.resolve_app_session(globals.profile.as_deref()),
    }
}

fn push_limit_and_cursor(query: &mut Vec<(String, String)>, pagination: &PaginationArgs) {
    if let Some(limit) = pagination.limit {
        query.push(("limit".into(), limit.to_string()));
    }
    if let Some(cursor) = pagination.cursor.clone() {
        query.push(("cursor".into(), cursor));
    }
}

fn push_timeline(query: &mut Vec<(String, String)>, window: &TimelineArgs) {
    if let Some(oldest) = window.oldest.clone() {
        query.push(("oldest".into(), oldest));
    }
    if let Some(latest) = window.latest.clone() {
        query.push(("latest".into(), latest));
    }
    if window.inclusive {
        query.push(("inclusive".into(), "true".into()));
    }
}

fn normalize_reaction_name(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_matches(':').trim().to_string();
    if trimmed.is_empty() {
        bail!("provide a reaction name")
    }
    Ok(trimmed)
}

fn normalize_conversation_name(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_start_matches('#').trim();
    if trimmed.is_empty() {
        bail!("provide a channel name")
    }
    if trimmed.len() > 80 {
        bail!("channel names must be 80 characters or less")
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        bail!("channel names may only contain lowercase letters, numbers, hyphens, and underscores")
    }
    Ok(trimmed.to_string())
}

fn read_file_bytes(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("failed to read file {}", path.display()))
}

fn resolved_upload_filename(path: &Path, override_name: Option<&str>) -> Result<String> {
    if let Some(override_name) = override_name {
        let value = override_name.trim();
        if value.is_empty() {
            bail!("provide a non-empty --filename value")
        }
        return Ok(value.to_string());
    }

    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("failed to determine a filename for {}", path.display()))
}

fn prompt_for_token() -> Result<String> {
    let token =
        prompt_password("Paste your Slack token: ").context("failed to read token input")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("provide a Slack token")
    }
    Ok(token)
}

fn resolve_auth_login_app_token(explicit: Option<String>) -> Result<Option<String>> {
    if let Some(token) = explicit {
        let token = token.trim().to_string();
        if token.is_empty() {
            bail!("provide a Slack app token")
        }
        return Ok(Some(token));
    }

    if let Ok(token) =
        std::env::var(SLACK_APP_TOKEN_ENV).or_else(|_| std::env::var(SLACKCLI_APP_TOKEN_ENV))
    {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(Some(token));
        }
    }

    if io::stdin().is_terminal() {
        return prompt_for_app_token().map(Some);
    }

    Ok(None)
}

fn prompt_for_app_token() -> Result<String> {
    let token =
        prompt_password("Paste your Slack app token: ").context("failed to read token input")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("provide a Slack app token")
    }
    Ok(token)
}

fn profile_meta_from_auth(token: &str, auth: &SlackAuthTest) -> ProfileMeta {
    ProfileMeta {
        auth_type: infer_auth_type(token, auth),
        team_id: auth.team_id.clone(),
        team_name: auth.team.clone(),
        enterprise_id: auth.enterprise_id.clone(),
        url: auth.url.clone(),
        user_id: auth.user_id.clone(),
        user_name: auth.user.clone(),
        bot_id: auth.bot_id.clone(),
    }
}

fn infer_auth_type(token: &str, auth: &SlackAuthTest) -> AuthType {
    if auth.bot_id.is_some() || token.starts_with("xoxb-") {
        return AuthType::Bot;
    }

    if token.starts_with("xoxp-")
        || token.starts_with("xoxe-")
        || token.starts_with("xoxc-")
        || auth.user_id.is_some()
    {
        return AuthType::User;
    }

    AuthType::Unknown
}

fn derive_profile_name(meta: &ProfileMeta, active_profile: Option<&str>) -> String {
    let mut pieces = Vec::new();
    if let Some(team_name) = meta.team_name.as_deref() {
        pieces.push(team_name);
    }
    if let Some(user_name) = meta.user_name.as_deref() {
        pieces.push(user_name);
    } else if let Some(bot_id) = meta.bot_id.as_deref() {
        pieces.push(bot_id);
    }
    if pieces.is_empty()
        && let Some(active) = active_profile
    {
        pieces.push(active);
    }

    let slug = slugify_profile_name(&pieces.join(" "));
    if slug.is_empty() {
        "slack".into()
    } else {
        slug
    }
}

fn parse_key_value_pair(raw: &str) -> Result<(String, String), String> {
    let Some((key, value)) = raw.split_once('=') else {
        return Err("expected KEY=VALUE".into());
    };

    let key = key.trim();
    if key.is_empty() {
        return Err("parameter key must not be empty".into());
    }

    Ok((key.to_string(), value.to_string()))
}

fn read_optional_json_input(args: &JsonInputArgs) -> Result<Option<Value>> {
    let sources = usize::from(args.body_json.is_some())
        + usize::from(args.from_file.is_some())
        + usize::from(args.stdin);

    if sources > 1 {
        bail!("use only one of --body-json, --from-file, or --stdin")
    }

    match (&args.body_json, &args.from_file, args.stdin) {
        (Some(raw), None, false) => Ok(Some(parse_json_value(raw)?)),
        (None, Some(path), false) => {
            let raw = read_text_file(path)?;
            Ok(Some(parse_json_value(&raw)?))
        }
        (None, None, false) => Ok(None),
        (None, None, true) => {
            let raw = read_stdin_to_string()?;
            Ok(Some(parse_json_value(&raw)?))
        }
        _ => unreachable!("multiple body sources are rejected above"),
    }
}

fn parse_json_value(raw: &str) -> Result<Value> {
    serde_json::from_str(raw.trim()).context("failed to parse JSON request body")
}

fn read_stdin_to_string() -> Result<String> {
    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .context("failed to read stdin")?;
    Ok(buffer)
}

struct ClassifiedError {
    status: u16,
    code: String,
    exit_code: i32,
}

fn classify_error(error: &anyhow::Error) -> ClassifiedError {
    if let Some(slack_error) = error.downcast_ref::<SlackApiError>() {
        return ClassifiedError {
            status: slack_error.status,
            code: slack_error.code.clone(),
            exit_code: exit_code_for_status(slack_error.status),
        };
    }

    let message = error.to_string().to_ascii_lowercase();

    if message.contains("rate limit") || message.contains("retry after") {
        return ClassifiedError {
            status: 429,
            code: "rate_limited".into(),
            exit_code: 7,
        };
    }

    if message.contains("no active profile")
        || message.contains("provide a slack token")
        || message.contains("provide a slack app token")
        || message.contains("failed to read token input")
    {
        return ClassifiedError {
            status: 401,
            code: "unauthorized".into(),
            exit_code: 3,
        };
    }

    if message.contains("failed to parse json request body")
        || message.contains("use only one of --body-json, --from-file, or --stdin")
        || message.contains("expected key=value")
        || message.contains("get requests do not support")
        || message.contains("request body must be a json object")
        || message.contains("provide a reaction name")
        || message.contains("provide at least one user other than yourself")
        || message.contains("--channel is required when using")
        || message.contains("provide a non-empty --filename value")
        || message.contains("multiple slack users matched")
        || message.contains("multiple slack conversations matched")
    {
        return ClassifiedError {
            status: 400,
            code: "validation_error".into(),
            exit_code: 6,
        };
    }

    if message.contains("could not resolve slack user")
        || message.contains("could not resolve slack conversation")
        || message.contains("failed to determine a filename")
        || message.contains("is not a valid dm target")
    {
        return ClassifiedError {
            status: 404,
            code: "object_not_found".into(),
            exit_code: 5,
        };
    }

    if message.contains("config")
        || message.contains("slack_api_base_url")
        || message.contains("credentials file")
        || message.contains("failed to read file")
        || message.contains("failed to read stdin")
    {
        return ClassifiedError {
            status: 400,
            code: "invalid_request".into(),
            exit_code: 4,
        };
    }

    ClassifiedError {
        status: 500,
        code: "internal_error".into(),
        exit_code: 1,
    }
}

fn exit_code_for_status(status: u16) -> i32 {
    match status {
        401 => 3,
        403 => 9,
        404 => 5,
        429 => 7,
        400..=499 => 6,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parses_key_value_pairs() {
        assert_eq!(
            parse_key_value_pair("channel=C123").unwrap(),
            ("channel".into(), "C123".into())
        );
    }

    #[test]
    fn rejects_invalid_key_value_pairs() {
        assert!(parse_key_value_pair("channel").is_err());
    }

    #[test]
    fn derives_profile_name_from_team_and_user() {
        let meta = ProfileMeta {
            auth_type: AuthType::User,
            team_id: None,
            team_name: Some("My Team".into()),
            enterprise_id: None,
            url: None,
            user_id: None,
            user_name: Some("Alice".into()),
            bot_id: None,
        };

        assert_eq!(derive_profile_name(&meta, None), "my-team-alice");
    }

    #[test]
    fn parses_inline_json_input() -> Result<()> {
        let args = JsonInputArgs {
            body_json: Some(r#"{"hello":"world"}"#.into()),
            from_file: None,
            stdin: false,
        };

        let value = read_optional_json_input(&args)?.context("missing JSON body")?;
        assert_eq!(value.get("hello").and_then(Value::as_str), Some("world"));
        Ok(())
    }

    #[test]
    fn normalizes_reaction_names() {
        assert_eq!(normalize_reaction_name(":thumbsup:").unwrap(), "thumbsup");
        assert!(normalize_reaction_name("::").is_err());
    }

    #[test]
    fn normalizes_conversation_names() {
        assert_eq!(
            normalize_conversation_name("#slackcli-smoke").unwrap(),
            "slackcli-smoke"
        );
        assert!(normalize_conversation_name("SlackCli").is_err());
        assert!(normalize_conversation_name("").is_err());
    }

    #[test]
    fn pushes_limit_and_cursor_values() {
        let mut query = Vec::new();
        let pagination = PaginationArgs {
            limit: Some(25),
            cursor: Some("abc123".into()),
        };

        push_limit_and_cursor(&mut query, &pagination);

        assert_eq!(
            query,
            vec![
                ("limit".into(), "25".into()),
                ("cursor".into(), "abc123".into()),
            ]
        );
    }

    #[test]
    fn pushes_timeline_values() {
        let mut query = Vec::new();
        let timeline = TimelineArgs {
            oldest: Some("1.23".into()),
            latest: Some("4.56".into()),
            inclusive: true,
        };

        push_timeline(&mut query, &timeline);

        assert_eq!(
            query,
            vec![
                ("oldest".into(), "1.23".into()),
                ("latest".into(), "4.56".into()),
                ("inclusive".into(), "true".into()),
            ]
        );
    }

    #[test]
    fn rejects_multiple_json_input_sources() {
        let args = JsonInputArgs {
            body_json: Some("{}".into()),
            from_file: Some(PathBuf::from("body.json")),
            stdin: false,
        };

        let error = read_optional_json_input(&args).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("use only one of --body-json, --from-file, or --stdin")
        );
    }

    #[test]
    fn reads_json_from_file() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        writeln!(file, "{{\"hello\":\"file\"}}")?;
        let args = JsonInputArgs {
            body_json: None,
            from_file: Some(file.path().to_path_buf()),
            stdin: false,
        };

        let value = read_optional_json_input(&args)?.context("missing JSON body")?;
        assert_eq!(value.get("hello").and_then(Value::as_str), Some("file"));
        Ok(())
    }

    #[test]
    fn classifies_validation_errors() {
        let error = anyhow!("provide a reaction name");
        let classified = classify_error(&error);
        assert_eq!(classified.status, 400);
        assert_eq!(classified.code, "validation_error");
        assert_eq!(classified.exit_code, 6);
    }

    #[test]
    fn classifies_not_found_errors() {
        let error = anyhow!("could not resolve slack conversation");
        let classified = classify_error(&error);
        assert_eq!(classified.status, 404);
        assert_eq!(classified.code, "object_not_found");
        assert_eq!(classified.exit_code, 5);
    }

    #[test]
    fn classifies_unauthorized_errors() {
        let error = anyhow!("no active profile configured");
        let classified = classify_error(&error);
        assert_eq!(classified.status, 401);
        assert_eq!(classified.code, "unauthorized");
        assert_eq!(classified.exit_code, 3);
    }

    #[test]
    fn classifies_slack_api_errors_by_status() {
        let error = anyhow!(SlackApiError {
            status: 403,
            code: "missing_scope".into(),
            message: "forbidden".into(),
        });
        let classified = classify_error(&error);
        assert_eq!(classified.status, 403);
        assert_eq!(classified.code, "missing_scope");
        assert_eq!(classified.exit_code, 9);
    }

    #[test]
    fn returns_expected_exit_code_for_status() {
        assert_eq!(exit_code_for_status(401), 3);
        assert_eq!(exit_code_for_status(403), 9);
        assert_eq!(exit_code_for_status(404), 5);
        assert_eq!(exit_code_for_status(429), 7);
        assert_eq!(exit_code_for_status(500), 1);
    }

    #[test]
    fn top_level_commands_include_resolve() {
        let names = Cli::command()
            .get_subcommands()
            .map(|command| command.get_name().to_string())
            .collect::<Vec<_>>();
        assert!(names.iter().any(|name| name == "resolve"), "{names:?}");
    }
}
