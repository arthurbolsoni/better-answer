use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub const API_KEY_ENV: &str = "OPENROUTER_API_KEY";
/// Aponta o config para outro arquivo. Usado pelos testes e2e e por instalacao portatil.
pub const CONFIG_PATH_ENV: &str = "BETTER_ANSWER_CONFIG";

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
            let cfg: Config = toml::from_str(&raw)
                .with_context(|| format!("parseando {}", path.display()))?;
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
    fn defaults_ship_both_hotkeys() {
        let cfg = Config::default();
        assert_eq!(cfg.hotkey, "ctrl+b");
        assert_eq!(cfg.quick_hotkey, "ctrl+s");
        assert!(crate::hotkey::parse(&cfg.hotkey).is_ok());
        assert!(crate::hotkey::parse(&cfg.quick_hotkey).is_ok());
    }

    /// Config antigo (sem `quick_hotkey`) precisa continuar carregando.
    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg: Config = toml::from_str(r#"model = "algum/modelo""#).unwrap();
        assert_eq!(cfg.model, "algum/modelo");
        assert_eq!(cfg.quick_hotkey, "ctrl+s");
        assert!(!cfg.tones.is_empty());
    }

    #[test]
    fn round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.quick_hotkey = "ctrl+shift+j".to_string();
        let raw = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&raw).unwrap();
        assert_eq!(back.quick_hotkey, "ctrl+shift+j");
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
