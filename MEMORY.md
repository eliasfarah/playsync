# PlaySync — estado da sessão (2026-07-04)

## Teste real "apagar save de verdade + restaurar" achou bug critico: RESOLVIDO (2026-07-04)

Usuario pediu o teste mais realista possivel: pegar um jogo de verdade
(escolheu DARK SOULS II: Scholar of the First Sin, appid 335300), fazer
backup, apagar o save REAL da Steam (nao um backup do playsync, o arquivo de
verdade que o jogo usa), e pedir pro `playsync restore` trazer de volta.

**Protocolo de seguranca usado antes de apagar qualquer coisa real:** copia
manual do arquivo real (`DS2SOFS0000.sl2`, 8.25MB) pro scratchpad + sha256,
INDEPENDENTE do proprio playsync (nao confiar que o sistema que estamos
testando vai funcionar antes de provar que funciona). So depois disso rodei
`playsync sync --app-id 335300` (confirmado "Local + GoogleDrive sim",
sha256 do arquivo dentro do `save.zip` local batendo com o arquivo real) e
apaguei o arquivo de save de verdade.

**Bug achado: `playsync restore` nao conseguia restaurar nada.** Erro:
`"..." nao tem pasta de save conhecida`. Causa raiz: tanto `restore` (CLI)
quanto o menu de acoes da TUI descobrem o "alvo" da restauracao chamando
`discover_games()` — uma varredura AO VIVO do disco. Isso sempre exigiu que
o caminho de save AINDA EXISTA pra ser "descoberto" (heuristica usa
`.is_dir()`, e a resolucao do manifest da Ludusavi usa `glob()`, que so
retorna o que existe). Ou seja: **restore nunca funcionou pro cenario mais
importante pra ele existir** — save apagado/corrompido/sumido de verdade —
porque a mesma logica usada pra "achar o que fazer backup" tambem decidia
"pra onde restaurar", e as duas perguntas tem respostas diferentes quando o
arquivo sumiu. Isso e um bug pre-existente (nao introduzido nesta sessao, a
heuristica antiga tinha a mesma limitacao) que simplesmente nunca tinha sido
testado dessa forma — sessoes anteriores sempre restauravam sobre uma pasta
de save que AINDA EXISTIA (so o conteudo mudava) ou "apagavam" so o backup
local do playsync (nao o save real) pra forcar um download.

**Fix:** `BackupEntry` (`ipc.rs`) ganhou `source_paths: Vec<PathBuf>` — os
`save_paths` detectados NO MOMENTO de cada backup bem sucedido, gravados no
sqlite (`db.rs`, nova coluna `source_paths` TEXT/JSON, migracao idempotente
via `ALTER TABLE ... ADD COLUMN` ignorando erro de coluna duplicada pra bancos
ja existentes). `playsyncd/sync.rs::sync_one` preenche esse campo com
`game.save_paths.clone()` a cada backup. Novo helper compartilhado
`actions::restore_candidate_paths(app_id, live_paths)`: usa os `save_paths`
ao vivo se existirem; se vazio, cai pro `source_paths` do ultimo backup bem
sucedido no historico (e sinaliza isso pro usuario com um aviso, ja que o
caminho pode nao existir mais de verdade). Usado tanto por `main.rs::restore`
quanto por `tui.rs::choose_action`/`run_action` (antes duplicavam a logica
`game.save_paths.is_empty()` cada um do seu jeito). `sanitized_and_file_name`
mudou de assinatura (`paths_len: usize` explicito) pra nao depender de
`game.save_paths.len()` quando os caminhos de verdade vieram do historico.

**Validado ao vivo, de ponta a ponta, repetindo o teste com o fix:**
rebuild release, reinstalado, daemon reiniciado, sync novo rodado (source_paths
confirmado gravado no sqlite: `["/mnt/games/.../DS2SOFS0000.sl2"]`), arquivo
real apagado de novo, `playsync restore --app-id 335300 --source local --yes`
— mostrou o aviso "pasta de save atual nao encontrada... usando o caminho do
ultimo backup", restaurou, sha256 do arquivo restaurado bateu EXATAMENTE com
o original (`fdf13fd1...`), `playsync status` voltou a mostrar "em dia".
Progresso real do usuario nunca ficou em risco: 2 copias de seguranca
independentes existiram o tempo todo (a copia manual + o `save.zip` local/
Drive ja verificado) antes de qualquer `rm` no arquivo de verdade.

**Ainda nao commitado.**


## Release v0.2.0 publicado

