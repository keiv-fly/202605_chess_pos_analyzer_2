use std::{
    borrow::Cow,
    env,
    io::{self, Write},
    path::PathBuf,
};

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use rmcp::{
    model::{CallToolRequestParam, CallToolResult, Tool},
    service::ServiceExt,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::process::Command;
use tracing_subscriber::EnvFilter;

const OPENROUTER_CHAT_COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEFAULT_MODEL: &str = "openrouter/auto";

#[derive(Debug, Parser)]
#[command(
    name = "chess-agent",
    about = "Analyze chess positions with an OpenRouter agent using the local MCP server"
)]
struct Args {
    /// OpenRouter model slug. Defaults to OpenRouter's auto router.
    #[arg(long, env = "OPENROUTER_MODEL", default_value = DEFAULT_MODEL)]
    model: String,

    /// OpenRouter chat completions endpoint.
    #[arg(
        long,
        env = "OPENROUTER_BASE_URL",
        default_value = OPENROUTER_CHAT_COMPLETIONS_URL
    )]
    openrouter_url: String,

    /// Command used to start the MCP server. Defaults to a sibling binary, then cargo run.
    #[arg(long, env = "CHESS_MCP_SERVER_COMMAND")]
    server_command: Option<PathBuf>,

    /// Additional arguments passed to the MCP server command.
    #[arg(long = "server-arg")]
    server_args: Vec<String>,

    /// Maximum consecutive model/tool-call rounds for one user prompt.
    #[arg(long, default_value_t = 8)]
    max_tool_rounds: usize,

    /// Start an interactive dialog. If a prompt is provided, send it as the first turn.
    #[arg(long)]
    chat: bool,

    /// Optional one-shot prompt. If omitted, the program starts an interactive loop.
    prompt: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Value],
    tools: &'a [Value],
    tool_choice: &'a str,
    parallel_tool_calls: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    model: Option<String>,
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: AssistantMessage,
}

