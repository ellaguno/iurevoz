//! F6b · `iurevoz --mcp`: IureVoz como servidor MCP por stdio.
//!
//! Claude Code, Claude Desktop o cualquier cliente MCP pueden hablarle al usuario y
//! escucharlo a través de la app abierta: este proceso no carga modelos, solo pasa cada
//! pedido por el socket de control y espera la respuesta. Así la voz, el micrófono y la
//! tarjeta de la ventana son los mismos que en el uso normal.
//!
//! Stdout es del protocolo: aquí nada se imprime.

use std::time::Duration;

use anyhow::{Context, anyhow};
use iurefficient_connect::mcp_server::{Call, CallError, Handler, Reply, ServerInfo, serve};
use serde_json::{Value, json};

use crate::control::preguntar_a_instancia;

struct Voz;

fn pedir(orden: &str, args: Value, espera: Duration) -> anyhow::Result<String> {
    preguntar_a_instancia(&format!("{orden} {args}"), espera)
        .context("IureVoz no está abierto: ábrelo para poder hablar y escuchar")
}

fn texto(a: &Value, k: &str) -> Option<String> {
    a[k].as_str().map(str::trim).filter(|t| !t.is_empty()).map(String::from)
}

impl Handler for Voz {
    fn info(&self) -> ServerInfo {
        ServerInfo {
            name: "iurevoz",
            title: "IureVoz",
            version: env!("CARGO_PKG_VERSION"),
            instructions: "Voz del usuario en su computadora (IureVoz, local). Usa `decir` para avisarle algo en voz alta \
                (frases cortas, sin markdown: se leen tal cual), `preguntar_si_no` para una confirmación rápida, \
                `preguntar_por_voz` para una respuesta breve y `dictar` para un texto largo. El usuario puede no estar \
                mirando la pantalla; si una pregunta vuelve vacía, no respondió."
                .into(),
        }
    }

    fn tools(&self) -> Value {
        let ro = |titulo: &str| json!({"title": titulo, "readOnlyHint": true, "openWorldHint": false});
        json!([
            {
                "name": "decir",
                "description": "Dice un texto en voz alta en la computadora del usuario y espera a que termine. Frases cortas, sin markdown.",
                "inputSchema": {"type": "object", "properties": {"texto": {"type": "string", "description": "lo que se dice"}}, "required": ["texto"]},
                "annotations": ro("Decir en voz alta"),
            },
            {
                "name": "preguntar_si_no",
                "description": "Hace una pregunta de sí o no en voz alta (y en la ventana de IureVoz) y devuelve la respuesta del usuario: si, no o sin_respuesta (tras unos 100 s).",
                "inputSchema": {"type": "object", "properties": {"pregunta": {"type": "string"}}, "required": ["pregunta"]},
                "annotations": ro("Preguntar sí o no"),
            },
            {
                "name": "preguntar_por_voz",
                "description": "Hace una pregunta en voz alta y devuelve lo que el usuario conteste (transcrito), hasta que haga una pausa. Vacío si no contestó en unos 25 s.",
                "inputSchema": {"type": "object", "properties": {
                    "pregunta": {"type": "string"},
                    "pausa_segundos": {"type": "number", "description": "silencio que da por terminada la respuesta (0.6-6, por omisión 1.2)"}},
                    "required": ["pregunta"]},
                "annotations": ro("Preguntar por voz"),
            },
            {
                "name": "dictar",
                "description": "Escucha un texto largo dictado por el usuario y lo devuelve transcrito. Termina con una pausa de unos 3 s. Opcionalmente dice antes una indicación.",
                "inputSchema": {"type": "object", "properties": {
                    "indicacion": {"type": "string", "description": "qué se le pide dictar (se dice en voz alta)"}}},
                "annotations": ro("Tomar dictado"),
            },
        ])
    }

    fn call(&self, name: &str, a: &Value, _call: &Call) -> Result<Reply, CallError> {
        match name {
            "decir" => {
                let t = texto(a, "texto").ok_or_else(|| CallError::Params("falta «texto»".into()))?;
                pedir("decir", json!({"texto": t}), Duration::from_secs(320))?;
                Ok(Reply::Data(json!({"dicho": true})))
            }
            "preguntar_si_no" => {
                let p = texto(a, "pregunta").ok_or_else(|| CallError::Params("falta «pregunta»".into()))?;
                let r = pedir("si_no", json!({"pregunta": p}), Duration::from_secs(120))?;
                let respuesta = match r.trim() {
                    "si" => "si",
                    "no" => "no",
                    _ => "sin_respuesta",
                };
                Ok(Reply::Data(json!({"respuesta": respuesta})))
            }
            "preguntar_por_voz" | "dictar" => {
                let (pregunta, pausa) = if name == "dictar" {
                    (texto(a, "indicacion"), 3.0)
                } else {
                    let p = texto(a, "pregunta").ok_or_else(|| CallError::Params("falta «pregunta»".into()))?;
                    (Some(p), a["pausa_segundos"].as_f64().unwrap_or(1.2))
                };
                let r = pedir("escuchar", json!({"pregunta": pregunta, "pausa": pausa}), Duration::from_secs(620))?;
                let r = r.trim().to_string();
                Ok(Reply::Data(json!({"texto": r, "respondio": !r.is_empty()})))
            }
            otro => Err(CallError::Tool(anyhow!("herramienta desconocida: {otro}"))),
        }
    }
}

/// `iurevoz --mcp`: atiende stdin hasta que el cliente cierre.
pub fn servir() -> i32 {
    serve(Voz)
}

/// Comando para registrar IureVoz en Claude Code (alcance de usuario).
pub fn comando_claude_code() -> anyhow::Result<Vec<String>> {
    let exe = iurefficient_connect::agents::command()?;
    Ok(["claude", "mcp", "add", "--scope", "user", "iurevoz", "--", &exe, "--mcp"].map(String::from).to_vec())
}

/// ¿Ya está registrado en Claude Code? (lee `~/.claude.json`)
pub fn registrado_en_claude_code() -> bool {
    let Some(h) = dirs::home_dir() else { return false };
    std::fs::read_to_string(h.join(".claude.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .is_some_and(|v| v["mcpServers"]["iurevoz"].is_object())
}

/// Registra IureVoz en Claude Code con `claude mcp add`.
pub fn registrar_en_claude_code() -> anyhow::Result<String> {
    let c = comando_claude_code()?;
    let out = crate::plataforma::claude().args(&c[1..]).output().context("no encontré el comando `claude` (Claude Code)")?;
    if !out.status.success() {
        anyhow::bail!("claude mcp add: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok("IureVoz quedó registrado en Claude Code; ábrelo de nuevo para que lo cargue".into())
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn herramientas_publicadas() {
        let t = Voz.tools();
        let nombres: Vec<&str> = t.as_array().unwrap().iter().filter_map(|x| x["name"].as_str()).collect();
        assert_eq!(nombres, ["decir", "preguntar_si_no", "preguntar_por_voz", "dictar"]);
        assert!(t.as_array().unwrap().iter().all(|x| x["inputSchema"]["type"] == "object"));
    }
}
