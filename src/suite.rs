//! Apps de escritorio de la suite Iurefficient: IureDav (archivos), IureOCR (PDF),
//! IureTranscribe (grabar y transcribir) e IureEditor (abrir documentos).
//!
//! Dos vías:
//! - MCP por stdio (`<app> --mcp`): trabajo en segundo plano, sin ventana. El proceso se
//!   conserva para no recargar modelos en cada llamada.
//! - Ventana abierta: enlaces propios de cada app (`iuretranscribe://record/start`…), que le
//!   llegan a la instancia ya abierta.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iurefficient_connect::apps::{self, AppId};
use serde_json::{Value, json};

// ── cliente MCP por stdio ────────────────────────────────────────────────────

struct ClienteMcp {
    hijo: Child,
    entrada: ChildStdin,
    salida: BufReader<ChildStdout>,
    siguiente: u64,
}

impl ClienteMcp {
    fn arrancar(app: AppId) -> Result<Self> {
        let def = apps::def(app);
        let bin = apps::locate(def).with_context(|| format!("{} no está instalada", def.name))?;
        let mut cmd = Command::new(bin);
        cmd.arg("--mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        // si IureVoz muere, el servidor también
        let mut hijo = crate::procesos::lanzar(cmd).with_context(|| format!("no pude arrancar {} --mcp", def.name))?;
        let entrada = hijo.stdin.take().unwrap();
        let salida = BufReader::new(hijo.stdout.take().unwrap());
        let mut c = Self { hijo, entrada, salida, siguiente: 1 };
        c.pedir(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "iurevoz", "version": env!("CARGO_PKG_VERSION")}}),
            Duration::from_secs(30),
        )?;
        c.notificar("notifications/initialized")?;
        Ok(c)
    }

    fn notificar(&mut self, metodo: &str) -> Result<()> {
        writeln!(self.entrada, "{}", json!({"jsonrpc": "2.0", "method": metodo}))?;
        self.entrada.flush()?;
        Ok(())
    }

    /// Envía una petición y espera su respuesta (las notificaciones de progreso se ignoran).
    fn pedir(&mut self, metodo: &str, params: Value, espera: Duration) -> Result<Value> {
        let id = self.siguiente;
        self.siguiente += 1;
        writeln!(self.entrada, "{}", json!({"jsonrpc": "2.0", "id": id, "method": metodo, "params": params}))?;
        self.entrada.flush()?;
        let t = Instant::now();
        let mut linea = String::new();
        loop {
            linea.clear();
            if self.salida.read_line(&mut linea)? == 0 {
                bail!("el servidor MCP terminó");
            }
            let Ok(v) = serde_json::from_str::<Value>(&linea) else { continue };
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(e) = v.get("error") {
                    bail!("{}", e["message"].as_str().unwrap_or("error del servidor MCP"));
                }
                return Ok(v["result"].clone());
            }
            if t.elapsed() > espera {
                bail!("el servidor MCP no respondió a tiempo");
            }
        }
    }

    /// Llama una herramienta y devuelve su contenido de texto (como JSON si lo es).
    fn llamar(&mut self, nombre: &str, args: Value, espera: Duration) -> Result<Value> {
        let r = self.pedir("tools/call", json!({"name": nombre, "arguments": args}), espera)?;
        let texto: String = r["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if r["isError"].as_bool() == Some(true) {
            bail!("{}", texto);
        }
        Ok(serde_json::from_str(&texto).unwrap_or(Value::String(texto)))
    }
}

static CLIENTES: Mutex<Option<HashMap<&'static str, ClienteMcp>>> = Mutex::new(None);

fn clave(app: AppId) -> &'static str {
    match app {
        AppId::Transcribe => "transcribe",
        AppId::Ocr => "ocr",
        AppId::Editor => "editor",
        AppId::Dav => "dav",
    }
}

