//! Modo comando: un LLM interpreta la instrucción hablada y llama herramientas de una
//! lista cerrada (`escritorio`). Nunca hay shell ni comandos arbitrarios.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::config::{Correccion as LlmCfg, Servicio};
use crate::escritorio as esc;
use crate::portal::Portal;
use crate::voz::Voz;

const MAX_PASOS: usize = 5;

fn sistema() -> String {
    format!(
        "Eres IureVoz, un asistente que controla el escritorio Linux (GNOME) de quien te habla. \
La instrucción viene de un reconocedor de voz y puede tener errores: interpreta la intención. \
Usa SOLO las herramientas disponibles; si algo no se puede hacer con ellas, dilo. \
Para cualquier cuenta usa la herramienta calcular, nunca calcules de memoria. \
Cuando termines responde en español, en una o dos frases cortas, porque tu respuesta se lee en voz alta. \
No uses markdown ni listas. Fecha y hora actuales: {}.",
        esc::hora_fecha()
    )
}

fn herramientas() -> Value {
    let f = |nombre: &str, desc: &str, props: Value, req: &[&str]| {
        json!({"type": "function", "function": {"name": nombre, "description": desc,
            "parameters": {"type": "object", "properties": props, "required": req}}})
    };
    let s = |d: &str| json!({"type": "string", "description": d});
    json!([
        f("abrir_aplicacion", "Abre una aplicación instalada por su nombre (p. ej. calculadora, Firefox, Archivos, terminal).", json!({"nombre": s("nombre de la aplicación")}), &["nombre"]),
        f("abrir_url", "Abre una dirección web en el navegador.", json!({"url": s("URL o dominio")}), &["url"]),
        f("buscar_web", "Busca algo en internet y abre los resultados en el navegador.", json!({"consulta": s("qué buscar")}), &["consulta"]),
        f("abrir_carpeta", "Abre una carpeta en el gestor de archivos: documentos, descargas, escritorio, imágenes, música, videos, inicio, o una ruta dentro de la carpeta personal.", json!({"nombre": s("carpeta")}), &["nombre"]),
        f("calcular", "Evalúa una expresión aritmética (+ - * / ^ %, paréntesis, sqrt). Devuelve el resultado.", json!({"expresion": s("expresión, p. ej. (25*4)+7")}), &["expresion"]),
        f("teclas", "Pulsa una combinación de teclas en la ventana activa, p. ej. ctrl+t (nueva pestaña), ctrl+w, alt+tab, super, ctrl+l, alt+f4, f11.", json!({"combinacion": s("teclas unidas con +")}), &["combinacion"]),
        f("escribir", "Escribe un texto en la ventana activa, como si se tecleara.", json!({"texto": s("texto a escribir")}), &["texto"]),
        f("volumen", "Cambia el volumen del sistema.", json!({"accion": {"type": "string", "enum": ["subir", "bajar", "silenciar", "activar"]}, "cantidad": {"type": "integer", "description": "porcentaje, 10 por omisión"}}), &["accion"]),
        f("hora_fecha", "Da la fecha y hora actuales.", json!({}), &[]),
        f("leer_seleccion", "Lee en voz alta el texto seleccionado (o el del portapapeles).", json!({}), &[]),
        f("leer_pagina", "Descarga una página web y lee en voz alta su texto principal.", json!({"url": s("URL de la página")}), &["url"]),
    ])
}

pub struct Agente {
    cfg: LlmCfg,
    agente: ureq::Agent,
    portal: Option<Portal>,
    voz: Option<Voz>,
}

/// Lo que pasó al ejecutar una instrucción.
#[derive(Debug, Clone, Default)]
pub struct Informe {
    pub acciones: Vec<String>,
    pub respuesta: String,
}

