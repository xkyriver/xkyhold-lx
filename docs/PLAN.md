# XkyHold-lx — plano base

Data: 13/11/2026 · Estado: **Fases 0 a 6 concluídas (motor, CLI, Google Drive, janela, cópia automática e leitura v2 na web, 14/11/2026); Fase 7 (prova real e release) a seguir. O deploy da web é do proprietário e tem de vir antes da release**

A XkyHold-lx é a app de Linux da família XkyHold. Faz cópias de segurança cifradas da pasta pessoal (`$HOME`) inteira, ou de partes dela, incluindo ficheiros ocultos, configurações, chaves, permissões e ligações simbólicas. Existe para resolver o que a XkyHold web não consegue por correr num browser. Segue o mesmo desenho e partilha o formato de repositório.

Ler antes de começar, no repositório irmão `../xkyhold`: `docs/FORMAT.md` (formato v1, congelado), `docs/PLAN.md` (secções 9 a 13), `CLAUDE.md`, `src/engine/` (motor de referência em TypeScript), `src/app.css` e `src/ui/` (desenho), `src/lib/i18n*.ts` (textos).

## 1. Âmbito

**Dentro:**

- Guardar `$HOME` inteira ou pastas e ficheiros dentro dela, com tudo o que lá está: ocultos, `~/.config`, `~/.local/share`, `~/.ssh`, `~/.gnupg`, etc.
- Repor para o sítio original ou para outra pasta, com permissões, datas e ligações simbólicas.
- Destinos: Google Drive do utilizador e uma pasta local (disco externo, NAS montado).
- Cópia manual, lembrete, e cópia automática opcional.
- Interface gráfica e linha de comandos.

**Fora (regra fixa):**

- Nada fora de `$HOME`. A app recusa caminhos fora dela, nunca segue ligações simbólicas ao copiar e nunca corre como root. Não é um backup de sistema (`/etc`, pacotes, arranque).
- Windows e macOS.
- Compatibilidade com restic ou com o BackMa.

**Público:** utilizadores avançados de Linux. O tom continua simples e direto, mas a app pode mostrar caminhos, padrões de exclusão e opções que a XkyHold web esconde.

## 2. Decisões do proprietário (13/11/2026)

| # | Tema | Decisão |
|---|---|---|
| L1 | Stack | **Tauri 2 + Svelte 5**, núcleo em **Rust**. Reaproveita o CSS, a estrutura dos ecrãs e as traduções da XkyHold |
| L2 | Formato | **Compatível: formato v2** = v1 + metadados POSIX. A lx lê v1 e v2 e escreve v2. A XkyHold web ganha leitura de v2, para restaurar ficheiros em qualquer browser |
| L3 | Automático | **Opcional.** Por omissão a app só lembra, como a web. Quem quiser liga a cópia automática (temporizador systemd de utilizador) |
| L4 | Destinos | **Google Drive + pasta local**, atrás da mesma interface `Destination` |

Herdadas da XkyHold e mantidas: cifra no cliente e sem servidor; "apagar" no Drive é sempre mover para o lixo; chave de recuperação; EN + pt-PT com "tu"; identidade visual (verde-água do xkyriver, layout neutro e calmo, tema claro/escuro); sem estatísticas nem telemetria; licença AGPL-3.0-only.

## 3. Limitações da XkyHold que a lx resolve

| Limitação na XkyHold web | Causa | Na XkyHold-lx |
|---|---|---|
| Não se pode escolher a pasta pessoal, Documentos nem Ambiente de trabalho | O Chrome recusa pastas "de sistema" | Acesso direto ao sistema de ficheiros: a raiz por omissão é `$HOME` |
| `~/.config`, `~/.local/share`, `~/.ssh` recusadas também por dentro | Idem | Lidas como qualquer pasta |
| Ficheiros ocultos, configurações e chaves não são guardados | Idem | Guardados (e sempre cifrados) |
| Sem permissões, bit de execução, dono, xattrs | A File System Access API só dá nome, tamanho e data | Guardados no formato v2 e repostos |
| Ligações simbólicas não são guardadas | Não são expostas ao browser | Guardadas como ligações (nunca seguidas) |
| O restauro para pasta perde a data de modificação | O browser não a deixa definir | Reposta com precisão de nanossegundo |
| Uma pasta de cada vez; pastas e ficheiros novos ao lado dos escolhidos não entram sozinhos | O browser não dá acesso à pasta de cima | Uma raiz (`$HOME`) e regras de exclusão: o que é novo entra sozinho |
| Autorização das pastas pedida de novo entre sessões | Modelo de permissões do browser | Não existe |
| Sem cópia com o browser fechado | Não há processo residente | Temporizador systemd de utilizador + binário sem janela |
| A sessão Google expira ao fim de 1 hora, a meio de cópias grandes | Sem servidor não há refresh token | Cliente OAuth de app instalada (PKCE + loopback) com refresh token |
| As chaves ficam no IndexedDB do browser, por origem | Web | Secret Service (GNOME Keyring / KWallet) |
| Só Chrome/Edge fazem cópias | API só nesses browsers | Não depende do browser |
| O restauro em `.zip` deixa uma cópia temporária no armazenamento do browser | OPFS | Restauro direto para o disco |
| Só Google Drive | Decisão da v1 | Drive + pasta local |
| Nomes de ficheiro que não são UTF-8 válido | O browser só dá texto | Guardados em bytes (campo próprio no v2) |

## 4. Arquitetura

### 4.1 Repositório

Novo repositório `xkyhold-lx` (git ainda por iniciar nesta pasta). Workspace Cargo com a interface ao lado:

```
crates/core/        motor: crypto, pack, repo, backup, restore, prune, lock, check; trait Destination. Sem UI, sem Tauri
crates/dest-local/  destino em pasta local
crates/dest-drive/  Google Drive API v3 + OAuth de app instalada
crates/cli/         binário `xkyhold-lx-cli` (init, backup, snapshots, ls, restore, check, forget, status)
src-tauri/          binário `xkyhold-lx` (janela); comandos Tauri finos por cima do core
ui/                 Svelte 5 + Vite + TypeScript: ecrãs, i18n, CSS
docs/               PLAN.md (PT-PT), o resto em inglês
packaging/          unidades systemd, .desktop, ícones, metainfo
```

