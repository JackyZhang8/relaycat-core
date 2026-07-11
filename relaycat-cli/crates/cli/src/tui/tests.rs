use super::*;
use crate::command::SessionKind;
use crate::config::CustomTool;

fn record(project: &str, relay: &str, last_used_at: u64) -> RecentRecord {
    RecentRecord {
        id: format!("{project}-id"),
        kind: SessionKind::shell(),
        program: "sh".to_string(),
        args: Vec::new(),
        project: PathBuf::from(project),
        relay: relay.to_string(),
        last_used_at,
        use_count: 1,
    }
}

#[test]
fn default_relay_precedence_project_then_config_then_recent() {
    let records = vec![
        record("/work/most-recent", "ws://recent", 200),
        record("/work/alpha", "ws://alpha", 100),
    ];
    let mut config = Config::default();

    // A known project uses the relay it was last paired with.
    assert_eq!(
        default_relay_for(&records, Some(Path::new("/work/alpha")), &config).as_deref(),
        Some("ws://alpha"),
    );
    // An unknown project with no config default falls back to most-recent.
    assert_eq!(
        default_relay_for(&records, Some(Path::new("/work/unknown")), &config).as_deref(),
        Some("ws://recent"),
    );
    // The config default takes precedence over most-recent for unknowns.
    config.default_relay = Some("ws://configured".to_string());
    assert_eq!(
        default_relay_for(&records, Some(Path::new("/work/unknown")), &config).as_deref(),
        Some("ws://configured"),
    );
    // With no history and no config default there is nothing to pre-fill.
    assert_eq!(
        default_relay_for(&[], Some(Path::new("/x")), &Config::default()),
        None,
    );
}

#[test]
fn project_choices_lists_favorites_then_recents() {
    let records = vec![
        record("/work/alpha", "ws://a", 200),
        record("/work/alpha", "ws://a", 150),
        record("/work/beta", "ws://b", 100),
    ];
    let config = Config {
        favorites: vec![PathBuf::from("/work/fav")],
        ..Config::default()
    };

    let choices = project_choices(&records, &config, CliLanguage::En);

    assert!(matches!(
        choices.first().unwrap().kind,
        ProjectChoiceKind::CurrentDir
    ));
    assert!(matches!(
        choices.last().unwrap().kind,
        ProjectChoiceKind::Custom
    ));
    // The favorite appears (starred) before the recent projects.
    let fav_pos = choices
        .iter()
        .position(|c| matches!(&c.kind, ProjectChoiceKind::Path(p) if p == Path::new("/work/fav")))
        .unwrap();
    let beta_pos = choices
        .iter()
        .position(|c| matches!(&c.kind, ProjectChoiceKind::Path(p) if p == Path::new("/work/beta")))
        .unwrap();
    assert!(fav_pos < beta_pos);
    assert!(choices[fav_pos].label.starts_with('★'));
    // /work/alpha appears once even though it has two records.
    let alpha = choices
        .iter()
        .filter(|c| matches!(&c.kind, ProjectChoiceKind::Path(p) if p == Path::new("/work/alpha")))
        .count();
    assert_eq!(alpha, 1);
}

#[test]
fn build_tools_appends_custom_and_default_index_matches() {
    let config = Config {
        default_tool: Some("my-agent".to_string()),
        tools: vec![CustomTool {
            name: "my-agent".to_string(),
            label: Some("My Agent".to_string()),
            cmd: "my-agent".to_string(),
            args: Vec::new(),
        }],
        ..Config::default()
    };
    let tools = build_tools(&config, CliLanguage::En);
    assert_eq!(tools.len(), BUILTIN_TOOLS.len() + 1);
    let index = default_tool_index(&tools, &config);
    assert_eq!(tool_id(&tools[index].tool), "my-agent");
    assert!(matches!(tools[index].tool, ToolKind::Custom { .. }));
}

