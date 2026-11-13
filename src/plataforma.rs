//! Lo que cambia entre Linux, Windows y macOS y no merece un módulo propio: abrir archivos y
//! enlaces, notificaciones, procesos sin consola, el portapapeles y el volumen.

use std::ffi::OsStr;
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;

use anyhow::{Context, Result};
#[cfg(not(target_os = "linux"))]
use anyhow::bail;

/// Abre un archivo, carpeta o URL con la aplicación predeterminada, sin esperar.
#[cfg(target_os = "linux")]
pub fn abrir(que: impl AsRef<OsStr>) -> Result<()> {
    Command::new("gio")
        .arg("open")
        .arg(que.as_ref())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("no pude ejecutar gio")?;
    Ok(())
}

#[cfg(windows)]
pub fn abrir(que: impl AsRef<OsStr>) -> Result<()> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, w};
    let que = HSTRING::from(que.as_ref());
    // ShellExecute devuelve un valor > 32 si lo abrió
    let r = unsafe { ShellExecuteW(None, w!("open"), &que, None, None, SW_SHOWNORMAL) };
    if r.0 as isize <= 32 {
        bail!("Windows no pudo abrir {que} ({})", r.0 as isize);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn abrir(que: impl AsRef<OsStr>) -> Result<()> {
    Command::new("open")
        .arg(que.as_ref())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("no pude ejecutar open")?;
    Ok(())
}

/// Notificación de escritorio: llega aunque la ventana esté oculta.
#[cfg(target_os = "linux")]
pub fn notificar(titulo: &str, cuerpo: &str, icono: &str) {
    let cuerpo: String = cuerpo.chars().take(300).collect();
    let _ = Command::new("notify-send")
        .args(["--app-name=IureVoz", &format!("--icon={icono}"), titulo, &cuerpo])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// En Windows es un aviso del Centro de notificaciones (WinRT). Windows solo lo muestra si
/// el AppUserModelID tiene un acceso directo en el menú Inicio: el del instalador lleva el
/// de IureVoz; sin él (una compilación de desarrollo) se usa el de PowerShell.
#[cfg(windows)]
pub fn notificar(titulo: &str, cuerpo: &str, _icono: &str) {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::core::HSTRING;
    const POWERSHELL: &str = r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe";
    let instalada = dirs::data_dir().is_some_and(|d| d.join(r"Microsoft\Windows\Start Menu\Programs\IureVoz.lnk").exists());
    let aumid = if instalada { crate::config::APP_ID } else { POWERSHELL };
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let cuerpo: String = cuerpo.chars().take(300).collect();
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        esc(titulo),
        esc(&cuerpo)
    );
    let r = (|| -> windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml))?;
        let aviso = ToastNotification::CreateToastNotification(&doc)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))?.Show(&aviso)
    })();
    if let Err(e) = r {
        eprintln!("notificación: {e}");
    }
}

/// En macOS, con `display notification` de AppleScript (sale a nombre del Editor de Scripts).
#[cfg(target_os = "macos")]
pub fn notificar(titulo: &str, cuerpo: &str, _icono: &str) {
    let cuerpo: String = cuerpo.chars().take(300).collect();
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let guion = format!("display notification \"{}\" with title \"{}\"", esc(&cuerpo), esc(titulo));
    let _ = Command::new("osascript").args(["-e", &guion]).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// Un programa de consola lanzado desde la ventana no debe abrir otra ventana negra.
pub fn sin_consola(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// `canonicalize` sin el prefijo `\\?\` que le pone Windows: así las rutas se comparan con
/// `starts_with` contra la carpeta personal y las entienden las demás apps.
pub fn canonica(p: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let r = p.canonicalize()?;
    #[cfg(windows)]
    {
        let s = r.to_string_lossy();
        if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
            return Ok(format!(r"\\{unc}").into());
        }
        if let Some(local) = s.strip_prefix(r"\\?\") {
            return Ok(local.into());
        }
    }
    Ok(r)
}

/// ¿Hay un proceso con este nombre exacto? (p. ej. «iureditor» ya abierto)
#[cfg(unix)]
pub fn corre(nombre: &str) -> bool {
    Command::new("pgrep").args(["-x", nombre]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

#[cfg(windows)]
pub fn corre(nombre: &str) -> bool {
    let exe = format!("{nombre}.exe");
    let out = sin_consola(Command::new("tasklist").args(["/FI", &format!("IMAGENAME eq {exe}"), "/NH", "/FO", "CSV"])).output();
    out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains(&format!("\"{}\"", exe.to_lowercase())))
}

/// El programa `claude` (Claude Code). En Windows puede ser `claude.exe` (instalador nativo)
/// o `claude.cmd` (npm); `Command` solo encuentra los .exe.
pub fn claude() -> Command {
    #[cfg(windows)]
    {
        let en_path = |n: &str| std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(n).is_file()));
        let local = dirs::home_dir().map(|h| h.join(".local").join("bin").join("claude.exe")).filter(|p| p.is_file());
        let mut cmd = match local {
            Some(p) if !en_path("claude.exe") => Command::new(p),
            _ if !en_path("claude.exe") && en_path("claude.cmd") => Command::new("claude.cmd"),
            _ => Command::new("claude"),
        };
        sin_consola(&mut cmd);
        cmd
    }
    #[cfg(not(windows))]
    Command::new("claude")
}