Regras, como na XkyHold: o motor não depende da interface; a interface só fala com o motor por uma fronteira única (comandos e eventos Tauri); os erros atravessam-na como um `kind`; os testes usam um destino em memória ou uma pasta temporária, nunca o Drive real.

### 4.2 Motor (Rust)

- Reimplementação do motor TypeScript segundo o `FORMAT.md`. O motor TS é a referência de comportamento: ordem das escritas (snapshot em último lugar), `abandon()` depois de uma falha, ordem da limpeza (índice novo → apagar índices antigos → apagar packs), locks.
- Criptografia com crates RustCrypto (`argon2`, `aes-gcm`, `hkdf`, `hmac`, `sha2`), `flate2` para DEFLATE em bruto, `zeroize` para chaves. Tem de produzir e ler exatamente os bytes da v1.
- Percurso do disco com `lstat`, sem seguir ligações. Só entram ficheiros normais, pastas e ligações simbólicas; sockets, FIFOs e dispositivos são saltados e contados à parte.
- Incremental: compara com a árvore do snapshot anterior (tamanho, mtime em ns, ctime, inode); o que não mudou não é relido.
- Ficheiro que muda durante a leitura: repetir uma vez; se continuar a mudar, guardar o que foi lido e contar como "alterado durante a cópia". Ficheiro ilegível: saltar e contar. Pasta escolhida ilegível: falhar a cópia.
- Envio em paralelo, retomável; índice intermédio como ponto de controlo (igual à v1). Cache local do índice em `$XDG_CACHE_HOME/xkyhold-lx/`.
- Chunks fixos de 4 MiB na primeira versão (o formato já permite mudar para chunks definidos pelo conteúdo sem versão nova; fica para depois).
- Restauro seguro: nomes validados (sem `/`, `..` nem vazio), escrita com `openat` + `O_NOFOLLOW` para nunca sair da pasta de destino, ficheiros escritos com nome temporário e renomeados no fim. Metadados aplicados no fim (as pastas por último). `chown` só quando o dono coincide com o utilizador; setuid/setgid nunca são repostos.

### 4.3 Formato v2

A especificação escreve-se **antes** do código, no repositório `xkyhold` (`docs/FORMAT.md` continua a ser a fonte única; a v2 entra como secção ou ficheiro ao lado). Direção, a fechar na especificação depois de ler como o leitor TS reage a campos e tipos desconhecidos:

- Entradas de árvore ganham campos opcionais: `mode`, `uid`, `gid`, `user`, `group`, `mtimeNs`, `xattrs`, e `nameBytes` (base64) quando o nome não é UTF-8 válido.
- Tipo novo `symlink`, com `target`.
- As pastas passam a ter metadados próprios (modo, data).
- Ligações físicas (hard links): decidir na especificação; mínimo aceitável é guardá-las como ficheiros separados (o conteúdo já é deduplicado).
- xattrs: guardar `user.*` e as ACL POSIX; `security.*` e `trusted.*` ficam de fora.
- O snapshot indica a origem (`home` e caminhos relativos a ela), para o restauro "para o sítio original".
- Como se marca a versão (no `config`, por snapshot, ou ambos) e como um repositório v1 passa a v2 decide-se na especificação. Restrição: uma XkyHold web antiga nunca pode ler mal um repositório v2; tem de recusar com uma mensagem clara.

Testes obrigatórios:

- Os `golden-v1*.json` da XkyHold são **copiados** para a lx e nunca regenerados: o motor Rust tem de os ler byte a byte.
- Fixtures `golden-v2` novas, criadas uma vez e congeladas com a especificação.
- Ida e volta cruzada: repositório escrito pelo Rust lido pelo motor TS, e o inverso.
- Ida e volta com metadados: modos, ligações, xattrs, nomes não UTF-8, pastas vazias, ficheiros de 0 bytes e de vários chunks.
- Limpeza (prune) com uma falha injetada em cada escrita e em cada remoção, como na XkyHold.

### 4.4 Destinos

- **Pasta local:** mesma disposição de `config`, `snapshots/`, `index/`, `packs/`, `locks/`. "Apagar" move para `trash/` dentro do repositório, com limpeza ao fim de 30 dias. Escrita atómica (temporário + `rename` + `fsync`).
- **Google Drive:** mesma pasta `XkyHold/<nome>` e scope `drive.file`. Cliente OAuth novo do tipo "Desktop app" **no mesmo projeto Google Cloud `xkyhold`**, para que os ficheiros criados pela web e pela lx sejam visíveis às duas (a confirmar na Fase 0; é o risco n.º 1). Fluxo: browser do sistema + PKCE + redirecionamento para `127.0.0.1` numa porta livre. O refresh token fica no Secret Service. O ID e o segredo do cliente entram na compilação por variável de ambiente, fora do git.

### 4.5 Segredos e estado local

| O quê | Onde |
|---|---|
| Chaves do repositório (para não pedir a palavra-passe em cada cópia) e refresh token | Secret Service. Sem Secret Service: a app pede a palavra-passe em cada cópia e a cópia automática fica indisponível |
| Configuração (repositório, o que guardar, exclusões, frequência, retenção, língua) | `$XDG_CONFIG_HOME/xkyhold-lx/config.toml` |
| Cache do índice, registos | `$XDG_CACHE_HOME/xkyhold-lx/`, `$XDG_STATE_HOME/xkyhold-lx/` |

A palavra-passe nunca é guardada. As pastas de cache e estado da própria app ficam sempre fora da cópia.

### 4.6 O que guardar e o que deixar de fora

- Por omissão: `$HOME` inteira. Em alternativa, pastas e ficheiros escolhidos dentro dela.
- "Deixar de fora": a árvore navegável da XkyHold, mais padrões (glob) para quem os quiser escrever.
- Sempre de fora: `~/.cache`, o lixo (`~/.local/share/Trash`), pastas com `CACHEDIR.TAG`, o estado da própria app.
- Sugestões desligadas por omissão, mostradas com o tamanho que ocupam: `node_modules`, `target`, `.venv`, caches de browsers e de Flatpak (`~/.var/app/*/cache`), `~/.local/share/containers`, bibliotecas Steam, imagens de máquinas virtuais, `Downloads`.
- Outros sistemas de ficheiros montados dentro de `$HOME` (discos, rede, FUSE): não são atravessados por omissão; a app lista-os e o utilizador escolhe.
- Aviso para pastas já sincronizadas com a nuvem (Dropbox, etc.): entram por omissão, mas a app mostra-as como candidatas a ficar de fora.

