//! Instancia web de Iurefficient (fases F0 y F1 del plan).
//!
//! F0 · Cuenta: se reutiliza la cuenta activa de la suite (`~/.config/iurefficient/
//! cuenta-activa.json`) y sus credenciales del llavero compartido. Con la sesión REST que
//! dejó otra app se crea un token MCP propio de IureVoz.
//!
//! F1 · Preguntar: cliente del servidor MCP de la instancia (`/api/mcp`, solo lectura). Sus
//! herramientas se descubren con `tools/list`, así que las nuevas aparecen solas.
//!
//! El llavero se lee y escribe con `secret-tool` (mismo Secret Service y mismos atributos
//! que el crate `keyring` de iurefficient-connect): así no hace falta libdbus para compilar.

use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iurefficient_connect::account::{self, Account};
use iurefficient_connect::mcp::{McpClient, Tool};
use iurefficient_connect::rest::{Session, SessionExport};
use serde_json::{Value, json};

const AGENTE_HTTP: &str = concat!("IureVoz/", env!("CARGO_PKG_VERSION"));

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().expect("runtime de tokio"))
}

// ── llavero ──────────────────────────────────────────────────────────────────

fn usuario(acc: &Account, tipo: &str) -> String {
    format!("{}:{}:{tipo}", acc.host(), acc.email)
}

fn leer_secreto(acc: &Account, tipo: &str) -> Option<String> {
    let out = Command::new("secret-tool")
        .args(["lookup", "service", "iurefficient", "username", &usuario(acc, tipo)])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    (out.status.success() && !s.is_empty()).then_some(s)
}

fn guardar_secreto(acc: &Account, tipo: &str, valor: &str) -> Result<()> {
    use std::io::Write;
    let u = usuario(acc, tipo);
    // mismos atributos y etiqueta que keyring v3, para que las demás apps lo encuentren
    let mut hijo = Command::new("secret-tool")
        .args(["store", "--label", &format!("{u}@iurefficient:default (keyring v3.6.3)")])
        .args(["application", "rust-keyring", "service", "iurefficient", "target", "default", "username", &u])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("falta secret-tool")?;
    hijo.stdin.take().unwrap().write_all(valor.as_bytes())?;
    if !hijo.wait()?.success() {
        bail!("no pude guardar en el llavero");
    }
    Ok(())
}

// ── F0 · cuenta ──────────────────────────────────────────────────────────────

pub fn cuenta() -> Option<Account> {
    account::active()?.account().ok()
}

#[derive(Clone, Debug)]
pub enum Estado {
    SinCuenta,
    /// Hay cuenta pero ni token MCP ni sesión: hay que entrar desde otra app de la suite.
    SinCredenciales(String),
    /// Hay sesión, falta crear el token de IureVoz.
    PorConectar(String),
    Conectado(String),
}

pub fn estado() -> Estado {
    let Some(acc) = cuenta() else { return Estado::SinCuenta };
    let quien = format!("{} en {}", acc.email, acc.host());
    if leer_secreto(&acc, "mcp").is_some() {
        Estado::Conectado(quien)
    } else if leer_secreto(&acc, "session").is_some() {
        Estado::PorConectar(quien)
    } else {
        Estado::SinCredenciales(quien)
    }
}

/// Con una sesión válida: crea el token MCP de IureVoz y guarda token y sesión en el
/// llavero (la sesión renovada también le sirve a las demás apps de la suite).
async fn crear_token(sess: &Session, acc: &Account) -> Result<String> {
    let nuevo = iurefficient_connect::api::create_mcp_token(sess, "IureVoz", Some(365)).await?;
    if let Ok(j) = serde_json::to_string(&sess.export()) {
        if let Err(e) = guardar_secreto(acc, "session", &j) {
            eprintln!("no pude actualizar la sesión compartida: {e:#}");
        }
    }
    guardar_secreto(acc, "mcp", &nuevo.secret)?;
    *CLIENTE.lock().unwrap() = None;
    Ok(format!("IureVoz quedó conectado a {} como {}", acc.host(), acc.email))
}

