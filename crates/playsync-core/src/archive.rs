//! Compacta um save (arquivo ou diretorio inteiro, recursivamente) num .zip.
//! Os backends de nuvem so sabem enviar arquivos unicos; a maioria dos saves
//! reais e um diretorio (ex: prefixo Proton).

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

/// Compacta `source` em `dest` (sobrescrevendo se ja existir). Cria os
/// diretorios pais de `dest` conforme necessario — e assim que vira tambem o
/// backup local (`dest` fica dentro de `~/PlaySync/<jogo>/...`).
pub fn zip_path(source: &Path, dest: &Path) -> Result<()> {
    anyhow::ensure!(source.exists(), "{} nao existe", source.display());

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("nao consegui criar {}", parent.display()))?;
    }

    let zip_file = File::create(dest)
        .with_context(|| format!("nao consegui criar {}", dest.display()))?;
    let mut writer = ZipWriter::new(zip_file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    if source.is_file() {
        let name = source
            .file_name()
            .context("caminho de save sem nome de arquivo")?
            .to_string_lossy();
        add_file(&mut writer, source, &name, options)?;
    } else {
        // Ancora os caminhos dentro do zip a partir do pai de `source`, pra
        // preservar o nome da pasta-raiz do save (ex: "LocalLow/...") em vez
        // de achatar tudo na raiz do zip.
        let anchor = source.parent().unwrap_or(source);
        for entry in walkdir::WalkDir::new(source) {
            let entry =
                entry.with_context(|| format!("erro ao percorrer {}", source.display()))?;
            let relative = entry
                .path()
                .strip_prefix(anchor)
                .context("entrada fora da arvore esperada")?;
            let name = relative.to_string_lossy();

            if entry.file_type().is_dir() {
                writer.add_directory(name, options)?;
            } else if entry.file_type().is_file() {
                add_file(&mut writer, entry.path(), &name, options)?;
            }
        }
    }

    writer.finish().context("falha ao finalizar o zip")?;
    Ok(())
}

fn add_file(
    writer: &mut ZipWriter<File>,
    path: &Path,
    name: &str,
    options: SimpleFileOptions,
) -> Result<()> {
    writer
        .start_file(name, options)
        .with_context(|| format!("falha ao iniciar entrada {name} no zip"))?;
    let mut buf = Vec::new();
    File::open(path)
        .with_context(|| format!("nao consegui abrir {}", path.display()))?
        .read_to_end(&mut buf)?;
    writer
        .write_all(&buf)
        .with_context(|| format!("falha ao escrever {name} no zip"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn entry_names(zip_path: &Path) -> HashSet<String> {
        let file = File::open(zip_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect()
    }

    #[test]
    fn zips_a_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("save.dat");
        std::fs::write(&file, b"hello").unwrap();
        let dest = dir.path().join("out").join("save.zip");

        zip_path(&file, &dest).unwrap();
        assert_eq!(entry_names(&dest), HashSet::from(["save.dat".to_string()]));
    }

    #[test]
    fn zips_a_directory_recursively_preserving_root_name() {
        let dir = tempfile::tempdir().unwrap();
        let save_dir = dir.path().join("LocalLow");
        std::fs::create_dir_all(save_dir.join("sub")).unwrap();
        std::fs::write(save_dir.join("file1.txt"), b"a").unwrap();
        std::fs::write(save_dir.join("sub").join("file2.txt"), b"b").unwrap();
        let dest = dir.path().join("out.zip");

        zip_path(&save_dir, &dest).unwrap();
        let names = entry_names(&dest);
        assert!(names.contains("LocalLow/"));
        assert!(names.contains("LocalLow/file1.txt"));
        assert!(names.contains("LocalLow/sub/"));
        assert!(names.contains("LocalLow/sub/file2.txt"));
    }

    #[test]
    fn overwrites_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("save.dat");
        let dest = dir.path().join("save.zip");

        std::fs::write(&file, b"first").unwrap();
        zip_path(&file, &dest).unwrap();
        std::fs::write(&file, b"second content, different size").unwrap();
        zip_path(&file, &dest).unwrap();

        assert_eq!(entry_names(&dest), HashSet::from(["save.dat".to_string()]));
    }

    #[test]
    fn errors_on_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = Path::new("/nonexistent/playsync-test-path");
        assert!(zip_path(missing, &dir.path().join("out.zip")).is_err());
    }
}
