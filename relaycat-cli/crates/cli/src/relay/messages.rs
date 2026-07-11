use super::*;

pub(crate) fn initial_relay_unavailable_message(target: &TargetCommand) -> String {
    format!(
        "initial relay connection failed; {} was not started. Start or check the relay, then run relaycat again",
        target.program
    )
}

pub(crate) fn cancelled_by_user_message() -> &'static str {
    "cancelled by user"
}

pub(crate) fn secure_pairing_header_message(language: CliLanguage) -> &'static str {
    language.t("RelayCat pairing", "RelayCat 配对")
}

pub(crate) fn relaycat_logs_message(path: &Path, language: CliLanguage) -> String {
    format!("{}:\n  {}", language.t("Logs", "日志"), path.display())
}

pub(crate) fn pairing_url_message(url: &str, language: CliLanguage) -> String {
    format!("{}:\n  {}", language.t("Pairing URL", "配对链接"), url)
}
