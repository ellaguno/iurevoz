//! macOS: teclear en la ventana activa con eventos de Quartz (`CGEventPost`) y atajos globales
//! con Carbon (crate `global-hotkey`). Misma interfaz que el portal de GNOME (`portal.rs`).
//!
//! El texto va como caracteres Unicode (`CGEventKeyboardSetUnicodeString`), así que no depende
//! de la distribución de teclado. Teclear necesita el permiso de Accesibilidad (Ajustes del
//! Sistema → Privacidad y seguridad); macOS lo pide la primera vez. Las combinaciones llegan
//! como keysyms de X11 y se traducen a teclas de Mac: Ctrl es ⌘ (Ctrl+C del resto de la app
//! es copiar), Alt es ⌥ y Super es ⌃.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, unbounded};

/// Atajos globales: (id, descripción, combinación). Los fija IureVoz al arrancar.
pub const ATAJOS: &[(&str, &str, &str)] = &[
    ("dictar", "Dictar / detener", "⌃⌥Espacio"),
    ("comando", "Dar una instrucción", "⌃⌥C"),
    ("leer", "Leer la selección en voz alta / callar", "⌃⌥L"),
];

#[derive(Debug)]
#[allow(dead_code)] // `TokenTeclado` solo existe en el portal de GNOME
pub enum Aviso {
    /// Un atajo global se activó (pulsado) o se soltó.
    Atajo { id: String, pulsado: bool },
    /// (id, combinación asignada).
    AtajoAsignado(String, String),
    /// Token para que el permiso de teclear no se vuelva a pedir (no aplica en macOS).
    TokenTeclado(String),
    Error(String),
}

enum Pedido {
    Escribir(String),
    /// Combinación: se pulsan en orden y se sueltan al revés.
    Teclas(Vec<u32>),
}

#[derive(Clone)]
pub struct Portal {
    tx: Sender<Pedido>,
}

impl Portal {
    /// Arranca el hilo que teclea y, con `atajo`, los atajos globales.
    pub fn iniciar(_token: Option<String>, atajo: bool, avisar: impl Fn(Aviso) + Send + Sync + 'static) -> Self {
        let (tx, rx) = unbounded::<Pedido>();
        let avisar = std::sync::Arc::new(avisar);
        if atajo {
            if !permiso_para_teclear(true) {
                avisar(Aviso::Error(
                    "Para escribir lo dictado, permite IureVoz en Ajustes del Sistema → Privacidad y seguridad → Accesibilidad y vuelve a abrirlo".into(),
                ));
            }
            atajos_globales(avisar.clone());
        }
        std::thread::Builder::new()
            .name("teclado".into())
            .spawn(move || {
                for p in rx {
                    match p {
                        Pedido::Escribir(t) => teclear(&t),
                        Pedido::Teclas(ks) => pulsar(&ks),
                    }
                }
            })
            .expect("hilo de teclado");
        Self { tx }
    }

    /// Encola texto para teclear en la ventana activa.
    pub fn escribir(&self, texto: String) {
        let _ = self.tx.send(Pedido::Escribir(texto));
    }

    /// Pulsa una combinación de teclas (keysyms) en la ventana activa.
    pub fn teclas(&self, keysyms: Vec<u32>) {
        let _ = self.tx.send(Pedido::Teclas(keysyms));
    }
}

/// Los atajos de Carbon se registran en el hilo principal (su bucle de eventos los entrega);
/// el hilo de la app los lee del canal de `global-hotkey`.
fn atajos_globales(avisar: std::sync::Arc<dyn Fn(Aviso) + Send + Sync>) {
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

    let teclas = [Code::Space, Code::KeyC, Code::KeyL];
    let atajos: Vec<(u32, &str)> =
        ATAJOS.iter().zip(teclas).map(|((id, ..), c)| (HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), c).id(), *id)).collect();
    let a = avisar.clone();
    dispatch2::DispatchQueue::main().exec_async(move || {
        let gestor = match GlobalHotKeyManager::new() {
            Ok(g) => g,
            Err(e) => return a(Aviso::Error(format!("atajos globales: {e}"))),
        };
        for ((id, _, combinacion), c) in ATAJOS.iter().zip(teclas) {
            match gestor.register(HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), c)) {
                Ok(()) => a(Aviso::AtajoAsignado(id.to_string(), combinacion.to_string())),
                Err(_) => a(Aviso::Error(format!("{combinacion} ya lo usa otra aplicación; ese atajo no funcionará"))),
            }
        }
        // el gestor vive lo que la app: al soltarlo se quitarían los atajos
        std::mem::forget(gestor);
    });
    std::thread::Builder::new()
        .name("atajos".into())
        .spawn(move || {
            for ev in GlobalHotKeyEvent::receiver().iter() {
                if let Some((_, id)) = atajos.iter().find(|(h, _)| *h == ev.id) {
                    avisar(Aviso::Atajo { id: id.to_string(), pulsado: ev.state == HotKeyState::Pressed });
                }
            }
        })
        .expect("hilo de atajos");
}

// ── Quartz (ApplicationServices) ─────────────────────────────────────────────

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CFTypeRef = *const c_void;

const ORIGEN_HID: i32 = 1; // kCGEventSourceStateHIDSystemState
const ESTADO_COMBINADO: i32 = 0; // kCGEventSourceStateCombinedSessionState
const TAP_HID: u32 = 0; // kCGHIDEventTap

