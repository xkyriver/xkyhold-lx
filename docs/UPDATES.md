# Atualizações — plano (XkyHold-lx e XkyHold web)

Data: 15/11/2026 · Estado: **aprovado pelo proprietário a 15/11/2026 (U1–U5 como recomendado); em execução**

Objetivo: quando sai uma versão nova, quem já tem a app (a de Linux ou a web) é avisado dentro da app e tem um botão para atualizar. Substitui a decisão "sem atualizador interno na primeira versão" do `PLAN.md` (secção 4.9).

## 1. Ponto de partida (visto a 15/11/2026)

| | Hoje |
|---|---|
| XkyHold-lx | v1.0.1 nas releases do GitHub (`xkyriver/xkyhold-lx`, público): AppImage, `.deb`, `.rpm`, `SHA256SUMS`. A app nunca contacta o GitHub. A CSP da janela não permite pedidos de rede; a rede é feita em Rust (`ureq`) |
| AppMan | Guarda o AppImage como `~/Applications/xkyhold-lx/xkyhold-lx`, com um ficheiro `version` (o URL descarregado) e um `AM-updater` ao lado. `appman -u` descarrega sem verificar assinatura |
| XkyHold web | Site estático, sem service worker. Recarregar a página traz sempre a versão publicada. Quem fica desatualizado é quem deixa o separador (ou a PWA instalada) aberto durante dias. A CSP só permite `connect-src 'self'` e a Google |
| Quem tem a lx 1.0.0 ou 1.0.1 | Não tem o verificador: só passa a ser avisado depois de atualizar à mão uma vez. Dizer isto nas notas da release que o introduzir |

## 2. Regras (iguais às do resto do produto)

- **Avisar, nunca atualizar sozinha.** A app mostra que há versão nova; só o botão atualiza.
- **Nunca a meio de uma cópia, restauro ou verificação.** O botão fica desativado com a frase "Atualiza quando esta cópia terminar".
- **Sem telemetria.** O pedido não leva identificadores; a app só descarrega um ficheiro público. A página de privacidade passa a dizê-lo.
- **Só se instala o que estiver assinado** com a chave do proprietário. A soma SHA-256 não chega: quem conseguisse trocar o ficheiro trocava também a soma.
- **Nunca para uma versão mais antiga** do que a instalada.
- A passagem automática (`--scheduled`) não procura versões nem atualiza: só a janela o faz.

## 3. XkyHold web

A web não precisa de instalar nada: "atualizar" é recarregar. O problema é só saber que há versão nova.

- O build escreve `version.json` (`{ "version": "1.2.0", "build": "<hash do commit>" }`), servido com `Cache-Control: no-store` (`public/_headers`).
- A app pede-o à própria origem ao abrir, quando o separador volta a ficar visível e a cada 6 horas. Se o `build` for diferente do que está compilado na página, mostra uma faixa (o mesmo componente dos avisos de cópia em atraso): "Há uma versão nova do XkyHold." com **Atualizar** e **Agora não**.
- **Atualizar** faz `location.reload()`. Durante uma cópia ou restauro o botão fica desativado.
- "Agora não" esconde a faixa até ao próximo `build` diferente.
- Falha de rede ou ficheiro em falta: silêncio, tenta de novo mais tarde.
- Se um ficheiro da versão antiga já não existir no servidor (evento `vite:preloadError`), mostrar a mesma faixa em vez de um erro.
- Sem alterações à CSP nem pedidos a terceiros. Se um dia houver service worker, o botão passa a ativar o worker novo antes de recarregar.

Testes: função pura "há versão nova?" com Vitest; visto no browser com dois builds servidos um a seguir ao outro. O deploy é do proprietário.

## 4. XkyHold-lx

### 4.1 De onde vem a informação

Um ficheiro `latest.json`, publicado como ficheiro de cada release e lido em
`https://github.com/xkyriver/xkyhold-lx/releases/latest/download/latest.json`.

```json
{
  "version": "1.1.0",
  "date": "2026-11-20T10:00:00Z",
  "notes": { "en": "…", "pt-PT": "…" },
  "page": "https://github.com/xkyriver/xkyhold-lx/releases/tag/v1.1.0",
  "files": {
    "appimage-x86_64": { "url": "…", "size": 109193720, "signature": "…" },
    "deb-amd64":       { "url": "…", "size": 5560000,   "signature": "…" },
    "rpm-x86_64":      { "url": "…", "size": 5560000,   "signature": "…" }
  }
}
```