// ── macOS ────────────────────────────────────────────────────────────────────

/// Abierta desde Finder, una app recibe un PATH mínimo (`/usr/bin:/bin:/usr/sbin:/sbin`) y no
/// encontraría `claude` ni lo instalado con Homebrew. Se agregan las carpetas de costumbre.
/// Va antes de crear hilos: cambiar el entorno con hilos corriendo no es seguro.
#[cfg(target_os = "macos")]
pub fn completar_path() {
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    let home = dirs::home_dir().unwrap_or_default();
    for d in [home.join(".local/bin"), home.join(".claude/local"), "/opt/homebrew/bin".into(), "/usr/local/bin".into()] {
        if d.is_dir() && !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    if let Ok(p) = std::env::join_paths(dirs) {
        unsafe { std::env::set_var("PATH", p) };
    }
}

/// `pbpaste` y `pbcopy` hablan en la codificación del entorno: se pide UTF-8.
#[cfg(target_os = "macos")]
fn pb(prog: &str) -> Command {
    let mut c = Command::new(prog);
    c.env("LANG", "es_MX.UTF-8").env("LC_ALL", "es_MX.UTF-8");
    c
}

/// Texto del portapapeles de macOS.
#[cfg(target_os = "macos")]
pub fn leer_portapapeles() -> Option<String> {
    let out = pb("pbpaste").stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Deja `texto` en el portapapeles (para devolver lo que había antes de copiar la selección).
#[cfg(target_os = "macos")]
pub fn escribir_portapapeles(texto: &str) {
    use std::io::Write;
    let Ok(mut hijo) = pb("pbcopy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { return };
    if let Some(mut e) = hijo.stdin.take() {
        let _ = e.write_all(texto.as_bytes());
    }
    let _ = hijo.wait();
}

/// Volumen de la salida con AppleScript. `subir` en fracción (0.1 = 10 %).
#[cfg(target_os = "macos")]
pub fn volumen(subir: Option<f32>, silenciar: Option<bool>) -> Result<()> {
    let mut guion = String::new();
    if let Some(d) = subir {
        let d = (d * 100.0).round() as i32;
        guion.push_str(&format!("set v to (output volume of (get volume settings)) + ({d})\n"));
        guion.push_str("if v < 0 then set v to 0\nif v > 100 then set v to 100\nset volume output volume v\n");
        if d > 0 {
            guion.push_str("set volume without output muted\n");
        }
    }
    if let Some(m) = silenciar {
        guion.push_str(if m { "set volume with output muted\n" } else { "set volume without output muted\n" });
    }
    let out = Command::new("osascript").args(["-e", &guion]).output().context("no pude ejecutar osascript")?;
    if !out.status.success() {
        bail!("osascript: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Apps de macOS (los paquetes `.app` de las carpetas de Aplicaciones): (nombre, ruta).
#[cfg(target_os = "macos")]
pub fn paquetes_de_aplicaciones() -> Vec<(String, std::path::PathBuf)> {
    let mut raices: Vec<std::path::PathBuf> =
        ["/Applications", "/Applications/Utilities", "/System/Applications", "/System/Applications/Utilities"].map(Into::into).to_vec();
    if let Some(h) = dirs::home_dir() {
        raices.insert(0, h.join("Applications"));
    }
    let mut v = Vec::new();
    for d in raices {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "app") {
                let nombre = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                v.push((nombre, p));
            } else if p.is_dir() && d.starts_with("/Applications") && !p.ends_with("Utilities") {
                // carpetas como «Microsoft Office» o «Adobe …» traen sus apps adentro
                if let Ok(sub) = std::fs::read_dir(&p) {
                    for e in sub.flatten() {
                        let q = e.path();
                        if q.extension().is_some_and(|x| x == "app") {
                            v.push((q.file_stem().unwrap_or_default().to_string_lossy().into_owned(), q));
                        }
                    }
                }
            }
        }
    }
    v
}

// ── Windows ──────────────────────────────────────────────────────────────────

/// Lanzado desde una terminal, el .exe (subsistema de ventanas) no tiene consola: se toma la
/// de quien lo lanzó para que los subcomandos impriman. Con la salida redirigida (como con
/// `--mcp`) no se toca nada.
#[cfg(windows)]
pub fn usar_consola_del_padre() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE};
    let redirigida = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.is_ok_and(|h| !h.is_invalid() && !h.0.is_null());
    if !redirigida {
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
}

/// Manda la salida de error a un archivo (el registro de diagnóstico).
#[cfg(windows)]
pub fn error_a_archivo(f: std::fs::File) {
    use std::os::windows::io::IntoRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
    // la salida estándar de Rust pide el handle en cada escritura, así que basta con cambiarlo
    let h = HANDLE(f.into_raw_handle());
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
        let _ = SetStdHandle(STD_ERROR_HANDLE, h);
    }
}

/// Segundos de CPU (usuario + sistema) que lleva el proceso.
#[cfg(windows)]
pub fn cpu_del_proceso() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut a, mut b, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u) }.is_err() {
        return 0.0;
    }
    let s = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
    s(k) + s(u)
}