### 4.7 Interface

Mesmos tokens de cor, tipografia, cartão central e componentes de `../xkyhold/src/app.css`; mesmos três destinos de navegação.

- **Assistente:** 1. Destino (Google ou pasta local; deteta um backup existente e encurta para "palavra-passe → restaurar") → 2. O que guardar (pasta pessoal inteira, com "Deixar de fora…" e as sugestões) → 3. Palavra-passe → 4. Chave de recuperação → 5. Lembrete ou cópia automática.
- **Início:** estado, última cópia, próxima, botão "Fazer cópia agora", progresso com o ficheiro atual, cancelar. Avisos acionáveis em faixas.
- **Restaurar:** datas, navegação com caixas de seleção. Destino: outra pasta (por omissão `~/XkyHold-restore/<data>/`) ou o sítio original. No sítio original, a app nunca substitui em silêncio: mostra o que já existe e o utilizador escolhe saltar, substituir ou guardar ao lado.
- **Definições:** o que guardar e exclusões, lembrete / cópia automática e frequência, retenção, verificar, destino e conta, alterar palavra-passe, língua, tema, remover deste computador.
- Textos: origem em inglês, tradução pt-PT com "tu". Reaproveitar as frases da XkyHold onde servirem.
- WebKitGTK em NVIDIA/Wayland pode abrir uma janela em branco: testar na Fase 0 e, se preciso, definir `WEBKIT_DISABLE_DMABUF_RENDERER=1` no arranque.

### 4.8 Cópia automática

- `xkyhold-lx.timer` + `xkyhold-lx.service` de utilizador, instalados e removidos pela app quando se liga ou desliga a opção. `Persistent=true` para recuperar cópias perdidas com o computador desligado.
- O serviço corre `xkyhold-lx-cli backup` com prioridade baixa (`Nice`, `IOSchedulingClass=idle`). Notificação no fim só em caso de erro ou de ficheiros saltados (configurável).
- Um lock local (`flock` em `$XDG_RUNTIME_DIR`) impede a janela e o temporizador de copiarem ao mesmo tempo; os locks do repositório continuam a proteger entre computadores.
- Sem ícone de tabuleiro na primeira versão.
- **Como ficou (Fase 5):** um único temporizador horário corre o próprio binário da janela com `--scheduled` (sem WebKit), em vez de `xkyhold-lx-cli backup`, porque os pacotes só levam um binário. Em cada passagem decide: cópia automática se estiver em atraso, senão lembrete se estiver em atraso, senão nada. Ver a secção 7.

### 4.9 Distribuição

AppImage, `.deb` e `.rpm` pelo empacotador do Tauri, publicados nas releases do GitHub. AUR e Flatpak ficam para depois (o Flatpak limita o acesso à home e não combina com o objetivo). Sem atualizador interno na primeira versão.

## 5. Fases

| Fase | Conteúdo | Feito quando |
|---|---|---|
| **0. Fundação e riscos** | Instalar Rust (via mise) e as dependências do Tauri; `git init`; esqueleto do workspace, CI (fmt, clippy, testes, build), `CLAUDE.md`, `README.md`, `LICENSE`. Protótipos descartáveis: (a) cliente OAuth "Desktop app" no projeto `xkyhold` vê os ficheiros criados pela web, e o inverso, e obtém refresh token; (b) janela Tauri abre bem no Hyprland com NVIDIA; (c) Rust lê o `golden-v1` | Os três riscos têm resposta escrita na secção 7 |
| **1. Especificação v2** | Ler o leitor TS; escrever a v2 no repositório `xkyhold`; rever com o proprietário | Especificação aprovada pelo proprietário |
| **2. Motor** | `core` com paridade de leitura v1, escrita v2, backup, restauro, prune, locks, check; destino em memória e pasta local; CLI | Cópia e restauro de uma árvore de teste com metadados dão resultado idêntico (conteúdo, modos, ligações, datas, xattrs); golden v1 e ida e volta cruzada com o motor TS a passar |
| **3. Google Drive** | `dest-drive`, OAuth, Secret Service, retoma de envios, renovação do token | Cópia e restauro reais no Drive pelo CLI, incluindo uma cópia que dura mais de 1 hora |
| **4. Interface** | App Tauri: assistente, Início, Restaurar, Definições, EN + pt-PT, claro/escuro | Fluxo completo pela janela, com o Drive e com pasta local |
| **5. Automático** | Unidades systemd, lock local, notificações, lembrete | Cópia automática vista a correr com a janela fechada e depois de um reinício |
| **6. XkyHold web lê v2** | No repositório `xkyhold`: leitura de v2 no motor TS e no Restaurar (o que não se pode repor num browser é ignorado), testes, golden v2 | Um repositório escrito pela lx abre e restaura na web em pré-visualização local. **O deploy é do proprietário e tem de acontecer antes da primeira release da lx** |
| **7. Prova real e release** | Cópia da home real do proprietário, "Verificar", restauro completo para outra pasta e comparação; restauro numa máquina ou utilizador limpo; pacotes; página no site; checkpoint | v1.0.0 publicada |

## 6. Regras de trabalho

- Conversa com o proprietário em português europeu, por "tu". Código, comentários, commits e documentação técnica em inglês; este `PLAN.md` em PT-PT.
- Commits: título e pontos, sem linhas de atribuição ao agente. Nos ficheiros versionados não aparecem caminhos do computador, o email do proprietário nem o nome da empresa dele.
- Google Cloud, Cloudflare, GitHub (criar o repositório, torná-lo público, releases) e qualquer deploy: só com o "sim" do proprietário para esse ato concreto. `npm run deploy` é sempre dele.
- Passos que dependem do proprietário (login Google, diálogos do sistema, palavras-passe): um de cada vez, com aviso antes.
- Segredos nunca em chat nem no git.
- **Proteção da home do proprietário durante o desenvolvimento:** os testes automáticos usam só pastas temporárias. A cópia lê e nunca escreve na origem. Nenhum restauro para o sítio original na home real sem um pedido explícito; os restauros de prova vão para uma pasta nova. No Drive, só repositórios de teste com nome próprio (`XkyHold-lx test…`) e nunca apagar de forma permanente.
- Nunca regenerar ficheiros `golden`. Mudar o formato: primeiro a especificação, depois o código.
- Relatórios: separar o que foi visto, o que só foi testado automaticamente e o que não foi testado. Sem estatísticas.
- No fim de cada fase: atualizar a secção 7 deste plano e o checkpoint do projeto.