/// Crea el token MCP de IureVoz con la sesión compartida de la suite.
pub fn conectar() -> Result<String> {
    let acc = cuenta().context("no hay cuenta de Iurefficient en este equipo: entra desde IureTranscribe o IureDav")?;
    let json = leer_secreto(&acc, "session").context("no hay sesión guardada: entra con tu contraseña en Ajustes")?;
    let guardada: SessionExport = serde_json::from_str(&json).context("la sesión guardada no se entiende")?;
    rt().block_on(async {
        let sess = Session::new(acc.clone(), AGENTE_HTTP)?;
        sess.import(&guardada).await.context("la sesión guardada venció: entra con tu contraseña en Ajustes")?;
        crear_token(&sess, &acc).await
    })
}

/// Resultado de entrar con contraseña.
pub enum Entrada {
    Listo(String),
    /// La cuenta tiene verificación en dos pasos: falta el código.
    PideCodigo,
}

/// Sesión a medio iniciar (entre la contraseña y el código de dos pasos).
static A_MEDIAS: Mutex<Option<(Session, Account, String)>> = Mutex::new(None);

/// Entra con la contraseña (que no se guarda) y crea el token de IureVoz.
pub fn entrar(contrasena: &str) -> Result<Entrada> {
    use iurefficient_connect::rest::Login;
    let acc = cuenta().context("no hay cuenta de Iurefficient en este equipo")?;
    rt().block_on(async {
        let sess = Session::new(acc.clone(), AGENTE_HTTP)?;
        match sess.login(contrasena).await? {
            Login::Ok(_) => Ok(Entrada::Listo(crear_token(&sess, &acc).await?)),
            Login::TotpRequired { totp_token } => {
                *A_MEDIAS.lock().unwrap() = Some((sess, acc, totp_token));
                Ok(Entrada::PideCodigo)
            }
        }
    })
}

/// Segundo paso: el código de la app de autenticación.
pub fn verificar_codigo(codigo: &str) -> Result<String> {
    let (sess, acc, token) = A_MEDIAS.lock().unwrap().take().context("primero entra con la contraseña")?;
    rt().block_on(async {
        sess.verify_totp(&token, codigo.trim()).await?;
        crear_token(&sess, &acc).await
    })
}

// ── F1 · MCP de la instancia ─────────────────────────────────────────────────

struct Cliente {
    mcp: McpClient,
    herramientas: Vec<Tool>,
    cargado: Instant,
}

static CLIENTE: Mutex<Option<Cliente>> = Mutex::new(None);
/// Las herramientas se vuelven a pedir cada tanto (pueden agregarse en el servidor).
const HERRAMIENTAS_DURAN: Duration = Duration::from_secs(30 * 60);

fn con_cliente<T>(f: impl FnOnce(&mut Cliente) -> Result<T>) -> Result<T> {
    let mut g = CLIENTE.lock().unwrap();
    if g.as_ref().is_none_or(|c| c.cargado.elapsed() > HERRAMIENTAS_DURAN) {
        let acc = cuenta().context("no hay cuenta de Iurefficient en este equipo")?;
        let token = leer_secreto(&acc, "mcp").context("IureVoz no está conectado a Iurefficient: conéctalo en Ajustes")?;
        let mcp = McpClient::new(acc, token.trim(), AGENTE_HTTP)?;
        let herramientas = rt().block_on(async {
            mcp.initialize("iurevoz", env!("CARGO_PKG_VERSION")).await?;
            mcp.tools().await
        })?;
        *g = Some(Cliente { mcp, herramientas, cargado: Instant::now() });
    }
    f(g.as_mut().unwrap())
}

/// Definiciones de las herramientas de la instancia en formato de llamada a funciones.
pub fn herramientas() -> Result<Value> {
    con_cliente(|c| {
        Ok(Value::Array(
            c.herramientas
                .iter()
                .map(|t| {
                    let mut params = t.input_schema.clone();
                    if !params.is_object() {
                        params = json!({"type": "object", "properties": {}});
                    }
                    // descripciones cortas: los modelos locales se pierden con textos largos
                    let desc: String = t.description.chars().take(300).collect();
                    json!({"type": "function", "function": {"name": t.name, "description": desc, "parameters": params}})
                })
                .collect(),
        ))
    })
}

pub fn es_herramienta(nombre: &str) -> bool {
    CLIENTE.lock().unwrap().as_ref().is_some_and(|c| c.herramientas.iter().any(|t| t.name == nombre))
}