const MAYUS: u64 = 0x0002_0000;
const CONTROL: u64 = 0x0004_0000;
const OPCION: u64 = 0x0008_0000;
const COMANDO: u64 = 0x0010_0000;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(estado: i32) -> CGEventSourceRef;
    fn CGEventCreateKeyboardEvent(origen: CGEventSourceRef, tecla: u16, abajo: bool) -> CGEventRef;
    fn CGEventKeyboardSetUnicodeString(ev: CGEventRef, largo: usize, texto: *const u16);
    fn CGEventSetFlags(ev: CGEventRef, flags: u64);
    fn CGEventPost(tap: u32, ev: CGEventRef);
    fn CGEventSourceFlagsState(estado: i32) -> u64;
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
}

/// ¿Puede IureVoz mandar teclas? Con `pedir`, macOS muestra su aviso (una vez) y agrega
/// IureVoz a la lista de Accesibilidad.
pub fn permiso_para_teclear(pedir: bool) -> bool {
    unsafe { CGPreflightPostEventAccess() || (pedir && CGRequestPostEventAccess()) }
}

/// Fuente de eventos compartida (como dirección: el puntero no es `Send`).
fn origen() -> CGEventSourceRef {
    static O: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *O.get_or_init(|| unsafe { CGEventSourceCreate(ORIGEN_HID) } as usize) as CGEventSourceRef
}

/// Manda una tecla (abajo o arriba) con los modificadores `flags` y, si hay, el texto que produce.
fn evento(tecla: u16, abajo: bool, flags: u64, texto: Option<&[u16]>) {
    unsafe {
        let ev = CGEventCreateKeyboardEvent(origen(), tecla, abajo);
        if ev.is_null() {
            return;
        }
        CGEventSetFlags(ev, flags);
        if let Some(t) = texto {
            CGEventKeyboardSetUnicodeString(ev, t.len(), t.as_ptr());
        }
        CGEventPost(TAP_HID, ev);
        CFRelease(ev as CFTypeRef);
    }
}

/// Con ⌃ o ⌥ aún pulsados (el atajo que lanzó la orden), cada letra sería un atajo de la
/// ventana activa: se espera a que se suelten, hasta un segundo.
fn esperar_modificadores() {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(1) && unsafe { CGEventSourceFlagsState(ESTADO_COMBINADO) } & (MAYUS | CONTROL | OPCION | COMANDO) != 0 {
        std::thread::sleep(Duration::from_millis(20));
    }
}

const RETORNO: u16 = 0x24;
const TABULADOR: u16 = 0x30;

fn teclear(texto: &str) {
    esperar_modificadores();
    for c in texto.chars() {
        match c {
            '\n' => {
                evento(RETORNO, true, 0, None);
                evento(RETORNO, false, 0, None);
            }
            '\t' => {
                evento(TABULADOR, true, 0, None);
                evento(TABULADOR, false, 0, None);
            }
            c => {
                let mut b = [0u16; 2];
                let u = c.encode_utf16(&mut b);
                evento(0, true, 0, Some(u));
                evento(0, false, 0, Some(u));
            }
        }
        // sin pausa algunas apps (terminales, Electron) pierden o reordenan teclas
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Keysym de X11 → (tecla de Mac, modificador que representa o 0).
fn tecla_de(ks: u32) -> Option<(u16, u64)> {
    let v = match ks {
        0xffe3 | 0xffe4 => (0x37, COMANDO), // Ctrl → ⌘
        0xffe1 | 0xffe2 => (0x38, MAYUS),
        0xffe9 | 0xffea | 0xfe03 => (0x3a, OPCION),
        0xffeb | 0xffec => (0x3b, CONTROL), // Super → ⌃
        0xff09 => (TABULADOR, 0),
        0xff0d => (RETORNO, 0),
        0xff1b => (0x35, 0),
        0xff08 => (0x33, 0),
        0xffff => (0x75, 0),
        0xff50 => (0x73, 0),
        0xff57 => (0x77, 0),
        0xff55 => (0x74, 0),
        0xff56 => (0x79, 0),
        0xff51 => (0x7b, 0),
        0xff53 => (0x7c, 0),
        0xff54 => (0x7d, 0),
        0xff52 => (0x7e, 0),
        0x20 => (0x31, 0),
        f @ 0xffbe..=0xffc9 => ([0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f][(f - 0xffbe) as usize], 0),
        c => (tecla_ansi(char::from_u32(c)?.to_ascii_lowercase())?, 0),
    };
    Some(v)
}

/// Posición de la tecla en un teclado ANSI (las letras coinciden con el latinoamericano y el
/// español; para atajos basta).
fn tecla_ansi(c: char) -> Option<u16> {
    const LETRAS: [u16; 26] = [
        0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f, 0x23, 0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d,
        0x07, 0x10, 0x06,
    ];
    const DIGITOS: [u16; 10] = [0x1d, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19];
    Some(match c {
        'a'..='z' => LETRAS[c as usize - 'a' as usize],
        '0'..='9' => DIGITOS[c as usize - '0' as usize],
        '-' => 0x1b,
        '=' | '+' => 0x18,
        '[' => 0x21,
        ']' => 0x1e,
        ';' => 0x29,
        '\'' => 0x27,
        ',' => 0x2b,
        '.' => 0x2f,
        '/' => 0x2c,
        '\\' => 0x2a,
        '`' => 0x32,
        _ => return None,
    })
}

/// Pulsa una combinación (keysyms) en la ventana activa: en orden y se sueltan al revés.
pub fn pulsar(ks: &[u32]) {
    esperar_modificadores();
    let teclas: Vec<_> = ks.iter().filter_map(|&k| tecla_de(k)).collect();
    if teclas.len() != ks.len() {
        eprintln!("combinación con teclas desconocidas en macOS: {ks:x?}");
        return;
    }
    let mut flags = 0;
    for &(t, m) in &teclas {
        flags |= m;
        evento(t, true, flags, None);
        std::thread::sleep(Duration::from_millis(10));
    }
    for &(t, m) in teclas.iter().rev() {
        evento(t, false, flags, None);
        flags &= !m;
    }
}