Usuario perguntou se o release ja tinha sido atualizado com tudo isso —
nao tinha (o `v0.1.0` era de antes do restore/TUI/fixes). Subida a versao
pra `0.2.0` (minor bump, features novas de verdade, nao so patch) nos 3
`Cargo.toml` via `workspace.package.version`, tag `v0.2.0` criada/empurrada,
release publicado com tarball + `.deb` novos (binarios rebuilded, testes
rodados antes). https://github.com/eliasfarah/playsync/releases/tag/v0.2.0

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

**Explicado (nao e bug):** a coluna "STATUS" mostrava "nunca sincronizado" pra
todo mundo logo apos reiniciar o daemon, mesmo com backup recente em "ULTIMO
BACKUP". Causa: `sync_status` e um `HashMap` em memoria (`SyncEngine.status`,
zerado a cada restart do daemon), enquanto "ultimo backup" vem do sqlite
(persistente). Ate um jogo ser sincronizado de novo NESSA execucao do daemon,
o status cai no default `NeverSynced` — nao ha leitura errada de campo, so
duas fontes com tempos de vida diferentes. Confirmado ao vivo (ver secao do
`SyncNow` em background abaixo): rodando um sync de tudo, cada jogo passa
visivelmente de "nunca sincronizado" → "sincronizando..." → "em dia" na
ordem em que o daemon processa a lista.

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

## TUI com menu por jogo (sync/baixar/restaurar): RESOLVIDO (2026-07-04)

Usuario pediu: na TUI, poder escolher um jogo (linha) e um menu com acoes:
baixar da nuvem (so local, sem restaurar), backup manual, restaurar no jogo
(do local), baixar da nuvem + restaurar, upload pra nuvem. Perguntei 3 coisas
antes de implementar (todas resolvidas com a opcao recomendada):
1. "Backup manual" e "upload pra nuvem" sao a mesma operacao hoje (zip local
   + upload sao acoplados) — usuario topou manter uma so acao pras duas.
2. Acoes de nuvem usam o `cloud_provider` ativo do config.toml (sem menu de
   escolher Drive vs Box).
3. So pergunta qual pasta de save se o jogo tiver mais de uma (senao roda
   direto).

**Refatoracao:** extraido `crates/playsync-cli/src/actions.rs` (novo) com a
logica sem I/O de terminal (antes vivia dentro do `restore()` do `main.rs`,
misturada com `println!`/stdin): `RestoreSource`/`parse_source`/
`parse_provider`, `sanitized_and_file_name`, `fetch_backup_bytes` (local ou
`CloudBackend::download`), `pull_from_cloud` (baixa e SO guarda local, novo —
antes so existia "baixar e restaurar"), `extract_over` (apaga + `unzip_to`).
`main.rs::restore()` (comando CLI) virou um wrapper fino sobre isso.

**TUI (`tui.rs`) reescrita** com maquina de estado (`Mode`): `Table` (com
`selected: usize`, navegacao ↑↓/j/k) → `Enter` abre `ActionMenu` (4 opcoes:
Sincronizar agora, Baixar da nuvem, Restaurar no jogo, Baixar da nuvem e
restaurar) → se o jogo tiver >1 pasta de save, `PathChoice` lista as opcoes
→ acoes destrutivas (as duas de "restaurar") passam por `Confirm` (`[y]`
executa, qualquer outra tecla cancela) → `Info` mostra o resultado (ou erro)
e qualquer tecla volta pra `Table`. Popups desenhados com `Clear` + `List`/
`Paragraph` centralizados (`centered_rect`, recipe padrao do ratatui).
Transicao pra `Info` sempre reconsulta `Status` + `discover_games()`, entao
o resultado ja aparece atualizado.

**Como testei uma TUI interativa sem terminal de verdade:** sem `tmux`/
`screen` instalados (e sem sudo interativo pra instalar), usei o modulo
`pty` do Python (`os.fork()` + `pty.openpty()` + `TIOCSWINSZ`) pra rodar o
`playsync` real anexado a um pseudo-terminal, mandar sequencias de teclado
(setas via `\x1b[A`/`\x1b[B`, Enter, Esc, `y`, `q`) e capturar a tela
(stripando ANSI com regex) entre cada tecla. Script descartado no fim
(`/tmp/.../scratchpad/drive_tui.py`), so pra validacao, nao faz parte do
repo.