## 7. Registo do estado

- 13/11/2026: plano base escrito; decisões L1–L4 tomadas. Nada implementado. Rust e Go não estão instalados no computador; Node, restic e rclone estão; `gtk4`, `libadwaita` e `webkit2gtk-4.1` estão instalados.

### Fase 0 (13/11/2026)

Feito:

- Rust 1.100.0 instalado pelo mise, fixado em `mise.toml` e `rust-toolchain.toml` (com `rustfmt` e `clippy`). Dependências do Tauri: já estavam todas instaladas (`webkit2gtk-4.1`, `gtk3`, `libsoup3`, `librsvg`, `base-devel`, `openssl`, `libayatana-appindicator`); nada foi instalado no sistema.
- `git init` local (ramo `main`, identidade `xkyriver` neste repositório), sem remoto.
- Esqueleto do workspace: `crates/core`, `crates/dest-local`, `crates/dest-drive`, `crates/cli`, `src-tauri` (Tauri 2.12), `ui` (Svelte 5 + Vite 8, porta 5174, com o `app.css` da XkyHold), `packaging`. Raiz com `package.json` (workspace npm com o `@tauri-apps/cli`). CI em `.github/workflows/ci.yml` (fmt, clippy com `-D warnings`, testes, build). `CLAUDE.md`, `README.md`, `LICENSE` (AGPL-3.0-only, o mesmo texto da XkyHold).
- `golden-v1.json` e `golden-v1-repo.json` copiados para `crates/core/tests/fixtures/` (SHA-256 iguais aos da XkyHold).

Os três riscos:

| Risco | Resposta |
|---|---|
| (b) Janela Tauri no Hyprland com NVIDIA | **Funciona com um ajuste; sem ele não abre.** Testado com a NVIDIA como GPU principal (`AQ_DRM_DEVICES` com a dGPU primeiro), Wayland nativo. Sem nada, o processo morre ao abrir: `Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display`. Com `WEBKIT_DISABLE_DMABUF_RENDERER=1` a janela abre, mostra o desenho da XkyHold (tema escuro, ícone) e a chamada ao motor Rust responde ("Engine 0.1.0 reads format 1"). `__NV_DISABLE_EXPLICIT_SYNC=1` sozinho também resolve. Visto em captura do ecrã pelo agente. **Consequência para a Fase 4:** a app tem de definir uma destas variáveis antes de o GTK arrancar, quando o utilizador não a definiu. Em Rust 2024, `std::env::set_var` é `unsafe` e o workspace proíbe `unsafe`: proponho uma exceção localizada e comentada no `main` (antes de haver threads), ou um lançador no `.desktop`; a primeira serve também quem corre o binário à mão. Não testado: com a Intel como GPU principal, outra máquina sem NVIDIA, cliques na janela (só a renderização e a chamada ao Rust) |
| (c) Rust lê o `golden-v1` | **Sim, byte a byte.** `crates/core` abre os dois fixtures com a palavra-passe e com a chave de recuperação (Argon2id, HKDF, AES-256-GCM, HMAC dos crates RustCrypto), deriva o mesmo id de blob, lê o pack (SHA-256 igual ao nome), o cabeçalho volta a dar o mesmo JSON campo a campo, e restaura o repositório inteiro com o conteúdo esperado (incluindo o ficheiro vazio, a pasta vazia e o `noise()` reimplementado). Também recusa uma palavra-passe errada e um pack adulterado. 8 testes a passar. Este código não é descartável: é o início da leitura v1 do `core` |
| (a) Cliente OAuth "Desktop app" no projeto `xkyhold` | **Funciona; risco n.º 1 afastado.** Cliente "XkyHold-lx" (tipo Desktop app) criado a 13/11/2026 pelo agente na consola, com o "sim" do proprietário; o ID e o segredo foram copiados pelo proprietário para `.env.local` (fora do git; o segredo não passou pelo agente). Programa de prova `crates/dest-drive/examples/oauth_probe.rs` (PKCE + redirecionamento para `127.0.0.1` numa porta livre), login do proprietário no Chrome dele: a Google concedeu só `drive.file`, mostrou o nome e o logótipo da XkyHold sem avisos, deu um refresh token, e o refresh devolveu um token novo válido por 1 hora. **Web → lx:** o cliente novo vê `XkyHold/Windows computer test`, criada pela XkyHold web. **lx → web:** o programa criou a pasta vazia `XkyHold/XkyHold-lx test probe` e, segundo o proprietário, ela aparece na lista de backups da XkyHold web (janela anónima, site publicado). Conclusão: os clientes do mesmo projeto partilham o acesso `drive.file`. Não testado: a renovação a meio de uma cópia longa (Fase 3) e o login a partir da app empacotada. A pasta de teste fica no Drive até o proprietário a apagar. O `docs/OAUTH.md` da XkyHold ainda não menciona o cliente novo |

**Fase 0 concluída a 13/11/2026:** os três riscos têm resposta. Ajuste no programa de prova: `PROBE_NO_OPEN=1` só mostra o endereço de login, porque o `xdg-open` abriu o Chrome errado (o proprietário tem dois).

Achado para a Fase 1: a XkyHold web 1.0, ao reabrir um backup que o browser já conhece (chaves guardadas no IndexedDB), não volta a ler o `config` e por isso não verifica o `format`. Se um repositório v1 passar a v2, um separador antigo leria trees v2 como v1. A especificação resolve isto selando os snapshots v2 com outro AAD (secção 3.2 do rascunho).

### Fase 1 (13–14/11/2026)

