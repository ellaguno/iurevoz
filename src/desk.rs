//! IureDesk (correo, calendario, contactos, notas y tareas de este equipo) por su servidor MCP
//! (`iuredesk --mcp`). Con IureDesk abierto cada llamada corre dentro de la app; cerrado, el
//! proceso MCP abre el almacén solo durante la llamada. connect no lo conoce todavía, así que
//! el programa se busca aquí.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate};
use serde_json::{Value, json};

const CLAVE: &str = "desk";
const NOMBRE: &str = "IureDesk";

/// Herramientas que escriben en IureDesk: se confirman antes (ninguna envía correo).
const ESCRITURAS: &[&str] = &["crear_borrador", "crear_evento", "crear_nota"];

/// El programa de IureDesk: `IUREVOZ_IUREDESK`, el PATH y los lugares donde lo dejan sus paquetes.
pub fn localizar() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("IUREVOZ_IUREDESK").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let exe = if cfg!(windows) { "iuredesk.exe" } else { "iuredesk" };
    let mut c: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).map(|d| d.join(exe)).collect()).unwrap_or_default();
    if let Some(h) = dirs::home_dir() {
        c.push(h.join(".local/bin").join(exe));
        // instalado por iureapps (AppImage en Linux)
        let apps = h.join(".local/share/iureapps/apps");
        c.extend(["iureDesk.AppImage", "IureDesk.AppImage", "iuredesk.AppImage"].map(|n| apps.join(n)));
        c.push(h.join("Applications/IureDesk.app/Contents/MacOS/iuredesk"));
    }
    c.extend(["/usr/bin/iuredesk", "/usr/local/bin/iuredesk", "/opt/iuredesk/iuredesk", "/Applications/IureDesk.app/Contents/MacOS/iuredesk"].map(PathBuf::from));
    for var in ["LOCALAPPDATA", "ProgramFiles"] {
        if let Some(b) = std::env::var_os(var).map(PathBuf::from) {
            c.push(b.join("Programs").join("IureDesk").join(exe));
            c.push(b.join("IureDesk").join(exe));
        }
    }
    c.into_iter().find(|p| p.is_file())
}

fn bin() -> Result<PathBuf> {
    localizar().context("IureDesk no está instalado")
}

fn mcp(herramienta: &str, args: Value) -> Result<Value> {
    crate::suite::mcp_programa(CLAVE, NOMBRE, bin, herramienta, args, Duration::from_secs(60))
}

/// Las descripciones de IureDesk traen inglés y español («… · …»): al modelo, solo el español.
fn en_espanol(t: &str) -> String {
    let t = t.rsplit_once(" · ").map_or(t, |(_, es)| es);
    t.chars().take(300).collect()
}

static HERRAMIENTAS: Mutex<Option<Value>> = Mutex::new(None);

/// Las herramientas que anuncia IureDesk, en el formato del modelo (se piden una vez).
pub fn herramientas() -> Result<Value> {
    let mut g = HERRAMIENTAS.lock().unwrap();
    if let Some(v) = g.as_ref() {
        return Ok(v.clone());
    }
    let lista = crate::suite::mcp_herramientas(CLAVE, NOMBRE, bin)?;
    let v: Vec<Value> = lista
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let nombre = t["name"].as_str()?;
            let mut params = t["inputSchema"].clone();
            if !params.is_object() {
                params = json!({"type": "object", "properties": {}});
            }
            if let Some(props) = params["properties"].as_object_mut() {
                for p in props.values_mut() {
                    if let Some(d) = p["description"].as_str().map(en_espanol) {
                        p["description"] = json!(d);
                    }
                }
            }
            let mut desc = en_espanol(t["description"].as_str().unwrap_or(""));
            if ESCRITURAS.contains(&nombre) {
                desc.push_str(" Pide confirmación (la pide la herramienta).");
            }
            Some(json!({"type": "function", "function": {"name": nombre, "description": desc, "parameters": params}}))
        })
        .collect();
    let v = Value::Array(v);
    *g = Some(v.clone());
    Ok(v)
}

pub fn es_herramienta(nombre: &str) -> bool {
    HERRAMIENTAS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(Value::as_array)
        .is_some_and(|v| v.iter().any(|h| h["function"]["name"] == nombre))
}

/// Herramientas propias de IureVoz para IureDesk (no vienen del servidor).
pub fn herramientas_propias() -> Vec<Value> {
    vec![json!({"type": "function", "function": {"name": "redactar_correo",
        "description": "Abre el redactor de IureDesk con un correo nuevo para que el usuario lo revise y lo envíe. Si no hay cuerpo, se lo pide por voz y lo toma dictado. Busca solo el correo del destinatario si se da un nombre. Nunca envía.",
        "parameters": {"type": "object", "properties": {
            "para": {"type": "string", "description": "nombres o correos de los destinatarios, separados por coma"},
            "asunto": {"type": "string", "description": "asunto, si se dijo"},
            "cuerpo": {"type": "string", "description": "texto del correo solo si el usuario ya lo dijo en la instrucción; si no, vacío para dictarlo"}},
            "required": []}}})]
}