Porquê este caminho e não a API do GitHub: é um ficheiro estático (sem limite de pedidos por IP), sai no mesmo ato da release (não depende de um deploy do site) e o próprio `latest.json` também é assinado (`latest.json.sig`), para ninguém conseguir anunciar uma versão falsa.

### 4.2 Quando procura

- Ao abrir a janela, no máximo uma vez por dia (data da última procura no estado da app), e em **Definições → Acerca → Procurar agora**.
- Definição "Avisar quando houver uma versão nova", ligada por omissão (decisão U1).
- O pedido é feito em Rust, com tempo limite curto; uma falha não mostra nada.

### 4.3 O que o utilizador vê

- **Início:** faixa "Está disponível o XkyHold-lx 1.1.0." com **Atualizar**, **O que mudou** (as notas, na língua da app) e **Agora não** (esconde até à versão seguinte).
- **Definições → Acerca:** versão instalada, última procura, "Procurar agora", o interruptor, e a mesma ação de atualizar.
- Durante a atualização: progresso da transferência, depois "A verificar…", depois **Reiniciar agora**.

### 4.4 O que o botão faz, por forma de instalação

A app descobre como foi instalada no arranque (variável `APPIMAGE`; ficheiros do AppMan ao lado do executável; `dpkg -S` / `rpm -qf` sobre o próprio executável).

| Instalação | O botão "Atualizar" |
|---|---|
| **AppImage** (descarregado à mão) | Descarrega o AppImage novo para a mesma pasta, verifica a assinatura, torna-o executável e troca-o pelo antigo com um `rename` (atómico; o antigo fica como `.old` até a nova versão arrancar bem). O caminho não muda, por isso o temporizador systemd continua válido. Depois, "Reiniciar agora" |
| **AppImage gerido pelo AppMan** | O mesmo, e no fim atualiza o ficheiro `version` do AppMan com o URL novo, para o `appman -u` seguinte não descarregar outra vez. Ganha a verificação de assinatura que o `AM-updater` não faz (decisão U3) |
| **`.deb` / `.rpm`** | Descarrega o pacote para a cache da app, verifica a assinatura e instala-o com a ferramenta do sistema através do `pkexec` (`apt-get install ./ficheiro.deb` ou `dnf install ./ficheiro.rpm`): o sistema pede a palavra-passe de administrador numa janela própria. A app em si continua a nunca correr como root (decisão U2) |
| Pasta sem permissão de escrita, sem `pkexec`, ou forma desconhecida | O botão passa a **Abrir a página da versão** (no browser), com uma frase a dizer porquê |

Se a app estiver aberta noutra janela ou a correr uma passagem automática (lock local), a atualização espera.

### 4.5 Assinatura

- Par de chaves minisign (Ed25519) gerado uma vez pelo proprietário. A **chave pública** fica no código; a **chave privada**, com palavra-passe, fica só com o proprietário, fora do git e de qualquer CI (decisão U4). Perder a chave privada significa que as versões instaladas deixam de aceitar atualizações: guardar uma cópia segura.
- Cada release assina os três pacotes e o `latest.json`. A verificação é feita em Rust (`minisign-verify`, uma dependência pequena) antes de tocar em qualquer ficheiro instalado.
- Implementação própria em `crates/app/src/update.rs`, com o `ureq` que já existe, em vez do `tauri-plugin-updater`. Razões: o plugin documenta só o AppImage no Linux, traz mais dependências, e assim a lógica fica testável fora do Tauri. O formato de assinatura é o mesmo do `tauri signer`, por isso a troca continua possível. **A confirmar na Fase U0:** se o plugin já instala `.deb`/`.rpm` de forma aceitável, reavaliar.

### 4.6 Processo de release

Um script `scripts/release` que, a partir dos pacotes compilados: calcula o `SHA256SUMS`, assina cada pacote e o `latest.json` (a palavra-passe da chave é pedida ao proprietário no momento; um passo dele, com aviso antes), e deixa tudo em `target/release-<versão>/` pronto para publicar. Publicar continua a precisar do "sim" do proprietário.

## 5. Decisões do proprietário

**Decididas a 15/11/2026:** o proprietário aceitou as cinco recomendações ("Sim, envia", pela sessão do plano base). A coluna "Recomendação" é a decisão.