- Rascunho da especificação v2 em `docs/FORMAT-v2.md` do repositório `xkyhold`, no ramo local `format-v2-spec` (sem push, sem deploy). **À espera da revisão do proprietário**; nenhum código depende dela ainda.
- Propostas no rascunho para as questões em aberto 3 e 4: ligações físicas guardadas como ficheiros separados (o conteúdo já é deduplicado; a relação pode vir depois sem versão nova); um repositório v1 só passa a v2 com o acordo do utilizador e só depois de a XkyHold web com leitura v2 estar publicada (alternativa: a lx só escreve em repositórios criados por ela).
- 14/11/2026: revisão do proprietário (transmitida pela sessão do plano base, com o "Sim, envia" dele) aplicada ao rascunho, commit no ramo `format-v2-spec`: o restauro nunca muda o dono e só repõe o grupo quando quem restaura pertence a ele (nomes têm prioridade sobre números); questão 4 decidida (ver secção 8); a web só restaura de repositórios v2 e nenhum escritor da primeira versão põe snapshots v1 num repositório v2; pontos de montagem não atravessados guardados como pastas vazias, com contador; dito o que não se guarda (buracos de ficheiros esparsos, atributos do `chattr`, data de criação); vetores de teste com uma data anterior a 1970, dois nomes inválidos com a mesma forma visível e uma ACL POSIX. Mantidos: o esquema de AAD da secção 3.2 (verificado contra o código da web), o campo `change`, as ligações físicas como ficheiros e a divisão `mtime` + `mtimeNanos`. **A especificação continua por aprovar.**
- 14/11/2026: a pedido do proprietário ("concordo com a tua opinião", via a sessão do plano base), campo opcional `leftOut` por raiz do snapshot v2 com as **regras** de exclusão em vigor nessa cópia (omissões, sugestões ligadas, pastas desmarcadas, padrões, pontos de montagem atravessados), não a lista de caminhos; só para mostrar. Commit `fc86104` no ramo `format-v2-spec`. **A especificação continua por aprovar.**
- **Fase 1 concluída a 14/11/2026: especificação v2 aprovada pelo proprietário** ("Aprovo") e marcada como congelada (commit `1c4ded6` no ramo `format-v2-spec` do `xkyhold`; o `FORMAT.md` passou a apontar para o `FORMAT-v2.md`). O ramo continua só local: juntá-lo ao `main` e fazer push no repositório público `xkyhold` precisa do "sim" do proprietário. O `golden-v2-repo.json` é escrito uma vez na Fase 2 e congelado.

### Fase 2 (14/11/2026)

**Concluída a 14/11/2026.** Motor em Rust no `crates/core`, segundo o `FORMAT-v2.md` aprovado:

- Escrita só em repositórios v2: `config` com `format: 2`, packs, índice com ponto de controlo, snapshot escrito por último e selado com `xkyhold/v2/snapshot/<id>`. Leitura de v1 e v2; os repositórios v1 abrem-se só para ler, restaurar e verificar.
- Cópia da `$HOME` por descritores (`openat` + `O_NOFOLLOW`): nunca segue ligações; guarda modos, dono e grupo (números e nomes), datas ao nanossegundo, xattrs `user.*` e ACL POSIX, ligações simbólicas e nomes que não são UTF-8. Ficheiros especiais contados à parte. Incremental por tamanho, mtime em ns, ctime e inode. Ficheiro que muda durante a leitura: lido de novo uma vez e, se continuar a mudar, guardado como lido e contado. Pontos de montagem (lidos de `/proc/self/mountinfo`) guardados como pastas vazias, salvo os escolhidos para atravessar. Regras de exclusão (omissões, `CACHEDIR.TAG`, sugestões, caminhos, padrões) gravadas no `leftOut` do snapshot. Um repositório dentro da home exclui-se a si próprio. Recusa correr como root e caminhos fora da home.
- Restauro seguro: tudo relativo à pasta de destino com `O_NOFOLLOW`, nome temporário + `rename`, metadados no fim (pastas depois do conteúdo), nunca muda o dono, grupo só se quem restaura pertencer a ele, nunca setuid/setgid; conflitos: saltar, substituir ou guardar ao lado.
- Locks (com renovação numa thread), limpeza na ordem segura, retenção (mantém sempre a mais recente), verificação rápida e completa.
- `dest-local` (escrita atómica, lixo dentro do repositório durante 30 dias) e `xkyhold-lx-cli` (`init`, `backup`, `snapshots`, `ls`, `restore`, `check`, `forget`, `status`, `presets`); o restauro pelo CLI só aceita destinos dentro da home.
- **`golden-v2-repo.json` escrito uma vez e congelado** (commit `8e28319`); o gerador recusa reescrevê-lo.

Critério da fase:

| Critério | Resultado |
|---|---|
| Cópia e restauro de uma árvore de teste com metadados dão resultado idêntico | **Testado automaticamente** (pastas temporárias): conteúdo, modos, ligações (relativa, quebrada, para fora da home), datas ao ns incluindo antes de 1970, xattr `user.` e ACL POSIX, nome não UTF-8, pasta vazia, setuid retirado, ponto de montagem vazio, FIFO contado e não guardado. A origem fica byte a byte igual depois da cópia |
| golden v1 | **Testado automaticamente**: os dois fixtures continuam a passar, e um repositório v1 é recusado para escrita |
| Ida e volta cruzada com o motor TS | **Testado automaticamente** com `scripts/cross-ts.sh` (worktree descartável do `xkyhold`, checkout principal intacto): o motor TS escreve um repositório v1 e o Rust restaura-o com conteúdo e datas; o Rust escreve um v2 e o TS lê todos os bytes (índice, packs, trees, dados) e confirma que a XkyHold 1.0 recusa o `format: 2` e falha ao listar snapshots v2. O restauro de v2 **pelo** motor TS é da Fase 6 |
| Visto | O agente correu o CLI numa home de teste na pasta temporária: `init`, duas cópias (a segunda, com `Videos` deixado de fora, enviou 526 B), `ls`, restauro (`.ssh` 0700, chave 0600, ligação e data de 1969 repostas), `check --full` e `status`. Nada na home real do proprietário |

30 testes a passar (mais 3 que só correm por script ou à mão), `clippy` sem avisos. Não testado: volumes grandes e desempenho, um disco externo real, outros sistemas de ficheiros além de tmpfs (btrfs, ext4), xattrs em ligações simbólicas, e o CI no GitHub (sem remoto).