/// Llama una herramienta MCP de una app de la suite, arrancando su servidor si hace falta.
fn mcp(app: AppId, herramienta: &str, args: Value, espera: Duration) -> Result<Value> {
    let mut g = CLIENTES.lock().unwrap();
    let mapa = g.get_or_insert_with(HashMap::new);
    let k = clave(app);
    // proceso muerto: se vuelve a arrancar
    if mapa.get_mut(k).is_some_and(|c| c.hijo.try_wait().ok().flatten().is_some()) {
        mapa.remove(k);
    }
    if !mapa.contains_key(k) {
        mapa.insert(k, ClienteMcp::arrancar(app)?);
    }
    let r = mapa.get_mut(k).unwrap().llamar(herramienta, args, espera);
    if r.is_err() {
        // ante un error de transporte, el siguiente intento arranca limpio
        if let Some(mut c) = mapa.remove(k) {
            let _ = c.hijo.kill();
        }
    }
    r
}

// ── rutas ────────────────────────────────────────────────────────────────────

/// Unidades de IureDav montadas en este equipo.
pub fn unidades() -> Vec<apps::DavMount> {
    apps::iuredav_mounts().into_iter().filter(|m| m.mounted).collect()
}

/// Resuelve una ruta dicha por el usuario: absoluta, con «~», o relativa a la unidad de
/// IureDav. Solo se permite dentro de la carpeta personal o de una unidad de IureDav.
pub fn resolver(ruta: &str) -> Result<PathBuf> {
    let ruta = ruta.trim().trim_matches(['"', '\'']);
    let home = dirs::home_dir().context("sin carpeta personal")?;
    let candidata = if let Some(r) = ruta.strip_prefix("~/") {
        home.join(r)
    } else if Path::new(ruta).is_absolute() {
        PathBuf::from(ruta)
    } else {
        // relativa: primero en la unidad de IureDav, luego en la carpeta personal
        unidades().iter().map(|m| Path::new(&m.mount_point).join(ruta)).find(|p| p.exists()).unwrap_or_else(|| home.join(ruta))
    };
    let real = candidata.canonicalize().with_context(|| format!("no existe «{ruta}»"))?;
    let permitido = real.starts_with(&home) || unidades().iter().any(|m| real.starts_with(&m.mount_point));
    if !permitido {
        bail!("solo puedo usar archivos de tu carpeta personal o de las unidades de IureDav");
    }
    Ok(real)
}

fn normalizar(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            'ú' | 'ü' => 'u',
            c => c,
        })
        .collect()
}

// ── IureDav ──────────────────────────────────────────────────────────────────

/// Lista una carpeta de la unidad o busca archivos por nombre (todas las palabras).
pub fn archivos(carpeta: Option<&str>, buscar: Option<&str>) -> Result<String> {
    let us = unidades();
    let Some(raiz) = us.first() else {
        bail!("IureDav no tiene ninguna unidad montada: ábrela y monta la unidad");
    };
    let raiz = PathBuf::from(&raiz.mount_point);
    let base = match carpeta.filter(|c| !c.trim().is_empty()) {
        Some(c) => resolver(c)?,
        None => raiz.clone(),
    };
    let relativa = |p: &Path| p.strip_prefix(&raiz).unwrap_or(p).display().to_string();
    if let Some(q) = buscar.filter(|q| !q.trim().is_empty()) {
        let palabras: Vec<String> = normalizar(q).split_whitespace().map(str::to_string).collect();
        let base_txt = base.to_string_lossy().into_owned();
        let mut hallados: Vec<&PathBuf> = Vec::new();
        let idx = indice(&raiz);
        for p in idx.iter() {
            let n = normalizar(&p.file_name().unwrap_or_default().to_string_lossy()).replace(['_', '-', '.'], " ");
            if p.to_string_lossy().starts_with(&base_txt) && palabras.iter().all(|w| n.contains(w.as_str())) {
                hallados.push(p);
            }
        }
        // primero las carpetas reales; «Vistas» repite los mismos archivos ordenados de otra forma
        hallados.sort_by_key(|p| p.to_string_lossy().contains("/Vistas (solo navegar)/"));
        let mut vistos = std::collections::HashSet::new();
        hallados.retain(|p| vistos.insert(p.file_name().map(|n| n.to_os_string())));
        hallados.truncate(15);
        if hallados.is_empty() {
            return Ok(format!("no encontré archivos con «{q}»"));
        }
        return Ok(format!("encontré: {}", hallados.iter().map(|p| relativa(p)).collect::<Vec<_>>().join("; ")));
    }
    let mut v: Vec<String> = std::fs::read_dir(&base)?
        .flatten()
        .map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            if e.file_type().is_ok_and(|f| f.is_dir()) { format!("{n}/") } else { n }
        })
        .filter(|n| !n.starts_with('.'))
        .collect();
    v.sort();
    let total = v.len();
    v.truncate(40);
    Ok(format!("{} ({total} elementos): {}", relativa(&base), v.join("; ")))
}