| # | Pergunta | Decisão |
|---|---|---|
| U1 | A procura de versões na lx vem ligada por omissão? | **Sim**, com interruptor nas Definições e uma linha na página de privacidade. É um pedido por dia a um ficheiro público no GitHub, sem identificadores |
| U2 | `.deb`/`.rpm`: o botão instala com `pkexec` (pede a palavra-passe de administrador), ou só abre a página da versão? | **Instala com `pkexec`.** Sem isso, o botão não atualiza de facto para quem instalou por pacote |
| U3 | No AppMan: a app atualiza-se a si própria (com assinatura) e acerta o ficheiro `version`, ou mostra só o comando `appman -u xkyhold-lx`? | **Atualiza-se a si própria.** É o único caminho com assinatura verificada |
| U4 | Onde fica a chave privada de assinatura? | **Só contigo**, num ficheiro com palavra-passe fora do repositório, com cópia de segurança |
| U5 | A web avisa também quem está na página inicial e nas páginas `/linux/`, ou só dentro da app (`/app/`)? | **Só dentro da app.** As outras páginas recarregam-se a cada visita |

## 6. Fases

| Fase | Conteúdo | Feito quando |
|---|---|---|
| **U0. Confirmar** (feito a 15/11/2026, secção 7) | Ler a documentação atual do `tauri-plugin-updater` para Linux e do `pkexec` com `apt-get`/`dnf`; confirmar que o `rename` sobre um AppImage em execução funciona e que o reinício arranca a versão nova | Respostas escritas neste ficheiro; 4.5 confirmada ou revista |
| **U1. Web** (feita a 15/11/2026, secção 9) | `version.json`, verificação, faixa, textos EN e pt-PT, testes (repositório `xkyhold`) | Visto no browser com dois builds seguidos; **deploy pelo proprietário** |
| **U2. lx: avisar** (feita a 15/11/2026, secção 9) | `latest.json`, procura diária, faixa no Início, Definições → Acerca, textos; botão a abrir a página da versão em todos os casos | Uma lx de teste com versão baixa mostra a faixa a partir de um `latest.json` de teste servido em `localhost` (endereço alternativo só em builds de desenvolvimento) |
| **U3. lx: assinar** (feita a 15/11/2026, menos a chave real; secção 9) | Chave (passo do proprietário), `scripts/release`, verificação de assinatura com testes: assinatura errada, ficheiro truncado, versão mais antiga, `latest.json` alterado | Tudo recusado nos testes; um pacote bem assinado aceite |
| **U4. lx: AppImage** (feita a 15/11/2026, secção 9) | Atualização no lugar, AppMan incluído, reinício, temporizador | Visto no computador do proprietário: de uma versão N de teste para N+1, com a cópia automática a correr bem depois |
| **U5. lx: `.deb`/`.rpm`** | Instalação com `pkexec`; alternativa "Abrir a página" | Visto numa máquina ou contentor Debian/Ubuntu e Fedora com sessão gráfica; se não houver onde testar, fica dito como **não testado** e o botão abre a página nesses sistemas até haver prova |
| **U6. Fecho** | Página de privacidade (EN e PT), páginas `/linux/`, `CHANGELOG.md`, `PLAN.md` (4.9 e registo), checkpoint; release que introduz o verificador, com a nota "esta atualização ainda é manual" | Release publicada com o "sim" do proprietário |

A fase U1 (web) não depende das outras e pode ir primeiro. U2 já dá valor sozinha: avisa em todos os formatos, mesmo antes de o botão instalar.

## 7. Fase U0: respostas (15/11/2026)

