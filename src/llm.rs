//! Cliente da OpenRouter com streaming SSE.

use crate::config::{Config, Tone, OPENROUTER_URL};
use anyhow::{anyhow, Result};
use std::io::{BufRead, BufReader};
use std::sync::mpsc::Sender;
use std::time::Duration;

#[derive(Debug)]
pub enum Msg {
    Delta(String),
    Done,
    Error(String),
}

/// Enviado junto de cada mensagem para o app descartar respostas de pedidos antigos.
pub type Generation = u64;

fn system_prompt(cfg: &Config, tone: &Tone) -> String {
    let mut prompt = String::from(crate::config::BASE_RULES);
    prompt.push_str("\n\n# Tom pedido: ");
    prompt.push_str(&tone.name);
    prompt.push('\n');
    prompt.push_str(&tone.prompt);

    let signature = cfg.signature.trim();
    if !signature.is_empty() {
        prompt.push_str("\n\n# Autor\nQuem assina o texto e: ");
        prompt.push_str(signature);
        prompt.push_str(". Use esse nome se o formato pedir assinatura.");
    }

    let extra = cfg.extra_context.trim();
    if !extra.is_empty() {
        prompt.push_str("\n\n# Contexto fixo\n");
        prompt.push_str(extra);
    }

    prompt
}

fn user_prompt(input: &str, instruction: &str) -> String {
    let instruction = instruction.trim();
    if instruction.is_empty() {
        format!("Texto original:\n<<<\n{input}\n>>>")
    } else {
        format!("Texto original:\n<<<\n{input}\n>>>\n\nInstrucao adicional do autor: {instruction}")
    }
}

/// Roda a chamada inteira em bloqueante; o app chama isso numa thread propria.
pub fn stream_improve(
    cfg: &Config,
    tone: &Tone,
    input: &str,
    instruction: &str,
    generation: Generation,
    tx: &Sender<(Generation, Msg)>,
    on_progress: impl Fn(),
) {
    let result = run(cfg, tone, input, instruction, generation, tx, &on_progress);
    let msg = match result {
        Ok(()) => Msg::Done,
        Err(err) => Msg::Error(format!("{err:#}")),
    };
    let _ = tx.send((generation, msg));
    on_progress();
}

fn run(
    cfg: &Config,
    tone: &Tone,
    input: &str,
    instruction: &str,
    generation: Generation,
    tx: &Sender<(Generation, Msg)>,
    on_progress: &impl Fn(),
) -> Result<()> {
    let api_key = cfg.resolved_api_key().ok_or_else(|| {
        anyhow!(
            "sem chave da OpenRouter. Defina a env {} ou preencha api_key em {}",
            crate::config::API_KEY_ENV,
            Config::path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "config.toml".into())
        )
    })?;

    let body = serde_json::json!({
        "model": cfg.model,
        "temperature": cfg.temperature,
        "max_tokens": cfg.max_tokens,
        "stream": true,
        "messages": [
            { "role": "system", "content": system_prompt(cfg, tone) },
            { "role": "user", "content": user_prompt(input, instruction) },
        ],
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let response = client
        .post(OPENROUTER_URL)
        .bearer_auth(api_key)
        .header("HTTP-Referer", "https://github.com/arthurbolsoni/better-answer")
        .header("X-Title", "better-answer")
        .json(&body)
        .send()?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().unwrap_or_default();
        let detail = detail.chars().take(400).collect::<String>();
        return Err(anyhow!("OpenRouter respondeu {status}: {detail}"));
    }

    let reader = BufReader::new(response);
    for line in reader.lines() {
        let line = line?;
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            if payload == "[DONE]" {
                break;
            }
            continue;
        }

        let value: serde_json::Value = match serde_json::from_str(payload) {
            Ok(value) => value,
            // Comentarios de keep-alive e fragmentos nao-JSON sao ignorados de proposito.
            Err(_) => continue,
        };

        if let Some(err) = value.get("error") {
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("erro desconhecido");
            return Err(anyhow!("OpenRouter: {message}"));
        }

        let delta = value
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or_default();

        if !delta.is_empty() {
            if tx.send((generation, Msg::Delta(delta.to_string()))).is_err() {
                return Ok(());
            }
            on_progress();
        }
    }

    Ok(())
}