#[test]
fn default_tool_index_matches_builtin_and_falls_back_to_zero() {
    let tools = build_tools(&Config::default(), CliLanguage::En);
    let config = Config {
        default_tool: Some("claude".to_string()),
        ..Config::default()
    };
    assert_eq!(
        tool_id(&tools[default_tool_index(&tools, &config)].tool),
        "claude"
    );

    let unknown = Config {
        default_tool: Some("nope".to_string()),
        ..Config::default()
    };
    assert_eq!(default_tool_index(&tools, &unknown), 0);
}

#[test]
fn project_choices_use_chinese_labels() {
    let choices = project_choices(&[], &Config::default(), CliLanguage::ZhHans);

    assert!(choices.first().unwrap().label.starts_with("当前目录"));
    assert_eq!(choices.last().unwrap().label, "输入自定义路径…");
}

#[test]
fn build_tools_uses_chinese_custom_label() {
    let config = Config {
        tools: vec![CustomTool {
            name: "my-agent".to_string(),
            label: Some("My Agent".to_string()),
            cmd: "my-agent".to_string(),
            args: Vec::new(),
        }],
        ..Config::default()
    };
    let tools = build_tools(&config, CliLanguage::ZhHans);

    assert!(tools.iter().any(|tool| tool.label == "My Agent  (自定义)"));
}

#[test]
fn pairing_controls_choose_next_launcher_screen() {
    let launch = Launch {
        tool: ToolKind::Builtin("codex"),
        project: Some(PathBuf::from("/work/relaycat")),
        relay: Some("ws://127.0.0.1:8787".to_string()),
    };

    assert_eq!(
        session_outcome_for_pairing_control(&launch, relay::PairingControlAction::BackOneLevel),
        SessionOutcome::BackToRelay(launch.clone())
    );
    assert_eq!(
        session_outcome_for_pairing_control(&launch, relay::PairingControlAction::BackToTools),
        SessionOutcome::BackToTools
    );
}

#[test]
fn launcher_relay_error_prompt_points_to_escape_edit() {
    assert!(
        relay_error_prompt_for_language(CliLanguage::ZhHans).contains("按 Esc 返回修改 relay 地址")
    );
    assert!(
        relay_error_prompt_for_language(CliLanguage::En)
            .contains("Press Esc to go back and edit the relay URL")
    );
}

#[test]
fn pairing_wait_message_lists_status_and_controls() {
    assert_eq!(
        pairing_wait_message_for_language(CliLanguage::En),
        "Waiting:\n  Scan the QR in the RelayCat app to join.\n\nControls:\n  Esc    edit relay URL\n  Ctrl-C return to tools"
    );
    assert_eq!(
        pairing_wait_message_for_language(CliLanguage::ZhHans),
        "等待:\n  请用 RelayCat app 扫描二维码并加入。\n\n操作:\n  Esc    修改 relay 地址\n  Ctrl-C 返回工具选择"
    );
}

#[test]
fn launcher_brand_copy_uses_relaycat_positioning() {
    let lines = launcher_brand_lines_for_language(CliLanguage::En);

    assert!(
        lines
            .iter()
            .any(|line| line.spans.iter().any(|span| span.content == "RelayCat"))
    );
    assert!(lines.iter().any(|line| line.spans.iter().any(|span| {
        span.content
            == "A lightweight terminal companion for secure remote access across your AI Coding Agent."
    })));
}

#[test]
fn launcher_brand_usage_copy_uses_english() {
    let text = launcher_brand_text_for_language(CliLanguage::En);

    assert!(text.contains(&"1. Choose a tool"));
    assert!(text.contains(&"2. Pick a project"));
    assert!(text.contains(&"3. Scan QR to connect"));
}

#[test]
fn launcher_brand_usage_copy_uses_chinese() {
    let text = launcher_brand_text_for_language(CliLanguage::ZhHans);

    assert!(text.contains(&"一款面向 AI Coding Agent 的轻量安全远程终端伴侣。"));
    assert!(text.contains(&"1. 选择工具"));
    assert!(text.contains(&"2. 选择项目"));
    assert!(text.contains(&"3. 扫码连接"));
}