Ficam para a Fase 3, por serem do Drive: envios em paralelo e retomáveis, e a cache local do índice em `$XDG_CACHE_HOME/xkyhold-lx/`. O lock local entre a janela e o temporizador é da Fase 5. Armadilha a lembrar: em btrfs, subvolumes têm `st_dev` próprio; por isso os pontos de montagem vêm do `mountinfo` e não da comparação de `st_dev`.

### Fase 3 (14/11/2026)

**Concluída a 14/11/2026.** Google Drive no `crates/dest-drive` e segredos no `crates/secrets` (crate nova, fora da lista inicial da secção 4.1: o acesso ao Secret Service não pertence ao motor nem a um destino):

- Cliente da Drive API v3 (porte do `drive.ts`): repete falhas temporárias com espera crescente, renova o token numa resposta 401, pagina as listagens, descarrega intervalos e faz envios simples ou retomáveis em blocos de 8 MiB que retomam a partir do ponto que o servidor indica. Limite opcional de velocidade (`--limit-upload`).
- `DriveFolder`: a mesma disposição `XkyHold/<nome>` da web; "apagar" é sempre mover para o lixo; lembra-se do que escreveu e apagou na sessão (as listagens do Drive atrasam-se). As subpastas são criadas por uma thread de cada vez, e a leitura junta as subpastas com o mesmo nome.
- Login de app instalada: browser do sistema, PKCE e redirecionamento para `127.0.0.1`. O refresh token fica no Secret Service (GNOME Keyring); os tokens de acesso ficam em memória e são renovados um minuto antes de expirarem. O ID e o segredo do cliente vêm do ambiente (na compilação ou na execução), nunca do git.
- Chave mestra do repositório no Secret Service (`remember`), para cópias sem palavra-passe; uma chave errada é detetada pelo primeiro objeto que decifra.
- No motor: envios em paralelo (3 packs), cache local dos índices cifrados em `$XDG_CACHE_HOME/xkyhold-lx`, restauro de 6 ficheiros e 4 blocos de cada vez, verificação completa de 6 blocos de cada vez.
- CLI: `--drive`, `login`, `logout`, `backups`, `remember`, `forget-key`; `config.toml` em `$XDG_CONFIG_HOME/xkyhold-lx`; `XKYHOLD_LX_LOG=1` regista cada token novo.

Critério da fase:

| Critério | Resultado |
|---|---|
| Cópia e restauro reais no Drive pelo CLI | **Visto pelo agente**, depois do login do proprietário no Chrome dele (o refresh token foi para o keyring, sem passar pelo agente). Repositório `XkyHold-lx test 1`, home de teste de 24 MB na pasta temporária: primeira cópia 25 MB em 14 s, segunda 0 B; restauro com cache vazia idêntico à origem (16 itens: conteúdo, modos, datas ao ns incluindo 1969, xattr, ligações, pasta vazia); `check --full` sem problemas |
| Uma cópia que dura mais de 1 hora | **Visto pelo agente.** Repositório `XkyHold-lx test 2`: 2 GB de dados aleatórios a 500 KB/s, 3997 s (66,6 min). Primeiro token às 01:23:25; **renovado sozinho às 02:22:32** e a cópia continuou. Verificação rápida: 160 packs, 482 blocos. Restauro dos 2 GB em 153 s com SHA-256 iguais aos originais |
| Testado automaticamente | 44 testes, incluindo um Drive falso em memória: o motor inteiro, erros 503, token revogado a meio de uma cópia, sessão revogada, envio de 20 MB com a ligação cortada a meio, paginação, limite de velocidade, uploads em paralelo a criar uma só pasta `packs`, pastas duplicadas |

Problemas encontrados nos testes reais e corrigidos: (1) a biblioteca HTTP tratava o `308 Resume Incomplete` do Drive como um redirecionamento (`cf0f51c`; o Drive falso não passava pela biblioteca real); (2) envios em paralelo podiam criar duas pastas `packs` e dividir os packs entre elas (`c2861dd`; apanhado por um teste que falhava de vez em quando). A cópia longa correu com o binário anterior a (2); a verificação e o restauro com o binário novo encontram tudo. (3) Restauro e verificação lentos (cerca de 1 MB/s): passaram a ir em paralelo.

Não testado: renovação depois de suspender o computador a meio, quota da Drive API em volume (questão 5: nenhum erro 429 nestes testes, mas nada foi medido a sério), contas Workspace, Secret Service ausente ou bloqueado (o caminho existe: a app pede a palavra-passe), KWallet. Ficam no Drive do proprietário as pastas de teste `XkyHold-lx test probe`, `XkyHold-lx test 1` e `XkyHold-lx test 2` (cerca de 2 GB), para ele apagar quando quiser.

### Fase 4 (14/11/2026)

**Concluída a 14/11/2026.** Janela Tauri com o assistente, Início, Restaurar e Definições (secção 4.7), em inglês e pt-PT, tema claro e escuro.

- **Arranque em NVIDIA (decisão do proprietário, 14/11/2026):** com o driver NVIDIA carregado e sem `WEBKIT_DISABLE_DMABUF_RENDERER` definido, o programa substitui-se a si próprio (`exec`) com a variável a 1. É Rust seguro, sem exceção à regra do `unsafe`, mantém o PID e o nome do processo, e funciona a partir do terminal, de um lançador ou de um AppImage. Um valor já definido prevalece (`=0` desliga). Consultada a sessão sénior (Fable 5.2), que confirmou a opção e propôs a condição NVIDIA em vez de Wayland. Visto: mesmo PID, variável presente, janela aberta.
- **Crate nova `crates/app`** (`xkyhold-app`), partilhada pela janela e pelo CLI: `config.toml` (destino, o que guardar, lembrete, retenção, língua, tema, última cópia), alterado sempre sob um único lock; abertura com o keyring ou com a palavra-passe; cópia com retenção; listagem de pastas e espaço de cada sugestão. O CLI sem `--repo`/`--drive` usa a cópia configurada neste computador, com as escolhas e a retenção (necessário para o temporizador da Fase 5).
- **Comandos Tauri finos** sobre essa crate; os erros chegam à interface como `kind` e o texto em bruto vai para `$XDG_STATE_HOME/xkyhold-lx/xkyhold-lx.log`. Cópia, restauro e verificação correm numa thread e informam por eventos.
- **Assistente:** Google Drive ou pasta/disco; deteta cópias existentes (no Drive pela lista, numa pasta pelo `config`) e encurta para a palavra-passe ou a chave de recuperação; ao voltar a ligar, recupera a última cópia deste computador e as escolhas do que guardar a partir das regras `leftOut` dessa cópia. Passo 5: só o lembrete; a cópia automática entra na Fase 5.
- **O que guardar:** pasta pessoal inteira ou só algumas pastas, árvore para deixar de fora, sugestões com o espaço que ocupam (contado em segundo plano), aviso de pastas de nuvem (Dropbox, etc.), outros discos montados dentro da home, padrões.
- **Restaurar:** datas, navegação, pasta nova `~/XkyHold-restore/<data local>/` por omissão, ou o sítio original; aí mostra o que já existe e o utilizador escolhe manter os dois, saltar ou substituir.
- **Definições:** o que guardar, lembrete, retenção, verificar (rápida e completa, com Parar), destino e conta, palavra-passe e "abrir sem palavra-passe" (keyring), língua e tema, remover deste computador.
- Fechar a janela durante um trabalho pergunta primeiro. Um script (`ui/scripts/check-i18n.mjs`, corre no `npm run check`) falha se algum texto não tiver tradução pt-PT. Só nas compilações de desenvolvimento: `XKYHOLD_LX_PROBE` deixa um script conduzir a janela (foi assim que o agente a testou).

