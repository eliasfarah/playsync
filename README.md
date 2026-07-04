# PlaySync

Backup automático de saves de jogos Steam para a nuvem, no Linux — 100% em
segundo plano. Um daemon (`systemd --user`) detecta quando um jogo abre e
fecha, e sincroniza os saves (nativos ou via Proton) assim que ele fecha, sem
precisar de watcher contínuo nas pastas de save.

## Recursos

- Detecção automática de jogos instalados e suas pastas de save (bibliotecas
  Steam, Proton, espelho da Steam Cloud).
- Gatilho de abrir/fechar jogo sem tocar em nenhuma configuração da Steam.
- Backup local **e** na nuvem, organizados da mesma forma nos dois lados:
  ```
  PlaySync/
    DARK SOULS II Scholar of the First Sin/
      save-0.zip
      save-1.zip
    Marvel's Spider-Man 2/
      save.zip
  ```
  (nomes sanitizados — sem caracteres problemáticos pra sistema de arquivos
  ou provedor de nuvem.)
- Backends de nuvem: **Google Drive** e **Box** (OAuth2, o app só enxerga os
  arquivos que ele mesmo cria).
- CLI simples (`playsync status` / `sync` / `history` / `cloud connect`).
- Histórico de backups em sqlite.

## Como funciona

Workspace Rust com três crates:

| Crate | O que é |
|---|---|
| `playsyncd` | Daemon (`systemd --user`) — detecta jogo abrindo/fechando e dispara o backup |
| `playsync` | CLI/TUI — fala com o daemon por Unix socket (`$XDG_RUNTIME_DIR/playsync.sock`) |
| `playsync-core` | Lib compartilhada: detecção Steam, histórico sqlite, protocolo IPC, backends de nuvem |

O gatilho de abrir/fechar jogo funciona por polling em `/proc/*/environ`
procurando `SteamAppId`/`SteamGameId` (o mesmo sinal que MangoHud/gamescope
usam) — não depende de nenhum arquivo de configuração da Steam.

## Instalação

### A partir do código-fonte

Pré-requisitos: [Rust](https://rustup.rs/) (edition 2021, `rust-version =
"1.81"` ou mais novo).

```bash
git clone https://github.com/eliasfarah/playsync.git
cd playsync
cargo build --release --workspace
```

Instala os binários e a unit do `systemd --user`:

```bash
install -Dm755 target/release/playsync  ~/.local/bin/playsync
install -Dm755 target/release/playsyncd ~/.local/bin/playsyncd
install -Dm644 packaging/systemd/playsyncd.service \
  ~/.config/systemd/user/playsyncd.service

systemctl --user daemon-reload
systemctl --user enable --now playsyncd
```

> **Importante:** não adicione `ProtectSystem=`, `ProtectHome=`,
> `PrivateTmp=` ou similares na unit do systemd. Essas diretivas colocam o
> daemon num mount/user namespace separado e quebram silenciosamente a
> leitura de `/proc/<pid>/environ` de processos que não são filhos dele —
> ou seja, o gatilho principal para de funcionar sem nenhum log de erro.
> `NoNewPrivileges=yes` é seguro e já vem habilitado na unit fornecida.

### Pacote `.deb`

```bash
cargo install cargo-deb
cargo build --release --workspace
cargo deb -p playsync --no-build
```

Gera um `.deb` com os dois binários e a unit do systemd
(`crates/playsync-cli/Cargo.toml` tem os metadados do pacote).

### Arch Linux (AUR)

```bash
cd packaging/aur
makepkg -si
```

(usa o `PKGBUILD` deste repo — requer uma tag `vX.Y.Z` publicada no GitHub
correspondente a `pkgver`.)

## Configuração inicial

### Google Drive

1. Crie um projeto e um **OAuth client ID** do tipo *Desktop app* em
   [console.cloud.google.com/apis/credentials](https://console.cloud.google.com/apis/credentials).
2. Adicione `http://localhost:8085` como redirect URI.
3. Baixe o JSON de credenciais e salve em
   `~/.config/playsync/gdrive_client_secret.json`.
4. Conecte:
   ```bash
   playsync cloud connect google-drive
   ```
   Isso abre o navegador pra autorizar; o token fica em
   `~/.config/playsync/tokens/gdrive.json` (permissão `0600`).

### Box

1. Crie um **Custom App** em
   [app.box.com/developers/console](https://app.box.com/developers/console)
   com "User Authentication (OAuth 2.0)".
2. Adicione `http://localhost:8086` como redirect URI.
3. Salve `client_id`/`client_secret` em
   `~/.config/playsync/box_client_secret.json`:
   ```json
   { "client_id": "...", "client_secret": "..." }
   ```
4. Conecte:
   ```bash
   playsync cloud connect box
   ```

Só um provedor fica ativo por vez (o último `cloud connect` bem-sucedido).

## Uso

```bash
playsync status              # tabela: jogo, ultimo backup, status de sync
playsync sync                # forca sync de todos os jogos elegiveis agora
playsync sync --app-id ID    # forca sync so de um jogo (AppID da Steam)
playsync history             # historico recente de backups (sucesso/falha, destino)
playsync history --limit N   # historico com um limite customizado (padrao: 20)
playsync cloud connect <google-drive|box>   # (re)autoriza um provedor
playsync cloud test-upload <google-drive|box>  # valida o pipeline OAuth2 + upload
```

No dia a dia, depois de configurado, nenhum comando é necessário — o daemon
detecta o jogo fechando e sincroniza sozinho após um debounce (5s por
padrão, configurável).

### Configuração opcional

`~/.config/playsync/config.toml`:

```toml
cloud_provider = "google-drive"   # ou "box" — setado por `cloud connect`
ignored_app_ids = [12345]         # AppIDs pra nunca sincronizar
sync_debounce_secs = 5            # espera apos fechar o jogo antes de sincronizar
local_backup_dir = "/caminho/custom"  # padrao: ~/PlaySync
```

## Desinstalação

```bash
systemctl --user disable --now playsyncd
rm ~/.config/systemd/user/playsyncd.service
systemctl --user daemon-reload

rm ~/.local/bin/playsync ~/.local/bin/playsyncd

# Config, credenciais e tokens OAuth2:
rm -rf ~/.config/playsync

# Historico (sqlite):
rm -rf ~/.local/state/playsync
```

Os backups em `~/PlaySync/` (ou o `local_backup_dir` configurado) **não** são
apagados por nenhum dos passos acima — são seus arquivos, apague manualmente
se quiser. O mesmo vale para os arquivos já enviados à nuvem (pasta
`PlaySync/` no Google Drive/Box).

## Licença

MIT — veja [LICENSE](LICENSE).