/// Índice de nombres de la unidad. Recorrerla es lento (es de red: ~90 s para 800
/// archivos), así que se arma en segundo plano, se guarda en disco y se renueva cada 20 min.
static INDICE: Mutex<Option<(Instant, Vec<PathBuf>)>> = Mutex::new(None);
static INDEXANDO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const INDICE_DURA: Duration = Duration::from_secs(20 * 60);

fn ruta_indice() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("iurevoz").join("iuredav-indice.json")
}

fn recorrer(raiz: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut cola = std::collections::VecDeque::from([(raiz.to_path_buf(), 0usize)]);
    while let Some((dir, nivel)) = cola.pop_front() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if nivel < 8 && e.file_type().is_ok_and(|f| f.is_dir()) {
                cola.push_back((p.clone(), nivel + 1));
            }
            v.push(p);
        }
    }
    v
}

/// Arma (o renueva) el índice en segundo plano.
pub fn indexar_en_fondo() {
    let Some(raiz) = unidades().first().map(|m| PathBuf::from(&m.mount_point)) else { return };
    if INDEXANDO.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(move || {
        let v = recorrer(&raiz);
        if let Some(d) = ruta_indice().parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(ruta_indice(), serde_json::to_string(&v).unwrap_or_default());
        *INDICE.lock().unwrap() = Some((Instant::now(), v));
        INDEXANDO.store(false, std::sync::atomic::Ordering::Relaxed);
    });
}

/// Índice actual: el de memoria, o el guardado en disco; si está viejo se renueva aparte.
fn indice(raiz: &Path) -> Vec<PathBuf> {
    let mut g = INDICE.lock().unwrap();
    if g.is_none() {
        if let Some(v) = std::fs::read_to_string(ruta_indice()).ok().and_then(|t| serde_json::from_str::<Vec<PathBuf>>(&t).ok()) {
            // del disco: sirve ya, pero se considera viejo
            *g = Some((Instant::now() - INDICE_DURA, v));
        }
    }
    match g.as_ref() {
        Some((t, v)) => {
            if t.elapsed() > INDICE_DURA {
                // el hilo de renovación toma el candado cuando este se suelta
                indexar_en_fondo();
            }
            v.clone()
        }
        None => {
            // primera vez sin índice: se recorre ahora (lento) y se guarda
            drop(g);
            let v = recorrer(raiz);
            let _ = std::fs::create_dir_all(ruta_indice().parent().unwrap());
            let _ = std::fs::write(ruta_indice(), serde_json::to_string(&v).unwrap_or_default());
            *INDICE.lock().unwrap() = Some((Instant::now(), v.clone()));
            v
        }
    }
}

// ── abrir ────────────────────────────────────────────────────────────────────