#[derive(Debug, Deserialize)]
struct AssistantMessage {
    role: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: FunctionCall,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct FunctionCall {
    name: String,
    arguments: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let args = Args::parse();
    let api_key = env::var("OPENROUTER_API_KEY")
        .context("OPENROUTER_API_KEY must be set in .env or the environment")?;

    let client = reqwest::Client::new();
    let mcp = start_mcp_client(&args).await?;
    let tools = mcp.list_tools(Default::default()).await?;
    let openrouter_tools = tools
        .tools
        .iter()
        .map(openrouter_tool_from_mcp)
        .collect::<Vec<_>>();

    eprintln!(
        "Connected to MCP server with {} tools. Model: {}",
        openrouter_tools.len(),
        args.model
    );

    let mut messages = vec![json!({
        "role": "system",
        "content": system_prompt()
    })];

    let initial_prompt = if args.prompt.is_empty() {
        None
    } else {
        Some(args.prompt.join(" "))
    };

    if args.chat || initial_prompt.is_none() {
        interactive_loop(
            args,
            api_key,
            client,
            mcp,
            openrouter_tools,
            &mut messages,
            initial_prompt,
        )
        .await
    } else {
        let prompt = initial_prompt.expect("checked above");
        messages.push(json!({ "role": "user", "content": prompt }));
        let answer = run_agent_turn(
            &args,
            &api_key,
            &client,
            &mcp,
            &openrouter_tools,
            &mut messages,
        )
        .await?;
        println!("{answer}");
        Ok(())
    }
}

async fn interactive_loop(
    args: Args,
    api_key: String,
    client: reqwest::Client,
    mcp: rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tools: Vec<Value>,
    messages: &mut Vec<Value>,
    initial_prompt: Option<String>,
) -> Result<()> {
    eprintln!("Dialog mode. Type /help for commands, or /exit to quit.");

    if let Some(prompt) = initial_prompt {
        submit_dialog_turn(&args, &api_key, &client, &mcp, &tools, messages, &prompt).await;
    }

    let mut line = String::new();
    loop {
        print!("chess-agent> ");
        io::stdout().flush()?;
        line.clear();
        let read = io::stdin().read_line(&mut line)?;
        if read == 0 {
            break;
        }
        let prompt = line.trim();
        if prompt.eq_ignore_ascii_case("exit")
            || prompt.eq_ignore_ascii_case("quit")
            || prompt.eq_ignore_ascii_case("/exit")
            || prompt.eq_ignore_ascii_case("/quit")
        {
            break;
        }
        if prompt.eq_ignore_ascii_case("/help") || prompt.eq_ignore_ascii_case("help") {
            print_dialog_help();
            continue;
        }
        if prompt.eq_ignore_ascii_case("/clear") {
            reset_conversation(messages);
            eprintln!("Conversation context cleared.");
            continue;
        }
        if prompt.is_empty() {
            continue;
        }

        submit_dialog_turn(&args, &api_key, &client, &mcp, &tools, messages, prompt).await;
    }
    Ok(())
}

async fn submit_dialog_turn(
    args: &Args,
    api_key: &str,
    client: &reqwest::Client,
    mcp: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tools: &[Value],
    messages: &mut Vec<Value>,
    prompt: &str,
) {
    let checkpoint = messages.len();
    messages.push(json!({ "role": "user", "content": prompt }));
    match run_agent_turn(args, api_key, client, mcp, tools, messages).await {
        Ok(answer) => println!("{answer}"),
        Err(err) => {
            messages.truncate(checkpoint);
            eprintln!("Turn failed: {err:#}");
        }
    }
}

fn reset_conversation(messages: &mut Vec<Value>) {
    messages.clear();
    messages.push(json!({
        "role": "system",
        "content": system_prompt()
    }));
}

fn print_dialog_help() {
    eprintln!(
        "Commands:\n  /help   Show this help\n  /clear  Clear conversation context\n  /exit   Quit"
    );
}

async fn run_agent_turn(
    args: &Args,
    api_key: &str,
    client: &reqwest::Client,
    mcp: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tools: &[Value],
    messages: &mut Vec<Value>,
) -> Result<String> {
    for _ in 0..args.max_tool_rounds {
        let response = chat_completion(args, api_key, client, tools, messages).await?;
        let selected_model = response.model.clone();
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("OpenRouter returned no choices"))?;
        let assistant = choice.message;

        let assistant_message = assistant_message_value(&assistant);
        let tool_calls = assistant.tool_calls.clone();
        messages.push(assistant_message);

        if tool_calls.is_empty() {
            let mut content = assistant.content.unwrap_or_default();
            if let Some(model) = selected_model {
                if args.model == DEFAULT_MODEL {
                    content.push_str(&format!("\n\nModel used: {model}"));
                }
            }
            return Ok(content);
        }

        for call in tool_calls {
            let tool_result = call_mcp_tool(mcp, &call).await?;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": tool_result,
            }));
        }
    }

    bail!(
        "agent exceeded {} tool-call rounds without a final answer",
        args.max_tool_rounds
    )
}

async fn chat_completion(
    args: &Args,
    api_key: &str,
    client: &reqwest::Client,
    tools: &[Value],
    messages: &[Value],
) -> Result<ChatResponse> {
    let request = ChatRequest {
        model: &args.model,
        messages,
        tools,
        tool_choice: "auto",
        parallel_tool_calls: false,
    };

    let mut builder = client
        .post(&args.openrouter_url)
        .bearer_auth(api_key)
        .json(&request);

    if let Ok(site_url) = env::var("OPENROUTER_SITE_URL") {
        builder = builder.header("HTTP-Referer", site_url);
    }
    if let Ok(app_name) = env::var("OPENROUTER_APP_NAME") {
        builder = builder.header("X-Title", app_name);
    }

    let response = builder.send().await.context("failed to call OpenRouter")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read OpenRouter response")?;
    if !status.is_success() {
        bail!("OpenRouter request failed with {status}: {body}");
    }

    serde_json::from_str(&body).context("failed to parse OpenRouter response")
}

async fn start_mcp_client(
    args: &Args,
) -> Result<rmcp::service::RunningService<rmcp::RoleClient, ()>> {
    let command_spec = resolve_server_command(args)?;
    let mut command = Command::new(&command_spec.program);
    command.args(&command_spec.args);
    if let Some(cwd) = command_spec.current_dir {
        command.current_dir(cwd);
    }

    let transport = TokioChildProcess::new(command.configure(|cmd| {
        cmd.kill_on_drop(true);
    }))
    .context("failed to spawn MCP server process")?;

    ().serve(transport)
        .await
        .context("failed to initialize MCP client")
}

