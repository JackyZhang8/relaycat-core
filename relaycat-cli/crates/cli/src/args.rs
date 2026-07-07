use std::path::PathBuf;

use clap::{Arg, ArgAction, Args, CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::i18n::CliLanguage;

#[derive(Debug, Parser)]
#[command(name = "relaycat")]
#[command(about = "Remote AI coding agent controller", version)]
#[command(after_help = "\
Examples:
  relaycat shell --relay ws://192.168.1.12:8787
  relaycat codex --relay ws://192.168.1.12:8787
  relaycat claude --project /path/to/project --relay wss://relay.example.com
  relaycat opencode --relay ws://127.0.0.1:8787
  relaycat gemini --relay ws://127.0.0.1:8787
  relaycat aider --project /path/to/project --relay wss://relay.example.com
  relaycat tool my-agent --cmd my-agent --relay wss://relay.example.com -- --model flash
  relaycat qr --project /path/to/project --kind codex

Relay:
  --relay starts relay mode and prints a pairing QR/code for the app.
  Run `relaycat <command> --help` for command-specific options.
")]
pub struct Cli {
    /// Subcommand to run. When omitted, `relaycat` launches the interactive TUI.
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub fn parse_for_language(language: CliLanguage) -> Self {
        let matches = Self::command_for_language(language).get_matches();
        Self::from_arg_matches(&matches).unwrap_or_else(|err| err.exit())
    }

    pub fn command_for_language(language: CliLanguage) -> clap::Command {
        localize_command(Self::command(), language)
            .about(language.t(
                "Remote AI coding agent controller",
                "远程 AI Coding Agent 控制器",
            ))
            .long_about(None)
            .after_help(top_level_after_help(language))
            .subcommand_help_heading(language.t("Commands", "命令"))
            .mut_subcommand("shell", |cmd| {
                common_command_help(cmd, language)
                    .about(language.t(
                        "Start a local shell in a PTY.",
                        "在 PTY 中启动本地 shell。",
                    ))
                    .long_about(None)
                    .mut_arg("cmd", |arg| {
                        arg.help(language.t(
                            "Shell program to run. Defaults to PowerShell on Windows, or $SHELL/zsh/sh elsewhere.",
                            "要运行的 shell 程序。Windows 默认 PowerShell，其他平台默认 $SHELL/zsh/sh。",
                        ))
                    })
                    .mut_arg("project", |arg| arg.help(project_help(language)))
                    .mut_arg("relay", |arg| arg.help(relay_help(language)))
            })
            .mut_subcommand("claude", |cmd| {
                agent_command_help(
                    cmd,
                    language,
                    "Start Claude Code in a PTY.",
                    "在 PTY 中启动 Claude Code。",
                )
            })
            .mut_subcommand("codex", |cmd| {
                agent_command_help(
                    cmd,
                    language,
                    "Start Codex in a PTY.",
                    "在 PTY 中启动 Codex。",
                )
            })
            .mut_subcommand("opencode", |cmd| {
                agent_command_help(
                    cmd,
                    language,
                    "Start OpenCode in a PTY.",
                    "在 PTY 中启动 OpenCode。",
                )
            })
            .mut_subcommand("gemini", |cmd| {
                agent_command_help(
                    cmd,
                    language,
                    "Start Gemini CLI in a PTY.",
                    "在 PTY 中启动 Gemini CLI。",
                )
            })
            .mut_subcommand("aider", |cmd| {
                agent_command_help(
                    cmd,
                    language,
                    "Start Aider in a PTY.",
                    "在 PTY 中启动 Aider。",
                )
            })
            .mut_subcommand("tool", |cmd| {
                common_command_help(cmd, language)
                    .about(language.t(
                        "Start any command as a named tool session in a PTY.",
                        "启动任意命令作为具名工具会话。",
                    ))
                    .long_about(None)
                    .mut_arg("name", |arg| {
                        arg.help(language.t(
                            "Stable tool id shown in the app and recent list, for tools without a built-in subcommand.",
                            "显示在 app 和最近列表中的稳定工具 ID，用于没有内置子命令的工具。",
                        ))
                        .help_heading(language.t("Arguments", "参数"))
                    })
                    .mut_arg("cmd", |arg| {
                        arg.help(language.t(
                            "Program to run for this tool session.",
                            "这个工具会话要运行的程序。",
                        ))
                    })
                    .mut_arg("project", |arg| arg.help(project_help(language)))
                    .mut_arg("relay", |arg| arg.help(relay_help(language)))
                    .mut_arg("args", |arg| {
                        arg.help(language.t(
                            "Arguments passed to the tool program after `--`.",
                            "`--` 后传递给工具程序的参数。",
                        ))
                        .help_heading(language.t("Arguments", "参数"))
                    })
            })
            .mut_subcommand("tui", |cmd| {
                common_command_help(cmd, language)
                    .about(language.t(
                        "Launch the interactive TUI to pick and start a tool.",
                        "启动交互式 TUI，用于选择并启动工具。",
                    ))
                    .long_about(None)
            })
            .mut_subcommand("qr", |cmd| {
                common_command_help(cmd, language)
                    .about(language.t(
                        "Re-display the pairing QR/URL for an existing session.",
                        "重新显示已有会话的配对二维码/URL。",
                    ))
                    .long_about(None)
                    .mut_arg("project", |arg| {
                        arg.help(language.t(
                            "Project directory whose pairing session should be shown. Defaults to the current directory.",
                            "要显示配对会话的项目目录。默认使用当前目录。",
                        ))
                    })
                    .mut_arg("kind", |arg| {
                        arg.help(language.t(
                            "Session kind to show, for example codex, claude, opencode, gemini, aider or shell.",
                            "要显示的会话类型，例如 codex、claude、opencode、gemini、aider 或 shell。",
                        ))
                    })
                    .mut_arg("png", |arg| {
                        arg.help(language.t(
                            "Also write the QR to a room-scoped PNG under `.relaycat/pairing/`.",
                            "同时把二维码写入 `.relaycat/pairing/` 下按房间区分的 PNG。",
                        ))
                    })
            })
            .mut_subcommand("recent", |cmd| {
                common_command_help(cmd, language)
                    .about(language.t(
                        "List previously used relay sessions.",
                        "列出之前使用过的 relay 会话。",
                    ))
                    .long_about(None)
            })
            .mut_subcommand("run", |cmd| {
                common_command_help(cmd, language)
                    .about(language.t(
                        "Run a previously used relay session by number or id.",
                        "按编号或 ID 运行之前使用过的 relay 会话。",
                    ))
                    .long_about(None)
                    .mut_arg("selector", |arg| {
                        arg.help(language.t(
                            "Recent session number or id.",
                            "最近会话编号或 ID。",
                        ))
                        .help_heading(language.t("Arguments", "参数"))
                    })
            })
            .mut_subcommand("forget", |cmd| {
                common_command_help(cmd, language)
                    .about(language.t(
                        "Remove a previously used relay session by number or id.",
                        "按编号或 ID 删除之前使用过的 relay 会话。",
                    ))
                    .long_about(None)
                    .mut_arg("selector", |arg| {
                        arg.help(language.t(
                            "Recent session number or id.",
                            "最近会话编号或 ID。",
                        ))
                        .help_heading(language.t("Arguments", "参数"))
                    })
            })
            .mut_subcommand("config", |cmd| {
                common_command_help(cmd, language)
                    .about(language.t(
                        "Show the launcher config file path and its current contents.",
                        "显示启动器配置文件路径及当前内容。",
                    ))
                    .long_about(None)
                    .mut_arg("path", |arg| {
                        arg.help(language.t(
                            "Print only the config file path (useful for scripting/editing).",
                            "只打印配置文件路径（便于脚本或编辑）。",
                        ))
                    })
            })
            .mut_subcommand("update", |cmd| {
                common_command_help(cmd, language)
                    .about(language.t(
                        "Check for a RelayCat CLI update and download it.",
                        "检查并下载 RelayCat CLI 升级。",
                    ))
                    .long_about(None)
                    .mut_arg("manifest_url", |arg| {
                        arg.help(language.t("CLI update manifest URL.", "CLI 升级清单 URL。"))
                    })
                    .mut_arg("download_only", |arg| {
                        arg.help(language.t(
                            "Download and verify the update without replacing the current executable.",
                            "只下载并校验升级文件，不替换当前可执行文件。",
                        ))
                    })
            })
    }
}

fn localize_command(command: clap::Command, language: CliLanguage) -> clap::Command {
    let command = common_command_help(command, language)
        .disable_version_flag(true)
        .arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .action(ArgAction::Version)
                .help(language.t("Print version", "打印版本")),
        );

    match language {
        CliLanguage::En => command,
        CliLanguage::ZhHans => command.help_template(
            "{before-help}{name} {version}\n{about}\n\n用法: {usage}\n\n{all-args}{after-help}",
        ),
    }
}