pub fn abrir(ruta: &str, con_editor: bool) -> Result<String> {
    let p = resolver(ruta)?;
    crate::iure::recordar(crate::iure::Referencia::Archivo(p.clone()));
    if con_editor {
        apps::open_with(AppId::Editor, &p)?;
        return Ok(format!("abrí {} en IureEditor", p.file_name().unwrap_or_default().to_string_lossy()));
    }
    Command::new("gio").arg("open").arg(&p).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    Ok(format!("abrí {}", p.file_name().unwrap_or_default().to_string_lossy()))
}

/// ¿El IureEditor instalado acepta `iureditor://insert`? (después de la 1.11.2). La versión
/// sale del paquete; `IUREVOZ_EDITOR_INSERTA=1` lo fuerza para probar una compilación propia.
pub fn editor_inserta() -> bool {
    if std::env::var_os("IUREVOZ_EDITOR_INSERTA").is_some() {
        return true;
    }
    let Ok(out) = Command::new("dpkg-query").args(["-W", "-f=${Version}", "iureditor"]).output() else { return false };
    let v: Vec<u32> = String::from_utf8_lossy(&out.stdout).split(['.', '-', '~']).take(3).filter_map(|x| x.parse().ok()).collect();
    v.len() == 3 && (v[0], v[1], v[2]) > (1, 11, 2)
}

/// Inserta texto en el cursor de `doc` en IureEditor ya abierto (enlace `iureditor://insert`).
/// Se espera a que el proceso que pasa el enlace termine: así las frases llegan en orden.
pub fn insertar_en_editor(doc: &Path, texto: &str) -> Result<()> {
    let exe = apps::locate(apps::def(AppId::Editor)).context("IureEditor no está instalado")?;
    let enlace = format!("iureditor://insert?path={}&text={}", codificar(&doc.to_string_lossy()), codificar(texto));
    let st = Command::new(exe).arg(enlace).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()?;
    if !st.success() {
        bail!("no aceptó el texto ({st})");
    }
    Ok(())
}

/// Crea un Markdown vacío en Documentos y lo abre en IureEditor: así el dictado tiene
/// dónde caer y el archivo existe aunque el editor no se guarde solo.
pub fn nuevo_documento(titulo: Option<&str>) -> Result<String> {
    let dir = dirs::document_dir().or_else(dirs::home_dir).context("no encuentro la carpeta Documentos")?;
    let base = titulo
        .map(|t| t.trim().chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect::<String>())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("Dictado {}", chrono::Local::now().format("%Y-%m-%d %H.%M")));
    let mut ruta = dir.join(format!("{base}.md"));
    let mut n = 2;
    while ruta.exists() {
        ruta = dir.join(format!("{base} ({n}).md"));
        n += 1;
    }
    std::fs::write(&ruta, "")?;
    crate::iure::recordar(crate::iure::Referencia::Archivo(ruta.clone()));
    apps::open_with(AppId::Editor, &ruta)?;
    Ok(format!("creé «{}» en Documentos y lo abrí en IureEditor", ruta.file_name().unwrap_or_default().to_string_lossy()))
}

// ── leer ─────────────────────────────────────────────────────────────────────

const MAX_TEXTO: usize = 6000;

/// Texto de un archivo: txt/md directo; PDF con IureOCR (`extract_text`).
pub fn texto(ruta: &str, desde: Option<u64>, hasta: Option<u64>) -> Result<String> {
    let p = resolver(ruta)?;
    crate::iure::recordar(crate::iure::Referencia::Archivo(p.clone()));
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let t = match ext.as_str() {
        "pdf" => {
            let mut a = json!({"path": p, "max_chars": MAX_TEXTO});
            if let Some(d) = desde {
                a["start_page"] = json!(d);
            }
            if let Some(h) = hasta {
                a["end_page"] = json!(h);
            }
            let r = mcp(AppId::Ocr, "extract_text", a, Duration::from_secs(120))?;
            let texto = r["text"].as_str().unwrap_or("").to_string();
            if texto.trim().is_empty() {
                bail!("el PDF no tiene texto (está escaneado): pásale OCR primero");
            }
            texto
        }
        "txt" | "md" | "markdown" | "csv" | "srt" | "vtt" => std::fs::read_to_string(&p)?,
        e => bail!("no sé leer archivos .{e}"),
    };
    Ok(t.chars().take(MAX_TEXTO).collect())
}

