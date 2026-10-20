//! Etapa 2: delegar tareas a Claude Code (`claude -p`) con la cuenta del usuario.
//!
//! Cada acción que Claude quiera hacer y que no sea de solo lectura pasa por
//! `iurevoz mcp-permisos`, un servidor MCP mínimo que le pregunta a la app abierta
//! (por el socket de control) y espera el «sí» o «no» del usuario.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// Herramientas de solo lectura que Claude puede usar sin preguntar.
const LECTURA: &[&str] = &["Read", "Glob", "Grep", "LS", "WebSearch", "WebFetch", "TodoWrite"];

const INSTRUCCIONES: &str = "Te habla en español una persona por voz, mediante IureVoz. \
Su instrucción viene de un reconocedor de voz y puede tener errores: interpreta la intención. \
Cada acción que modifique algo se le pregunta en voz alta, así que no pidas confirmaciones extra. \
En cada comando de Bash pon en el campo description, en español, qué hace en pocas palabras (se lee en voz alta). \
Al terminar responde en español en una a tres frases cortas, sin markdown, porque se leerán en voz alta.";

#[allow(dead_code)]
pub fn disponible() -> bool {
    Command::new("claude").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Versión hablada: solo qué se va a hacer, sin el comando ni rutas completas.
pub fn describir_para_voz(herramienta: &str, entrada: &Value) -> String {
    let s = |k: &str| entrada[k].as_str().unwrap_or("").to_string();
    let nombre = |k: &str| s(k).rsplit('/').next().unwrap_or("").to_string();
    match herramienta {
        "Bash" => {
            let d = s("description");
            if d.is_empty() { "ejecutar un comando".into() } else { d }
        }
        "Write" => format!("crear o sobrescribir el archivo {}", nombre("file_path")),
        "Edit" | "MultiEdit" => format!("editar el archivo {}", nombre("file_path")),
        "NotebookEdit" => format!("editar el cuaderno {}", nombre("notebook_path")),
        h => format!("usar {h}"),
    }
}

/// Descripción de una acción para la ventana (con el comando completo).
pub fn describir(herramienta: &str, entrada: &Value) -> String {
    let s = |k: &str| entrada[k].as_str().unwrap_or("").to_string();
    let corto = |t: String| if t.chars().count() > 160 { format!("{}…", t.chars().take(160).collect::<String>()) } else { t };
    match herramienta {
        "Bash" => {
            let d = s("description");
            if d.is_empty() { format!("ejecutar: {}", corto(s("command"))) } else { format!("{d} ({})", corto(s("command"))) }
        }
        "Write" => format!("crear o sobrescribir el archivo {}", s("file_path")),
        "Edit" | "MultiEdit" => format!("editar el archivo {}", s("file_path")),
        "NotebookEdit" => format!("editar el cuaderno {}", s("notebook_path")),
        h => format!("usar {h}"),
    }
}

/// Lo que va pasando mientras Claude trabaja.
pub enum Progreso {
    Texto(String),
    Herramienta(String),
}

/// Corre una tarea con Claude Code y devuelve su respuesta final.
pub fn ejecutar(tarea: &str, mut progreso: impl FnMut(Progreso)) -> Result<String> {
    let exe = std::env::current_exe()?;
    let mcp = json!({"mcpServers": {"iurevoz": {"command": exe, "args": ["mcp-permisos"]}}});
    let ruta_mcp = std::env::temp_dir().join(format!("iurevoz-mcp-{}.json", std::process::id()));
    std::fs::write(&ruta_mcp, mcp.to_string())?;
    let home = dirs::home_dir().context("sin carpeta personal")?;

    let mut cmd = Command::new("claude");
    // si IureVoz se lanzó desde una sesión de Claude Code, no heredar su configuración
    for (k, _) in std::env::vars() {
        if k.starts_with("CLAUDE") && k != "CLAUDE_CONFIG_DIR" {
            cmd.env_remove(k);
        }
    }
    let mut hijo = cmd
        .current_dir(&home)
        .args(["-p", tarea, "--output-format", "stream-json", "--verbose"])
        .arg("--mcp-config")
        .arg(&ruta_mcp)
        .args(["--strict-mcp-config", "--permission-prompt-tool", "mcp__iurevoz__aprobar"])
        .args(["--append-system-prompt", INSTRUCCIONES])
        .arg("--allowedTools")
        .arg(LECTURA.join(","))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("no pude ejecutar claude (¿está instalado?)")?;

    let mut resultado = None;
    for linea in BufReader::new(hijo.stdout.take().unwrap()).lines() {
        let Ok(linea) = linea else { break };
        let Ok(v) = serde_json::from_str::<Value>(&linea) else { continue };
        match v["type"].as_str() {
            Some("assistant") => {
                for c in v["message"]["content"].as_array().into_iter().flatten() {
                    match c["type"].as_str() {
                        Some("text") => progreso(Progreso::Texto(c["text"].as_str().unwrap_or("").trim().to_string())),
                        Some("tool_use") => {
                            let h = c["name"].as_str().unwrap_or("");
                            if !h.starts_with("mcp__iurevoz") {
                                progreso(Progreso::Herramienta(describir(h, &c["input"])));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("result") => resultado = Some(v),
            _ => {}
        }
    }
    let estado = hijo.wait()?;
    let _ = std::fs::remove_file(&ruta_mcp);
    match resultado {
        Some(r) if r["is_error"].as_bool() != Some(true) => Ok(r["result"].as_str().unwrap_or("").trim().to_string()),
        Some(r) => bail!("Claude terminó con error: {}", r["result"].as_str().unwrap_or("sin detalle")),
        None => {
            let mut err = String::new();
            if let Some(mut e) = hijo.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            bail!("claude salió sin resultado ({estado}): {}", err.trim())
        }
    }
}

// ── servidor MCP de permisos ─────────────────────────────────────────────────

/// Pregunta a la app abierta. Sin app (o sin respuesta en 2 minutos), se niega.
fn preguntar_a_la_app(pedido: &Value) -> (bool, String) {
    let ruta = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join("iurevoz.sock");
    let Ok(mut s) = UnixStream::connect(ruta) else {
        return (false, "IureVoz no está abierto para pedir permiso".into());
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(120)));
    if s.write_all(format!("permiso {pedido}\n").as_bytes()).is_err() {
        return (false, "no pude preguntar al usuario".into());
    }
    let _ = s.shutdown(std::net::Shutdown::Write);
    let mut r = String::new();
    let _ = s.read_to_string(&mut r);
    match r.trim() {
        "si" => (true, String::new()),
        "" => (false, "el usuario no respondió a tiempo".into()),
        _ => (false, "el usuario no lo permitió".into()),
    }
}

/// `iurevoz mcp-permisos`: servidor MCP por stdio con una herramienta, `aprobar`.
pub fn servidor_permisos() -> Result<()> {
    let log = std::env::var_os("IUREVOZ_MCP_LOG");
    let registrar = |m: &str| {
        if let Some(r) = &log {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(r) {
                let _ = writeln!(f, "{m}");
            }
        }
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for linea in stdin.lock().lines() {
        let linea = linea?;
        registrar(&format!("<- {linea}"));
        let Ok(m) = serde_json::from_str::<Value>(&linea) else { continue };
        let Some(id) = m.get("id").cloned() else { continue }; // notificaciones: sin respuesta
        let resultado = match m["method"].as_str().unwrap_or("") {
            "initialize" => json!({
                "protocolVersion": m["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "iurevoz", "version": env!("CARGO_PKG_VERSION")},
            }),
            "tools/list" => json!({"tools": [{
                "name": "aprobar",
                "description": "Pregunta al usuario de IureVoz si permite una acción.",
                "inputSchema": {"type": "object", "properties": {
                    "tool_name": {"type": "string"}, "input": {"type": "object"}, "tool_use_id": {"type": "string"}
                }, "required": ["tool_name", "input"]},
            }]}),
            "tools/call" => {
                let a = &m["params"]["arguments"];
                let herramienta = a["tool_name"].as_str().unwrap_or("");
                let entrada = a["input"].clone();
                let pedido = json!({
                    "herramienta": herramienta,
                    "descripcion": describir(herramienta, &entrada),
                    "hablada": describir_para_voz(herramienta, &entrada),
                });
                let (ok, motivo) = match std::env::var("IUREVOZ_PERMISO_PRUEBA").as_deref() {
                    Ok("si") => (true, String::new()),
                    Ok(_) => (false, "prueba: negado".into()),
                    Err(_) => preguntar_a_la_app(&pedido),
                };
                let decision = if ok {
                    json!({"behavior": "allow", "updatedInput": entrada})
                } else {
                    json!({"behavior": "deny", "message": motivo})
                };
                json!({"content": [{"type": "text", "text": decision.to_string()}]})
            }
            "ping" => json!({}),
            otro => {
                let e = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("método desconocido: {otro}")}});
                writeln!(out, "{e}")?;
                out.flush()?;
                continue;
            }
        };
        let r = json!({"jsonrpc": "2.0", "id": id, "result": resultado});
        registrar(&format!("-> {r}"));
        writeln!(out, "{r}")?;
        out.flush()?;
    }
    Ok(())
}
