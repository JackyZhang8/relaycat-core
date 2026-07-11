use super::*;

pub(crate) fn launcher_main_layout(area: Rect) -> LauncherMainLayout {
    const BRAND_MIN_TERMINAL_WIDTH: u16 = 96;
    const BRAND_PANEL_WIDTH: u16 = 42;

    if area.width < BRAND_MIN_TERMINAL_WIDTH {
        return LauncherMainLayout {
            content: area,
            brand: None,
        };
    }

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(48), Constraint::Length(BRAND_PANEL_WIDTH)])
        .split(area);

    LauncherMainLayout {
        content: columns[0],
        brand: Some(columns[1]),
    }
}

pub(crate) fn launcher_brand_text_for_language(language: CliLanguage) -> Vec<&'static str> {
    vec![
        "RelayCat",
        language.t(
            "A lightweight terminal companion for secure remote access across your AI Coding Agent.",
            "一款面向 AI Coding Agent 的轻量安全远程终端伴侣。",
        ),
        language.t("1. Choose a tool", "1. 选择工具"),
        language.t("2. Pick a project", "2. 选择项目"),
        language.t("3. Scan QR to connect", "3. 扫码连接"),
    ]
}

pub(crate) fn launcher_brand_lines_for_language(language: CliLanguage) -> Vec<Line<'static>> {
    let brand_text = launcher_brand_text_for_language(language);

    vec![
        Line::from("").alignment(Alignment::Center),
        Line::from("/\\_/\\").alignment(Alignment::Center),
        Line::from("( o.o )").alignment(Alignment::Center),
        Line::from("> ^ <").alignment(Alignment::Center),
        Line::from("").alignment(Alignment::Center),
        Line::from("").alignment(Alignment::Center),
        Line::from(brand_text[0]).alignment(Alignment::Center),
        Line::from("").alignment(Alignment::Center),
        Line::from(brand_text[1]).alignment(Alignment::Center),
        Line::from("").alignment(Alignment::Center),
        Line::from(brand_text[2]).alignment(Alignment::Center),
        Line::from(brand_text[3]).alignment(Alignment::Center),
        Line::from(brand_text[4]).alignment(Alignment::Center),
    ]
}

pub(crate) fn launcher_brand_padding() -> Padding {
    Padding::new(2, 2, 2, 1)
}

pub(crate) fn launcher_brand_panel_for_language(language: CliLanguage) -> Paragraph<'static> {
    Paragraph::new(launcher_brand_lines_for_language(language))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("RelayCat")
                .padding(launcher_brand_padding()),
        )
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
}

pub(crate) fn render_launcher_brand(frame: &mut ratatui::Frame, area: Rect, language: CliLanguage) {
    let panel = launcher_brand_panel_for_language(language);
    frame.render_widget(panel, area);
}

pub(crate) fn launcher_page_areas(frame_area: Rect) -> (Rect, Rect, Rect, Rect, Option<Rect>) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(frame_area);
    let main = launcher_main_layout(layout[1]);
    (layout[0], main.content, layout[2], layout[3], main.brand)
}

pub(crate) fn launcher_update_status_text(
    language: CliLanguage,
    current_version: &str,
    update_notice: Option<&UpdateNotice>,
) -> String {
    let mut text = format!(
        "{} relaycat update, {} {}",
        language.t("Update command:", "更新命令:"),
        language.t("current version:", "当前版本:"),
        current_version
    );
    if let Some(notice) = update_notice {
        text.push_str(&format!(
            "  {} {}",
            language.t("new version:", "发现新版:"),
            notice.latest_version
        ));
    }
    text
}

pub(crate) fn render_update_footer(
    frame: &mut ratatui::Frame,
    area: Rect,
    language: CliLanguage,
    update_notice: Option<&UpdateNotice>,
) {
    let footer = Paragraph::new(Line::from(launcher_update_status_text(
        language,
        env!("CARGO_PKG_VERSION"),
        update_notice,
    )))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, area);
}

#[cfg(test)]
pub(crate) fn draw_picker(
    frame: &mut ratatui::Frame,
    tools: &[ToolChoice],
    state: &mut ListState,
    language: CliLanguage,
) {
    draw_picker_with_update_notice(frame, tools, state, language, None);
}

