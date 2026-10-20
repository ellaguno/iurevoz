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
Si la instrucción suena a abrir algo (aunque esté mal escrita, p. ej. «apre» o «habre»), usa abrir_aplicacion. \
Nunca digas que hiciste o harás algo sin haber llamado la herramienta correspondiente. \
Para cualquier cuenta usa la herramienta calcular, nunca calcules de memoria. \
Cuando termines responde en español, en una o dos frases cortas, porque tu respuesta se lee en voz alta. \
No uses markdown ni listas. Si la instrucción completa o responde a algo de los mensajes anteriores \
(p. ej. solo un número tras preguntar «¿de qué cantidad?»), úsala junto con ese contexto. \
Fecha y hora actuales: {}.",
        esc::hora_fecha()
    )
}

fn herramientas(con_claude: bool) -> Value {
    let f = |nombre: &str, desc: &str, props: Value, req: &[&str]| {
        json!({"type": "function", "function": {"name": nombre, "description": desc,
            "parameters": {"type": "object", "properties": props, "required": req}}})
    };
    let s = |d: &str| json!({"type": "string", "description": d});
    let mut v = json!([
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
    ]);
    if con_claude {
        v.as_array_mut().unwrap().push(f(
            "pedir_a_claude",
            "Pasa la tarea a Claude, un asistente con acceso completo a la computadora, para lo que las demás herramientas no pueden: manejar o buscar archivos, instalar o configurar programas, ejecutar comandos, preguntas complejas. El usuario confirma antes.",
            json!({"tarea": s("la tarea completa, en español")}),
            &["tarea"],
        ));
    }
    v
}

/// Pide confirmación y, si se concede, lanza la tarea con Claude. Devuelve qué pasó.
pub type Delegar = Box<dyn Fn(&str) -> Result<String> + Send + Sync>;

pub struct Agente {
    cfg: LlmCfg,
    agente: ureq::Agent,
    portal: Option<Portal>,
    voz: Option<Voz>,
    delegar: Option<Delegar>,
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
        Self { cfg, agente, portal, voz, delegar: None }
    }

    pub fn con_claude(mut self, delegar: Delegar) -> Self {
        self.delegar = Some(delegar);
        self
    }

    /// `historial`: turnos recientes (usuario/asistente) para dar continuidad a la conversación.
    pub fn ejecutar(&self, instruccion: &str, historial: &[Value], mut paso: impl FnMut(&str)) -> Result<Informe> {
        let mut inf = Informe::default();
        let mut ultimo = String::new();
        let mut mensajes = vec![json!({"role": "system", "content": sistema()})];
        mensajes.extend(historial.iter().cloned());
        mensajes.push(json!({"role": "user", "content": instruccion}));
        for _ in 0..MAX_PASOS {
            let (msg, mut llamadas) = self.llamar(&mensajes)?;
            if llamadas.is_empty() {
                // modelos chicos a veces escriben la llamada como texto en vez de estructurarla
                llamadas = llamadas_en_texto(msg["content"].as_str().unwrap_or(""), self.delegar.is_some());
            }
            if llamadas.is_empty() {
                inf.respuesta = msg["content"].as_str().unwrap_or("").trim().to_string();
                if inf.respuesta.is_empty() && !ultimo.is_empty() {
                    // sin redacción del modelo: se informa el resultado de la última acción
                    inf.respuesta = format!("Listo: {ultimo}.");
                }
                return Ok(inf);
            }
            mensajes.push(msg.clone());
            for (id, nombre, args) in llamadas {
                let res = self.herramienta(&nombre, &args);
                let texto = match &res {
                    Ok(t) => t.clone(),
                    Err(e) => format!("error: {e:#}"),
                };
                ultimo = texto.clone();
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
            "pedir_a_claude" => self.delegar.as_ref().context("Claude no está habilitado")?(&s("tarea")?),
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
                    "options": {"temperature": 0, "num_ctx": 4096}, "messages": mensajes, "tools": herramientas(self.delegar.is_some())}),
                None,
            ),
            Servicio::OpenRouter | Servicio::OpenAi => {
                let mut c = json!({"model": self.cfg.modelo, "temperature": 0, "messages": mensajes, "tools": herramientas(self.delegar.is_some())});
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

/// Reconoce llamadas escritas como texto, p. ej. el formato nativo de Gemma:
/// `abrir_aplicacion{nombre:<|"|>Excel<|"|>}` o `calcular{expresion:"2+2"}`.
fn llamadas_en_texto(texto: &str, con_claude: bool) -> Vec<(String, String, Value)> {
    let nombres: Vec<String> = herramientas(con_claude)
        .as_array()
        .map(|v| v.iter().filter_map(|h| h["function"]["name"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    for n in nombres {
        let mut resto = texto;
        while let Some(i) = resto.find(&format!("{n}{{")) {
            let cuerpo_ini = i + n.len() + 1;
            let Some(fin) = resto[cuerpo_ini..].find('}') else { break };
            let cuerpo = resto[cuerpo_ini..cuerpo_ini + fin].replace("<|\"|>", "\"");
            let mut args = serde_json::Map::new();
            for par in cuerpo.split(',') {
                if let Some((k, v)) = par.split_once(':') {
                    let v = v.trim().trim_matches('"');
                    let valor = v.parse::<i64>().map(Value::from).unwrap_or_else(|_| Value::from(v));
                    args.insert(k.trim().trim_matches('"').to_string(), valor);
                }
            }
            out.push((String::new(), n.clone(), Value::Object(args)));
            resto = &resto[cuerpo_ini + fin..];
        }
    }
    out
}

fn args_cortos(a: &Value) -> String {
    let s = match a.as_object() {
        Some(o) => o.values().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).collect::<Vec<_>>().join(", "),
        None => String::new(),
    };
    if s.chars().count() > 60 { format!("{}…", s.chars().take(60).collect::<String>()) } else { s }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn llamada_escrita_como_texto() {
        let v = llamadas_en_texto("abrir_aplicacion{nombre:<|\"|>Excel<|\"|>}", false);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].1, "abrir_aplicacion");
        assert_eq!(v[0].2["nombre"], "Excel");
        let v = llamadas_en_texto("volumen{accion:\"subir\",cantidad:20}", false);
        assert_eq!(v[0].2["cantidad"], 20);
        assert!(llamadas_en_texto("Listo, ya abrí la calculadora.", false).is_empty());
    }
}
