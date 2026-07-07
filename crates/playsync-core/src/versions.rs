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
//! timestamp, de antes dessa mudanca existir) SAO reconhecidos por
//! `sort_versions` como a versao mais antiga de cada `path_index` — sem
//! isso, um jogo cujo unico backup na nuvem seja de antes dessa migracao
//! ficava com a lista de versoes vazia (nada pra "baixar da nuvem"/restaurar,
//! mesmo tendo um backup de verdade la). Sempre tratado como o MAIS ANTIGO,
//! nunca o mais recente — mesmo que, por nao ter timestamp, ordenasse depois
//! na comparacao lexicografica pura.

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

/// Nome do arquivo "legado" (de antes da nomenclatura com timestamp) que
/// corresponde a esse `prefix`: `save.zip` (path unico, `prefix = "save-"`)
/// ou `save-{idx}.zip` (multiplos paths, `prefix = "save-{idx}-"`) — so tira
/// o traco final do prefixo e troca por `.zip`.
fn legacy_version_name(prefix: &str) -> String {
    format!("{}.zip", prefix.trim_end_matches('-'))
}

/// Filtra `names` pros que pertencem a esse `prefix` (mesmo path_index) e
/// devolve ordenados do mais antigo pro mais novo — incluindo o nome legado
/// sem timestamp, se existir, sempre como o primeiro (mais antigo) da lista:
/// como ele nao carrega data nenhuma, nao da pra confiar na ordenacao
/// lexicografica pura pra saber onde ele entra (o `.` do `.zip` ordena antes
/// do `-` de um timestamp de verdade, o que colocaria o legado por ULTIMO,
/// como se fosse o mais recente — exatamente o oposto do que e).
pub fn sort_versions(names: Vec<String>, prefix: &str) -> Vec<String> {
    let legacy_name = legacy_version_name(prefix);
    let (mut legacy, mut dated): (Vec<String>, Vec<String>) = names
        .into_iter()
        .filter(|n| n.ends_with(".zip") && (*n == legacy_name || n.starts_with(prefix)))
        .partition(|n| *n == legacy_name);
    legacy.sort();
    dated.sort();
    legacy.extend(dated);
    legacy
}

/// Quais nomes (de uma lista ja ordenada do mais antigo pro mais novo) devem
/// ser podados: todos exceto os `keep` mais recentes.
pub fn names_to_prune(sorted_oldest_first: &[String], keep: usize) -> &[String] {
    let excess = sorted_oldest_first.len().saturating_sub(keep);
    &sorted_oldest_first[..excess]
}

/// Inverso de `version_file_name`: extrai o timestamp embutido no nome,
/// dado o `prefix` esperado (`file_prefix`). `None` se o nome nao bater com
/// o prefixo ou o timestamp nao for parseavel (ex: nomenclatura antiga,
/// sem timestamp) — usado pra correlacionar um arquivo de versao com a
/// entrada de historico mais proxima (duracao da sessao que o gerou).
pub fn parse_version_timestamp(name: &str, prefix: &str) -> Option<DateTime<Utc>> {
    let rest = name.strip_prefix(prefix)?.strip_suffix(".zip")?;
    // O "Z" no formato e literal (sempre UTC, escrito por `version_file_name`
    // via `Utc::now()`), nao o especificador `%Z`/`%z` — por isso parseia
    // como `NaiveDateTime` e so depois assume UTC, em vez de
    // `DateTime::parse_from_str` (que exigiria um offset de verdade no texto).
    chrono::NaiveDateTime::parse_from_str(rest, "%Y%m%dT%H%M%SZ")
        .ok()
        .map(|naive| naive.and_utc())
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
    fn sort_versions_filters_by_prefix_and_ignores_unrelated_files() {
        let names = vec![
            "save-0-20260704T100000Z.zip".to_string(),
            "save-1-20260704T110000Z.zip".to_string(),
            "save-0-20260703T090000Z.zip".to_string(),
            "outro-arquivo.txt".to_string(),
        ];
        let sorted = sort_versions(names, "save-0-");
        assert_eq!(
            sorted,
            vec!["save-0-20260703T090000Z.zip".to_string(), "save-0-20260704T100000Z.zip".to_string()]
        );
    }

    #[test]
    fn sort_versions_puts_legacy_undated_file_first_as_the_oldest() {
        let names = vec![
            "save-0-20260704T100000Z.zip".to_string(),
            "save-0.zip".to_string(), // nomenclatura antiga, sem timestamp — na verdade a mais antiga
        ];
        let sorted = sort_versions(names, "save-0-");
        assert_eq!(
            sorted,
            vec!["save-0.zip".to_string(), "save-0-20260704T100000Z.zip".to_string()]
        );
    }

    #[test]
    fn sort_versions_recognizes_single_path_legacy_name() {
        let names = vec!["save-20260704T100000Z.zip".to_string(), "save.zip".to_string()];
        let sorted = sort_versions(names, "save-");
        assert_eq!(sorted, vec!["save.zip".to_string(), "save-20260704T100000Z.zip".to_string()]);
    }

    #[test]
    fn names_to_prune_keeps_only_the_most_recent() {
        let sorted = vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()];
        assert_eq!(names_to_prune(&sorted, 2), &["a".to_string(), "b".to_string()]);
        assert_eq!(names_to_prune(&sorted, 10), &[] as &[String]);
    }

    #[test]
    fn parse_version_timestamp_roundtrips_with_version_file_name() {
        let ts = Utc.with_ymd_and_hms(2026, 7, 4, 19, 20, 14).unwrap();
        let name = version_file_name(0, 1, ts);
        assert_eq!(parse_version_timestamp(&name, &file_prefix(0, 1)), Some(ts));
    }

    #[test]
    fn parse_version_timestamp_none_for_old_naming_or_wrong_prefix() {
        assert_eq!(parse_version_timestamp("save-0.zip", "save-"), None);
        assert_eq!(
            parse_version_timestamp("save-1-20260704T192014Z.zip", "save-"),
            None
        );
    }
}
