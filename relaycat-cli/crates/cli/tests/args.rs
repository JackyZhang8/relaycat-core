use clap::Parser;
use relaycat_cli::args::{Cli, Command, ForgetArgs, RecentArgs, RunArgs, UpdateArgs};
use relaycat_cli::command::TargetCommand;
use relaycat_cli::i18n::CliLanguage;

#[test]
fn parses_qr_command_with_project_and_kind() {
    let cli = Cli::parse_from([
        "relaycat",
        "qr",
        "--project",
        "/tmp/project",
        "--kind",
        "codex",
        "--png",
    ]);

    match cli.command.expect("subcommand") {
        Command::Qr(qr) => {
            assert_eq!(
                qr.project.as_deref(),
                Some(std::path::Path::new("/tmp/project"))
            );
            assert_eq!(qr.kind, "codex");
            assert!(qr.png);
        }
        other => panic!("expected qr command, got {other:?}"),
    }
}

#[test]
fn qr_command_defaults_kind_to_shell_without_png() {
    let cli = Cli::parse_from(["relaycat", "qr"]);

    match cli.command.expect("subcommand") {
        Command::Qr(qr) => {
            assert_eq!(qr.project, None);
            assert_eq!(qr.kind, "shell");
            assert!(!qr.png);
        }
        other => panic!("expected qr command, got {other:?}"),
    }
}

#[test]
fn top_level_help_shows_relay_usage_examples() {
    let help = Cli::command_for_language(CliLanguage::En)
        .render_help()
        .to_string();

    assert!(help.contains("Examples:"));
    assert!(help.contains("relaycat codex --relay ws://192.168.1.12:8787"));
    assert!(
        help.contains("relaycat claude --project /path/to/project --relay wss://relay.example.com")
    );
    assert!(help.contains("relaycat opencode --relay ws://127.0.0.1:8787"));
    assert!(help.contains("relaycat gemini --relay ws://127.0.0.1:8787"));
    assert!(
        help.contains("relaycat aider --project /path/to/project --relay wss://relay.example.com")
    );
    assert!(!help.contains("--room"));
}

#[test]
fn top_level_help_can_render_chinese() {
    let help = Cli::command_for_language(CliLanguage::ZhHans)
        .render_help()
        .to_string();

    assert!(help.contains("远程 AI Coding Agent 控制器"));
    assert!(help.contains("示例:"));
    assert!(help.contains("命令:"));
    assert!(help.contains("选项:"));
    assert!(help.contains("启动本地 shell"));
    assert!(help.contains("打印帮助"));
    assert!(!help.contains("Print this message"));
}

#[test]
fn subcommand_help_can_render_chinese() {
    let help = Cli::command_for_language(CliLanguage::ZhHans)
        .find_subcommand_mut("tool")
        .expect("tool subcommand")
        .render_help()
        .to_string();

    assert!(help.contains("启动任意命令作为具名工具会话"));
    assert!(help.contains("工具 ID"));
    assert!(help.contains("项目目录"));
    assert!(help.contains("中继 WebSocket URL"));
    assert!(help.contains("传递给工具程序的参数"));
    assert!(!help.contains("Options:"));
}

#[test]
fn parses_shell_default_command() {
    let cli = Cli::parse_from(["relaycat", "shell"]);

    match cli.command.expect("subcommand") {
        Command::Shell(shell) => {
            assert_eq!(shell.cmd, None);
            assert_eq!(shell.project, None);
        }
        other => panic!("expected shell command, got {other:?}"),
    }
}

#[test]
fn parses_shell_custom_command_and_project() {
    let cli = Cli::parse_from([
        "relaycat",
        "shell",
        "--cmd",
        "bash",
        "--project",
        "/tmp/project",
    ]);

    match cli.command.expect("subcommand") {
        Command::Shell(shell) => {
            assert_eq!(shell.cmd.as_deref(), Some("bash"));
            assert_eq!(
                shell.project.as_deref(),
                Some(std::path::Path::new("/tmp/project"))
            );
        }
        other => panic!("expected shell command, got {other:?}"),
    }
}

