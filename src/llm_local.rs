//! LLM local integrado: la app descarga y administra su propio `llama-server` (llama.cpp),
//! sin depender de Ollama. Se habla con él por la API OpenAI-compatible.
//!
//! - Binario oficial precompilado con Vulkan (usa la GPU dedicada sin instalar CUDA).
//! - Modelo GGUF de Hugging Face, descargado una vez.
//! - El servidor se arranca al primer uso; tras 10 min sin uso duerme el modelo
//!   (libera RAM y VRAM) y lo vuelve a cargar solo en la siguiente petición.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::config::dir_datos;

/// Versión de llama.cpp probada con la app.
const VERSION: &str = "b11756";

/// Modelos disponibles: (clave, repositorio en Hugging Face, archivo, descripción).
pub const MODELOS: &[(&str, &str, &str, &str)] = &[
    ("gemma4-e2b", "unsloth/gemma-4-E2B-it-GGUF", "gemma-4-E2B-it-Q4_K_M.gguf", "Gemma 4 E2B · 3.1 GB (recomendado)"),
    ("qwen3.5-4b", "unsloth/Qwen3.5-4B-GGUF", "Qwen3.5-4B-Q4_K_M.gguf", "Qwen 3.5 4B · 2.7 GB"),
    ("qwen3.5-2b", "unsloth/Qwen3.5-2B-GGUF", "Qwen3.5-2B-Q4_K_M.gguf", "Qwen 3.5 2B · 1.3 GB (ligero)"),
];

fn dir() -> PathBuf {
    dir_datos().join("llama")
}

fn modelo(clave: &str) -> Result<&'static (&'static str, &'static str, &'static str, &'static str)> {
    MODELOS.iter().find(|m| m.0 == clave).with_context(|| format!("modelo local desconocido: {clave}"))
}

fn ruta_binario() -> PathBuf {
    dir().join(format!("llama-{VERSION}")).join(if cfg!(windows) { "llama-server.exe" } else { "llama-server" })
}

fn ruta_modelo(clave: &str) -> Result<PathBuf> {
    Ok(dir().join("modelos").join(modelo(clave)?.2))
}

/// (binario listo, modelo listo)
pub fn estado(clave: &str) -> (bool, bool) {
    (ruta_binario().exists(), ruta_modelo(clave).is_ok_and(|p| p.exists()))
}

/// Descarga lo que falte. `progreso` recibe (qué, fracción 0..1).
pub fn preparar(clave: &str, progreso: &dyn Fn(&str, f32)) -> Result<()> {
    std::fs::create_dir_all(dir().join("modelos"))?;
    if !ruta_binario().exists() {
        let paquete = if cfg!(windows) {
            "win-vulkan-x64.zip"
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            "macos-arm64.tar.gz"
        } else if cfg!(target_os = "macos") {
            "macos-x64.tar.gz"
        } else {
            "ubuntu-vulkan-x64.tar.gz"
        };
        let url = format!("https://github.com/ggml-org/llama.cpp/releases/download/{VERSION}/llama-{VERSION}-bin-{paquete}");
        let tmp = dir().join(".binario.parcial");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        let lector = descarga(&url, "llama.cpp", progreso)?;
        desempacar(lector, &tmp)?;
        // el .tar.gz trae una carpeta `llama-bXXXX`; el .zip de Windows, los archivos sueltos
        let origen = if tmp.join(format!("llama-{VERSION}")).is_dir() { tmp.join(format!("llama-{VERSION}")) } else { tmp.clone() };
        let destino = dir().join(format!("llama-{VERSION}"));
        let _ = std::fs::remove_dir_all(&destino);
        std::fs::rename(&origen, &destino)?;
        let _ = std::fs::remove_dir_all(&tmp);
    }
    let destino = ruta_modelo(clave)?;
    if !destino.exists() {
        let (_, repo, archivo, _) = modelo(clave)?;
        let url = format!("https://huggingface.co/{repo}/resolve/main/{archivo}");
        let tmp = destino.with_extension("parcial");
        let mut lector = descarga(&url, archivo, progreso)?;
        std::io::copy(&mut lector, &mut std::fs::File::create(&tmp)?)?;
        std::fs::rename(&tmp, &destino)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn desempacar(lector: impl Read, destino: &Path) -> Result<()> {
    tar::Archive::new(flate2::read::GzDecoder::new(lector)).unpack(destino)?;
    Ok(())
}

/// El .zip necesita acceso aleatorio: se guarda completo y luego se extrae.
#[cfg(windows)]
fn desempacar(mut lector: impl Read, destino: &Path) -> Result<()> {
    let zip = destino.join("paquete.zip");
    std::io::copy(&mut lector, &mut std::fs::File::create(&zip)?)?;
    zip::ZipArchive::new(std::fs::File::open(&zip)?)?.extract(destino)?;
    std::fs::remove_file(&zip)?;
    Ok(())
}

struct Progreso<'a, R> {
    r: R,
    leido: u64,
    total: f32,
    que: &'a str,
    cb: &'a dyn Fn(&str, f32),
}

impl<R: Read> Read for Progreso<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.r.read(buf)?;
        let antes = self.leido;
        self.leido += n as u64;
        if self.total > 0.0 && self.leido >> 23 != antes >> 23 {
            (self.cb)(self.que, self.leido as f32 / self.total);
        }
        Ok(n)
    }
}

fn descarga<'a>(url: &str, que: &'a str, cb: &'a dyn Fn(&str, f32)) -> Result<Progreso<'a, impl Read + use<'a>>> {
    let resp = ureq::get(url).call().with_context(|| format!("descargando {url}"))?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok()).unwrap_or(0.0);
    Ok(Progreso { r: resp.into_body().into_reader(), leido: 0, total, que, cb })
}