- **`tauri-plugin-updater` no Linux.** A página de documentação (v2.tauri.app/plugin/updater) só descreve o AppImage, mas o código da versão atual (2.14.1, lido no crate) já instala `.deb` e `.rpm`: escreve o pacote num diretório temporário e corre `pkexec dpkg -i` / `pkexec rpm -U`; se o `pkexec` falhar, **pede a palavra-passe de administrador numa janela `zenity`/`kdialog` e passa-a ao `sudo`**, e por fim tenta `sudo` no terminal. **Decisão mantida: implementação própria** (4.5), porque (1) a app nunca deve ver a palavra-passe de administrador; (2) `dpkg -i`/`rpm -U` não resolvem dependências, ao contrário de `apt-get install ./…` e `dnf install ./…`; (3) o plugin não conhece o AppMan (ficheiro `version`) nem guarda o AppImage antigo; (4) traz mais dependências. O formato de assinatura continua compatível (minisign), por isso a troca segue possível.
- **`rename` sobre um AppImage em execução: confirmado** nesta máquina. Com o AppImage 1.0.0 a correr (`--appimage-mount`), um `mv` do 1.0.1 por cima foi aceite; o processo antigo continuou a funcionar sobre a imagem montada (respondeu `1.0.0`), e o mesmo caminho, lançado de novo, arrancou o 1.0.1. O reinício faz-se lançando o mesmo caminho.
- **`pkexec` com a ferramenta do sistema.** Debian/Ubuntu: `pkexec apt-get install -y ./ficheiro.deb` (o caminho tem de conter `/`; o `apt` pode avisar que leu o ficheiro "unsandboxed as root" quando a pasta não é legível pelo utilizador `_apt` — usar uma pasta legível ou ignorar o aviso). Fedora: `pkexec dnf install -y ./ficheiro.rpm`; o `localpkg_gpgcheck` vem desligado por omissão (o nosso `.rpm` não tem assinatura GPG; a assinatura minisign é verificada pela app antes), mas sistemas endurecidos podem ligá-lo: nesse caso a instalação falha e o botão passa a "Abrir a página da versão". O `pkexec` precisa de um agente polkit na sessão gráfica; sem ele, a mesma alternativa. **A provar na Fase U5** numa máquina ou contentor real.

## 8. Riscos

- **Uma atualização estragada numa app de cópias de segurança é grave.** Por isso: assinatura obrigatória, troca atómica, o ficheiro antigo guardado até a versão nova arrancar, e nunca durante uma operação.
- **Formato do repositório:** uma versão nova nunca pode deixar de ler os repositórios das anteriores (já é regra do `FORMAT.md`); o atualizador não muda isso, mas torna as atualizações mais frequentes.
- **`.deb`/`.rpm` sem máquina de teste:** hoje os pacotes nunca foram instalados num Debian ou Fedora reais. A fase U5 precisa dessa prova antes de o botão instalar.
- **Chave privada:** se for comprometida, quem a tiver consegue distribuir uma versão falsa a quem carregar em "Atualizar". Fica só com o proprietário e com palavra-passe.

## 9. Registo

