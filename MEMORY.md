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

Nota a parte, achada durante a validacao (nao mexida, so registrada): a coluna
"STATUS" de `playsync status` mostra "nunca sincronizado" pra todo mundo mesmo
quando ha backup recente na coluna "ULTIMO BACKUP" — parece nao ler
`sync_status` corretamente. Bug de exibicao, nao afeta o backup em si.

## Organizacao em pastas (local + nuvem) + backend do Box: RESOLVIDO (2026-07-04)

Pedido do usuario: backup local **e** na nuvem organizados como
`PlaySync/<jogo sanitizado>/<arquivo>`, sem caractere problematico no nome do
jogo (nomes reais tem `™`, `:`, apostrofo).

- `playsync-core/src/naming.rs` (novo): `sanitize()` — remove `™`/`®`/`©`/`:`,
  troca `/ \ * ? " < > |` por `_`, colapsa espaco, mantem acento/apostrofo.
- `Config::local_backup_root()` (novo, `config.rs`): `~/PlaySync` por padrao,
  ou `local_backup_dir` no config.toml se setado.
- `archive::zip_path()` mudou de assinatura: agora recebe `(source, dest)` e
  escreve direto no destino (sobrescrevendo), em vez de gerar um path
  temporario que o chamador tinha que apagar — o "temp" virou o backup local
  de verdade, nao se apaga mais depois do upload.
- `CloudBackend::upload(local_path, remote_path)`: `remote_path` agora e um
  caminho logico tipo `PlaySync/<jogo>/save.zip` — os segmentos antes do
  ultimo sao pastas, criadas sob demanda (`ensure_folder` em cada backend).
- `sync.rs::sync_one`: pra cada `save_path`, zipa em
  `~/PlaySync/<sanitizado>/save.zip` (ou `save-{idx}.zip` se o jogo tiver mais
  de um save_path) e, se tiver backend configurado, sobe o mesmo zip pra
  `PlaySync/<sanitizado>/<mesmo arquivo>` la. Historico agora registra destino
  `"Local"` (sem nuvem) ou `"Local + GoogleDrive"`/`"Local + Box"`.
- Refatorado `wait_for_redirect`/`ReqwestHttpClient`/`HttpAdapterError` de
  `gdrive.rs` pra `cloud/mod.rs` (`pub(crate)`), compartilhado entre os dois
  backends OAuth2.

**Box.com implementado** (`cloud/box_com.rs`, antes stub): Authorization Code
puro (sem PKCE, app confidencial com secret) em `localhost:8086` (Drive usa
8085). Credenciais do usuario em `~/.config/playsync/box_client_secret.json`
(formato proprio, flat: `{"client_id","client_secret"}`, 0600 — NAO commitar).
Upload via Box Content API (`multipart/form-data`, precisou habilitar a
feature `multipart` do reqwest). Pasta encontrada/criada via
`GET /folders/{id}/items` (Box nao filtra por nome no servidor como o Drive).

**Gotcha real da API do Box** (so apareceu testando ao vivo, a doc oficial diz
o contrario): no 409 de upload de arquivo duplicado, a doc lista
`context_info.conflicts` como **array**, mas a resposta real e um **objeto
solto** (`conflicts.id`, nao `conflicts[0].id`). Codigo tenta os dois formatos
(`conflicts["id"].as_str().or_else(|| conflicts[0]["id"].as_str())`) — sem
isso o fallback de "sobrescrever versao existente" falhava sempre com
"sem id do arquivo existente". Confirmado com curl bruto reproduzindo o 409.

**Validado ao vivo, os dois provedores:**
- Google Drive: pasta `PlaySync/` + `DARK SOULS II Scholar of the First Sin/`
  criadas na raiz do Drive de verdade (confirmado via API), 3 zips dentro.
- Box: `playsync cloud connect box` (usuario confirmou redirect URI
  `http://localhost:8086` ja cadastrado no app), `cloud test-upload box` (cria
  `PlaySync/playsync-test-upload.zip`), rodado 2x seguidas pra confirmar que a
  segunda vira "nova versao" (nao 409) — confirmado. Depois `playsync sync
  --app-id 335300` com `cloud_provider = "box"` no config: 3 uploads reais,
  pasta do jogo criada dentro de `PlaySync/`, tudo confirmado via API do Box.
- Local: `~/PlaySync/DARK SOULS II Scholar of the First Sin/save-{0,1,2}.zip`
  confirmado no disco, mesma estrutura dos dois lados da nuvem.

**Lixo deixado no Drive de antes do fix** (nao apagado, so registrado): 3
arquivos soltos na raiz do "Meu Drive" do formato `DARK SOULS™ II: Scholar of
the First Sin (0/1/2).zip`, de quando o upload ainda ia direto pra raiz sem
pasta. Sao orfaos agora — o usuario pode apagar manualmente quando quiser.

`uuid` removido do workspace (nao usado mais depois que `zip_path` parou de
gerar nome aleatorio).

## Proxima tarefa em aberto

Empacotar (.deb/AUR) — ha um diretorio `packaging/` com stubs de `aur` e
`systemd`, ainda nao explorado a fundo nesta sessao. O usuario passou o repo
real: `https://github.com/eliasfarah/playsync.git` (o PKGBUILD ainda aponta
pro placeholder `yourname` — precisa atualizar antes de tentar buildar o
pacote).

## Maquina de dev (hostname "gaming", Arch Linux)

- Binarios instalados em `~/.local/bin/{playsync,playsyncd}` (build release,
  atualizados nesta sessao com o fix do zip)
- Unit em `~/.config/systemd/user/playsyncd.service`, enabled + active
- Repo commitado (2026-07-04, commit `68bc211`, autor "Elias Farah" configurado
  localmente so neste repo, `git config user.*` sem `--global`). Sem remote
  ainda — nao existe `github.com/yourname/playsync` de verdade, entao o
  PKGBUILD em `packaging/aur/` nao pode ser testado ate ter um repo real com
  release/tag.

## Como validar antes de dizer "pronto"

Os bugs acima so apareceram porque testamos ao vivo (jogo real abrindo/
fechando, systemd real rodando, upload real pro Drive) em vez de confiar em
"deveria funcionar" so porque compilou. Antes de declarar algo "pronto pra
producao" ou sugerir empacotar, rodar o cenario real de ponta a ponta e
mostrar a evidencia (logs, output de verdade, `playsync history`) — os bugs
anteriores nao davam erro nenhum, so "nao acontece nada" ou uma falha
silenciosa no historico.