/// Descarga en curso, para mostrarla en la ventana: (texto, fracción).
static PREPARANDO: Mutex<Option<(String, f32)>> = Mutex::new(None);

pub fn preparando() -> Option<(String, f32)> {
    PREPARANDO.lock().unwrap().clone()
}

/// Descarga en segundo plano; el progreso se consulta con `preparando()`.
pub fn preparar_en_fondo(clave: String, repintar: impl Fn() + Send + 'static) {
    if PREPARANDO.lock().unwrap().is_some() {
        return;
    }
    *PREPARANDO.lock().unwrap() = Some(("empezando…".into(), 0.0));
    std::thread::spawn(move || {
        let r = preparar(&clave, &|q, f| {
            *PREPARANDO.lock().unwrap() = Some((q.to_string(), f));
            repintar();
        });
        *PREPARANDO.lock().unwrap() = None;
        if let Err(e) = r {
            eprintln!("preparar modelo local: {e:#}");
        }
        repintar();
    });
}

// ── servidor ─────────────────────────────────────────────────────────────────

struct Servidor {
    hijo: Child,
    clave: String,
    puerto: u16,
}

static SERVIDOR: Mutex<Option<Servidor>> = Mutex::new(None);

/// GPU dedicada para Vulkan («Vulkan1: Quadro P1000…»): se evita la integrada, que es
/// lenta y comparte la RAM. Sin GPU dedicada, todo en CPU. En una Mac con Apple Silicon la
/// GPU es Metal («MTL0: Apple M2…») y sí conviene: comparte la memoria, pero es rápida.
fn elegir_dispositivo(bin: &Path) -> String {
    let out = crate::plataforma::sin_consola(&mut Command::new(bin)).arg("--list-devices").env("LD_LIBRARY_PATH", bin.parent().unwrap()).output();
    let texto = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)).unwrap_or_default();
    texto
        .lines()
        .filter_map(|l| l.trim().split_once(':'))
        .filter(|(d, _)| d.starts_with("Vulkan") || d.starts_with("MTL"))
        .find(|(_, nombre)| {
            let n = nombre.to_lowercase();
            !(n.contains("intel") || n.contains("llvmpipe") || n.contains("software"))
        })
        .map_or_else(|| "none".to_string(), |(d, _)| d.to_string())
}

fn puerto_libre() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn log(m: &str) {
    if std::env::var_os("IUREVOZ_LOG").is_some() {
        eprintln!("[iurevoz] llm local: {m}");
    }
}

/// URL base OpenAI-compatible del servidor con `clave` cargado; lo arranca si hace falta.
pub fn url(clave: &str) -> Result<String> {
    let mut s = SERVIDOR.lock().unwrap();
    if let Some(v) = s.as_mut() {
        let vivo = v.hijo.try_wait().ok().flatten().is_none();
        if vivo && v.clave == clave {
            return Ok(format!("http://127.0.0.1:{}/v1", v.puerto));
        }
        let _ = v.hijo.kill();
        let _ = v.hijo.wait();
        *s = None;
    }
    let (bin_ok, modelo_ok) = estado(clave);
    if !bin_ok || !modelo_ok {
        bail!("el modelo local no está descargado: prepáralo en Ajustes («Descargar»)");
    }
    let bin = ruta_binario();
    let dispositivo = elegir_dispositivo(&bin);
    let puerto = puerto_libre()?;
    log(&format!("arrancando llama-server con {clave} en {dispositivo}, puerto {puerto}"));
    let mut cmd = Command::new(&bin);
    cmd.env("LD_LIBRARY_PATH", bin.parent().unwrap())
        .arg("-m")
        .arg(ruta_modelo(clave)?)
        .args(["--host", "127.0.0.1", "--port", &puerto.to_string()])
        .args(["--device", &dispositivo, "-c", "8192", "--jinja", "--no-webui"])
        // sin cadena de razonamiento: respuestas cortas y rápidas
        .args(["--reasoning-budget", "0"])
        // duerme el modelo (libera RAM/VRAM) tras 10 min sin uso
        .args(["--sleep-idle-seconds", "600", "-np", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(if std::env::var_os("IUREVOZ_LOG").is_some() { Stdio::inherit() } else { Stdio::null() });
    // si la app muere, el servidor también (lanzado desde un hilo que dura lo que la app)
    let hijo = crate::procesos::lanzar(cmd).context("no pude arrancar llama-server")?;
    *s = Some(Servidor { hijo, clave: clave.to_string(), puerto });

    // espera a que cargue el modelo
    let t = Instant::now();
    let salud = format!("http://127.0.0.1:{puerto}/health");
    loop {
        if let Some(v) = s.as_mut() {
            if let Ok(Some(st)) = v.hijo.try_wait() {
                *s = None;
                bail!("llama-server terminó al arrancar ({st})");
            }
        }
        if ureq::get(&salud).call().is_ok() {
            log(&format!("listo en {:.1} s", t.elapsed().as_secs_f32()));
            return Ok(format!("http://127.0.0.1:{puerto}/v1"));
        }
        if t.elapsed() > Duration::from_secs(180) {
            bail!("llama-server no respondió en 3 minutos");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Detiene el servidor (al cerrar la app).
pub fn apagar() {
    if let Some(mut v) = SERVIDOR.lock().unwrap().take() {
        let _ = v.hijo.kill();
        let _ = v.hijo.wait();
    }
}