// ── IureOCR ──────────────────────────────────────────────────────────────────

pub fn ocr(ruta: &str, idiomas: Option<&str>) -> Result<String> {
    let p = resolver(ruta)?;
    let mut a = json!({"path": p, "include_text": false});
    if let Some(i) = idiomas {
        a["languages"] = json!(i);
    }
    // un PDF largo puede tardar varios minutos
    let r = mcp(AppId::Ocr, "ocr_file", a, Duration::from_secs(1800))?;
    if r["skipped"].as_bool() == Some(true) {
        return Ok(format!("no hizo falta: {}", r["note"].as_str().unwrap_or("el PDF ya tiene texto")));
    }
    Ok(format!(
        "listo: {} ({} páginas) en {:.0} s",
        r["pdf_path"].as_str().map(|s| Path::new(s).file_name().unwrap_or_default().to_string_lossy().into_owned()).unwrap_or_default(),
        r["pages"],
        r["elapsed_secs"].as_f64().unwrap_or(0.0)
    ))
}

pub fn info_pdf(ruta: &str) -> Result<String> {
    let p = resolver(ruta)?;
    crate::iure::recordar(crate::iure::Referencia::Archivo(p.clone()));
    let r = mcp(AppId::Ocr, "pdf_info", json!({"path": p}), Duration::from_secs(60))?;
    Ok(format!(
        "{} páginas, {}",
        r["pages"],
        if r["hasTextLayer"].as_bool() == Some(true) { "con texto" } else { "escaneado (sin texto)" }
    ))
}

/// Describe una edición de páginas (para pedir confirmación) sin hacerla.
pub fn describir_edicion(ruta: &str, operacion: &str, paginas: &[u64], grados: Option<i64>) -> String {
    let nombre = Path::new(ruta).file_name().map_or(ruta.to_string(), |n| n.to_string_lossy().into_owned());
    let lista = paginas.iter().map(u64::to_string).collect::<Vec<_>>().join(", ");
    match operacion {
        "delete" => format!("quitar las páginas {lista} de {nombre} (en una copia)"),
        "keep" => format!("dejar solo las páginas {lista} de {nombre} (en una copia)"),
        "rotate" => format!("girar {} grados las páginas {lista} de {nombre} (en una copia)", grados.unwrap_or(90)),
        "reorder" => format!("reordenar {nombre} como {lista} (en una copia)"),
        o => format!("{o} en {nombre}"),
    }
}

pub fn editar_pdf(ruta: &str, operacion: &str, paginas: &[u64], grados: Option<i64>) -> Result<String> {
    let p = resolver(ruta)?;
    let mut a = json!({"path": p, "operation": operacion, "pages": paginas});
    if let Some(g) = grados {
        a["degrees"] = json!(g);
    }
    let r = mcp(AppId::Ocr, "edit_pdf_pages", a, Duration::from_secs(300))?;
    let nuevo = r["path"].as_str().map(|s| Path::new(s).file_name().unwrap_or_default().to_string_lossy().into_owned()).unwrap_or_default();
    Ok(format!("guardé {nuevo} ({} páginas)", r["pages"]))
}

// ── IureTranscribe ───────────────────────────────────────────────────────────

