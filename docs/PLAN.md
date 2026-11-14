# XkyHold-lx — plano base

Data: 13/11/2026 · Estado: **Fase 0 concluída; Fase 1 em rascunho, à espera da revisão do proprietário**

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

### Fase 1 (13/11/2026, rascunho)

- Rascunho da especificação v2 em `docs/FORMAT-v2.md` do repositório `xkyhold`, no ramo local `format-v2-spec` (sem push, sem deploy). **À espera da revisão do proprietário**; nenhum código depende dela ainda.
- Propostas no rascunho para as questões em aberto 3 e 4: ligações físicas guardadas como ficheiros separados (o conteúdo já é deduplicado; a relação pode vir depois sem versão nova); um repositório v1 só passa a v2 com o acordo do utilizador e só depois de a XkyHold web com leitura v2 estar publicada (alternativa: a lx só escreve em repositórios criados por ela).
- 14/11/2026: revisão do proprietário (transmitida pela sessão do plano base, com o "Sim, envia" dele) aplicada ao rascunho, commit no ramo `format-v2-spec`: o restauro nunca muda o dono e só repõe o grupo quando quem restaura pertence a ele (nomes têm prioridade sobre números); questão 4 decidida (ver secção 8); a web só restaura de repositórios v2 e nenhum escritor da primeira versão põe snapshots v1 num repositório v2; pontos de montagem não atravessados guardados como pastas vazias, com contador; dito o que não se guarda (buracos de ficheiros esparsos, atributos do `chattr`, data de criação); vetores de teste com uma data anterior a 1970, dois nomes inválidos com a mesma forma visível e uma ACL POSIX. Mantidos: o esquema de AAD da secção 3.2 (verificado contra o código da web), o campo `change`, as ligações físicas como ficheiros e a divisão `mtime` + `mtimeNanos`. **A especificação continua por aprovar.**
- 14/11/2026: a pedido do proprietário ("concordo com a tua opinião", via a sessão do plano base), campo opcional `leftOut` por raiz do snapshot v2 com as **regras** de exclusão em vigor nessa cópia (omissões, sugestões ligadas, pastas desmarcadas, padrões, pontos de montagem atravessados), não a lista de caminhos; só para mostrar. Commit `fc86104` no ramo `format-v2-spec`. **A especificação continua por aprovar.**

## 8. Questões em aberto

1. Repositório GitHub `xkyriver/xkyhold-lx`: público desde o início ou só na release?
2. Subdomínio ou página própria (ex.: `xkyhold.xkyriver.com/linux`), e entrada no xkyriver.com.
3. ~~Ligações físicas~~: mantida na revisão de 14/11/2026 a proposta do rascunho, guardá-las como ficheiros separados (fecha com a aprovação da especificação).
4. ~~Repositório v1 → v2~~: **decidido pelo proprietário a 14/11/2026:** a lx só escreve em repositórios criados como v2 e abre os v1 só para navegar, restaurar e verificar. A especificação v2 não tem atualização no lugar (pode vir depois, se houver procura). Na primeira versão com leitura v2, a XkyHold web só restaura de repositórios v2: não copia para eles nem os limpa.
5. Quota da Drive API: é partilhada com a XkyHold web por ser o mesmo projeto; medir na Fase 3.
6. O landing da XkyHold sugere hoje o Déjà Dup para configurações e chaves; passa a sugerir a XkyHold-lx depois da release.