#[derive(Debug)]
struct ServerCommand {
    program: PathBuf,
    args: Vec<String>,
    current_dir: Option<PathBuf>,
}

fn resolve_server_command(args: &Args) -> Result<ServerCommand> {
    if let Some(program) = &args.server_command {
        return Ok(ServerCommand {
            program: program.clone(),
            args: args.server_args.clone(),
            current_dir: None,
        });
    }

    let exe_name = if cfg!(windows) {
        "chess-pos-analyzer.exe"
    } else {
        "chess-pos-analyzer"
    };

    if let Ok(current_exe) = env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let sibling = dir.join(exe_name);
            if sibling.exists() {
                return Ok(ServerCommand {
                    program: sibling,
                    args: args.server_args.clone(),
                    current_dir: workspace_root(),
                });
            }
        }
    }

    let mut server_args = vec![
        "run".into(),
        "--quiet".into(),
        "--package".into(),
        "chess-pos-analyzer".into(),
        "--".into(),
    ];
    server_args.extend(args.server_args.clone());

    Ok(ServerCommand {
        program: PathBuf::from("cargo"),
        args: server_args,
        current_dir: workspace_root(),
    })
}

fn workspace_root() -> Option<PathBuf> {
    if let Ok(current_exe) = env::current_exe() {
        for ancestor in current_exe.ancestors() {
            let manifest = ancestor.join("Cargo.toml");
            if let Ok(text) = std::fs::read_to_string(manifest) {
                if text.contains("[workspace]") && text.contains("mcp-server") {
                    return Some(ancestor.to_path_buf());
                }
            }
        }
    }
    env::current_dir().ok()
}

fn openrouter_tool_from_mcp(tool: &Tool) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name.as_ref(),
            "description": tool.description.as_deref().unwrap_or(""),
            "parameters": Value::Object(tool.input_schema.as_ref().clone()),
        }
    })
}

async fn call_mcp_tool(
    mcp: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    call: &ToolCall,
) -> Result<String> {
    if call.kind != "function" {
        bail!("unsupported tool call type: {}", call.kind);
    }

    let arguments_value = if call.function.arguments.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(&call.function.arguments)
            .with_context(|| format!("failed to parse arguments for tool {}", call.function.name))?
    };

    let arguments = match arguments_value {
        Value::Object(map) => Some(map),
        Value::Null => None,
        other => bail!(
            "tool {} arguments must be a JSON object, got {other}",
            call.function.name
        ),
    };

    let result = mcp
        .call_tool(CallToolRequestParam {
            name: Cow::Owned(call.function.name.clone()),
            arguments,
        })
        .await
        .with_context(|| format!("MCP tool {} failed", call.function.name))?;

    Ok(tool_result_to_text(result))
}

fn tool_result_to_text(result: CallToolResult) -> String {
    if let Some(value) = result.structured_content {
        return serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    }

    let mut chunks = Vec::new();
    for content in result.content {
        if let Some(text) = content.as_text() {
            chunks.push(text.text.clone());
        } else {
            chunks.push(serde_json::to_string(&content).unwrap_or_default());
        }
    }

    if result.is_error == Some(true) {
        format!("MCP tool returned an error:\n{}", chunks.join("\n"))
    } else {
        chunks.join("\n")
    }
}

fn assistant_message_value(message: &AssistantMessage) -> Value {
    let mut object = Map::new();
    object.insert("role".into(), Value::String(message.role.clone()));
    object.insert(
        "content".into(),
        message
            .content
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    if !message.tool_calls.is_empty() {
        object.insert(
            "tool_calls".into(),
            serde_json::to_value(&message.tool_calls).unwrap_or(Value::Null),
        );
    }
    Value::Object(object)
}

fn system_prompt() -> &'static str {
    "You are a chess analysis agent. Use the provided MCP tools for concrete board state \
     and engine analysis instead of inventing lines or evaluations. If the user gives a FEN, \
     call analyze_position with that FEN. If the user gives a move sequence, create a board \
     and apply moves before analyzing. Use depth 20 and multipv 6 unless the user asks for \
     different analysis settings. Summarize the engine result in practical chess terms and \
     include the main candidate moves, evaluations, and tactical ideas."
}