pub(crate) fn draw_picker_with_update_notice(
    frame: &mut ratatui::Frame,
    tools: &[ToolChoice],
    state: &mut ListState,
    language: CliLanguage,
    update_notice: Option<&UpdateNotice>,
) {
    let (header_area, content_area, footer_area, update_area, brand_area) =
        launcher_page_areas(frame.area());

    let header = Paragraph::new(Line::from(language.t(
        "RelayCat — choose a tool to launch",
        "RelayCat — 选择要启动的工具",
    )))
    .block(Block::default().borders(Borders::ALL).title("relaycat"));
    frame.render_widget(header, header_area);

    let items: Vec<ListItem> = tools
        .iter()
        .map(|choice| ListItem::new(choice.label.clone()))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(language.t("Tools", "工具")),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, content_area, state);
    if let Some(area) = brand_area {
        render_launcher_brand(frame, area, language);
    }

    let footer = Paragraph::new(Line::from(language.t(
        "↑/↓ or j/k to move · Enter to choose project · q/Esc to quit",
        "↑/↓ 或 j/k 移动 · Enter 选择项目 · q/Esc 退出",
    )))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, footer_area);
    render_update_footer(frame, update_area, language, update_notice);
}

pub(crate) fn draw_project(
    frame: &mut ratatui::Frame,
    tool_label: &str,
    choices: &[ProjectChoice],
    state: &mut ListState,
    language: CliLanguage,
    update_notice: Option<&UpdateNotice>,
) {
    let (header_area, content_area, footer_area, update_area, brand_area) =
        launcher_page_areas(frame.area());

    let header = Paragraph::new(Line::from(format!(
        "{} {tool_label} — {}",
        language.t("Launch", "启动"),
        language.t("choose a project", "选择项目")
    )))
    .block(Block::default().borders(Borders::ALL).title("relaycat"));
    frame.render_widget(header, header_area);

    let items: Vec<ListItem> = choices
        .iter()
        .map(|choice| ListItem::new(choice.label.clone()))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(language.t("Project", "项目")),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, content_area, state);
    if let Some(area) = brand_area {
        render_launcher_brand(frame, area, language);
    }

    let footer = Paragraph::new(Line::from(language.t(
        "↑/↓ or j/k to move · Enter to select · f to favorite · d to delete · Esc to go back",
        "↑/↓ 或 j/k 移动 · Enter 选择 · f 收藏 · d 删除 · Esc 返回",
    )))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, footer_area);
    render_update_footer(frame, update_area, language, update_notice);
}

pub(crate) fn draw_project_custom(
    frame: &mut ratatui::Frame,
    tool_label: &str,
    input: &str,
    language: CliLanguage,
    update_notice: Option<&UpdateNotice>,
) {
    let (header_area, content_area, footer_area, update_area, brand_area) =
        launcher_page_areas(frame.area());

    let header = Paragraph::new(Line::from(format!(
        "{} {tool_label} — {}",
        language.t("Launch", "启动"),
        language.t("project path", "项目路径")
    )))
    .block(Block::default().borders(Borders::ALL).title("relaycat"));
    frame.render_widget(header, header_area);

    // Trailing block glyph stands in for the text cursor.
    let body = vec![
        Line::from(format!("{input}\u{2588}")),
        Line::from(""),
        Line::from(language.t(
            "Leave empty to use the current directory.",
            "留空则使用当前目录。",
        )),
    ];
    let input_box = Paragraph::new(body).block(
        Block::default()
            .borders(Borders::ALL)
            .title(language.t("Project directory path", "项目目录路径")),
    );
    frame.render_widget(input_box, content_area);
    if let Some(area) = brand_area {
        render_launcher_brand(frame, area, language);
    }

    let footer = Paragraph::new(Line::from(language.t(
        "Type to edit · Enter to continue · Esc to go back",
        "输入编辑 · Enter 继续 · Esc 返回",
    )))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, footer_area);
    render_update_footer(frame, update_area, language, update_notice);
}

pub(crate) fn draw_relay(
    frame: &mut ratatui::Frame,
    tool_label: &str,
    input: &str,
    language: CliLanguage,
    update_notice: Option<&UpdateNotice>,
) {
    let (header_area, content_area, footer_area, update_area, brand_area) =
        launcher_page_areas(frame.area());

    let header = Paragraph::new(Line::from(format!(
        "{} {tool_label} — {}",
        language.t("Launch", "启动"),
        language.t("relay address", "relay 地址")
    )))
    .block(Block::default().borders(Borders::ALL).title("relaycat"));
    frame.render_widget(header, header_area);

    // Trailing block glyph stands in for the text cursor.
    let body = vec![
        Line::from(format!("{input}\u{2588}")),
        Line::from(""),
        Line::from(language.t(
            "Leave empty to start locally without pairing.",
            "留空则在本地启动，不进行配对。",
        )),
    ];
    let input_box =
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(language.t(
            "Relay WebSocket URL (ws:// or wss://)",
            "Relay WebSocket URL（ws:// 或 wss://）",
        )));
    frame.render_widget(input_box, content_area);
    if let Some(area) = brand_area {
        render_launcher_brand(frame, area, language);
    }

    let footer = Paragraph::new(Line::from(language.t(
        "Type to edit · Enter to start · Esc to go back",
        "输入编辑 · Enter 启动 · Esc 返回",
    )))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, footer_area);
    render_update_footer(frame, update_area, language, update_notice);
}