/// Resumen hablado de una escritura, armado con lo que de verdad se va a mandar.
pub fn describir(nombre: &str, a: &Value) -> String {
    let s = |k: &str| a[k].as_str().map(str::trim).filter(|t| !t.is_empty());
    match nombre {
        "crear_borrador" => {
            let para = s("to").map(|t| format!(" para {t}")).unwrap_or_default();
            let asunto = s("subject").map(|t| format!(" con asunto «{t}»")).unwrap_or_default();
            format!("Guardo en Borradores de IureDesk un correo{para}{asunto}. No se envía.")
        }
        "crear_evento" => {
            let titulo = s("titulo").unwrap_or("un evento");
            let cuando = s("inicio").map(cuando_hablado).unwrap_or_default();
            let lugar = s("lugar").map(|l| format!(" en {l}")).unwrap_or_default();
            format!("Creo en tu calendario de IureDesk «{titulo}»{cuando}{lugar}.")
        }
        "crear_nota" => format!("Creo en IureDesk la nota «{}».", s("title").unwrap_or("sin título")),
        n => format!("Uso {n} en IureDesk."),
    }
}

/// «2026-11-27T10:00» → « mañana a las 10:00»; «2026-11-27» → « mañana, todo el día».
fn cuando_hablado(t: &str) -> String {
    let Ok(d) = NaiveDate::parse_from_str(&t[..t.len().min(10)], "%Y-%m-%d") else {
        return format!(" el {t}");
    };
    let dia = crate::escribir::dia_hablado(d, Local::now().date_naive());
    match t.get(11..16) {
        Some(h) => format!(" {dia} a las {}", h.trim_start_matches('0')),
        None => format!(" {dia}, todo el día"),
    }
}

/// Llama una herramienta del servidor; las escrituras, solo con el permiso del usuario.
pub fn llamar(nombre: &str, a: &Value, confirmar: &dyn Fn(&str) -> bool) -> Result<String> {
    if ESCRITURAS.contains(&nombre) && !confirmar(&describir(nombre, a)) {
        return Ok("el usuario no lo permitió".into());
    }
    let r = mcp(nombre, a.clone())?;
    let texto = match r {
        Value::String(s) => s,
        v => v.to_string(),
    };
    Ok(texto.chars().take(6000).collect())
}

/// Destinatarios: los correos se quedan; cada nombre se busca en los contactos de IureDesk.
fn resolver_destinatarios(para: &str) -> Result<(Vec<String>, Vec<String>)> {
    let mut correos = Vec::new();
    let mut nombres = Vec::new();
    for parte in para.split([',', ';']).flat_map(|p| p.split(" y ")).map(str::trim).filter(|p| !p.is_empty()) {
        if parte.contains('@') {
            correos.push(parte.replace(' ', ""));
            continue;
        }
        let r = mcp("buscar_contacto", json!({"q": parte, "limit": 3}))?;
        let c = &r["contacts"][0];
        match c["emails"][0]["value"].as_str() {
            Some(e) => {
                correos.push(e.to_string());
                nombres.push(c["name"].as_str().unwrap_or(parte).to_string());
            }
            None if r["count"].as_u64().unwrap_or(0) > 0 => bail!("{} no tiene correo en tus contactos", c["name"].as_str().unwrap_or(parte)),
            None => bail!("no encontré a «{parte}» en tus contactos de IureDesk"),
        }
    }
    Ok((correos, nombres))
}

/// Enlace `iuredesk://compose?…` (el mismo que abre `mailto:`).
fn enlace_redactor(para: &[String], asunto: Option<&str>, cuerpo: &str) -> String {
    let mut q = Vec::new();
    if !para.is_empty() {
        q.push(format!("to={}", crate::suite::codificar(&para.join(","))));
    }
    if let Some(a) = asunto.filter(|a| !a.trim().is_empty()) {
        q.push(format!("subject={}", crate::suite::codificar(a.trim())));
    }
    if !cuerpo.is_empty() {
        q.push(format!("body={}", crate::suite::codificar(cuerpo)));
    }
    format!("iuredesk://compose?{}", q.join("&"))
}

/// «Coma», «punto y aparte»…: el dictado de un correo lleva la misma puntuación que al dictar.
fn puntuar(t: &str) -> String {
    let mut p = crate::puntuacion::Puntuador::new();
    p.procesar(t, true).trim().to_string()
}