/// Llama una herramienta de la instancia; el resultado se acota para el modelo.
pub fn llamar(nombre: &str, args: Value) -> Result<String> {
    let r = con_cliente(|c| rt().block_on(c.mcp.call(nombre, args)))?;
    recordar_de(&r);
    let texto = match r {
        Value::String(s) => s,
        v => v.to_string(),
    };
    Ok(texto.chars().take(6000).collect())
}

// ── estado para la ventana ───────────────────────────────────────────────────

/// Lo que la sección «Iurefficient» de Ajustes muestra. Se calcula en un hilo aparte:
/// leer una unidad de red o el llavero puede tardar segundos y congelaría la ventana.
#[derive(Clone)]
pub struct Panorama {
    pub apps: Vec<iurefficient_connect::apps::AppStatus>,
    pub unidades: Vec<iurefficient_connect::apps::DavMount>,
    pub cuenta: Option<iurefficient_connect::account::ActiveAccount>,
    pub estado: Estado,
    pub cuando: Instant,
}

static PANORAMA: Mutex<Option<Panorama>> = Mutex::new(None);
static CALCULANDO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Último panorama calculado (si hay). Si es viejo o se pide `renovar`, se recalcula aparte.
pub fn panorama(renovar: bool, avisar: impl Fn() + Send + 'static) -> Option<Panorama> {
    let actual = PANORAMA.lock().unwrap().clone();
    let viejo = actual.as_ref().is_none_or(|p| p.cuando.elapsed() > Duration::from_secs(30));
    if (renovar || viejo) && !CALCULANDO.swap(true, std::sync::atomic::Ordering::Relaxed) {
        std::thread::spawn(move || {
            let p = Panorama {
                apps: iurefficient_connect::apps::installed(),
                unidades: iurefficient_connect::apps::iuredav_mounts(),
                cuenta: account::active(),
                estado: estado(),
                cuando: Instant::now(),
            };
            *PANORAMA.lock().unwrap() = Some(p);
            CALCULANDO.store(false, std::sync::atomic::Ordering::Relaxed);
            avisar();
        });
    }
    actual
}

// ── F3 · «ábrelo» ────────────────────────────────────────────────────────────

/// Lo último de lo que se habló, para que «ábrelo» sepa qué abrir.
#[derive(Clone, Debug, PartialEq)]
pub enum Referencia {
    Proyecto { id: String, titulo: String },
    Tarea { id: String, titulo: String },
    Archivo(std::path::PathBuf),
}

static ULTIMA: Mutex<Option<Referencia>> = Mutex::new(None);

pub fn recordar(r: Referencia) {
    *ULTIMA.lock().unwrap() = Some(r);
}

pub fn ultima() -> Option<Referencia> {
    ULTIMA.lock().unwrap().clone()
}

