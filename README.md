# PlaySync

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/eliasfarah/playsync)](https://github.com/eliasfarah/playsync/releases/latest)
[![Rust](https://img.shields.io/badge/rust-2021-orange.svg)](https://www.rust-lang.org)

Automatic cloud backup for Steam game saves, on Linux — fully in the
background. **[Português abaixo ⬇](#playsync-pt-br)**

---

## English

A `systemd --user` daemon detects when a game starts and stops, and syncs its
save data (native or via Proton) as soon as it closes — no continuous
filesystem watcher, no manual steps once it's set up.

### Features

- Automatic detection of installed Steam games and their save folders (Steam
  libraries, Proton prefixes, Steam Cloud mirror).
- Game start/stop trigger with zero Steam configuration required.
- Local **and** cloud backup, mirrored identically on both sides:
  ```
  PlaySync/
    DARK SOULS II Scholar of the First Sin/
      save-0.zip
      save-1.zip
    Marvel's Spider-Man 2/
      save.zip
  ```
  (game names are sanitized — no characters that would trip up a filesystem
  or cloud provider.)
- Cloud backends: **Google Drive** and **Box** (OAuth2; the app only ever
  sees the files it creates itself).
- Auto-restore on launch: if the cloud has a newer save than the one on this
  machine (e.g. you played on another PC), it's downloaded and restored
  automatically before you'd otherwise start playing on a stale save. On by
  default whenever a cloud provider is configured; toggle with `playsync
  config auto-restore <on|off>` or from the TUI settings screen (`c`).
- CLI/TUI available in 8 languages (English, Português (BR), Español,
  Français, Deutsch, 简体中文, 日本語, Русский) — auto-detected from the
  system locale, or set explicitly with `playsync config language <code>`.
- Simple CLI (`playsync status` / `sync` / `history` / `cloud connect`).
- Backup history kept in sqlite.

> **Status:** early (`v0.1.0`), built and used daily on a single real
> machine. Should work on any modern Linux + systemd setup, but hasn't been
> tested across distros yet — issues/PRs welcome.

### How it works

A Rust workspace with three crates:

| Crate | What it is |
|---|---|
| `playsyncd` | Daemon (`systemd --user`) — detects the game starting/stopping and triggers the backup |
| `playsync` | CLI/TUI — talks to the daemon over a Unix socket (`$XDG_RUNTIME_DIR/playsync.sock`) |
| `playsync-core` | Shared library: Steam detection, sqlite history, IPC protocol, cloud backends |

The start/stop trigger works by polling `/proc/*/environ` looking for
`SteamAppId`/`SteamGameId` — the same signal MangoHud/gamescope rely on — so
it doesn't depend on any Steam configuration file.

### Installation

#### From source

Prerequisites: [Rust](https://rustup.rs/) (edition 2021, `rust-version =
"1.81"` or newer).

```bash
git clone https://github.com/eliasfarah/playsync.git
cd playsync
cargo build --release --workspace
```

Install the binaries and the `systemd --user` unit:

```bash
install -Dm755 target/release/playsync  ~/.local/bin/playsync
install -Dm755 target/release/playsyncd ~/.local/bin/playsyncd
install -Dm644 packaging/systemd/playsyncd.service \
  ~/.config/systemd/user/playsyncd.service

systemctl --user daemon-reload
systemctl --user enable --now playsyncd
```

> **Important:** do not add `ProtectSystem=`, `ProtectHome=`, `PrivateTmp=`
> or similar directives to the systemd unit. Those put the daemon in a
> separate mount/user namespace and silently break reading
> `/proc/<pid>/environ` for processes that aren't its children — meaning the
> core trigger stops working with no error logged anywhere.
> `NoNewPrivileges=yes` is safe and already enabled in the provided unit.

#### Prebuilt binaries

Download `playsync-<version>-x86_64-linux.tar.gz` from the [latest
release](https://github.com/eliasfarah/playsync/releases/latest) — it
contains both binaries, the systemd unit, README and LICENSE. Follow the same
`install -Dm755`/`systemctl` steps above instead of building from source.

#### `.deb` package

Download the `.deb` from the [latest
release](https://github.com/eliasfarah/playsync/releases/latest) and:

```bash
sudo apt install ./playsync_<version>_amd64.deb
```

Or build it yourself:

```bash
cargo install cargo-deb
cargo build --release --workspace
cargo deb -p playsync --no-build
```

#### Arch Linux (AUR)

```bash
cd packaging/aur
makepkg -si
```

(uses this repo's `PKGBUILD` — requires a published `vX.Y.Z` GitHub tag
matching `pkgver`.)

> **Known issue:** `makepkg` currently fails to link due to a build quirk in
> a transitive TLS dependency (`aws-lc-sys`) that doesn't reproduce under a
> plain `cargo build` — see the project's `MEMORY.md` for what's been ruled
> out so far. Building from source or using the prebuilt binaries/`.deb`
> works fine in the meantime.

### Initial setup

#### Google Drive

1. Create a project and an **OAuth client ID** of type *Desktop app* at
   [console.cloud.google.com/apis/credentials](https://console.cloud.google.com/apis/credentials).
2. Add `http://localhost:8085` as a redirect URI.
3. Save the Client ID/Secret with:
   ```bash
   playsync cloud setup google-drive
   ```
   Prompts for the Client ID and Client Secret (the secret isn't echoed) and
   writes `~/.config/playsync/gdrive_client_secret.json` for you — or pass
   `--client-id`/`--client-secret` to skip the prompts. Alternatively,
   download the credentials JSON from the console and save it at that path
   yourself.
4. Connect:
   ```bash
   playsync cloud connect google-drive
   ```
   This opens your browser to authorize; the token is stored in
   `~/.config/playsync/tokens/gdrive.json` (mode `0600`).

#### Box

1. Create a **Custom App** at
   [app.box.com/developers/console](https://app.box.com/developers/console)
   with "User Authentication (OAuth 2.0)".
2. Add `http://localhost:8086` as a redirect URI.
3. Save the Client ID/Secret with:
   ```bash
   playsync cloud setup box
   ```
   (same prompts/flags as above) — or write
   `~/.config/playsync/box_client_secret.json` yourself:
   ```json
   { "client_id": "...", "client_secret": "..." }
   ```
4. Connect:
   ```bash
   playsync cloud connect box
   ```

Only one provider is active at a time (whichever `cloud connect` succeeded
most recently).

### Usage

Running `playsync` with no arguments opens an interactive TUI: navigate the
game list with `↑↓`, press `Enter` on a game for a per-game action menu
(sync now, download from cloud only, restore from local, or download +
restore), `s` to sync everything, `r` to refresh, `c` for settings (cloud
provider, auto-restore on launch, language, and — from there — entering the
Client ID/Secret for a provider), `q` to quit. Destructive
actions (restoring over the live save) ask for confirmation first.

Everything is also available non-interactively:

```bash
playsync status              # table: game, last backup, sync status
playsync sync                # force-sync all eligible games now
playsync sync --app-id ID    # force-sync a single game (Steam AppID)
playsync history             # recent backup history (success/failure, destination)
playsync history --limit N   # history with a custom limit (default: 20)
playsync cloud setup <google-drive|box>        # save the OAuth Client ID/Secret (prompts interactively)
playsync cloud connect <google-drive|box>      # (re)authorize a provider
playsync cloud test-upload <google-drive|box>  # sanity-check the OAuth2 + upload pipeline
playsync restore --app-id ID --source <local|google-drive|box>  # restore a backup over the current save
playsync config auto-restore <on|off>          # toggle auto-restore-on-launch
playsync config language <code>                # en, pt-BR, es, fr, de, zh-CN, ja, ru
```

Day to day, once set up, no command is needed — the daemon detects the game
closing and syncs on its own after a debounce (5s by default, configurable).

#### Restoring a backup

```bash
# a game with more than one save folder lists them (with an index) instead of restoring:
playsync restore --app-id ID --source local

# restore the most recent backup, from local or from a cloud provider:
playsync restore --app-id ID --source local --path-index 0
playsync restore --app-id ID --source google-drive --path-index 0
playsync restore --app-id ID --source box --path-index 0

# see the available versions for a save path (oldest first, most recent last):
playsync restore --app-id ID --source local --path-index 0 --list-versions

# restore a specific one instead of the most recent (exact name from --list-versions):
playsync restore --app-id ID --source local --path-index 0 --version save-20260704T192014Z.zip

# skip the confirmation prompt (e.g. scripting):
playsync restore --app-id ID --source local --path-index 0 --yes
```

Restoring **overwrites the live save folder** with the backup's contents
(the existing folder/file is removed first, then the backup is extracted in
its place) — you'll be asked to confirm unless `--yes` is passed.

> Every sync writes a new timestamped file instead of overwriting the same
> one, both locally and in the cloud — a bad automatic sync (e.g. the game
> was launched without a save and created a fresh/empty one, then closing it
> synced *that*) can't destroy the only good backup you had. Only the most
> recent `backup_versions_to_keep` (5 by default) are kept; older ones are
> pruned automatically. If the live save folder is gone entirely (deleted,
> corrupted), `restore` falls back to the path recorded in backup history
> instead of refusing to run.

`--list-versions` also shows how each backup came to be: `(sessão de 42min)`
for one triggered by closing the game after playing for that long, `(manual)`
for one from `playsync sync`/the TUI, or `⚠ sessao curta` for a session
shorter than `short_session_warning_secs` (2 minutes by default) — a strong
sign it was a quick test on a fresh/empty save rather than real progress,
not something to restore from. The TUI's restore/download actions show the
same picker whenever more than one version exists for a save path.

#### Optional configuration

`~/.config/playsync/config.toml`:

```toml
cloud_provider = "google-drive"      # or "box" — set by `cloud connect`
ignored_app_ids = [12345]            # AppIDs to never sync
sync_debounce_secs = 5               # wait after the game closes before syncing
local_backup_dir = "/custom/path"    # default: ~/PlaySync
backup_versions_to_keep = 5          # how many timestamped backups to keep per save path
short_session_warning_secs = 120     # sessions shorter than this are flagged in --list-versions
language = "pt-BR"                   # unset: auto-detected from the system locale, falls back to English
auto_restore_on_launch = true        # unset: on by default whenever cloud_provider is set
```

### Uninstalling

```bash
systemctl --user disable --now playsyncd
rm ~/.config/systemd/user/playsyncd.service
systemctl --user daemon-reload

rm ~/.local/bin/playsync ~/.local/bin/playsyncd

# Config, OAuth2 credentials and tokens:
rm -rf ~/.config/playsync

# Backup history (sqlite):
rm -rf ~/.local/state/playsync
```

Backups in `~/PlaySync/` (or your configured `local_backup_dir`) are **not**
deleted by any of the steps above — they're your files, remove them manually
if you want to. The same goes for anything already uploaded to the cloud
(the `PlaySync/` folder in Google Drive/Box).

### Credits

Save file location detection is powered by the [Ludusavi
manifest](https://github.com/mtkennerly/ludusavi-manifest) (MIT), a
community-curated database of ~19k games' save locations built for
[Ludusavi](https://github.com/mtkennerly/ludusavi). PlaySync caches it
locally and falls back to heuristic directory scanning only for games not
yet in the manifest.

### License

MIT — see [LICENSE](LICENSE).

---

## PlaySync (PT-BR)

**[⬆ English version above](#playsync)**

Backup automático de saves de jogos Steam para a nuvem, no Linux — 100% em
segundo plano. Um daemon (`systemd --user`) detecta quando um jogo abre e
fecha, e sincroniza os saves (nativos ou via Proton) assim que ele fecha, sem
precisar de watcher contínuo nas pastas de save.

### Recursos

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
- Restauração automática ao abrir o jogo: se a nuvem tiver um save mais
  recente do que o desta máquina (ex: você jogou em outro PC), ele é baixado
  e restaurado automaticamente antes de você jogar em cima de um save
  desatualizado. Ligado por padrão sempre que houver um provedor de nuvem
  configurado; dá pra ligar/desligar com `playsync config auto-restore
  <on|off>` ou pela tela de configurações da TUI (`c`).
- CLI/TUI disponível em 8 idiomas (English, Português (BR), Español,
  Français, Deutsch, 简体中文, 日本語, Русский) — detectado automaticamente
  a partir do locale do sistema, ou definido na mão com `playsync config
  language <código>`.
- CLI simples (`playsync status` / `sync` / `history` / `cloud connect`).
- Histórico de backups em sqlite.

> **Status:** projeto recente (`v0.1.0`), feito e usado no dia a dia numa
> máquina real. Deve funcionar em qualquer Linux moderno com systemd, mas
> ainda não foi testado em outras distros — issues/PRs são bem-vindos.

### Como funciona

Workspace Rust com três crates:

| Crate | O que é |
|---|---|
| `playsyncd` | Daemon (`systemd --user`) — detecta jogo abrindo/fechando e dispara o backup |
| `playsync` | CLI/TUI — fala com o daemon por Unix socket (`$XDG_RUNTIME_DIR/playsync.sock`) |
| `playsync-core` | Lib compartilhada: detecção Steam, histórico sqlite, protocolo IPC, backends de nuvem |

O gatilho de abrir/fechar jogo funciona por polling em `/proc/*/environ`
procurando `SteamAppId`/`SteamGameId` (o mesmo sinal que MangoHud/gamescope
usam) — não depende de nenhum arquivo de configuração da Steam.

### Instalação

#### A partir do código-fonte

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

#### Binários prontos

Baixe `playsync-<versão>-x86_64-linux.tar.gz` na [última
release](https://github.com/eliasfarah/playsync/releases/latest) — traz os
dois binários, a unit do systemd, README e LICENSE. Siga os mesmos passos de
`install -Dm755`/`systemctl` acima em vez de compilar do zero.

#### Pacote `.deb`

Baixe o `.deb` na [última
release](https://github.com/eliasfarah/playsync/releases/latest) e:

```bash
sudo apt install ./playsync_<versão>_amd64.deb
```

Ou gere o seu:

```bash
cargo install cargo-deb
cargo build --release --workspace
cargo deb -p playsync --no-build
```

#### Arch Linux (AUR)

```bash
cd packaging/aur
makepkg -si
```

(usa o `PKGBUILD` deste repo — requer uma tag `vX.Y.Z` publicada no GitHub
correspondente a `pkgver`.)

> **Problema conhecido:** o `makepkg` atualmente falha ao linkar por causa de
> uma peculiaridade de build numa dependência transitiva de TLS
> (`aws-lc-sys`) que não reproduz num `cargo build` comum — veja o
> `MEMORY.md` do projeto pra ver o que já foi descartado como causa.
> Compilar do fonte ou usar os binários/`.deb` prontos funciona normalmente
> enquanto isso.

### Configuração inicial

#### Google Drive

1. Crie um projeto e um **OAuth client ID** do tipo *Desktop app* em
   [console.cloud.google.com/apis/credentials](https://console.cloud.google.com/apis/credentials).
2. Adicione `http://localhost:8085` como redirect URI.
3. Salve o Client ID/Secret com:
   ```bash
   playsync cloud setup google-drive
   ```
   Pergunta o Client ID e o Client Secret (o secret não aparece na tela) e
   grava `~/.config/playsync/gdrive_client_secret.json` pra você — ou passe
   `--client-id`/`--client-secret` pra pular as perguntas. Também dá pra
   baixar o JSON de credenciais do console e salvar nesse caminho na mão.
4. Conecte:
   ```bash
   playsync cloud connect google-drive
   ```
   Isso abre o navegador pra autorizar; o token fica em
   `~/.config/playsync/tokens/gdrive.json` (permissão `0600`).

#### Box

1. Crie um **Custom App** em
   [app.box.com/developers/console](https://app.box.com/developers/console)
   com "User Authentication (OAuth 2.0)".
2. Adicione `http://localhost:8086` como redirect URI.
3. Salve o Client ID/Secret com:
   ```bash
   playsync cloud setup box
   ```
   (mesmas perguntas/flags acima) — ou escreva
   `~/.config/playsync/box_client_secret.json` na mão:
   ```json
   { "client_id": "...", "client_secret": "..." }
   ```
4. Conecte:
   ```bash
   playsync cloud connect box
   ```

Só um provedor fica ativo por vez (o último `cloud connect` bem-sucedido).

### Uso

Rodar `playsync` sem argumentos abre uma TUI interativa: navega na lista de
jogos com `↑↓`, aperta `Enter` num jogo pra abrir um menu de acoes (sincronizar
agora, baixar da nuvem so pra local, restaurar do local, ou baixar da nuvem e
restaurar), `s` sincroniza tudo, `r` atualiza, `c` abre as configurações
(provedor de nuvem, restauração automática ao abrir, idioma, e — dali —
digitar o Client ID/Secret de um provedor), `q` sai. Acoes
destrutivas (restaurar por cima do save atual) pedem confirmacao antes.

Tudo tambem esta disponivel sem interatividade:

```bash
playsync status              # tabela: jogo, ultimo backup, status de sync
playsync sync                # forca sync de todos os jogos elegiveis agora
playsync sync --app-id ID    # forca sync so de um jogo (AppID da Steam)
playsync history             # historico recente de backups (sucesso/falha, destino)
playsync history --limit N   # historico com um limite customizado (padrao: 20)
playsync cloud setup <google-drive|box>        # salva o Client ID/Secret OAuth (pergunta interativamente)
playsync cloud connect <google-drive|box>      # (re)autoriza um provedor
playsync cloud test-upload <google-drive|box>  # valida o pipeline OAuth2 + upload
playsync restore --app-id ID --source <local|google-drive|box>  # restaura um backup por cima do save atual
playsync config auto-restore <on|off>          # liga/desliga a restauração automática ao abrir
playsync config language <código>              # en, pt-BR, es, fr, de, zh-CN, ja, ru
```

No dia a dia, depois de configurado, nenhum comando é necessário — o daemon
detecta o jogo fechando e sincroniza sozinho após um debounce (5s por
padrão, configurável).

#### Restaurando um backup

```bash
# um jogo com mais de uma pasta de save lista as opcoes (com indice) em vez de restaurar:
playsync restore --app-id ID --source local

# restaura o backup mais recente, do local ou de um provedor de nuvem:
playsync restore --app-id ID --source local --path-index 0
playsync restore --app-id ID --source google-drive --path-index 0
playsync restore --app-id ID --source box --path-index 0

# ve as versoes disponiveis pra uma pasta de save (mais antiga primeiro, mais recente por ultimo):
playsync restore --app-id ID --source local --path-index 0 --list-versions

# restaura uma especifica em vez da mais recente (nome exato de --list-versions):
playsync restore --app-id ID --source local --path-index 0 --version save-20260704T192014Z.zip

# pula a confirmacao (ex: uso em script):
playsync restore --app-id ID --source local --path-index 0 --yes
```

Restaurar **sobrescreve a pasta de save atual** com o conteudo do backup (a
pasta/arquivo existente e apagado primeiro, depois o backup e extraido no
lugar) — voce vai ser perguntado antes, a menos que passe `--yes`.

> Cada sync grava um arquivo novo com timestamp em vez de sobrescrever
> sempre o mesmo, local e na nuvem — um sync automatico ruim (ex: o jogo foi
> aberto sem save e criou um novo/vazio, o fechamento sincronizou isso) nao
> destroi o unico backup bom que existia. So as `backup_versions_to_keep`
> mais recentes (5 por padrao) sao mantidas; as mais antigas sao podadas
> sozinhas. Se a pasta de save ao vivo sumiu de vez (apagada, corrompida),
> `restore` cai pro caminho gravado no historico em vez de simplesmente recusar.

`--list-versions` tambem mostra como cada backup surgiu: `(sessao de 42min)`
pra um disparado ao fechar o jogo depois desse tempo jogando, `(manual)` pra
um vindo de `playsync sync`/da TUI, ou `⚠ sessao curta` pra sessao mais curta
que `short_session_warning_secs` (2 minutos por padrao) — forte sinal de
teste rapido num save novo/vazio, nao progresso de verdade, melhor conferir
antes de restaurar. As acoes de restaurar/baixar da TUI mostram o mesmo
seletor sempre que houver mais de uma versao pra uma pasta de save.

#### Configuração opcional

`~/.config/playsync/config.toml`:

```toml
cloud_provider = "google-drive"        # ou "box" — setado por `cloud connect`
ignored_app_ids = [12345]              # AppIDs pra nunca sincronizar
sync_debounce_secs = 5                 # espera apos fechar o jogo antes de sincronizar
local_backup_dir = "/caminho/custom"   # padrao: ~/PlaySync
backup_versions_to_keep = 5            # quantos backups com timestamp manter por pasta de save
short_session_warning_secs = 120       # sessoes mais curtas que isso sao marcadas no --list-versions
language = "pt-BR"                     # sem essa linha: detecta do locale do sistema, cai pro ingles
auto_restore_on_launch = true          # sem essa linha: ligado por padrao se cloud_provider estiver setado
```

### Desinstalação

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

### Créditos

A detecção do local dos saves usa o [manifest do
Ludusavi](https://github.com/mtkennerly/ludusavi-manifest) (MIT), um banco de
dados mantido pela comunidade com o local de save de ~19 mil jogos, feito
para o [Ludusavi](https://github.com/mtkennerly/ludusavi). O PlaySync guarda
esse manifest em cache local e só cai pra varredura heurística de pastas nos
jogos que ainda não estão nele.

### Licença

MIT — veja [LICENSE](LICENSE).
