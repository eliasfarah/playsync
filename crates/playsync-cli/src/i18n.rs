//! Deteccao/selecao de idioma da CLI/TUI. Prioridade: `Config::language`
//! (se setado) > locale do sistema (`LANGUAGE`/`LC_ALL`/`LC_MESSAGES`/`LANG`,
//! nessa ordem, mesma prioridade que ferramentas gettext-based usam) > "en".

/// Codigos suportados, na mesma ordem mostrada no seletor de idioma da TUI.
pub const SUPPORTED_LANGUAGES: &[&str] = &["en", "pt-BR", "es", "fr", "de", "zh-CN", "ja", "ru"];

pub fn display_name(code: &str) -> &'static str {
    match code {
        "en" => "English",
        "pt-BR" => "Português (BR)",
        "es" => "Español",
        "fr" => "Français",
        "de" => "Deutsch",
        "zh-CN" => "简体中文",
        "ja" => "日本語",
        "ru" => "Русский",
        _ => "?",
    }
}

/// Resolve o idioma efetivo: `config_language` (se for um codigo suportado)
/// tem prioridade; senao cai pra deteccao do sistema.
pub fn resolve_language(config_language: Option<&str>) -> String {
    if let Some(lang) = config_language {
        if let Some(matched) = normalize_and_match(lang) {
            return matched;
        }
    }
    detect_system_language()
}

fn detect_system_language() -> String {
    for var in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(val) = std::env::var(var) {
            // `LANGUAGE` pode ser uma lista de prioridade separada por ":"
            // (ex: "pt_BR:en"), como em ferramentas gettext-based.
            for candidate in val.split(':') {
                if let Some(matched) = normalize_and_match(candidate) {
                    return matched;
                }
            }
        }
    }
    "en".to_string()
}

/// Normaliza um valor de locale (`"pt_BR.UTF-8"`, `"zh-CN"`, `"C"`, ...) e
/// casa com um dos `SUPPORTED_LANGUAGES`. So considera o codigo de idioma
/// primario (antes do "-"/"_") — variantes regionais nao suportadas caem no
/// idioma base mais proximo (ex: `es_MX` -> `es`, `pt_PT` -> `pt-BR`, unico
/// portugues suportado hoje).
fn normalize_and_match(raw: &str) -> Option<String> {
    let lang_part = raw.split(['.', '@']).next().unwrap_or(raw);
    if lang_part.is_empty() || lang_part.eq_ignore_ascii_case("C") || lang_part.eq_ignore_ascii_case("POSIX") {
        return None;
    }
    let normalized = lang_part.replace('_', "-");
    let primary = normalized.split('-').next().unwrap_or(&normalized).to_lowercase();

    let matched = match primary.as_str() {
        "pt" => "pt-BR",
        "zh" => "zh-CN",
        "en" => "en",
        "es" => "es",
        "fr" => "fr",
        "de" => "de",
        "ja" => "ja",
        "ru" => "ru",
        _ => return None,
    };
    Some(matched.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_posix_locale_strings() {
        assert_eq!(normalize_and_match("pt_BR.UTF-8"), Some("pt-BR".to_string()));
        assert_eq!(normalize_and_match("en_US.UTF-8"), Some("en".to_string()));
        assert_eq!(normalize_and_match("zh_CN.UTF-8"), Some("zh-CN".to_string()));
        assert_eq!(normalize_and_match("es_MX"), Some("es".to_string()));
        assert_eq!(normalize_and_match("pt_PT"), Some("pt-BR".to_string()));
        assert_eq!(normalize_and_match("C"), None);
        assert_eq!(normalize_and_match("POSIX"), None);
        assert_eq!(normalize_and_match("it_IT"), None);
    }

    #[test]
    fn config_language_overrides_detection() {
        assert_eq!(resolve_language(Some("fr")), "fr".to_string());
        assert_eq!(resolve_language(Some("not-a-real-lang")), detect_system_language());
    }
}