/// Toma de un resultado del MCP el proyecto o la tarea de la que se habla: si hay uno solo
/// (o es una ficha), ese; con varios no se adivina.
fn recordar_de(v: &Value) {
    let txt = |x: &Value| x.as_str().unwrap_or("").to_string();
    let id = |x: &Value| match x {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    // ficha de proyecto
    if v.get("codigo").is_some() && !id(&v["id"]).is_empty() {
        return recordar(Referencia::Proyecto { id: id(&v["id"]), titulo: txt(&v["titulo"]) });
    }
    if let Some(ps) = v["proyectos"].as_array().filter(|a| a.len() == 1) {
        return recordar(Referencia::Proyecto { id: id(&ps[0]["id"]), titulo: txt(&ps[0]["titulo"]) });
    }
    for lista in ["tareas", "vencimientos"] {
        if let Some(ts) = v[lista].as_array().filter(|a| a.len() == 1) {
            let t = &ts[0];
            let tid = if t["tarea_id"].is_null() { id(&t["id"]) } else { id(&t["tarea_id"]) };
            return recordar(Referencia::Tarea { id: tid, titulo: txt(&t["titulo"]) });
        }
    }
    if let Some(p) = v.get("proyecto").filter(|p| p.is_object() && !id(&p["id"]).is_empty()) {
        recordar(Referencia::Proyecto { id: id(&p["id"]), titulo: txt(&p["titulo"]) });
    }
}

/// Abre la instancia en el navegador. `que`: proyecto, tarea, agenda, vencimientos, horas,
/// notificaciones, tareas o inicio. Para proyecto/tarea sin id se usa `nombre` (se busca) o
/// lo último mencionado.
pub fn abrir(que: &str, id: Option<&str>, nombre: Option<&str>) -> Result<String> {
    let acc = cuenta().context("no hay cuenta de Iurefficient en este equipo")?;
    let base = acc.base.as_str().trim_end_matches('/').to_string();
    let que = que.trim().to_lowercase();
    let (ruta, desc) = match que.as_str() {
        "agenda" | "calendario" => ("/dashboard/calendar".to_string(), "la agenda".to_string()),
        "vencimientos" | "plazos" => ("/dashboard/deadlines".into(), "los vencimientos".into()),
        "horas" | "hoja de horas" | "timesheet" => ("/dashboard/timesheet".into(), "la hoja de horas".into()),
        "notificaciones" => ("/dashboard/notifications".into(), "las notificaciones".into()),
        "tareas" => ("/dashboard/tasks".into(), "las tareas".into()),
        "inicio" | "tablero" | "iurefficient" => ("/dashboard".into(), "Iurefficient".into()),
        "tarea" => {
            let (tid, t) = match (id, ultima()) {
                (Some(i), _) => (i.to_string(), "la tarea".to_string()),
                (None, Some(Referencia::Tarea { id, titulo })) => (id, format!("la tarea «{titulo}»")),
                _ => bail!("no sé qué tarea abrir: dime su nombre o pregunta primero por ella"),
            };
            (format!("/dashboard/tasks/{tid}"), t)
        }
        _ => {
            // proyecto (o cualquier otra cosa que suene a expediente)
            let (pid, t) = if let Some(i) = id {
                (i.to_string(), "el proyecto".to_string())
            } else if let Some(n) = nombre.filter(|n| !n.trim().is_empty()) {
                let r = con_cliente(|c| rt().block_on(c.mcp.call("listar_proyectos", json!({"busqueda": n, "limite": 1}))))?;
                let p = r["proyectos"].get(0).with_context(|| format!("no encontré un proyecto «{n}»"))?;
                let pid = p["id"].as_str().map(str::to_string).unwrap_or_else(|| p["id"].to_string());
                let titulo = p["titulo"].as_str().unwrap_or(n).to_string();
                recordar(Referencia::Proyecto { id: pid.clone(), titulo: titulo.clone() });
                (pid, format!("el proyecto «{titulo}»"))
            } else if let Some(Referencia::Proyecto { id, titulo }) = ultima() {
                (id, format!("el proyecto «{titulo}»"))
            } else {
                bail!("no sé qué proyecto abrir: dime su nombre");
            };
            (format!("/dashboard/cases/{pid}"), t)
        }
    };
    let url = format!("{base}{ruta}");
    Command::new("gio").args(["open", &url]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    Ok(format!("abrí {desc} en el navegador"))
}

/// «Ábrelo»: abre lo último mencionado, sea de la instancia o un archivo.
pub fn abrir_lo_ultimo() -> Result<String> {
    match ultima() {
        Some(Referencia::Proyecto { id, titulo }) => abrir("proyecto", Some(&id), None).map(|_| format!("abrí el proyecto «{titulo}» en el navegador")),
        Some(Referencia::Tarea { id, titulo }) => abrir("tarea", Some(&id), None).map(|_| format!("abrí la tarea «{titulo}» en el navegador")),
        Some(Referencia::Archivo(p)) => crate::suite::abrir(&p.to_string_lossy(), false),
        None => bail!("no sé qué abrir: aún no hemos hablado de ningún proyecto, tarea o archivo"),
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn recuerda_lo_mencionado() {
        recordar_de(&json!({"proyectos": [{"id": "abc", "titulo": "Heralva"}], "total": 1}));
        assert_eq!(ultima(), Some(Referencia::Proyecto { id: "abc".into(), titulo: "Heralva".into() }));
        // con varios no se adivina
        recordar_de(&json!({"proyectos": [{"id": "x"}, {"id": "y"}]}));
        assert_eq!(ultima(), Some(Referencia::Proyecto { id: "abc".into(), titulo: "Heralva".into() }));
        recordar_de(&json!({"vencimientos": [{"tarea_id": 7, "titulo": "Informe"}]}));
        assert_eq!(ultima(), Some(Referencia::Tarea { id: "7".into(), titulo: "Informe".into() }));
    }
}