#[test]
fn parses_shell_relay_options() {
    let cli = Cli::parse_from(["relaycat", "shell", "--relay", "ws://127.0.0.1:8787"]);

    match cli.command.expect("subcommand") {
        Command::Shell(shell) => {
            assert_eq!(shell.relay.as_deref(), Some("ws://127.0.0.1:8787"));
        }
        other => panic!("expected shell command, got {other:?}"),
    }
}

#[test]
fn builds_secure_relay_target_when_room_is_omitted() {
    let cli = Cli::parse_from(["relaycat", "claude", "--relay", "wss://relay.example.com"]);
    let target = TargetCommand::from_cli(&cli).expect("target command");

    let relay = target.relay.expect("relay options");
    assert_eq!(relay.url, "wss://relay.example.com");
    assert_eq!(relay.room_id, None);
}

#[test]
fn rejects_custom_room_option() {
    let err = Cli::try_parse_from([
        "relaycat",
        "shell",
        "--relay",
        "ws://127.0.0.1:8787",
        "--room",
        "room-1",
    ])
    .expect_err("custom room should not be accepted");

    assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn builds_claude_target_command() {
    let cli = Cli::parse_from(["relaycat", "claude", "--project", "/tmp/project"]);
    let target = TargetCommand::from_cli(&cli).expect("target command");

    assert_eq!(target.program, "claude");
    assert_eq!(
        target.session_kind,
        relaycat_cli::command::SessionKind::claude()
    );
    assert!(target.args.is_empty());
    assert_eq!(
        target.cwd.as_deref(),
        Some(std::path::Path::new("/tmp/project"))
    );
    assert_eq!(target.relay, None);
}

#[test]
fn builds_claude_target_command_with_current_dir_when_project_is_omitted() {
    let cli = Cli::parse_from(["relaycat", "claude"]);
    let target = TargetCommand::from_cli(&cli).expect("target command");
    let current_dir = std::env::current_dir().expect("current dir");

    assert_eq!(target.program, "claude");
    assert_eq!(target.cwd, Some(current_dir));
}

#[test]
fn builds_codex_target_command() {
    let cli = Cli::parse_from(["relaycat", "codex"]);
    let target = TargetCommand::from_cli(&cli).expect("target command");
    let current_dir = std::env::current_dir().expect("current dir");

    assert_eq!(target.program, "codex");
    assert_eq!(
        target.session_kind,
        relaycat_cli::command::SessionKind::codex()
    );
    assert_eq!(target.args, ["--no-alt-screen"]);
    assert_eq!(target.cwd, Some(current_dir));
    assert_eq!(target.relay, None);
}

#[test]
fn builds_opencode_target_command() {
    let cli = Cli::parse_from(["relaycat", "opencode"]);
    let target = TargetCommand::from_cli(&cli).expect("target command");
    let current_dir = std::env::current_dir().expect("current dir");

    assert_eq!(target.program, "opencode");
    assert_eq!(
        target.session_kind,
        relaycat_cli::command::SessionKind::opencode()
    );
    assert!(target.args.is_empty());
    assert_eq!(target.cwd, Some(current_dir));
    assert_eq!(target.relay, None);
}

#[test]
fn builds_gemini_target_command() {
    let cli = Cli::parse_from(["relaycat", "gemini"]);
    let target = TargetCommand::from_cli(&cli).expect("target command");
    let current_dir = std::env::current_dir().expect("current dir");

    assert_eq!(target.program, "gemini");
    assert_eq!(
        target.session_kind,
        relaycat_cli::command::SessionKind::gemini()
    );
    assert!(target.args.is_empty());
    assert!(!target.session_kind.uses_managed_alt_screen());
    assert_eq!(target.cwd, Some(current_dir));
    assert_eq!(target.relay, None);
}

#[test]
fn builds_aider_target_command() {
    let cli = Cli::parse_from(["relaycat", "aider"]);
    let target = TargetCommand::from_cli(&cli).expect("target command");
    let current_dir = std::env::current_dir().expect("current dir");

    assert_eq!(target.program, "aider");
    assert_eq!(
        target.session_kind,
        relaycat_cli::command::SessionKind::aider()
    );
    assert!(target.args.is_empty());
    assert!(!target.session_kind.uses_managed_alt_screen());
    assert_eq!(target.cwd, Some(current_dir));
    assert_eq!(target.relay, None);
}

#[test]
fn builds_shell_target_command_with_shell_session_kind() {
    let cli = Cli::parse_from(["relaycat", "shell", "--cmd", "bash"]);
    let target = TargetCommand::from_cli(&cli).expect("target command");

    assert_eq!(target.program, "bash");
    assert_eq!(
        target.session_kind,
        relaycat_cli::command::SessionKind::shell()
    );
}

#[test]
fn builds_shell_target_command_with_current_dir_when_project_is_omitted() {
    let cli = Cli::parse_from(["relaycat", "shell", "--cmd", "bash"]);
    let target = TargetCommand::from_cli(&cli).expect("target command");
    let current_dir = std::env::current_dir().expect("current dir");

    assert_eq!(target.cwd, Some(current_dir));
}

#[test]
fn builds_generic_tool_target_command() {
    let cli = Cli::parse_from([
        "relaycat",
        "tool",
        "gemini",
        "--cmd",
        "gemini",
        "--relay",
        "wss://relay.example.com",
        "--",
        "--model",
        "flash",
    ]);
    let target = TargetCommand::from_cli(&cli).expect("target command");

    assert_eq!(target.program, "gemini");
    assert_eq!(target.args, ["--model", "flash"]);
    assert_eq!(target.session_kind.as_str(), "gemini");
    assert!(!target.session_kind.is_shell());
}

#[test]
fn rejects_invalid_generic_tool_name() {
    let cli = Cli::parse_from(["relaycat", "tool", "../bad", "--cmd", "bad"]);

    let err = TargetCommand::from_cli(&cli).expect_err("invalid tool name");

    assert!(err.to_string().contains("invalid session kind"));
}

#[test]
fn parses_recent_command() {
    let cli = Cli::parse_from(["relaycat", "recent"]);

    match cli.command.expect("subcommand") {
        Command::Recent(RecentArgs {}) => {}
        other => panic!("expected recent command, got {other:?}"),
    }
}

#[test]
fn parses_run_command_selector() {
    let cli = Cli::parse_from(["relaycat", "run", "1"]);

    match cli.command.expect("subcommand") {
        Command::Run(RunArgs { selector }) => assert_eq!(selector, "1"),
        other => panic!("expected run command, got {other:?}"),
    }
}

#[test]
fn parses_forget_command_selector() {
    let cli = Cli::parse_from(["relaycat", "forget", "codex-relaycat"]);

    match cli.command.expect("subcommand") {
        Command::Forget(ForgetArgs { selector }) => assert_eq!(selector, "codex-relaycat"),
        other => panic!("expected forget command, got {other:?}"),
    }
}

#[test]
fn no_subcommand_defaults_to_tui() {
    let cli = Cli::parse_from(["relaycat"]);
    assert!(cli.command.is_none());
}

#[test]
fn parses_tui_command() {
    let cli = Cli::parse_from(["relaycat", "tui"]);

    match cli.command.expect("subcommand") {
        Command::Tui(_) => {}
        other => panic!("expected tui command, got {other:?}"),
    }
}

#[test]
fn parses_update_command_with_manifest_url() {
    let cli = Cli::parse_from([
        "relaycat",
        "update",
        "--manifest-url",
        "https://example.com/update/cli.json",
        "--download-only",
    ]);

    match cli.command.expect("subcommand") {
        Command::Update(UpdateArgs {
            manifest_url,
            download_only,
        }) => {
            assert_eq!(manifest_url, "https://example.com/update/cli.json");
            assert!(download_only);
        }
        other => panic!("expected update command, got {other:?}"),
    }
}
