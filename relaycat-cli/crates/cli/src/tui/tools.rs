use super::*;

/// Build the tool picker entries: the built-ins followed by any custom tools
/// defined in the config.
pub(crate) fn build_tools(config: &Config, language: CliLanguage) -> Vec<ToolChoice> {
    let mut tools: Vec<ToolChoice> = BUILTIN_TOOLS
        .iter()
        .map(|(label, kind)| ToolChoice {
            label: (*label).to_string(),
            tool: ToolKind::Builtin(kind),
        })
        .collect();
    for custom in &config.tools {
        tools.push(ToolChoice {
            label: format!(
                "{}  ({})",
                custom.display_label(),
                language.t("custom", "自定义")
            ),
            tool: ToolKind::Custom {
                name: custom.name.clone(),
                program: custom.cmd.clone(),
                args: custom.args.clone(),
            },
        });
    }
    tools
}

/// The id used to match a tool against `config.default_tool`: the built-in kind
/// or the custom tool name.
pub(crate) fn tool_id(tool: &ToolKind) -> &str {
    match tool {
        ToolKind::Builtin(kind) => kind,
        ToolKind::Custom { name, .. } => name,
    }
}

/// Index of the configured default tool within `tools`, or 0 when there is no
/// match.
pub(crate) fn default_tool_index(tools: &[ToolChoice], config: &Config) -> usize {
    let Some(default) = config.default_tool.as_deref() else {
        return 0;
    };
    tools
        .iter()
        .position(|choice| tool_id(&choice.tool) == default)
        .unwrap_or(0)
}

pub(crate) fn tool_index_for_launch(tools: &[ToolChoice], launch: &Launch) -> usize {
    tools
        .iter()
        .position(|choice| tool_id(&choice.tool) == tool_id(&launch.tool))
        .unwrap_or(0)
}

pub(crate) fn selected_tool<'a>(tools: &'a [ToolChoice], state: &ListState) -> &'a ToolChoice {
    let index = state
        .selected()
        .unwrap_or(0)
        .min(tools.len().saturating_sub(1));
    &tools[index]
}
