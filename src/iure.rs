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
    let texto = match r {
        Value::String(s) => s,
        v => v.to_string(),
    };
    Ok(texto.chars().take(6000).collect())
}
