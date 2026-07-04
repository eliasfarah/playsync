# PlaySync — estado da sessão (2026-07-04)

Backup automático de saves da Steam pra nuvem, Linux, 100% background.
Workspace Rust: `playsyncd` (daemon, `systemd --user`) + `playsync` (CLI/TUI) +
`playsync-core` (lib compartilhada: deteccao Steam via `steamlocate`, historico
sqlite via `rusqlite`, protocolo IPC, backends de nuvem). CLI fala com o daemon
por Unix socket em `$XDG_RUNTIME_DIR/playsync.sock`, JSON por linha.

## Validado ao vivo (nao so compilado)

- Deteccao de bibliotecas/AppIDs/saves da Steam (23 jogos reais nesta maquina)
- Protocolo IPC daemon↔CLI (status/sync/history)
- Historico em sqlite
- OAuth2 do Google Drive completo (Authorization Code + PKCE, `oauth2` crate,
  `tiny_http` em `localhost:8085` pro redirect). Credenciais reais em
  `~/.config/playsync/gdrive_client_secret.json` (client OAuth Desktop app,
  project_id `playsync-501400`). Token em `~/.config/playsync/tokens/gdrive.json`.
  Fluxo `playsync cloud connect google-drive` confirmado funcionando.
- Gatilho de abrir/fechar jogo, confirmado ao vivo (AppID 567090, "8-Bit
  Bayonetta"): detecta abrir, detecta fechar, agenda sync apos debounce.
- **Upload real de save (diretorio) pro Google Drive**, ver secao do zip abaixo.

## Dois bugs criticos achados por teste ao vivo (nenhum dava erro visivel)

1. **Gatilho original (descartado):** observar `~/.steam/registry.vdf` (chave
   `RunningAppID`) via inotify. Nao funciona no client atual da Steam — a chave
   nunca e escrita numa sessao normal de jogo (parece atrelada a Steam
   Input/config de controle, nao a sessao em si). Substituido por polling em
   `/proc/*/environ` a cada 3s procurando `SteamAppId`/`SteamGameId` — o mesmo
   sinal que MangoHud/gamescope/protontricks usam. Confirmado real com uma sonda
   manual de 1s durante uma sessao ao vivo.

2. **Sandboxing do systemd quebrava o gatilho novo:** `ProtectSystem=strict`,
   `ProtectHome=read-only`, `PrivateTmp=yes`, `ReadWritePaths=`,
   `ConfigurationDirectory=`, `StateDirectory=` no `playsyncd.service` exigem que
   o systemd monte um mount namespace privado pro daemon. Efeito colateral nao
   documentado: isso tambem coloca o daemon num **user namespace** separado
   (confirmado via `/proc/<pid>/ns/user`, mesmo com `PrivateUsers=no` reportado).
   Isso quebra silenciosamente a leitura de `/proc/<pid>/environ` de processos que
   NAO sao filhos do daemon (ou seja, todo processo de jogo da Steam) — o `read()`
   simplesmente volta vazio, sem erro. **Fix:** o unit NAO pode usar nenhuma
   dessas diretivas. So `NoNewPrivileges=yes` e seguro. Os diretorios
   (`~/.config/playsync`, `~/.local/state/playsync`) sao criados pelo proprio
   codigo Rust (`Config::save()`, `HistoryDb::open_default()`), nao precisam que
   o systemd os pre-crie.

## Lacuna do zip: RESOLVIDA (2026-07-04)

`CloudBackend::upload()` le `local_path` como arquivo unico, mas os caminhos de
save reais sao **diretorios**. Fix: novo modulo `playsync-core/src/archive.rs`
(`zip_path()`, usa a crate `zip` + `walkdir`) compacta arquivo-ou-diretorio
recursivamente num .zip temporario em `/tmp/playsync-uploads/`, ancorando os
caminhos no pai da pasta-raiz do save (preserva o nome, ex: `LocalLow/...`).
`playsyncd/src/sync.rs::sync_one` chama isso via `spawn_blocking` antes de cada
`backend.upload()` e apaga o zip depois (sucesso ou erro). 3 testes unitarios
em `archive.rs` (arquivo unico, diretorio recursivo, caminho inexistente).

**Validado ao vivo de ponta a ponta:** rebuild release, reinstalado em
`~/.local/bin`, daemon reiniciado, `playsync sync --app-id 335300` (DARK SOULS
II) — 3 uploads reais bem-sucedidos pro Google Drive (`playsync history`
mostra "sim" apos o fix, "nao" antes). Zips temporarios confirmados apagados
depois. Esse era exatamente o jogo que dava erro "nao consegui ler ...
AppData/LocalLow" nos logs do daemon antes do fix.

Nomeacao do remote: se o jogo tem 1 so save_path, sobe como `{nome}.zip`; se
tem mais de um, `{nome} (0).zip`, `{nome} (1).zip` etc (evita colisao de nomes
no Drive, que nao aplica unicidade sozinho).

Backend do Box.com (`cloud/box_com.rs`) continua stub de proposito (`bail!`).

Nota a parte, achada durante a validacao (nao mexida, so registrada): a coluna
"STATUS" de `playsync status` mostra "nunca sincronizado" pra todo mundo mesmo
quando ha backup recente na coluna "ULTIMO BACKUP" — parece nao ler
`sync_status` corretamente. Bug de exibicao, nao afeta o backup em si.

## Proxima tarefa em aberto

Empacotar (.deb/AUR) — ha um diretorio `packaging/` com stubs de `aur` e
`systemd`, ainda nao explorado a fundo nesta sessao.

## Maquina de dev (hostname "gaming", Arch Linux)

- Binarios instalados em `~/.local/bin/{playsync,playsyncd}` (build release,
  atualizados nesta sessao com o fix do zip)
- Unit em `~/.config/systemd/user/playsyncd.service`, enabled + active
- O repo tem `.git` proprio (auto-inicializado, vazio, zero commits — nunca foi
  pedido pra commitar, nao assumir que pode)

## Como validar antes de dizer "pronto"

Os bugs acima so apareceram porque testamos ao vivo (jogo real abrindo/
fechando, systemd real rodando, upload real pro Drive) em vez de confiar em
"deveria funcionar" so porque compilou. Antes de declarar algo "pronto pra
producao" ou sugerir empacotar, rodar o cenario real de ponta a ponta e
mostrar a evidencia (logs, output de verdade, `playsync history`) — os bugs
anteriores nao davam erro nenhum, so "nao acontece nada" ou uma falha
silenciosa no historico.
