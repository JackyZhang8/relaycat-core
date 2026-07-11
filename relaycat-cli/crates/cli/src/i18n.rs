#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliLanguage {
    En,
    ZhHans,
}

impl CliLanguage {
    pub fn from_system_locale() -> Self {
        Self::from_locale_tag(
            std::env::var("LC_ALL")
                .ok()
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    std::env::var("LC_MESSAGES")
                        .ok()
                        .filter(|value| !value.is_empty())
                })
                .or_else(|| std::env::var("LANG").ok().filter(|value| !value.is_empty()))
                .as_deref(),
        )
    }

    pub fn from_locale_tag(locale: Option<&str>) -> Self {
        let Some(locale) = locale else {
            return Self::En;
        };
        if locale.to_ascii_lowercase().starts_with("zh") {
            Self::ZhHans
        } else {
            Self::En
        }
    }

    pub fn t(self, en: &'static str, zh: &'static str) -> &'static str {
        match self {
            Self::En => en,
            Self::ZhHans => zh,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CliLanguage;

    #[test]
    fn system_locale_prefix_selects_chinese() {
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
    fn t_returns_selected_language() {
        assert_eq!(CliLanguage::En.t("English", "中文"), "English");
        assert_eq!(CliLanguage::ZhHans.t("English", "中文"), "中文");
    }
}