Critério da fase:

| Critério | Resultado |
|---|---|
| Fluxo completo pela janela, com pasta local | **Visto pelo agente** (capturas e leitura do DOM), numa home temporária: assistente até à chave de recuperação, cópia, restauro para pasta nova e para o sítio original com conflito ("manter os dois"), verificação rápida e completa, alterar palavra-passe, desligar o keyring e abrir com a palavra-passe (e com a errada: mensagem certa), remover deste computador e voltar a ligar com a chave de recuperação, pt-PT e tema claro |
| Fluxo completo pela janela, com o Drive | **Visto pelo agente**, com o **login Google feito pelo proprietário a partir da janela**: lista das cópias do Drive, criação de `XkyHold-lx test 3`, cópia, restauro de 2,2 MB com SHA-256 iguais, verificação completa, ligar outra vez a uma cópia existente. Fechar durante uma cópia pergunta; "Parar e fechar" fecha em cerca de 5 s. Uma cópia parada a meio e a janela fechada e reaberta: a seguinte enviou 40 MB de um ficheiro de 80 MB |
| Testado automaticamente | 53 testes (mais 9 que na Fase 3), incluindo a cópia parada continuada pela seguinte (falha sem a correção) e o CLI com a cópia configurada; verificação de traduções |

Problemas encontrados pela janela e corrigidos: (1) gravações das definições em paralelo perdiam uma delas; (2) uma cópia parada só registava os packs no índice de 8 em 8, e reenviava até cerca de 128 MB; agora regista os que terminaram; (3) os dados do web view da própria app (`~/.local/share/com.xkyriver.xkyhold-lx`) entravam na cópia: passaram para a regra fixa `app-state` (o formato não muda: as regras gravadas na cópia dizem os padrões usados); (4) "manter os dois" chamava `notes.txt (restored)` e perdia a extensão: agora `notes (restored).txt`; (5) a pasta de restauro tinha a hora em UTC; (6) uma pasta com uma cópia ainda sem nenhuma cópia feita não era reconhecida; (7) a verificação não se podia parar; (8) "enviados" somava as cópias anteriores da mesma sessão.

Não testado: sessão Google a expirar a meio pela janela (o caminho existe: faixa com "Entrar de novo"); abrir na janela uma cópia v1 feita pela XkyHold web (só pelo CLI e pelos testes); disco montado dentro da home (sem um nesta máquina de teste); "Imprimir" da chave de recuperação; leitores de ecrã; empacotamento (Fase 7). Nota do teste: o `xdg-open` herda a `HOME` da janela, e o Chromium criou um perfil novo na home temporária; numa home real isto não acontece. Fica no Drive do proprietário a pasta de teste `XkyHold-lx test 3` (cerca de 370 MB).

### Fase 5 (14/11/2026)

**Concluída a 14/11/2026.** Cópias automáticas e lembretes com a janela fechada (secção 4.8), em `crates/app/src/schedule.rs`.

- **Um temporizador, hora a hora:** `xkyhold-lx.timer` (`OnCalendar=hourly`, `Persistent=true`, 5 min aleatórios) e `xkyhold-lx.service` (`Type=oneshot`, `Nice=10`, `IOSchedulingClass=idle`) correm `xkyhold-lx --scheduled`. Cada passagem decide pelas definições: cópia automática quando está em atraso (a cada hora, dia ou semana, com uma pequena folga), senão a notificação de lembrete (no máximo uma por dia), senão nada. Desvio da secção 4.8: corre o binário da janela e não o `xkyhold-lx-cli`, porque os pacotes só levam um binário; com `--scheduled` não arranca o WebKit.
- **Instalação:** os ficheiros ficam na pasta de configuração da app e são ligados ao systemd (`systemctl --user link`/`enable`); a janela instala-os, atualiza-os e remove-os conforme as definições, e atualiza o caminho do programa no arranque (num AppImage usa o próprio AppImage). Existem enquanto há uma cópia configurada e lembretes ou cópias automáticas ligados; "Remover deste computador" apaga-os.
- **A cópia automática exige a chave no keyring;** a janela não a deixa ligar sem isso e desliga-a quando a chave é removida.
- **Notificações (`notify-send`):** só em problemas (no máximo uma por dia; rede em baixo só depois de dois dias sem cópia), ficheiros que não se leram, e êxito se o utilizador pedir. Textos em EN e pt-PT.
- **Lock local:** `flock` em `$XDG_RUNTIME_DIR/xkyhold-lx.lock`, tomado por qualquer cópia (janela, temporizador, CLI); a segunda recusa com `running-here`. A janela mostra uma cópia a correr noutro processo e atualiza o estado a cada 20 s.
- Assistente: passo 5 com "lembrar-me" ou "cópia automática todos os dias". Definições: frequência e aviso de êxito.

Critério da fase:

| Critério | Resultado |
|---|---|
| Cópia automática vista a correr com a janela fechada | **Visto pelo agente**, numa home de teste em `/var/tmp` com destino numa pasta local: assistente com "cópia automática todos os dias" instalou o temporizador; janela fechada; o serviço fez a cópia sozinho com a chave do keyring. Também vistos: disco desligado → notificação, e a segunda falha seguida não notificou; disco de volta → cópia e problema limpo; lembrete com a cópia automática desligada, sem repetir; com uma cópia automática de 400 MB a correr, a janela mostrou-a, recusou outra e atualizou a última cópia no fim |
| … e depois de um reinício | **Visto pelo agente**, com o **reinício feito pelo proprietário** às 13:41: o temporizador continuou ativo e a passagem das 14:02:53 fez a cópia (5 ficheiros) sem nenhuma janela aberta |
| Testado automaticamente | 56 testes: decisão de cada passagem, ficheiros de unidade (caminho com espaços), lock local, o CLI com o lock |

Notas: o temporizador e a chave de teste foram removidos no fim; ao remover, o gestor do systemd guardava o temporizador como "failed": a remoção passou a fazer `reset-failed`. Não confirmado pelo proprietário: se viu as notificações (o `notify-send` não deu erro). Não testado: cópia automática para o Drive (o caminho é o mesmo da janela; precisa do cliente Google compilado no binário, como na release), sistema sem systemd, KWallet, portátil em bateria (a cópia corre na mesma, com prioridade baixa).

### Fase 6 (14/11/2026)

**Concluída a 14/11/2026**, no repositório `xkyhold`, ramo `web-reads-v2` (sobre `format-v2-spec`; commits `7dc11cd`, `bd626f2`, `f0e55bc`; nada enviado). Detalhe na secção 14 do `docs/PLAN.md` da XkyHold.

- Motor TS: lê `format` 1 e 2, abre os snapshots com o AAD v2 e depois o v1, confere o `format` do snapshot com o AAD; prune para em tipos desconhecidos. Os repositórios v2 são só de restauro: o motor recusa cópia e mudança de palavra-passe (`read-only`), também quando reabre com as chaves guardadas.
- Restauro no browser: ligações simbólicas para o `.zip` como ligações e saltadas numa pasta; tipos desconhecidos saltados; nomes que aparecem iguais não se sobrepõem; o aviso diz quantos de cada. O ecrã esconde fazer cópia, lembretes, pastas e palavra-passe.
- `golden-v2-repo.json` copiado byte a byte (sha256 `bdacbe3d…`) e lido pelo motor TS. 169 testes na web. O `scripts/cross-ts.sh` da lx passou a verificar o que a XkyHold em causa faz: com leitura v2 usa o leitor real, sem ela confirma a recusa; passa contra o ramo novo e contra o `main` da XkyHold.

Critério da fase:

| Critério | Resultado |
|---|---|
| Um repositório escrito pela lx abre e restaura na web em pré-visualização local | **Visto pelo agente**, no browser integrado da app do Claude com o **login Google feito pelo proprietário** (o Chrome dele já tinha outra cópia configurada em `localhost:5173`, e ficou intacto): `XkyHold-lx test 3` (feita pela janela da lx) abriu em modo só restauro; Início e Definições sem fazer cópia; "Verificar" da cópia das 13:08 com 11 ficheiros intactos; navegação com a ligação `notes-link`; "Transferir em .zip" (o proprietário confirmou o nome do ficheiro): 908 bytes com os ficheiros e a ligação em modo `0o120777`, e o `unzip` recriou a ligação |
| Testado automaticamente | 169 testes na web (9 novos) e a ida e volta cruzada nos dois sentidos |

Não testado: restaurar para uma pasta a partir do browser (precisa do seletor de pastas do sistema; o caminho é o mesmo do `.zip` sem as ligações), Firefox e Safari, cópias v2 com nomes inválidos no Drive real (só no golden). **Por fazer pelo proprietário:** juntar `format-v2-spec` e `web-reads-v2` ao `main` da XkyHold e fazer o deploy antes da release da lx. Fica no `~/Downloads` do proprietário o `XkyHold 2026-11-14.zip` de teste.

### Fase 7: decisões do proprietário (14/11/2026)

- **Prova real nos dois destinos:** um disco externo que o proprietário vai ligar, e o Google Drive de **outra conta Google**, com mais espaço, para caber a home inteira. A home tem cerca de 56 GB sem `.cache`; o disco interno só tem 22 GB livres, por isso o restauro completo para comparação também tem de ir para o disco externo. Hoje a app só restaura para pastas dentro da home: proposta a apresentar nessa altura (restaurar para uma pasta escolhida fora da home, como um disco; a cópia continua limitada à home).
- **GitHub:** privado já, público na release.
- **Site:** página no site da XkyHold.
- **XkyHold web:** os ramos `format-v2-spec` e `web-reads-v2` foram juntados ao `main` local da XkyHold a 14/11/2026 (avanço rápido até `f0e55bc`; 169 testes a passar); o push e o deploy ficam para o proprietário, antes da release da lx.

## 8. Questões em aberto

1. ~~Repositório GitHub~~: **decidido pelo proprietário a 14/11/2026:** privado desde já (com o "sim" dele no momento de o criar), público na release v1.0.0.
2. ~~Subdomínio ou página própria~~: **decidido pelo proprietário a 14/11/2026:** página no site da XkyHold (`xkyhold.xkyriver.com/linux/` e `/pt/linux/`), com a landing a apontar para ela em vez do Déjà Dup.
3. ~~Ligações físicas~~: mantida na revisão de 14/11/2026 a proposta do rascunho, guardá-las como ficheiros separados (fecha com a aprovação da especificação).
4. ~~Repositório v1 → v2~~: **decidido pelo proprietário a 14/11/2026:** a lx só escreve em repositórios criados como v2 e abre os v1 só para navegar, restaurar e verificar. A especificação v2 não tem atualização no lugar (pode vir depois, se houver procura). Na primeira versão com leitura v2, a XkyHold web só restaura de repositórios v2: não copia para eles nem os limpa.
5. Quota da Drive API: é partilhada com a XkyHold web por ser o mesmo projeto; medir na Fase 3.
6. O landing da XkyHold sugere hoje o Déjà Dup para configurações e chaves; passa a sugerir a XkyHold-lx depois da release.