fn codificar(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b',' => (b as char).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// Empieza a grabar en la ventana de IureTranscribe (requiere la versión con `record/start`).
pub fn grabar(participantes: Option<&str>, lugar: Option<&str>, notas: Option<&str>) -> Result<String> {
    let mut q = Vec::new();
    if let Some(p) = participantes.filter(|s| !s.trim().is_empty()) {
        q.push(format!("participants={}", codificar(p)));
    }
    if let Some(l) = lugar.filter(|s| !s.trim().is_empty()) {
        q.push(format!("place={}", codificar(l)));
    }
    if let Some(n) = notas.filter(|s| !s.trim().is_empty()) {
        q.push(format!("notes={}", codificar(n)));
    }
    let url = format!("iuretranscribe://record/start{}{}", if q.is_empty() { "" } else { "?" }, q.join("&"));
    apps::launch(AppId::Transcribe, &[url])?;
    Ok("pedí a IureTranscribe que empiece a grabar".into())
}

pub fn detener_grabacion() -> Result<String> {
    apps::launch(AppId::Transcribe, &["iuretranscribe://record/stop".to_string()])?;
    Ok("pedí a IureTranscribe que detenga y guarde la grabación".into())
}

pub fn grabaciones(limite: u64) -> Result<String> {
    let r = mcp(AppId::Transcribe, "list_recordings", json!({"limit": limite.clamp(1, 20)}), Duration::from_secs(30))?;
    Ok(r.to_string().chars().take(3000).collect())
}

/// Transcribe un archivo; si tarda, sigue preguntando hasta 10 min.
pub fn transcribir(ruta: &str) -> Result<String> {
    let p = resolver(ruta)?;
    let mut r = mcp(AppId::Transcribe, "transcribe_file", json!({"path": p, "wait_seconds": 100, "max_chars": MAX_TEXTO}), Duration::from_secs(130))?;
    let t = Instant::now();
    while let Some(job) = r["jobId"].as_str().map(str::to_string) {
        if t.elapsed() > Duration::from_secs(600) {
            bail!("la transcripción sigue en curso; pregúntame más tarde");
        }
        r = mcp(AppId::Transcribe, "transcription_status", json!({"job_id": job, "wait_seconds": 100, "max_chars": MAX_TEXTO}), Duration::from_secs(130))?;
    }
    let texto = r["text"].as_str().map(str::to_string).unwrap_or_else(|| r.to_string());
    Ok(texto.chars().take(MAX_TEXTO).collect())
}

pub fn leer_transcripcion(ruta: &str) -> Result<String> {
    let p = resolver(ruta)?;
    let r = mcp(AppId::Transcribe, "read_transcript", json!({"path": p, "max_chars": MAX_TEXTO}), Duration::from_secs(60))?;
    Ok(r["text"].as_str().map(str::to_string).unwrap_or_else(|| r.to_string()).chars().take(MAX_TEXTO).collect())
}

/// Apaga los servidores MCP (al cerrar la app).
pub fn apagar() {
    if let Some(mapa) = CLIENTES.lock().unwrap().take() {
        for (_, mut c) in mapa {
            let _ = c.hijo.kill();
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn rutas_fuera_de_casa_no() {
        assert!(resolver("/etc/passwd").is_err());
        assert!(resolver("~/").is_ok());
    }

    #[test]
    fn busqueda_con_indice() {
        // necesita la unidad de IureDav montada; si no está, no prueba nada
        if unidades().is_empty() {
            return;
        }
        let r = archivos(None, Some("flujo despacho heralva")).unwrap();
        assert!(r.contains("Flujo_despacho_Heralva"), "{r}");
        assert!(!r.starts_with("encontré: Vistas"), "las carpetas reales van primero: {r}");
    }

    #[test]
    fn enlaces_codificados() {
        assert_eq!(codificar("Pérez, García"), "P%C3%A9rez,%20Garc%C3%ADa");
    }
}

#[cfg(test)]
mod pruebas_editor {
    #[test]
    fn codifica_enlaces() {
        assert_eq!(super::codificar("Hola, mundo.\n"), "Hola,%20mundo.%0A");
        assert_eq!(super::codificar("/home/a/Dictado 1.md"), "%2Fhome%2Fa%2FDictado%201.md");
        assert_eq!(super::codificar("año"), "a%C3%B1o");
    }
}
