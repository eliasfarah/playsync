//! Nomenclatura e retencao de versoes de backup.
//!
//! Cada sync grava um arquivo NOVO com timestamp (`save-<ts>.zip` ou
//! `save-{idx}-<ts>.zip`, se o jogo tiver mais de um save_path) em vez de
//! sempre sobrescrever o mesmo `save.zip` — assim, um sync automatico ruim
//! (ex: o jogo foi aberto sem save, criou um save novo/vazio, e o
//! fechamento disparou a sincronizacao de volta) nao destroi a unica copia
//! boa que existia. So as `Config::backup_versions_to_keep` mais recentes
//! sao mantidas (local e na nuvem); o resto e podado a cada sync.
//!
//! Arquivos da nomenclatura antiga (`save.zip`/`save-{idx}.zip`, sem
//! timestamp, de antes dessa mudanca) nao sao reconhecidos como versao por
//! este modulo — ficam no disco/nuvem como estao, sem serem listados nem
//! podados, ate serem sobrescritos manualmente ou apagados a mao.

use chrono::{DateTime, Utc};

/// Prefixo do nome de arquivo pra um dado `path_index` de um jogo com
/// `total_paths` save_paths — distingue indices diferentes (`save-0-`,
/// `save-1-`, ...) do caso comum de um so path (`save-`).
pub fn file_prefix(path_index: usize, total_paths: usize) -> String {
    if total_paths > 1 {
        format!("save-{path_index}-")
    } else {
        "save-".to_string()
    }
}

/// Nome de arquivo pra uma nova versao gravada em `timestamp` (UTC). O
/// formato (`%Y%m%dT%H%M%SZ`) ordena lexicograficamente igual a
/// cronologicamente, entao `sort_versions` nao precisa parsear nada.
pub fn version_file_name(path_index: usize, total_paths: usize, timestamp: DateTime<Utc>) -> String {
    format!(
        "{}{}.zip",
        file_prefix(path_index, total_paths),
        timestamp.format("%Y%m%dT%H%M%SZ")
    )
}

/// Filtra `names` pros que pertencem a esse `prefix` (mesmo path_index) e
/// devolve ordenados do mais antigo pro mais novo.
pub fn sort_versions(names: Vec<String>, prefix: &str) -> Vec<String> {
    let mut matching: Vec<String> = names
        .into_iter()
        .filter(|n| n.starts_with(prefix) && n.ends_with(".zip"))
        .collect();
    matching.sort();
    matching
}

/// Quais nomes (de uma lista ja ordenada do mais antigo pro mais novo) devem
/// ser podados: todos exceto os `keep` mais recentes.
pub fn names_to_prune(sorted_oldest_first: &[String], keep: usize) -> &[String] {
    let excess = sorted_oldest_first.len().saturating_sub(keep);
    &sorted_oldest_first[..excess]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn file_prefix_distinguishes_single_vs_multiple_paths() {
        assert_eq!(file_prefix(0, 1), "save-");
        assert_eq!(file_prefix(0, 2), "save-0-");
        assert_eq!(file_prefix(1, 2), "save-1-");
    }

    #[test]
    fn version_file_name_sorts_lexicographically_by_time() {
        let earlier = Utc.with_ymd_and_hms(2026, 7, 4, 10, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 7, 4, 11, 0, 0).unwrap();
        let a = version_file_name(0, 1, earlier);
        let b = version_file_name(0, 1, later);
        assert!(a < b, "{a} deveria vir antes de {b}");
    }

    #[test]
    fn sort_versions_filters_by_prefix_and_ignores_old_naming() {
        let names = vec![
            "save-0-20260704T100000Z.zip".to_string(),
            "save-1-20260704T110000Z.zip".to_string(),
            "save-0-20260703T090000Z.zip".to_string(),
            "save-0.zip".to_string(), // nomenclatura antiga, sem timestamp
            "outro-arquivo.txt".to_string(),
        ];
        let sorted = sort_versions(names, "save-0-");
        assert_eq!(
            sorted,
            vec!["save-0-20260703T090000Z.zip".to_string(), "save-0-20260704T100000Z.zip".to_string()]
        );
    }

    #[test]
    fn names_to_prune_keeps_only_the_most_recent() {
        let sorted = vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()];
        assert_eq!(names_to_prune(&sorted, 2), &["a".to_string(), "b".to_string()]);
        assert_eq!(names_to_prune(&sorted, 10), &[] as &[String]);
    }
}