- **U1 (web) feita a 15/11/2026**, commit `951c53d` no `xkyhold` (só local; o push e o deploy são do proprietário). O build escreve `version.json` com a versão e o commit (`git rev-parse --short=12 HEAD`), servido com `no-store`; a faixa fica no topo da app (também no assistente), com **Atualizar** desativado durante uma cópia, restauro ou verificação. Testes: `src/lib/update.test.ts` (4) e o dicionário pt-PT; 173 testes a passar. **Visto** no `vite preview`: separador aberto no build `498a6f18cdc0` sem faixa; depois do build `951c53db5f95` servido no mesmo sítio, a faixa apareceu; "Atualizar" recarregou para o código novo e a faixa desapareceu. Não visto: o botão desativado durante uma cópia, e o caso `vite:preloadError`. Nota: dois builds do mesmo commit têm o mesmo identificador, por isso um deploy de alterações por fazer commit não é anunciado; os deploys saem de commits.
- **U2 (lx: avisar) feita a 15/11/2026.** `crates/app/src/update.rs`: lê `latest.json` da release mais recente (com redirecionamentos, 20 s, até 256 KB; em builds de desenvolvimento, `XKYHOLD_LX_UPDATE_URL`), no máximo uma vez por dia ao abrir a janela; compara versões `x.y.z` (nunca para trás, sem pré-versões); só abre páginas debaixo de `github.com/xkyriver/xkyhold-lx/releases/`. Definições novas: `check_updates` (ligado), `update_checked`, `update_latest`, `update_skipped`. Janela: faixa no Início (**Atualizar**, **O que mudou**, **Agora não**) e cartão **Acerca** nas Definições (versão, interruptor, **Procurar agora**, última procura). Nesta fase, **Atualizar** abre a página da versão. A passagem automática não procura. Para testar janelas de desenvolvimento numa casa temporária sem tocar no temporizador real: `XKYHOLD_LX_NO_TIMER` (só em builds de desenvolvimento). **Visto** numa janela de desenvolvimento (versão 1.0.1) com casa temporária e um `latest.json` 9.9.9 servido em `127.0.0.1`: faixa em pt-PT; notas; "Agora não" esconde e não volta ao reabrir (sem novo pedido); **Procurar agora** mostra-a outra vez; sem rede, aparece "Não foi possível procurar…" e a faixa anterior fica. O temporizador real ficou igual (mesmos ficheiros, ativo). Encontrado e corrigido no teste: "Procurar agora" dizia "Tens a versão mais recente" depois de "Agora não"; o erro sem rede não aparecia ao lado da faixa. **Não visto:** o clique em **Atualizar** a abrir o navegador; o botão desativado durante uma cópia. Testes automáticos: 3 novos (versões, `latest.json`, uma vez por dia / posta de lado).
- **U3 (lx: assinar) feita a 15/11/2026, menos a chave real.** Verificação em `crates/app/src/update.rs` com `minisign-verify` 0.3: assinatura no formato do `tauri signer` (minisign em base64), só a forma com pré-hash, e o comentário de confiança tem de dizer `file:<nome>` (um ficheiro assinado não serve por outro). O `latest.json` só é lido depois de verificado com `latest.json.sig`; cada pacote tem de ter o tamanho, a assinatura e um nome com a versão de `latest.json`. Chave pública em `PUBLIC_KEY` (vazia até o proprietário a criar: sem ela não se anuncia nem instala nada; em builds de desenvolvimento, `XKYHOLD_LX_UPDATE_KEY`). Testes com uma chave descartável (`crates/app/tests/fixtures/update/`, nunca uma chave de release): `latest.json` alterado, assinatura de outro ficheiro, outra chave, pacote alterado, truncado, um pacote antigo bem assinado e o pacote de outro formato — tudo recusado; o pacote certo aceite. Versões mais antigas recusadas pelo teste das versões. `scripts/release`: copia os pacotes, `SHA256SUMS`, assina-os, escreve e assina o `latest.json` (notas EN do `CHANGELOG.md`, pt-PT de um ficheiro opcional) e verifica tudo com `xkyhold-lx-cli check-release` (comando escondido). **Visto** com a chave descartável sobre os pacotes 1.0.1, numa pasta temporária. **Falta (passo do proprietário):** gerar a chave real e pôr a pública em `PUBLIC_KEY`.
- **U4 (lx: AppImage) feita a 15/11/2026.** A app descobre como foi instalada (`APPIMAGE`; `AM-updater` e `version` ao lado = AppMan; `dpkg -S` / `rpm -qf` sobre o executável). Num AppImage com pasta gravável, **Atualizar** volta a ler e verificar o `latest.json`, recusa versões que não sejam mais novas, descarrega o AppImage com progresso, verifica tamanho, assinatura e nome, escreve-o ao lado, guarda o antigo como `<nome>.old` (ligação física, ou cópia), troca-o com um `rename` e, no AppMan, escreve o URL novo no ficheiro `version`. Nunca durante um trabalho da janela, e só com o lock local (uma cópia do temporizador, do CLI ou outra janela fazem esperar). **Reiniciar agora** lança o mesmo caminho com o ambiente limpo do que aponta para a montagem do AppImage antigo e fecha a janela; a versão nova apaga o `.old` ao arrancar. Noutros casos o botão diz "Abrir a página da versão". `--version` de um build de teste diz "(test build: not for release)" e o `scripts/release` recusa-o; um build de teste nunca toca no temporizador. **Visto** nesta máquina com dois AppImages de teste (1.0.90 → 1.0.91, compilados com endereço e chave de teste) numa pasta de AppMan falsa dentro de uma casa temporária: faixa, "Atualizar", "A verificar a assinatura…", "O XkyHold-lx 1.0.91 está instalado", `version` do AppMan atualizado, "Reiniciar agora" abriu a 1.0.91 (rodapé) e o `.old` desapareceu; depois, `xkyhold-lx --scheduled` com o programa atualizado fez a cópia automática (5 ficheiros, código 0). O temporizador real do proprietário não foi tocado (mesmos ficheiros, ativo). **Não visto:** o temporizador systemd real a correr a versão atualizada (os builds de teste não o tocam; o caminho é o mesmo, por isso continua válido), a falta de permissão de escrita, e a espera pelo lock. Nota: num build de desenvolvimento a verificação de 170 MB demora uns segundos.