#[cfg(unix)]
pub fn cpu_del_proceso() -> f64 {
    let mut u = unsafe { std::mem::zeroed::<libc::rusage>() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 / 1e6;
    t(u.ru_utime) + t(u.ru_stime)
}

/// Texto del portapapeles de Windows.
#[cfg(windows)]
pub fn leer_portapapeles() -> Option<String> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    unsafe {
        abrir_portapapeles(|| OpenClipboard(None).is_ok())?;
        let texto = (|| {
            let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(g);
            Some(s)
        })();
        let _ = CloseClipboard();
        texto
    }
}

/// Deja `texto` en el portapapeles (para devolver lo que había antes de copiar la selección).
#[cfg(windows)]
pub fn escribir_portapapeles(texto: &str) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    let w: Vec<u16> = texto.encode_utf16().chain(Some(0)).collect();
    unsafe {
        if abrir_portapapeles(|| OpenClipboard(None).is_ok()).is_none() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let p = GlobalLock(g) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(g);
                // el portapapeles se queda con la memoria
                let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(g.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

/// Otra app puede tener el portapapeles abierto un momento: se reintenta.
#[cfg(windows)]
fn abrir_portapapeles(abrir: impl Fn() -> bool) -> Option<()> {
    for _ in 0..10 {
        if abrir() {
            return Some(());
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    None
}

/// Texto seleccionado en la ventana activa: Windows y macOS no tienen selección primaria, así
/// que se copia con Ctrl+C (⌘C en Mac) y se devuelve el portapapeles como estaba.
#[cfg(not(target_os = "linux"))]
pub fn seleccion() -> Result<String> {
    let antes = leer_portapapeles();
    // marca para saber si Ctrl+C copió algo
    escribir_portapapeles("");
    crate::portal::pulsar(&[0xffe3, 'c' as u32]);
    let t = std::time::Instant::now();
    let mut copiado = None;
    while t.elapsed() < std::time::Duration::from_millis(800) {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Some(s) = leer_portapapeles().filter(|s| !s.trim().is_empty()) {
            copiado = Some(s);
            break;
        }
    }
    escribir_portapapeles(antes.as_deref().unwrap_or(""));
    match copiado.or(antes.filter(|s| !s.trim().is_empty())) {
        Some(s) => Ok(s),
        None => bail!("no hay texto seleccionado ni en el portapapeles"),
    }
}

/// Volumen del dispositivo de salida predeterminado. `cambio` en fracción (0.1 = 10 %).
#[cfg(windows)]
pub fn volumen(subir: Option<f32>, silenciar: Option<bool>) -> Result<()> {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender};
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).context("audio de Windows")?;
        let disp = en.GetDefaultAudioEndpoint(eRender, eConsole).context("no hay salida de audio")?;
        let vol: IAudioEndpointVolume = disp.Activate(CLSCTX_ALL, None)?;
        if let Some(d) = subir {
            let v = (vol.GetMasterVolumeLevelScalar()? + d).clamp(0.0, 1.0);
            vol.SetMasterVolumeLevelScalar(v, std::ptr::null())?;
            if d > 0.0 {
                vol.SetMute(false, std::ptr::null())?;
            }
        }
        if let Some(m) = silenciar {
            vol.SetMute(m, std::ptr::null())?;
        }
    }
    Ok(())
}

/// Accesos directos del menú Inicio (los del usuario primero): (nombre, ruta del .lnk).
#[cfg(windows)]
pub fn accesos_menu_inicio() -> Vec<(String, std::path::PathBuf)> {
    let mut raices = Vec::new();
    if let Some(d) = dirs::data_dir() {
        raices.push(d.join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Some(d) = std::env::var_os("ProgramData") {
        raices.push(std::path::PathBuf::from(d).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    let mut v = Vec::new();
    let mut pila = raices;
    while let Some(d) = pila.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                pila.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
                let nombre = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                // desinstaladores y ayudas no son apps que alguien pida abrir
                let n = nombre.to_lowercase();
                if ["uninstall", "desinstalar", "readme", "léame", "help", "ayuda"].iter().any(|x| n.contains(x)) {
                    continue;
                }
                v.push((nombre, p));
            }
        }
    }
    v
}

#[cfg(test)]
mod pruebas {
    #[test]
    fn cpu_avanza() {
        let a = super::cpu_del_proceso();
        let mut x = 0u64;
        for i in 0..20_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        std::hint::black_box(x);
        assert!(super::cpu_del_proceso() >= a);
    }
}