**Validado ao vivo, as 4 acoes, via essa automacao real da TUI** (jogo "The
Surge 2", appid 644830, 3 pastas de save):
- Navegacao ↑↓ move a linha selecionada (confirmado visualmente no capture).
- `Enter` abre o menu com o titulo do jogo certo; ↑↓ move o cursor do menu.
- "Restaurar no jogo" com >1 pasta → `PathChoice` lista as 3; `Esc` cancela
  e volta pra tabela.
- "Baixar da nuvem" (path 0, antes apagado de proposito pra forcar um
  download de verdade) → baixou do Google Drive e recriou
  `~/PlaySync/The Surge 2/save-0.zip` (confirmado no disco, zip valido).
- "Restaurar no jogo" (path 0, local) → popup de confirmacao aparece, `y`
  confirma, mensagem "Restaurado (backup local) em .../Roaming", hash
  identico antes/depois (pasta era vazia dos dois lados, restauracao
  correta de um save vazio).
- "Sincronizar agora" (por linha, nao "tudo") → dispara na hora (mesmo fix
  do `SyncNow` em background da secao acima), linha mostra
  "sincronizando..." ao vivo.
- "Baixar da nuvem e restaurar no jogo" (path 1, Local) → popup de
  confirmacao, `y`, mensagem "Restaurado (nuvem) em .../Local", hash
  identico antes/depois.

## TUI travando no `[s]` (sync tudo) + "Steam" listado como jogo: RESOLVIDO (2026-07-04)

Usuario reportou: apertar `[s]` na TUI pra sincronizar tudo "trava" — e
perguntou se e bug (sim) e se tem alguma animacao de upload (nao tinha
nenhuma). Tambem notou "Steamworks Common Redistributables" (e outros
utilitarios) aparecendo como se fossem jogo.

**Causa do travamento (confirmada lendo o codigo, nao so suposicao):**
`Request::SyncNow` no daemon (`playsyncd/src/ipc.rs`) fazia
`engine.sync_now(app_id).await` **antes** de responder — ou seja, sincronizar
"tudo" so respondia depois de zipar+subir CADA jogo elegivel, sequencialmente.
A TUI (`tui.rs`) da `.await` nessa chamada direto dentro do handler da tecla
`[s]`, entao o loop de render inteiro fica parado (sem redesenhar nada, sem
spinner nenhum) ate a sincronizacao inteira acabar. Com ~18 jogos e upload
real pra nuvem, parece travado de verdade mas so estava lento.

**Fix:**
1. `ipc.rs`: `SyncNow` agora dispara `engine.sync_now(app_id)` num
   `tokio::spawn` e responde `Ack` na hora. Bate com o texto que a CLI ja
   usava ("sincronizacao disparada") — a intencao sempre foi fire-and-forget,
   so a implementacao nao acompanhava.
2. `sync.rs::sync_one`: agora chama `mark_running()` **antes** de zipar/subir
   (nao so no fim) — sem isso nao daria pra ver progresso nenhum mesmo com o
   `SyncNow` em background.
3. `tui.rs`: loop principal agora re-consulta `Status` sozinho a cada ~1s
   (4 polls de 250ms), nao so quando o usuario aperta `[r]`. E o que faz o
   progresso aparecer sem precisar ficar apertando tecla.

**Validado ao vivo:** `time playsync sync` (sem app-id, ~18 jogos elegiveis)
voltou em 3ms (antes ficaria bloqueado pelo tempo total do sync). Rodando
`playsync status` repetidas vezes logo em seguida, cada jogo migrou
visivelmente "nunca sincronizado" → "sincronizando..." → "em dia" na ordem
processada pelo daemon, terminando com todos "em dia" e `history` mostrando
os uploads reais completos.

**"Steam" listado como jogo — causa:** `steamlocate` (e o Steam local, via
appmanifest `.acf`) nao guarda um campo "isso e jogo vs ferramenta" — essa
distincao vem do catalogo remoto da Valve (appinfo.vdf, formato binario, fora
do escopo do que o steamlocate parseia). Sem esse campo, a unica forma de
diferenciar seria bater numa API externa por AppID (rede, lento) ou parsear
o binario na mao (fragil).

**Fix (deteccao automatica, sem lista manual):** `steam::is_steam_tool(name)`
em `steam.rs` filtra por prefixo de nome — `"Steamworks Common
Redistributables"` (exato), `"Proton"*`, `"Steam Linux Runtime"*` — aplicado
direto em `discover_games()`, entao vale pra tudo (status, TUI, sync,
restore) sem precisar tocar em `ignored_app_ids`. Cobre os 5 casos reais desta
maquina (Steamworks Common Redistributables 228980, Proton Experimental
1493710, Proton 10.0 3658110, Steam Linux Runtime 3.0 (sniper) 1628350, Steam
Linux Runtime 4.0 4183110) e versoes futuras pelo mesmo padrao de nome (ex:
"Proton 11.0"), ja que e a convencao de nomenclatura da Valve pra essas
ferramentas. `ignored_app_ids` continua disponivel pro usuario ignorar jogos
de verdade que ele nao quer sincronizar.

## `playsync restore` + fim das duplicatas no Drive: RESOLVIDO (2026-07-04)

Usuario pediu: comando pra restaurar um backup (local ou nuvem), escolhendo
qual save (quando o jogo tem mais de uma pasta) e a origem.

Antes de implementar, respondi a pergunta "quantos backups voces guarda,
ultimos 3?" com a realidade do codigo (nao havia nenhum "ultimos N"):
- **Local:** so 1 copia — `archive::zip_path` sempre sobrescrevia o mesmo
  arquivo.
- **Google Drive:** cada sync criava um arquivo NOVO (so `POST`, nunca
  checava se ja existia) — crescimento sem limite, confirmado ao vivo (uma
  sync anterior de TODOS os jogos, ver nota abaixo, deixou dezenas de
  arquivos duplicados no Drive).
- **Box:** ja tinha overwrite-como-nova-versao desde a implementacao
  original (409 → `overwrite()`).

Corrigido o gap do Drive como parte deste trabalho (nao só documentado):
`gdrive.rs` ganhou `find_entry` (pasta OU arquivo, com `orderBy=createdTime
desc` pra lidar com duplicatas ja existentes) e `upload()` agora faz `PATCH
.../files/{id}` (update) quando ja existe um arquivo com esse nome, em vez de
sempre `POST` (create). Confirmado ao vivo: rodar sync 2x seguidas manteve o
mesmo `id` de arquivo, so `modifiedTime` mudou.

**`CloudBackend` ganhou `download(remote_path) -> Vec<u8>`:**
- Google Drive: `GET .../files/{id}?alt=media`, direto (sem redirect).
- Box: `GET .../files/{id}/content` redireciona (302) pra
  `dl.boxcloud.com` (URL pre-assinada, sem precisar do Authorization no
  segundo hop) — como os clientes reqwest dos dois backends tem
  `redirect::Policy::none()` (protecao SSRF original), o codigo segue esse
  UM redirect na mao, validando que o host e `*.boxcloud.com` antes de
  seguir.

**`archive::zip_path` mudou de `(source) -> PathBuf`** (versao antiga, ja
alterada numa sessao anterior) **continua `(source, dest)`**; adicionado
`archive::unzip_to(bytes, anchor)` — o inverso, usa `enclosed_name()` do
crate `zip` (protecao built-in contra zip-slip) e extrai ancorado no mesmo
diretorio-pai usado ao compactar.

**CLI:** `playsync restore --app-id ID --source <local|google-drive|box>
[--path-index N] [--yes]`. Sem `--path-index` e o jogo tiver mais de uma
pasta de save, lista as opcoes (indice + caminho) e para, em vez de adivinhar.
Pede confirmacao antes de apagar a pasta/arquivo atual (a menos que `--yes`).
Fala direto com `playsync-core` (Steam, config, cloud), sem passar pelo
daemon/IPC — mesmo padrao de `cloud connect`.

**Validado ao vivo, as 3 origens:** pra DARK SOULS II (appid 335300, 3
save_paths), tirei um hash (`sha256sum` recursivo) da pasta `Roaming` antes
de mexer, rodei `restore --source local`, depois `--source box`, depois
`--source google-drive` (trocando `cloud_provider` no config pra sincronizar
com cada um antes) — hash identico nas 3 vezes. Confirma tambem que o fix do
redirect do Box e o fix de overwrite do Drive funcionam de ponta a ponta.

**Achado a parte (nao e bug):** durante a validacao, o Drive mostrou pastas
de VARIOS outros jogos (Forza Horizon 5, Returnal, Marvel's Spider-Man 2,
etc.), nao so DARK SOULS II. Motivo: a TUI (`tui.rs`) tem uma tecla que manda
`Request::SyncNow { app_id: None }` (sync de todos os jogos elegiveis) —
alguem (eu ou o usuario) deve ter aberto a TUI e apertado essa tecla em algum
momento anterior desta sessao. Comportamento esperado, so registrando pra nao
confundir uma sessao futura.

**Nota:** `cloud_provider` no `~/.config/playsync/config.toml` deste
maquina ficou como `"google-drive"` ao final da validacao (estava `"box"`
antes — troquei pra testar o restore dos dois provedores). Nao revertido de
proposito, avisar o usuario.

## GitHub: RESOLVIDO (2026-07-04)

Repo real: `git@github.com:eliasfarah/playsync.git`, branch `main` empurrada
(3 commits: inicial, pastas PlaySync+Box, fix do PKGBUILD). PKGBUILD
atualizado pra apontar pro repo real (antes tinha o placeholder `yourname`).

**Autenticacao SSH:** o usuario primeiro tentou colar uma chave RSA existente
do Mac (`~/.ssh/macos`) — corrompida/incompativel, `ssh-keygen -y` falhava
localmente com `error in libcrypto: unsupported` (nem chegava a tentar
rede, entao nao era problema de agent/config). Gerada uma chave ed25519 nova
neste Linux (`~/.ssh/playsync_github`), cadastrada pelo usuario em
github.com/settings/keys, `~/.ssh/config` aponta `Host github.com` pra ela.

**Email privado do GitHub:** primeiro push falhou (`GH007`) porque os
commits usavam `eliasfa@gmail.com` (email real, protegido). Como nada tinha
sido empurrado ainda, reescrevemos os commits locais (`git filter-branch
--env-filter`, nao rebase -i) pro noreply do GitHub:
`234085+eliasfarah@users.noreply.github.com` (id via
`api.github.com/users/eliasfarah`, publico). `git config user.email` deste
repo ja fica configurado assim daqui pra frente.

## README + LICENSE + release v0.1.0: RESOLVIDO (2026-07-04)

`README.md` (instalacao fonte/.deb/AUR, config Google Drive/Box, uso do CLI,
desinstalacao) e `LICENSE` (MIT — Cargo.toml ja declarava mas o arquivo nao
existia) adicionados. Ultimos placeholders `yourname` trocados pro repo real
(`Cargo.toml` `repository=`, `packaging/systemd/playsyncd.service`
`Documentation=`).

`gh` CLI instalado (`sudo pacman -S github-cli`, usuario rodou) e autenticado
(`gh auth login`, usuario rodou — ambos interativos, nao dava pra fazer por
aqui). Tag `v0.1.0` criada e empurrada; Release publicado em
github.com/eliasfarah/playsync/releases/tag/v0.1.0 com
`playsync-0.1.0-x86_64-linux.tar.gz` anexado (binarios + unit systemd + README
+ LICENSE).

## README bilingue + `.deb` no release: RESOLVIDO (2026-07-04)

README reescrito: ingles primeiro (`## English`), pt-BR depois (`##
PlaySync (PT-BR)`), com link cruzado nas duas pontas, badges (license/release/
rust) e um aviso de "Status: v0.1.0, recente". `.deb` gerado com `cargo-deb`
(instalado via `cargo install cargo-deb`) e anexado ao release `v0.1.0`.

Achado corrigido no processo: `cargo-deb` com `depends = "$auto"` (Cargo.toml
da `playsync-cli`) resolve pra `Depends:` **vazio** quando rodado numa Arch
(sem `dpkg-shlibdeps` instalado — so existe em sistemas Debian-like). Trocado
pra `depends = "libc6"` explicito, confirmado com `ldd` nos dois binarios que
so linkam contra `libc.so.6`/`libgcc_s.so.1`/`libm.so.6` (todas essenciais em
qualquer Debian/Ubuntu, `libc6` cobre o caso que importa).

## Achado (nao resolvido): `makepkg` do PKGBUILD falha ao linkar

Testando o PKGBUILD de ponta a ponta (`makepkg -f`, fora do escopo pedido, so
validacao extra): falha reproduzivel com `ld.lld: error: undefined symbol:
aws_lc_0_42_0_*` (varios simbolos da `aws-lc-sys`/`rustls`, dependencia
transitiva do `reqwest`). Investigado a fundo:

- **Nao e flakiness** — falha 2x seguidas, deterministico.
- **Nao reproduz** com os MESMOS passos do PKGBUILD (`cargo fetch --locked`
  + `cargo build --frozen --release --workspace`) rodados na mao, inclusive
  com o tarball baixado de verdade do GitHub e as mesmas `CFLAGS`/`LDFLAGS`
  do `/etc/makepkg.conf` exportadas manualmente.
- **So falha dentro do `makepkg` de verdade.** Sinal encontrado: o log da
  `aws-lc-sys` dentro do `makepkg` mostra o aviso `_FORTIFY_SOURCE requires
  compiling with optimization (-O)` (ausente no build manual) e a
  `libaws_lc_0_42_0_crypto.a` resultante fica bem maior (16.8MB vs 6.6MB) —
  ou seja, o `CFLAGS` (`-O2` etc.) nao esta chegando no compilador C dentro
  do ambiente do `makepkg`, mesmo com a mesma variavel exportada. Ainda
  assim, o simbolo (ex: `aws_lc_0_42_0_EVP_sha1`) **esta presente** no `.a`
  gerado dos dois lados (confirmado com `nm`) — entao a causa exata de por
  que o link final falha so no `makepkg` nao foi encontrada (suspeita: ordem
  dos objetos/arquivos na linha de comando do linker, ou algo no
  `cc`/`cc-rs` que se comporta diferente sob o ambiente/PATH sanitizado do
  `makepkg`).
- Nao e falta de `cmake`/`nasm`/`clang` (nenhum dos tres esta instalado, e
  ainda assim o build manual funciona sem eles).

**Nao investigado mais fundo** (decisao de escopo, nao limitacao): o pedido
da sessao era so README + release, isso foi validacao extra por conta
propria. Antes de recomendar o pacote AUR como pronto pra uso, esse link
precisa ser resolvido ou contornado (candidatos: fixar `opt-level` do
profile release pra algo que o `cc` aceite sem ambiguidade, forcar
`AWS_LC_SYS_STATIC=1` ou outra env var do `aws-lc-sys` pra pular a deteccao
"dynamic vs static", ou builds isolados tipo Docker/`extra-x86_64-build` pra
reproduzir e comparar `strace`/ordem de linkedit).

## Maquina de dev (hostname "gaming", Arch Linux)

- Binarios instalados em `~/.local/bin/{playsync,playsyncd}` (build release,
  atualizados nesta sessao com zip + pastas PlaySync + Box)
- Unit em `~/.config/systemd/user/playsyncd.service`, enabled + active
- Repo commitado e com push pro GitHub (ver secao acima). `git config
  user.*` configurado so neste repo (nao `--global`).

## Deteccao de save em Documents/My Games/Saved Games + extra_save_paths: RESOLVIDO (2026-07-04)

Retomando um trabalho que ja estava no working tree (nao commitado, nao
documentado ainda) no inicio desta sessao: `find_save_candidates`
(`steam.rs`) so olhava `AppData/{Roaming,Local,LocalLow}` e o espelho da
Steam Cloud — jogos que guardam save em `Documents/<jogo>`, `Documents/My
Games/<jogo>` ou `Saved Games/<jogo>` (convencao comum, sobretudo
Unity/Unreal) ficavam de fora, silenciosamente.

- `find_save_candidates` ganhou 3 fontes novas: subpastas diretas de
  `Documents/` (exceto as padrao que Wine/Proton cria vazias em todo prefixo
  novo: Pictures/Music/Videos/Downloads/Templates/**My Games**), subpastas de
  `Documents/My Games/` (descendo mais um nivel), e subpastas de `Saved
  Games/`. Helper novo `subdirs_excluding()`.
- `Config` ganhou `extra_save_paths: HashMap<String, Vec<PathBuf>>` (por
  AppID, chave string por causa do TOML) pro usuario apontar na mao um save
  que a deteccao automatica nao ache — `discover_games()` agora le o config
  e mescla. `GameStatus`/`ipc.rs` ganhou `has_save_paths: bool`; CLI/TUI
  mostram "⚠ sem save detectado" quando `false`, em vez de confundir com
  "nunca sincronizado".
- **Bug achado e corrigido durante a validacao ao vivo** (nao existia no
  diff original): `"My Games"` nao estava na lista de exclusao do scan de
  `Documents/` de nivel superior, entao pra jogos com `Documents/My
  Games/<jogo>` (ex: The Division) o candidato virava TANTO
  `Documents/My Games` (a pasta toda) QUANTO `Documents/My Games/<jogo>` —
  zipava/subia o mesmo save duas vezes. Corrigido adicionando `"My Games"`
  a `DEFAULT_DOCUMENTS_SUBFOLDERS`.

**Validado ao vivo, end-to-end, nesta maquina** (rebuild release, reinstalado
em `~/.local/bin`, daemon reiniciado):
- Exemplo mais claro: **God of War** (appid 1593500) — antes do fix so tinha
  os 3 diretorios AppData (todos praticamente vazios, so pastas padrao do
  Wine). O save real (`game.sav`, 33MB) fica em `Saved Games/God of War/`,
  invisivel pra deteccao antiga. Rodei `playsync sync --app-id 1593500` de
  verdade: 4 save_paths detectados (Roaming/Local/LocalLow/Saved Games),
  `save-3.zip` confirmado contendo `game.sav` de 33MB, upload real pro
  Google Drive confirmado (`playsync history` mostra "Local + GoogleDrive
  sim").
- Outros 6 jogos desta maquina onde a deteccao nova achou pasta real (nao so
  testado, so confirmado que os caminhos aparecem): Horizon Zero Dawn,
  Ghost of Tsushima, The Last of Us Part II, Marvel's Spider-Man 2, Tom
  Clancy's The Division (com o fix da duplicacao acima), Grand Theft Auto
  IV, FINAL FANTASY VII (2013) — este ultimo com save aninhado 2 niveis
  (`Documents/Square Enix/FINAL FANTASY VII Steam/`), a pasta inteira
  `Square Enix` vira o save_path (zipa tudo dentro, inclusive o que nao e
  save, ex: launcher logs — aceitavel, so nao granular).
- `extra_save_paths` em si (o caminho manual configuravel) nao foi exercitado
  ao vivo — nenhum jogo desta maquina precisou dele (todos acharam save por
  deteccao automatica). Continua sem teste end-to-end real.

**Ainda nao commitado** — 6 arquivos modificados (`main.rs`, `tui.rs`,
`config.rs`, `ipc.rs`, `steam.rs`, `sync.rs`), aguardando o usuario decidir.

## Integracao com o manifest da Ludusavi (fonte confiavel de save location): RESOLVIDO (2026-07-04)

Usuario pediu uma fonte mais confiavel de onde os saves ficam no Linux/Steam
do que a heuristica de pastas conhecidas (a da secao acima), citando
PCGamingWiki e Ludusavi. Pesquisado a fundo antes de implementar: o
`ludusavi-manifest` (github.com/mtkennerly/ludusavi-manifest, MIT) e um YAML
de ~17MB com +19 mil jogos, curado do PCGamingWiki mas ja estruturado pra
consumo automatico — cada jogo tem `steam.id`, uma secao `files` com
caminhos exatos (`tags: [save]` ou `[config]`) e `when` (os/store) pra
filtrar. O crate `ludusavi` em si NAO e biblioteca (so binario), mas o dado
(YAML) e livre pra consumir direto. Perguntei ao usuario 2 coisas antes de
codar (respondidas com a opcao recomendada nas duas):
1. Papel do manifest vs. heuristica: **manifest manda quando documentado,
   heuristica so cobre jogos ausentes do manifest**.
2. Estrategia de fetch: **baixar e cachear localmente
   (`~/.local/state/playsync/ludusavi_manifest.yaml`), revalidar por ETag**
   (`If-None-Match`, max_age 7 dias) — offline depois do 1o download.

**Modulo novo `playsync-core/src/manifest.rs`:**
- `refresh_cache(client, max_age)`: async, so faz rede se o cache local tiver
  mais de `max_age` (ou nao existir). 304 so atualiza o arquivo de ETag (pra
  nao re-checar antes do prazo); 200 grava manifest + etag novos. Chamado
  em background no startup do daemon (`playsyncd/main.rs`, `tokio::spawn`,
  nao atrasa nem bloqueia o daemon se a rede cair) e da CLI (`main.rs`, best
  effort — util pro TUI de longa duracao, um `restore` de execucao rapida
  pode nao dar tempo de terminar, tudo bem, o daemon mantem o cache quente).
- `appid_index()`: le SO o cache local (sem rede) e reparseia so quando o
  mtime do arquivo muda (`Mutex<Option<(SystemTime, Arc<HashMap<..>>)>>`) —
  essencial porque `discover_games()` roda com frequencia (poll de status da
  TUI a cada ~250ms) e o YAML tem 17MB, reparsear toda vez seria lento
  demais. Retorna `Arc` pra clonar barato entre chamadas.
- `resolve_save_paths(entry, app_id, library_path, steam_root, install_dir)`:
  substitui os placeholders do template (`<home>`, `<winAppData>`,
  `<winLocalAppData>`, `<winLocalAppDataLow>`, `<winDocuments>`, `<root>`,
  `<base>`, `<storeGameId>`, `<xdgData>`, `<xdgConfig>` pro caso nativo
  Linux) e roda o resultado como glob (`<storeUserId>`/`<osUserName>`/
  `<language>` viram `*`, igual o proprio Ludusavi trata). So considera
  entradas com `tags: [save]`; `when` filtra por ambiente (Proton = "Windows"
  dentro do prefixo, ou Linux nativo) e por store (so aceita quando ausente
  ou `"steam"`). Placeholder sem traducao conhecida (`<winPublic>` etc,
  raro em save) descarta a entrada em vez de arriscar caminho errado.
- `steam.rs::discover_games()`: pra cada AppID, se o manifest tem uma entrada
  com `files` nao-vazio, usa `resolve_save_paths` (mesmo que resolva pra
  ZERO caminhos — ver bug abaixo); so cai pra heuristica quando o manifest
  nao tem NENHUM `files` documentado pro jogo.

**2 bugs achados e corrigidos so por testar contra o manifest baixado de
verdade (nao davam erro nenhum, so resultado silenciosamente errado):**
1. **Fallback errado pra "The Division":** a entrada do manifest pra esse
   jogo so tem `tags: [config]` (progresso e 100% em servidor, sem save
   local — faz sentido, e sempre-online). A logica original caia pra
   heuristica quando `resolve_save_paths` retornava vazio, sem distinguir
   "jogo ausente do manifest" de "jogo documentado, mas sem save real" —
   ou seja, reintroduzia exatamente o falso positivo que motivou pedir essa
   fonte confiavel. Corrigido checando `!entry.files.is_empty()` (existe
   algo documentado, mesmo que nada seja `save`) em vez de conferir se o
   resultado resolvido ficou vazio.
2. **Placeholder `<root>` nao implementado:** Mad Max, The Surge, The Surge
   2 e Forza Horizon 5 apontam o save real via
   `<root>/userdata/<storeUserId>/<appid>/remote` (espelho da Steam Cloud) —
   sem suporte a `<root>`, essas 4 entradas viravam "sem save" (regressao:
   Mad Max ja estava validado funcionando por heuristica antes). Corrigido
   adicionando `<root>` (= `steam_dir.path()`, a instalacao Steam
   *principal* — **nao** a biblioteca onde o jogo esta instalado, que pode
   estar num disco/mount diferente) e `<base>` (= `install_dir` ja
   resolvido por `library.resolve_app_dir`, entao funciona certo mesmo pra
   jogos numa biblioteca secundaria).

**Validado ao vivo, end-to-end** (rebuild release, reinstalado, daemon
reiniciado, manifest baixado de verdade — 17MB, `~/.local/state/playsync/
ludusavi_manifest.yaml`): comparando a lista completa de jogos desta maquina
antes/depois:
- **God of War**: manifest aponta so `Saved Games/God of War` (1 caminho, o
  save real) em vez dos 4 da heuristica (3 AppData quase vazios + o certo).
- **DARK SOULS II**: aponta o ARQUIVO exato `DS2SOFS0000.sl2` (nao mais a
  pasta `AppData/Roaming` inteira).
- **Ghost of Tsushima**: `<storeUserId>` resolvido certo via glob
  (`76561197994945166`, o steamid3 real desta conta) — sync real rodado
  (`playsync sync --app-id 2215430`), `save.zip` confirmado contendo SO os
  12 `.sav` reais + `steam_autocloud.vdf` desse perfil (antes zipava a pasta
  `Documents/Ghost of Tsushima...` inteira). Upload real pro Google Drive
  confirmado (log do daemon: "upload para o Google Drive concluido";
  `playsync history` mostra sucesso).
- **Mad Max**: `<root>/userdata/.../remote` resolvido certo apos o fix do
  placeholder, batendo com o que a heuristica ja achava antes (era o teste
  de regressao).
- **The Division**: 0 save_paths agora (antes, 1 — a pasta de config sendo
  tratada como se fosse save). Vira "⚠ sem save detectado" na UI, o que e
  honesto pra um jogo sempre-online.
- **The Surge / The Surge 2 / Forza Horizon 5**: tambem 0 save_paths — nao e
  bug, e que nenhum dos tres tem pasta `userdata/<id>/<appid>` criada nesta
  conta (nunca sincronizaram via Steam Cloud, possivelmente nunca jogados
  nesta maquina) nem save local documentado pra Steam (so pra Microsoft
  Store, `store: microsoft`, que nao se aplica). Confirmado olhando o
  filesystem direto — a heuristica antiga dava falsa confianca aqui (3
  pastas AppData vazias, sem save real nenhum dentro).

**Gaps conhecidos, nao resolvidos:**
- `<storeUserId>` nao e resolvido pro ID exato da conta logada, vira glob
  `*` — funciona bem em maquina de usuario unico (so um steamid3 sob
  `userdata/`), mas pegaria saves de TODAS as contas Steam que ja logaram
  nesta maquina se houver mais de uma.
- `<base>`/`<root>` cobrem os casos vistos nesta maquina; outros
  placeholders raros (`<winPublic>`, `<winProgramData>`, `<winDir>`, jogos
  com `<game>` em `files`) nao tem traducao — a entrada e descartada nesse
  caso (log em `debug`), nao trava nada, so significa que aquele arquivo
  especifico do manifest fica de fora ate alguem implementar.
- Sem comando explicito pra forcar refresh do manifest (`playsync manifest
  update` ou similar) — so acontece automatico no startup do daemon/CLI, ou
  esperando os 7 dias do `max_age`.

**Ainda nao commitado** junto com a secao anterior (deteccao Documents/My
Games/Saved Games) — dependencias novas (`serde_yaml`, `glob`, `reqwest`
promovido pra `workspace.dependencies` e agora usado tambem por `playsyncd`/
`playsync-cli`), modulo `manifest.rs` novo, `steam.rs`/`main.rs` (dos dois
binarios) modificados.

## Como validar antes de dizer "pronto"

Os bugs acima so apareceram porque testamos ao vivo (jogo real abrindo/
fechando, systemd real rodando, upload real pro Drive) em vez de confiar em
"deveria funcionar" so porque compilou. Antes de declarar algo "pronto pra
producao" ou sugerir empacotar, rodar o cenario real de ponta a ponta e
mostrar a evidencia (logs, output de verdade, `playsync history`) — os bugs
anteriores nao davam erro nenhum, so "nao acontece nada" ou uma falha
silenciosa no historico.