#[test]
fn launcher_update_footer_uses_english_copy_without_notice() {
    assert_eq!(
        launcher_update_status_text(CliLanguage::En, "0.1.1", None),
        "Update command: relaycat update, current version: 0.1.1"
    );
}

#[test]
fn launcher_update_footer_uses_chinese_copy_with_notice() {
    let notice = UpdateNotice {
        current_version: "0.1.1".to_string(),
        latest_version: "0.1.2".to_string(),
        mandatory: false,
    };

    assert_eq!(
        launcher_update_status_text(CliLanguage::ZhHans, "0.1.1", Some(&notice)),
        "更新命令: relaycat update, 当前版本: 0.1.1  发现新版: 0.1.2"
    );
}

#[test]
fn launcher_language_follows_system_locale_prefix() {
    assert_eq!(
        CliLanguage::from_locale_tag(Some("zh_CN.UTF-8")),
        CliLanguage::ZhHans
    );
    assert_eq!(
        CliLanguage::from_locale_tag(Some("zh-Hans-CN")),
        CliLanguage::ZhHans
    );
    assert_eq!(
        CliLanguage::from_locale_tag(Some("en_US.UTF-8")),
        CliLanguage::En
    );
    assert_eq!(CliLanguage::from_locale_tag(None), CliLanguage::En);
}

#[test]
fn launcher_brand_uses_line_cat_shape() {
    let text = launcher_brand_lines_for_language(CliLanguage::En)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert!(text.iter().any(|line| line.contains("/\\_/\\")));
    assert!(text.iter().any(|line| line.contains("( o.o )")));
    assert!(text.iter().any(|line| line.contains("> ^ <")));
}

#[test]
fn launcher_brand_cat_lines_avoid_manual_edge_padding() {
    let logo_lines = launcher_brand_lines_for_language(CliLanguage::En)
        .into_iter()
        .filter_map(|line| {
            let text = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            matches!(text.as_str(), "/\\_/\\" | "( o.o )" | "> ^ <").then_some(text)
        })
        .collect::<Vec<_>>();

    assert_eq!(logo_lines, vec!["/\\_/\\", "( o.o )", "> ^ <"]);
    assert!(logo_lines.iter().all(|line| {
        !line.starts_with(char::is_whitespace) && !line.ends_with(char::is_whitespace)
    }));
}

#[test]
fn launcher_brand_uses_terminal_default_colors() {
    let lines = launcher_brand_lines_for_language(CliLanguage::En);

    assert!(
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .all(|span| { span.style.fg.is_none() && span.style.bg.is_none() })
    );
}

#[test]
fn picker_render_uses_terminal_default_colors() {
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    let tools = build_tools(&Config::default(), CliLanguage::En);
    let mut state = ListState::default();
    state.select(Some(0));
    let backend = TestBackend::new(120, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|frame| draw_picker(frame, &tools, &mut state, CliLanguage::En))
        .unwrap();

    let buffer = terminal.backend().buffer();
    assert!(
        buffer
            .content()
            .iter()
            .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset)
    );
}

#[test]
fn launcher_brand_lines_are_centered() {
    let lines = launcher_brand_lines_for_language(CliLanguage::En);

    assert!(
        lines
            .iter()
            .all(|line| line.alignment == Some(Alignment::Center))
    );
}

#[test]
fn launcher_brand_panel_uses_inner_padding() {
    assert_eq!(launcher_brand_padding(), Padding::new(2, 2, 2, 1));
}

#[test]
fn launcher_main_layout_uses_brand_panel_on_wide_terminals() {
    let area = ratatui::layout::Rect {
        x: 0,
        y: 3,
        width: 120,
        height: 20,
    };

    let layout = launcher_main_layout(area);

    assert!(layout.brand.is_some());
    assert!(layout.content.width < area.width);
}

#[test]
fn launcher_main_layout_hides_brand_panel_on_narrow_terminals() {
    let area = ratatui::layout::Rect {
        x: 0,
        y: 3,
        width: 72,
        height: 20,
    };

    let layout = launcher_main_layout(area);

    assert!(layout.brand.is_none());
    assert_eq!(layout.content, area);
}
