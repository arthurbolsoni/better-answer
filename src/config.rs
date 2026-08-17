use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub const API_KEY_ENV: &str = "OPENROUTER_API_KEY";
/// Aponta o config para outro arquivo. Usado pelos testes e2e e por instalacao portatil.
pub const CONFIG_PATH_ENV: &str = "BETTER_ANSWER_CONFIG";
/// Nome do tom que o atalho de chamado usa. E por nome, nao por indice, porque a lista de tons e
/// editavel: o indice do usuario muda, o nome sobrevive.
pub const TICKET_TONE: &str = "Chamado";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tone {
    pub name: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Chave da OpenRouter. Deixe vazio para usar a variavel de ambiente OPENROUTER_API_KEY.
    pub api_key: String,
    pub model: String,
    /// Abre o popup. Ex.: "ctrl+b", "ctrl+alt+e", "ctrl+shift+space".
    pub hotkey: String,
    /// Melhora com o tom padrao e cola direto, so com a caixinha de progresso. Vazio desliga.
    pub quick_hotkey: String,
    /// Abre o popup com o tom "Chamado", para registrar uma ocorrencia. Vazio desliga.
    pub ticket_hotkey: String,
    pub temperature: f32,
    pub max_tokens: u32,
    /// Assinatura/nome usado quando o texto virar e-mail.
    pub signature: String,
    /// Instrucao fixa somada a todos os tons (contexto da empresa, jargao, etc).
    pub extra_context: String,
    /// Tons disponiveis. O primeiro e o padrao.
    pub tones: Vec<Tone>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "anthropic/claude-sonnet-5".to_string(),
            hotkey: "ctrl+b".to_string(),
            quick_hotkey: "ctrl+s".to_string(),
            ticket_hotkey: "ctrl+d".to_string(),
            temperature: 0.4,
            max_tokens: 2000,
            signature: String::new(),
            extra_context: String::new(),
            tones: default_tones(),
        }
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        if let Ok(custom) = std::env::var(CONFIG_PATH_ENV) {
            let custom = custom.trim();
            if !custom.is_empty() {
                return Ok(PathBuf::from(custom));
            }
        }
        let dir = dirs::config_dir().context("nao consegui achar o diretorio de config do usuario")?;
        Ok(dir.join("better-answer").join("config.toml"))
    }

    pub fn load_or_create() -> Result<Self> {
        let path = Self::path()?;
        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("lendo {}", path.display()))?;
            let mut cfg: Config = toml::from_str(&raw)
                .with_context(|| format!("parseando {}", path.display()))?;
            cfg.ensure_ticket_tone();
            return Ok(cfg);
        }
        let cfg = Config::default();
        cfg.save()?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("criando {}", parent.display()))?;
        }
        let raw = toml::to_string_pretty(self).context("serializando config")?;
        std::fs::write(&path, raw).with_context(|| format!("escrevendo {}", path.display()))?;
        Ok(())
    }

    /// A env var ganha da chave do arquivo, para nao obrigar a guardar segredo em disco.
    pub fn resolved_api_key(&self) -> Option<String> {
        if let Ok(key) = std::env::var(API_KEY_ENV) {
            let key = key.trim().to_string();
            if !key.is_empty() {
                return Some(key);
            }
        }
        let key = self.api_key.trim();
        if key.is_empty() {
            None
        } else {
            Some(key.to_string())
        }
    }

    /// Indice do tom de chamado. Cai no primeiro tom se alguem tiver apagado o "Chamado" da lista
    /// depois do `ensure_ticket_tone` — nesse caso o atalho ainda funciona, so com o tom padrao.
    pub fn ticket_tone_index(&self) -> usize {
        self.tones
            .iter()
            .position(|tone| tone.name.trim().eq_ignore_ascii_case(TICKET_TONE))
            .unwrap_or(0)
    }

    /// Config escrito antes do atalho de chamado existir nao tem o tom dele. Sem isso o Ctrl+D
    /// abriria com o tom padrao e a pill "Chamado" nunca apareceria para quem ja usa o app.
    pub fn ensure_ticket_tone(&mut self) {
        if self
            .tones
            .iter()
            .any(|tone| tone.name.trim().eq_ignore_ascii_case(TICKET_TONE))
        {
            return;
        }
        if let Some(ticket) = default_tones()
            .into_iter()
            .find(|tone| tone.name == TICKET_TONE)
        {
            self.tones.push(ticket);
        }
    }

    /// Nunca entra em panico: um config com `tones = []` cai no tom embutido.
    pub fn tone(&self, index: usize) -> &Tone {
        static FALLBACK: std::sync::OnceLock<Tone> = std::sync::OnceLock::new();
        self.tones
            .get(index)
            .or_else(|| self.tones.first())
            .unwrap_or_else(|| FALLBACK.get_or_init(|| default_tones().swap_remove(0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_ship_the_three_hotkeys() {
        let cfg = Config::default();
        assert_eq!(cfg.hotkey, "ctrl+b");
        assert_eq!(cfg.quick_hotkey, "ctrl+s");
        assert_eq!(cfg.ticket_hotkey, "ctrl+d");
        assert!(crate::hotkey::parse(&cfg.hotkey).is_ok());
        assert!(crate::hotkey::parse(&cfg.quick_hotkey).is_ok());
        assert!(crate::hotkey::parse(&cfg.ticket_hotkey).is_ok());
    }

    /// Config antigo (sem `quick_hotkey`) precisa continuar carregando.
    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg: Config = toml::from_str(r#"model = "algum/modelo""#).unwrap();
        assert_eq!(cfg.model, "algum/modelo");
        assert_eq!(cfg.quick_hotkey, "ctrl+s");
        assert_eq!(cfg.ticket_hotkey, "ctrl+d");
        assert!(!cfg.tones.is_empty());
    }

    #[test]
    fn ticket_tone_ships_by_default() {
        let cfg = Config::default();
        let index = cfg.ticket_tone_index();
        assert_eq!(cfg.tone(index).name, TICKET_TONE);
        assert!(cfg.tone(index).prompt.contains("Ocorrência"));
    }

    /// Config antigo tem os tons dele, mas nao o de chamado: o Ctrl+D precisa ganhar o tom.
    #[test]
    fn ticket_tone_is_added_to_old_configs() {
        let mut cfg: Config = toml::from_str(
            r#"
[[tones]]
name = "Meu tom"
prompt = "reescreva"
"#,
        )
        .unwrap();
        assert_eq!(cfg.tones.len(), 1);
        cfg.ensure_ticket_tone();
        assert_eq!(cfg.tones.len(), 2);
        assert_eq!(cfg.tone(cfg.ticket_tone_index()).name, TICKET_TONE);

        // Idempotente: recarregar duas vezes nao empilha copias.
        cfg.ensure_ticket_tone();
        assert_eq!(cfg.tones.len(), 2);
    }

    /// Quem renomeou/apagou o tom de chamado nao pode ver o app entrar em panico nem apontar para
    /// um indice fora da lista.
    #[test]
    fn ticket_tone_index_falls_back_when_absent() {
        let cfg: Config = toml::from_str(
            r#"
[[tones]]
name = "Meu tom"
prompt = "reescreva"
"#,
        )
        .unwrap();
        assert_eq!(cfg.ticket_tone_index(), 0);
        assert_eq!(cfg.tone(cfg.ticket_tone_index()).name, "Meu tom");

        let empty: Config = toml::from_str("tones = []").unwrap();
        assert_eq!(empty.ticket_tone_index(), 0);
        assert!(!empty.tone(empty.ticket_tone_index()).prompt.is_empty());
    }

    #[test]
    fn round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.quick_hotkey = "ctrl+shift+j".to_string();
        cfg.ticket_hotkey = "ctrl+alt+d".to_string();
        let raw = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&raw).unwrap();
        assert_eq!(back.quick_hotkey, "ctrl+shift+j");
        assert_eq!(back.ticket_hotkey, "ctrl+alt+d");
        assert_eq!(back.tones.len(), cfg.tones.len());
    }

    /// Um config editado a mao com `tones = []` nao pode derrubar o app.
    #[test]
    fn tone_survives_empty_list() {
        let cfg: Config = toml::from_str("tones = []").unwrap();
        assert!(cfg.tones.is_empty());
        assert!(!cfg.tone(0).prompt.is_empty());
        assert!(!cfg.tone(7).prompt.is_empty());
    }
}

pub const BASE_RULES: &str = "\
Voce reescreve textos de trabalho. Regras invioláveis:
- Responda APENAS com o texto reescrito. Sem preâmbulo, sem explicação, sem aspas em volta, sem blocos de código, sem markdown de enfeite.
- Escreva no MESMO idioma do texto original.
- Preserve todos os fatos, números, nomes, datas, prazos e links. Nunca invente informação que não esteja no original.
- Se o original é uma pergunta, o resultado continua sendo uma pergunta.
- Mantenha o tamanho proporcional ao original: melhore a forma, não infle o conteúdo.
- Tire agressividade, ironia, passivo-agressivo e desabafo. Mantenha a firmeza e o pedido claro.
- Não use jargão corporativo vazio nem elogio bajulador.";

pub fn default_tones() -> Vec<Tone> {
    vec![
        Tone {
            name: "Líder".to_string(),
            prompt: "\
Reescreva como um líder que a equipe respeita e gosta de verdade: seguro, humano e direto ao ponto.
- Comece pelo contexto ou pelo reconhecimento honesto do trabalho de quem está do outro lado, em uma linha, sem bajulação.
- Deixe explícito o que precisa acontecer, quem faz e até quando (se o original disser).
- Assuma responsabilidade em vez de apontar culpado: fale do problema e da solução, não da pessoa.
- Trate discordância com respeito: valide o ponto do outro antes de apresentar o seu.
- Feche com abertura real para dúvida ou ajuda."
                .to_string(),
        },
        Tone {
            name: "E-mail formal".to_string(),
            prompt: "\
Reescreva como um e-mail corporativo formal e cordial em português do Brasil.
- Estrutura: saudação, um parágrafo de contexto, o pedido/informação principal, próximos passos, encerramento cortês.
- Sugira uma linha de assunto na primeira linha, no formato 'Assunto: ...', seguida de uma linha em branco.
- Trate por 'você' (ou o pronome já usado no original). Sem gírias, sem emoji, sem abreviação de chat.
- Parágrafos curtos. Use lista com hífen só se o original tiver vários itens."
                .to_string(),
        },
        Tone {
            name: "Direto".to_string(),
            prompt: "\
Reescreva curto, claro e educado, do jeito que um sênior ocupado escreveria.
- Vá ao ponto na primeira frase. Corte rodeio, desculpa desnecessária e preenchimento.
- No máximo um parágrafo curto, ou uma lista de bullets se houver mais de um item.
- Continue gentil: 'direto' não é seco nem ríspido."
                .to_string(),
        },
        Tone {
            name: "Chat / Teams".to_string(),
            prompt: "\
Reescreva como mensagem de chat de trabalho (Teams, Slack, WhatsApp corporativo).
- Tom leve, profissional e simpático. Frases curtas, sem parágrafo longo.
- Sem saudação formal de e-mail nem assinatura.
- No máximo um emoji, e só se combinar com o original."
                .to_string(),
        },
        Tone {
            name: TICKET_TONE.to_string(),
            prompt: "\
Reescreva como um chamado informativo: o texto serve para REGISTRAR e INFORMAR uma ocorrência para quem cuida do sistema (suporte, TI, equipe responsável). É informe, não pedido.
- Primeira linha: 'Ocorrência: <resumo em uma frase>'. Depois uma linha em branco.
- Em seguida, apenas os rótulos que o original de fato informa, um por linha, nesta ordem: 'Quando', 'Onde' (sistema, tela, módulo, empresa/filial), 'O que aconteceu', 'Mensagem de erro', 'Como reproduzir' (passos numerados, se houver), 'Impacto', 'Já verificado'.
- Rótulo sem informação no original é omitido. Não invente data, horário, versão, código, quantidade nem causa provável. Se algo essencial faltar, não preencha: apenas não escreva a linha.
- Mensagem de erro, código, ID, número de documento, caminho de arquivo e nome de tela vão transcritos igual ao original, sem reescrita.
- Tom neutro de registro: fato observado, não reclamação nem desabafo. Sem cobrança de prazo e sem urgência que o original não pediu.
- Feche com uma linha curta se colocando à disposição para mais detalhes, só se o original não tiver um encerramento próprio."
                .to_string(),
        },
        Tone {
            name: "Feedback difícil".to_string(),
            prompt: "\
Reescreva como um feedback difícil dado por um líder que as pessoas confiam.
- Fato observado -> impacto concreto -> o que se espera daqui pra frente. Nessa ordem.
- Fale de comportamento e resultado, nunca de caráter ou personalidade.
- Sem sanduíche de elogio falso e sem ameaça velada.
- Termine convidando para a resposta do outro lado."
                .to_string(),
        },
    ]
}
