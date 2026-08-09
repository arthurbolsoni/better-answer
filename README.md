# better-answer

Atalho global no Windows que pega o texto selecionado em **qualquer** app, reescreve com uma LLM
(via OpenRouter) e devolve pronto — no tom de um líder que a equipe respeita: claro, cordial e firme,
sem parecer bruto nem bajulador.

Seleciona o texto → `Ctrl+Alt+E` → um popup abre do lado do cursor com a versão melhorada →
`Ctrl+Enter` cola por cima do original.

## Como funciona

1. O app fica na bandeja do sistema, sem janela.
2. O atalho global (`RegisterHotKey`) dispara a captura: ele solta os modificadores ainda
   pressionados, envia `Ctrl+C` sintético via `SendInput` e observa o
   `GetClipboardSequenceNumber` até o app de origem responder. Esse é o único caminho que funciona
   igual em Chrome, Outlook, Teams, Electron e terminal.
3. O texto vai para a OpenRouter com streaming SSE; o resultado aparece token a token.
4. `Substituir` devolve o foco para a janela de origem e envia `Ctrl+V`.

Se você cancelar (`Esc`), o clipboard anterior é restaurado.

## Instalação

```powershell
cargo build --release
# binário em target\release\better-answer.exe
```

Coloque um atalho do `.exe` em `shell:startup` para subir junto com o Windows.

## Configuração

Primeira execução cria `%APPDATA%\better-answer\config.toml`:

| Campo | O que é |
|---|---|
| `api_key` | Chave da OpenRouter. **Prefira deixar vazio** e usar a variável de ambiente `OPENROUTER_API_KEY`. |
| `model` | Ex.: `anthropic/claude-sonnet-5`, `google/gemini-3.5-flash`, `openai/gpt-5.6-sol`. |
| `hotkey` | Ex.: `ctrl+alt+e`, `ctrl+shift+space`, `alt+f2`. Exige ao menos um modificador. |
| `temperature` | Padrão `0.4`. |
| `max_tokens` | Padrão `2000`. |
| `signature` | Seu nome/cargo, usado quando o formato pede assinatura. |
| `extra_context` | Instrução fixa somada a todos os tons (contexto da empresa, jargão a evitar...). |
| `[[tones]]` | Lista de tons. O primeiro é o padrão. Edite à vontade. |

Mudança de `hotkey` só vale depois de reiniciar o app. O resto vale na hora.

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
| 5 | Feedback difícil | Fato → impacto → expectativa, sem ataque pessoal. |

## Teclas no popup

| Tecla | Ação |
|---|---|
| `Alt+1..9` | Troca o tom e regera |
| `Ctrl+R` | Refaz |
| `Ctrl+Enter` | Cola no app de origem |
| `Ctrl+Shift+C` | Copia e fecha |
| `Esc` | Fecha e restaura o clipboard |
| `Ctrl+Alt+E` | Com o popup aberto, fecha |

O texto de saída é editável antes de colar. O campo "Instrução extra" aceita ajustes pontuais
("mais curto", "para o cliente", "sem prazo") e regera no `Enter`.

## Modo linha de comando

Útil para validar chave e modelo sem depender do atalho:

```powershell
better-answer.exe --improve "texto bruto aqui"
better-answer.exe --config-path
```

## Limitações conhecidas

- Windows apenas (`SendInput`, `RegisterHotKey`, clipboard sequence number).
- Apps que não respondem a `Ctrl+C` não entregam seleção — nesses casos o popup abre com o campo
  "Texto original" vazio para você colar/digitar.
- Colar depende de `Ctrl+V` funcionar na janela de origem.
