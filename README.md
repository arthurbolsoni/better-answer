# better-answer

Atalho global no Windows que pega o texto selecionado em **qualquer** app, reescreve com uma LLM
(via OpenRouter) e devolve pronto — no tom de um líder que a equipe respeita: claro, cordial e firme,
sem parecer bruto nem bajulador.

Três atalhos:

| Atalho | O que faz |
|---|---|
| `Ctrl+B` | Abre o popup ao lado do cursor com a versão melhorada. `Ctrl+Enter` cola por cima do original. |
| `Ctrl+S` | Melhora com o tom padrão e substitui o texto selecionado. Só aparece uma caixinha ao lado do cursor com o progresso, do tamanho de um menu de contexto. |
| `Ctrl+D` | Abre o popup já no tom **Chamado**: transforma a anotação solta num registro de ocorrência (resumo, quando, onde, o que aconteceu, mensagem de erro, impacto). |

## Como funciona

1. O app fica na bandeja do sistema, sem janela.
2. Os atalhos vivem num hook `WH_KEYBOARD_LL`, não no `RegisterHotKey`: combos que o shell usa
   para si (`Win+B` foca a área de notificação) são tratados por ele antes de chegarem ao app, e
   o registro simplesmente falha. O hook vê a tecla antes de todo mundo e a engole.
3. O atalho dispara a captura: solta os modificadores ainda pressionados, envia `Ctrl+C` sintético
   via `SendInput` e observa o `GetClipboardSequenceNumber` até o app de origem responder. Esse é
   o único caminho que funciona igual em Chrome, Outlook, Teams, Electron e terminal.
4. O texto vai para a OpenRouter com streaming SSE; o resultado aparece token a token.
5. `Substituir` devolve o foco para a janela de origem e envia `Ctrl+V`.

Se você cancelar (`Esc`), o clipboard anterior é restaurado.

### O atalho rápido nunca rouba o foco

`Ctrl+S` mostra só a caixinha de progresso, e ela não recebe foco: a janela de origem continua sendo a
ativa, que é o que faz o `Ctrl+V` do final cair no lugar certo. Deu erro, a caixinha mostra o motivo,
some sozinha depois de 5s e o clipboard anterior é restaurado. O erro também vai para o tooltip da
bandeja e vira um aviso na próxima vez que o popup abrir.

### O atalho de chamado

`Ctrl+D` é o mesmo popup do `Ctrl+B`, com o tom **Chamado** já selecionado — serve para jogar a
anotação crua ("sistema travou na tela de faturamento agora, erro X, cliente Y parado") e receber um
registro de ocorrência limpo: resumo na primeira linha e depois só os rótulos que o texto original
realmente informa (`Quando`, `Onde`, `O que aconteceu`, `Mensagem de erro`, `Como reproduzir`,
`Impacto`, `Já verificado`). Rótulo sem informação é omitido em vez de preenchido com suposição, e
código, ID e mensagem de erro vão transcritos igual.

Ele não substitui o texto sozinho de propósito: chamado se lê antes de mandar. Revise no popup e
mande com `Ctrl+Enter` (cola no app de origem) ou `Ctrl+Shift+C` (só copia).

O tom é encontrado **pelo nome**, porque a lista de tons é sua para editar: um config antigo, sem o
tom `Chamado`, ganha ele na próxima abertura do app. Se você renomear ou apagar esse tom, o `Ctrl+D`
passa a abrir no primeiro tom da lista.

Alt e Win têm ação própria quando são soltos sem nenhuma tecla no meio — barra de menu e Menu
Iniciar. Como o hook engole a tecla principal, o modificador vira um toque isolado; por isso a
captura injeta antes uma tecla sem função (VK `0xE8`), o mesmo recurso que o AutoHotkey usa.

## Instalação

```powershell
cargo build --release
# binário em target\release\better-answer.exe
```

Para instalar de verdade, copie o `.exe` para fora da pasta de build (senão um `cargo clean` leva o
app junto) e crie os atalhos:

```powershell
$dest = "$env:LOCALAPPDATA\Programs\better-answer"
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item .\target\release\better-answer.exe "$dest\better-answer.exe" -Force

$shell = New-Object -ComObject WScript.Shell
foreach ($dir in @([Environment]::GetFolderPath('Startup'), [Environment]::GetFolderPath('Programs'))) {
  $s = $shell.CreateShortcut((Join-Path $dir 'better-answer.lnk'))
  $s.TargetPath = "$dest\better-answer.exe"
  $s.WorkingDirectory = $dest
  $s.Save()
}
```

O atalho em `Startup` sobe o app junto com o Windows. O atalho em `Programs` coloca ele no Menu
Iniciar, em "Todos os aplicativos".

**Fixar no Menu Iniciar precisa ser manual.** Desde o Windows 10 1903 a Microsoft bloqueia o verbo
"Fixar em Iniciar" por script — invocá-lo devolve `E_ACCESSDENIED`. Abra o Menu Iniciar, ache
`better-answer` em "Todos os aplicativos", clique com o botão direito e escolha **Fixar em Iniciar**.

## Configuração

Primeira execução cria `%APPDATA%\better-answer\config.toml`:

| Campo | O que é |
|---|---|
| `api_key` | Chave da OpenRouter. **Prefira deixar vazio** e usar a variável de ambiente `OPENROUTER_API_KEY`. |
| `model` | Ex.: `anthropic/claude-sonnet-5`, `google/gemini-3.5-flash`, `openai/gpt-5.6-sol`. |
| `hotkey` | Atalho do popup. Ex.: `ctrl+b`, `ctrl+alt+e`, `alt+f2`. Exige ao menos um modificador. |
| `quick_hotkey` | Atalho rápido. Ex.: `ctrl+s`, `win+b`. Vazio desliga. |
| `ticket_hotkey` | Atalho do chamado. Ex.: `ctrl+d`. Vazio desliga. |
| `temperature` | Padrão `0.4`. |
| `max_tokens` | Padrão `2000`. |
| `signature` | Seu nome/cargo, usado quando o formato pede assinatura. |
| `extra_context` | Instrução fixa somada a todos os tons (contexto da empresa, jargão a evitar...). |
| `[[tones]]` | Lista de tons. O primeiro é o padrão. Edite à vontade. |

Mudança de atalho só vale depois de reiniciar o app. O resto vale na hora.

O atalho aceita modificador + **uma** tecla. Acorde de duas teclas (`ctrl+x+1`) é recusado no parse.
Teclas de pontuação usam códigos OEM, que seguem o layout US — num ABNT2 a tecla física pode ser
outra. Letras, dígitos e teclas nomeadas não têm esse problema.

`BETTER_ANSWER_CONFIG` aponta o app para outro arquivo de config — útil para instalação portátil e
usado pelos testes e2e.

### Chave da API

```powershell
# sessão atual
$env:OPENROUTER_API_KEY = "sk-or-v1-..."
# permanente para o usuário
setx OPENROUTER_API_KEY "sk-or-v1-..."
```

A chave **não** é versionada: o `config.toml` mora no `%APPDATA%`, fora do repositório.

## Tons que já vêm prontos

| # | Tom | Para quê |
|---|---|---|
| 1 | Líder | Padrão. Contexto, pedido claro, responsabilidade sobre o problema, abertura para dúvida. |
| 2 | E-mail formal | Sugere assunto, saudação, corpo e encerramento cortês. |
| 3 | Direto | Curto e objetivo, sem ficar seco. |
| 4 | Chat / Teams | Mensagem curta de chat de trabalho. |
| 5 | Chamado | O tom do `Ctrl+D`. Registro informativo de ocorrência, em linhas rotuladas. |
| 6 | Feedback difícil | Fato → impacto → expectativa, sem ataque pessoal. |

## Teclas no popup

| Tecla | Ação |
|---|---|
| `Alt+1..9` | Troca o tom e regera |
| `Ctrl+R` | Refaz |
| `Ctrl+Enter` | Cola no app de origem |
| `Ctrl+Shift+C` | Copia e fecha |
| `Esc` | Fecha e restaura o clipboard |
| `Ctrl+B` / `Ctrl+D` | Com o popup aberto, fecham |

O texto de saída é editável antes de colar. O campo de instrução aceita ajustes pontuais
("mais curto", "para o cliente", "sem prazo") e regera no `Enter`.

## Testes

```powershell
cargo test                      # unitários + o e2e que não mexe no teclado
cargo test -- --ignored         # e2e que dispara atalho global (sequestra foco e teclado)
```

Os e2e sobem o binário de verdade e inspecionam as janelas pelo Win32. Cada um usa seu próprio
`config.toml` com atalhos improváveis (`ctrl+alt+shift+f9`) e chave vazia, então não brigam com a
instância real nem fazem chamada de rede.

Os testes marcados com `#[ignore]` disparam atalhos globais: eles põem em foco uma janela inerte
própria antes, para o `Ctrl+C` sintético não cair no console que roda a suíte nem na janela em que
você estava trabalhando. Rode sozinho, sem digitar durante.

## Créditos

Os ícones são do [Phosphor Icons](https://phosphoricons.com) (MIT). A fonte vive em
`assets/Phosphor.ttf`, com a licença em `assets/PHOSPHOR-LICENSE-MIT`. O crate `egui-phosphor`
resolveria isso sozinho, mas a versão publicada depende do egui 0.35 e este app roda no 0.36 — duas
versões do egui no mesmo binário não compilam, então só a fonte foi vendorizada.

## Limitações conhecidas

- Windows apenas (`SendInput`, `WH_KEYBOARD_LL`, clipboard sequence number).
- Os atalhos usam um hook global de teclado: toda tecla do sistema passa pelo callback do app. Ele
  lê apenas o virtual-key e o estado dos modificadores, encaminha tudo adiante e não guarda nem
  transmite nada — mas é bom saber que a superfície existe.
- O atalho escolhido é **engolido em todo o sistema** enquanto o app roda. O padrão `ctrl+s` tira o
  "salvar" de todos os apps e o `ctrl+d` tira o EOF do terminal e o "duplicar linha / próximo
  match" dos editores; se isso atrapalhar, troque `quick_hotkey`/`ticket_hotkey` no `config.toml`
  e reinicie.
- Dois atalhos com a mesma combinação: o segundo fica mudo. O app avisa em vez de deixar quieto.
- Apps que não respondem a `Ctrl+C` não entregam seleção — nesses casos o popup abre com o campo
  "original" vazio para você colar/digitar.
- Colar depende de `Ctrl+V` funcionar na janela de origem.
- Atalho de um processo comum não dispara enquanto uma janela **elevada** (rodando como
  administrador) estiver em foco. É bloqueio de UIPI do Windows, não do app.
- Fixar no Menu Iniciar é manual (bloqueio da Microsoft, veja *Instalação*).
- A janela precisa desfazer o `set_visible(true)` que o eframe força depois do primeiro frame; por
  isso ela nasce estacionada fora do monitor. Sem isso sobra um retângulo preto na tela.