fn common_command_help(command: clap::Command, language: CliLanguage) -> clap::Command {
    let command = command
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .next_help_heading(language.t("Options", "选项"))
        .mut_args(|arg| arg.help_heading(language.t("Options", "选项")))
        .arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::Help)
                .help(language.t("Print help", "打印帮助")),
        );

    match language {
        CliLanguage::En => command,
        CliLanguage::ZhHans => command.help_template(
            "{before-help}{name} {version}\n{about}\n\n用法: {usage}\n\n{all-args}{after-help}",
        ),
    }
}

fn agent_command_help(
    command: clap::Command,
    language: CliLanguage,
    en_about: &'static str,
    zh_about: &'static str,
) -> clap::Command {
    common_command_help(command, language)
        .about(language.t(en_about, zh_about))
        .long_about(None)
        .mut_arg("project", |arg| arg.help(project_help(language)))
        .mut_arg("relay", |arg| arg.help(relay_help(language)))
}

fn project_help(language: CliLanguage) -> &'static str {
    language.t(
        "Working directory for the child process.",
        "子进程的项目目录。",
    )
}

fn relay_help(language: CliLanguage) -> &'static str {
    language.t(
        "Relay websocket URL for secure app pairing.",
        "用于安全 app 配对的中继 WebSocket URL。",
    )
}