/// Abre el redactor con un correo nuevo; sin cuerpo, se lo pide al usuario por voz.
/// `escuchar(pregunta, pausa en s)` devuelve lo dictado (vacío si no dijo nada).
pub fn redactar(para: Option<&str>, asunto: Option<&str>, cuerpo: Option<&str>, escuchar: &dyn Fn(&str, f64) -> String) -> Result<String> {
    let bin = bin()?;
    let (correos, nombres) = match para.filter(|p| !p.trim().is_empty()) {
        Some(p) => resolver_destinatarios(p)?,
        None => (Vec::new(), Vec::new()),
    };
    let quien = if nombres.is_empty() { correos.join(", ") } else { nombres.join(" y ") };
    let cuerpo = match cuerpo.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => c.to_string(),
        None => {
            let pregunta = if quien.is_empty() { "Dicta el correo.".to_string() } else { format!("Dicta el correo para {quien}.") };
            let t = escuchar(&pregunta, 3.0);
            if t.trim().is_empty() {
                return Ok("no oí el correo; no abrí nada".into());
            }
            puntuar(&t)
        }
    };
    let url = enlace_redactor(&correos, asunto, &cuerpo);
    // IureDesk es de instancia única: con la app abierta, el enlace le llega a ella
    let mut cmd = std::process::Command::new(bin);
    cmd.arg(url).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.spawn().context("no pude abrir IureDesk")?;
    let para = if quien.is_empty() { String::new() } else { format!(" para {quien}") };
    Ok(format!("abrí en IureDesk el correo{para} ({} palabras) para que lo revises y lo envíes", cuerpo.split_whitespace().count()))
}

/// ¿La instrucción es de correo, contactos o notas (IureDesk)? Para cuando falla el enrutador.
pub fn es_de_iuredesk(instruccion: &str) -> bool {
    let t = instruccion.to_lowercase();
    const CLAVES: &[&str] = &[
        "correo", "mail", "email", "e-mail", "mensaje de", "mensajes de", "bandeja", "borrador", "contacto", "teléfono de",
        "telefono de", "una nota", "mis notas", "la nota", "iuredesk", "le escribo a", "escríbele", "escribele", "contéstale",
        "contestale", "responde a",
    ];
    CLAVES.iter().any(|c| t.contains(c))
}

/// Olvida las herramientas guardadas (para pedirlas de nuevo).
#[cfg(test)]
fn olvidar() {
    *HERRAMIENTAS.lock().unwrap() = None;
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn descripciones_en_espanol() {
        assert_eq!(en_espanol("Finds contacts. · Busca contactos."), "Busca contactos.");
        assert_eq!(en_espanol("Solo texto"), "Solo texto");
    }

    #[test]
    fn enlace_del_redactor() {
        let u = enlace_redactor(&["ana@x.org".into(), "luis@y.mx".into()], Some("Junta"), "Hola, Ana.\nNos vemos.");
        assert_eq!(u, "iuredesk://compose?to=ana%40x.org,luis%40y.mx&subject=Junta&body=Hola,%20Ana.%0ANos%20vemos.");
        assert_eq!(enlace_redactor(&[], None, "x"), "iuredesk://compose?body=x");
    }

    #[test]
    fn resumen_de_escrituras() {
        let d = describir("crear_borrador", &json!({"to": "ana@x.org", "subject": "Junta", "body_markdown": "…"}));
        assert_eq!(d, "Guardo en Borradores de IureDesk un correo para ana@x.org con asunto «Junta». No se envía.");
        let hoy = Local::now().date_naive();
        let d = describir("crear_evento", &json!({"titulo": "Comida", "inicio": format!("{}T09:30", hoy.format("%Y-%m-%d"))}));
        assert_eq!(d, "Creo en tu calendario de IureDesk «Comida» hoy a las 9:30.");
        let d = describir("crear_evento", &json!({"titulo": "Viaje", "inicio": hoy.format("%Y-%m-%d").to_string()}));
        assert_eq!(d, "Creo en tu calendario de IureDesk «Viaje» hoy, todo el día.");
    }

    #[test]
    fn escrituras_sin_permiso_no_llaman() {
        // sin permiso no se arranca nada (ni hace falta IureDesk instalado)
        let r = llamar("crear_nota", &json!({"title": "x", "markdown": "y"}), &|_| false).unwrap();
        assert_eq!(r, "el usuario no lo permitió");
    }

    #[test]
    fn enrutamiento() {
        assert!(es_de_iuredesk("dicta un correo para Ana López"));
        assert!(es_de_iuredesk("¿tengo correos de Pérez?"));
        assert!(es_de_iuredesk("¿cuál es el teléfono de Juan Ramón?"));
        assert!(es_de_iuredesk("crea una nota con la lista del súper"));
        assert!(!es_de_iuredesk("¿qué tareas tengo pendientes?"));
        assert!(!es_de_iuredesk("abre la calculadora"));
    }

    /// Con IureDesk instalado (o `IUREVOZ_IUREDESK`): `cargo test -- --ignored iuredesk_real`.
    #[test]
    #[ignore]
    fn iuredesk_real() {
        olvidar();
        let h = herramientas().expect("tools/list");
        let nombres: Vec<&str> = h.as_array().unwrap().iter().filter_map(|t| t["function"]["name"].as_str()).collect();
        println!("{nombres:?}");
        assert!(nombres.contains(&"buscar_correo") && nombres.contains(&"crear_borrador"));
        let r = llamar("agenda", &json!({}), &|_| false).unwrap();
        println!("{}", r.chars().take(300).collect::<String>());
    }
}
