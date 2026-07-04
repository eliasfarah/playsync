//! Sanitizacao de nomes de jogos pra uso seguro como nome de pasta/arquivo,
//! tanto no sistema de arquivos local quanto em provedores de nuvem.

/// Remove/substitui caracteres problematicos (reservados no Windows, so
/// decorativos como TM/copyright, ou de controle) mantendo o resto do nome
/// (acentos, espacos, apostrofos) intacto — tanto Linux quanto o Google Drive
/// aceitam UTF-8 livremente, o cuidado extra e so pra evitar surpresa em
/// outros sistemas de arquivos (ex: um Drive sincronizado depois num Windows).
pub fn sanitize(name: &str) -> String {
    let replaced: String = name
        .chars()
        .filter_map(|c| match c {
            '/' | '\\' | '*' | '?' | '"' | '<' | '>' | '|' => Some('_'),
            ':' | '™' | '®' | '©' => None,
            c if c.is_control() => Some(' '),
            c => Some(c),
        })
        .collect();

    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_end_matches('.').trim();

    if trimmed.is_empty() {
        "jogo".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_trademark_and_colon() {
        assert_eq!(
            sanitize("DARK SOULS™ II: Scholar of the First Sin"),
            "DARK SOULS II Scholar of the First Sin"
        );
    }

    #[test]
    fn replaces_reserved_characters() {
        assert_eq!(sanitize("Foo/Bar*Baz?"), "Foo_Bar_Baz_");
    }

    #[test]
    fn keeps_apostrophes_and_accents() {
        assert_eq!(sanitize("Marvel's Spider-Man 2"), "Marvel's Spider-Man 2");
    }

    #[test]
    fn falls_back_on_empty_result() {
        assert_eq!(sanitize(":::"), "jogo");
    }
}