fn top_level_after_help(language: CliLanguage) -> &'static str {
    language.t(
        "\
Examples:
  relaycat shell --relay ws://192.168.1.12:8787
  relaycat codex --relay ws://192.168.1.12:8787
  relaycat claude --project /path/to/project --relay wss://relay.example.com
  relaycat opencode --relay ws://127.0.0.1:8787
  relaycat gemini --relay ws://127.0.0.1:8787
  relaycat aider --project /path/to/project --relay wss://relay.example.com
  relaycat tool my-agent --cmd my-agent --relay wss://relay.example.com -- --model flash
  relaycat qr --project /path/to/project --kind codex
  relaycat update

Relay:
  --relay starts relay mode and prints a pairing QR/code for the app.
  Run `relaycat <command> --help` for command-specific options.
",
        "\
示例:
  relaycat shell --relay ws://192.168.1.12:8787
  relaycat codex --relay ws://192.168.1.12:8787
  relaycat claude --project /path/to/project --relay wss://relay.example.com
  relaycat opencode --relay ws://127.0.0.1:8787
  relaycat gemini --relay ws://127.0.0.1:8787
  relaycat aider --project /path/to/project --relay wss://relay.example.com
  relaycat tool my-agent --cmd my-agent --relay wss://relay.example.com -- --model flash
  relaycat qr --project /path/to/project --kind codex
  relaycat update

Relay:
  --relay 会启动 relay 模式，并为 app 打印配对二维码/链接。
  运行 `relaycat <command> --help` 查看具体命令选项。
",
    )
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start a local shell in a PTY.
    Shell(ShellArgs),
    /// Start Claude Code in a PTY.
    Claude(AgentArgs),
    /// Start Codex in a PTY.
    Codex(AgentArgs),
    /// Start OpenCode in a PTY.
    Opencode(AgentArgs),
    /// Start Gemini CLI in a PTY.
    Gemini(AgentArgs),
    /// Start Aider in a PTY.
    Aider(AgentArgs),
    /// Start any command as a named tool session in a PTY.
    Tool(ToolArgs),
    /// Launch the interactive TUI to pick and start a tool.
    Tui(TuiArgs),
    /// Re-display the pairing QR/URL for an existing session.
    Qr(QrArgs),
    /// List previously used relay sessions.
    Recent(RecentArgs),
    /// Run a previously used relay session by number or id.
    Run(RunArgs),
    /// Remove a previously used relay session by number or id.
    Forget(ForgetArgs),
    /// Show the launcher config file path and its current contents.
    Config(ConfigArgs),
    /// Check for a RelayCat CLI update and download it.
    Update(UpdateArgs),
}

#[derive(Debug, Args)]
pub struct ShellArgs {
    /// Shell program to run. Defaults to PowerShell on Windows, or $SHELL/zsh/sh elsewhere.
    #[arg(long)]
    pub cmd: Option<String>,

    /// Working directory for the child process.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Relay websocket URL for secure app pairing.
    #[arg(long)]
    pub relay: Option<String>,
}

#[derive(Debug, Args)]
pub struct AgentArgs {
    /// Working directory for the agent process.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Relay websocket URL for secure app pairing.
    #[arg(long)]
    pub relay: Option<String>,
}

#[derive(Debug, Args)]
pub struct ToolArgs {
    /// Stable tool id shown in the app and recent list, for tools without a built-in subcommand.
    pub name: String,

    /// Program to run for this tool session.
    #[arg(long)]
    pub cmd: String,

    /// Working directory for the tool process.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Relay websocket URL for secure app pairing.
    #[arg(long)]
    pub relay: Option<String>,

    /// Arguments passed to the tool program after `--`.
    #[arg(last = true)]
    pub args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct TuiArgs {}

#[derive(Debug, Args)]
pub struct QrArgs {
    /// Project directory whose pairing session should be shown. Defaults to the
    /// current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Session kind to show, for example codex, claude, opencode or shell.
    #[arg(long, default_value = "shell")]
    pub kind: String,

    /// Also write the QR to a room-scoped PNG under `.relaycat/pairing/`.
    #[arg(long)]
    pub png: bool,
}

#[derive(Debug, Args)]
pub struct RecentArgs {}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Recent session number or id.
    pub selector: String,
}

#[derive(Debug, Args)]
pub struct ForgetArgs {
    /// Recent session number or id.
    pub selector: String,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// Print only the config file path (useful for scripting/editing).
    #[arg(long)]
    pub path: bool,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// CLI update manifest URL.
    #[arg(long, default_value = crate::update::DEFAULT_CLI_MANIFEST_URL)]
    pub manifest_url: String,

    /// Download and verify the update without replacing the current executable.
    #[arg(long)]
    pub download_only: bool,
}