impl Agente {
    pub fn new(cfg: LlmCfg, portal: Option<Portal>, voz: Option<Voz>) -> Self {
        let agente = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(120))).build().into();
        Self { cfg, agente, portal, voz }
    }

    pub fn ejecutar(&self, instruccion: &str, mut paso: impl FnMut(&str)) -> Result<Informe> {
        let mut inf = Informe::default();
        let mut mensajes = vec![json!({"role": "system", "content": sistema()}), json!({"role": "user", "content": instruccion})];
        for _ in 0..MAX_PASOS {
            let (msg, llamadas) = self.llamar(&mensajes)?;
            if llamadas.is_empty() {
                inf.respuesta = msg["content"].as_str().unwrap_or("").trim().to_string();
                return Ok(inf);
            }
            mensajes.push(msg);
            for (id, nombre, args) in llamadas {
                let res = self.herramienta(&nombre, &args);
                let texto = match &res {
                    Ok(t) => t.clone(),
                    Err(e) => format!("error: {e:#}"),
                };
                let accion = format!("{nombre}({}) → {texto}", args_cortos(&args));
                paso(&accion);
                inf.acciones.push(accion);
                let mut r = json!({"role": "tool", "content": texto});
                if self.cfg.servicio == Servicio::Ollama {
                    r["tool_name"] = json!(nombre);
                } else {
                    r["tool_call_id"] = json!(id);
                }
                mensajes.push(r);
            }
        }
        inf.respuesta = "Hice lo que pude, pero la instrucción requirió demasiados pasos.".into();
        Ok(inf)
    }

    fn herramienta(&self, nombre: &str, a: &Value) -> Result<String> {
        let s = |k: &str| a[k].as_str().map(str::to_string).with_context(|| format!("falta «{k}»"));
        match nombre {
            "abrir_aplicacion" => esc::abrir_aplicacion(&s("nombre")?),
            "abrir_url" => esc::abrir_url(&s("url")?),
            "buscar_web" => esc::buscar_web(&s("consulta")?),
            "abrir_carpeta" => esc::abrir_carpeta(&s("nombre")?),
            "calcular" => esc::calcular(&s("expresion")?),
            "teclas" => {
                let c = s("combinacion")?;
                let ks = esc::combinacion(&c)?;
                self.portal.as_ref().context("sin acceso al teclado")?.teclas(ks);
                Ok(format!("pulsé {c}"))
            }
            "escribir" => {
                let t = s("texto")?;
                self.portal.as_ref().context("sin acceso al teclado")?.escribir(t.clone());
                Ok(format!("escribí {} caracteres", t.chars().count()))
            }
            "volumen" => esc::volumen(&s("accion")?, a["cantidad"].as_u64().map(|n| n as u32)),
            "hora_fecha" => Ok(esc::hora_fecha()),
            "leer_seleccion" => {
                let t = esc::seleccion()?;
                self.voz.as_ref().context("la voz está desactivada")?.decir(t.clone());
                Ok(format!("leyendo {} palabras en voz alta", t.split_whitespace().count()))
            }
            "leer_pagina" => {
                let t = esc::texto_de_pagina(&s("url")?)?;
                self.voz.as_ref().context("la voz está desactivada")?.decir(t.clone());
                Ok(format!("leyendo la página ({} palabras)", t.split_whitespace().count()))
            }
            n => bail!("herramienta desconocida: {n}"),
        }
    }

    /// Devuelve el mensaje del asistente y sus llamadas: (id, nombre, argumentos).
    fn llamar(&self, mensajes: &[Value]) -> Result<(Value, Vec<(String, String, Value)>)> {
        let base = self.cfg.url.trim_end_matches('/');
        let (url, cuerpo, clave) = match self.cfg.servicio {
            Servicio::Ollama => (
                format!("{base}/api/chat"),
                json!({"model": self.cfg.modelo, "stream": false, "think": false, "keep_alive": "30m",
                    "options": {"temperature": 0, "num_ctx": 4096}, "messages": mensajes, "tools": herramientas()}),
                None,
            ),
            Servicio::OpenRouter | Servicio::OpenAi => {
                let mut c = json!({"model": self.cfg.modelo, "temperature": 0, "messages": mensajes, "tools": herramientas()});
                if self.cfg.servicio == Servicio::OpenRouter {
                    c["reasoning"] = json!({"enabled": false});
                }
                let clave = self.cfg.clave_efectiva();
                if self.cfg.servicio == Servicio::OpenRouter && clave.is_none() {
                    bail!("falta la API key de OpenRouter");
                }
                (format!("{base}/chat/completions"), c, clave)
            }
        };
        let mut req = self.agente.post(&url).header("X-Title", "iurevoz");
        if let Some(c) = &clave {
            req = req.header("Authorization", &format!("Bearer {c}"));
        }
        let r: Value = req.send_json(&cuerpo).with_context(|| format!("POST {url}"))?.body_mut().read_json()?;
        let msg = if self.cfg.servicio == Servicio::Ollama { r["message"].clone() } else { r["choices"][0]["message"].clone() };
        let llamadas = msg["tool_calls"]
            .as_array()
            .map(|v| {
                v.iter()
                    .map(|c| {
                        let f = &c["function"];
                        // OpenAI manda los argumentos como texto JSON; Ollama como objeto
                        let args = match &f["arguments"] {
                            Value::String(s) => serde_json::from_str(s).unwrap_or(json!({})),
                            v => v.clone(),
                        };
                        (c["id"].as_str().unwrap_or("").to_string(), f["name"].as_str().unwrap_or("").to_string(), args)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok((msg, llamadas))
    }
}

fn args_cortos(a: &Value) -> String {
    let s = match a.as_object() {
        Some(o) => o.values().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).collect::<Vec<_>>().join(", "),
        None => String::new(),
    };
    if s.chars().count() > 60 { format!("{}…", s.chars().take(60).collect::<String>()) } else { s }
}
