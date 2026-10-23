//! Windows: teclear en la ventana activa con `SendInput` y atajos globales con
//! `RegisterHotKey`. Misma interfaz que el portal de GNOME (`portal.rs`).
//!
//! El texto va como caracteres Unicode (`KEYEVENTF_UNICODE`), así que no depende de la
//! distribución de teclado: «á», «ñ» y «¿» llegan sin teclas muertas. Las combinaciones
//! llegan como keysyms de X11 (las usa el resto de la app) y aquí se traducen a teclas
//! virtuales.

use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, unbounded};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

/// Atajos globales: (id, descripción, combinación). Los fija IureVoz al arrancar.
pub const ATAJOS: &[(&str, &str, &str)] = &[
    ("dictar", "Dictar / detener", "Ctrl+Alt+Espacio"),
    ("comando", "Dar una instrucción", "Ctrl+Alt+C"),
    ("leer", "Leer la selección en voz alta / callar", "Ctrl+Alt+L"),
];

#[derive(Debug)]
#[allow(dead_code)] // `TokenTeclado` solo existe en el portal de GNOME
pub enum Aviso {
    /// Un atajo global se activó (pulsado) o se soltó.
    Atajo { id: String, pulsado: bool },
    /// (id, combinación asignada).
    AtajoAsignado(String, String),
    /// Token para que el permiso de teclear no se vuelva a pedir (no aplica en Windows).
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
    /// Arranca el hilo que teclea y, con `atajo`, el de los atajos globales.
    pub fn iniciar(_token: Option<String>, atajo: bool, avisar: impl Fn(Aviso) + Send + Sync + 'static) -> Self {
        let (tx, rx) = unbounded::<Pedido>();
        let avisar = std::sync::Arc::new(avisar);
        if atajo {
            let a = avisar.clone();
            std::thread::Builder::new().name("atajos".into()).spawn(move || atajos_globales(&*a)).expect("hilo de atajos");
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

/// Los atajos se registran en este hilo y sus mensajes llegan a su cola.
fn atajos_globales(avisar: &(dyn Fn(Aviso) + Send + Sync)) {
    let teclas = [VK_SPACE, VIRTUAL_KEY('C' as u16), VIRTUAL_KEY('L' as u16)];
    for (i, ((id, _, combinacion), vk)) in ATAJOS.iter().zip(teclas).enumerate() {
        match unsafe { RegisterHotKey(None, i as i32 + 1, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, vk.0 as u32) } {
            Ok(()) => avisar(Aviso::AtajoAsignado(id.to_string(), combinacion.to_string())),
            Err(_) => avisar(Aviso::Error(format!("{combinacion} ya lo usa otra aplicación; ese atajo no funcionará"))),
        }
    }
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        if msg.message == WM_HOTKEY {
            if let Some((id, ..)) = ATAJOS.get(msg.wParam.0.wrapping_sub(1)) {
                avisar(Aviso::Atajo { id: id.to_string(), pulsado: true });
            }
        }
    }
}

/// Con Ctrl o Alt aún pulsados (el atajo que lanzó la orden), cada letra sería un atajo de
/// la ventana activa: se espera a que se suelten, hasta un segundo.
fn esperar_modificadores() {
    let t = Instant::now();
    let pulsada = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000 != 0;
    while t.elapsed() < Duration::from_secs(1) && [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN].into_iter().any(pulsada) {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn evento(vk: VIRTUAL_KEY, unidad: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: unidad, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn enviar(eventos: &[INPUT]) {
    unsafe { SendInput(eventos, std::mem::size_of::<INPUT>() as i32) };
}

fn tecla_virtual(vk: VIRTUAL_KEY, extendida: bool) -> [INPUT; 2] {
    let f = if extendida { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
    [evento(vk, 0, f), evento(vk, 0, f | KEYEVENTF_KEYUP)]
}

fn teclear(texto: &str) {
    esperar_modificadores();
    for c in texto.chars() {
        let ev: Vec<INPUT> = match c {
            '\n' => tecla_virtual(VK_RETURN, false).to_vec(),
            '\t' => tecla_virtual(VK_TAB, false).to_vec(),
            c => {
                let mut b = [0u16; 2];
                c.encode_utf16(&mut b)
                    .iter()
                    .flat_map(|&u| [evento(VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE), evento(VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)])
                    .collect()
            }
        };
        enviar(&ev);
        // sin pausa algunas apps (terminales, Electron) pierden o reordenan teclas
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Keysym de X11 → (tecla virtual, ¿extendida?).
fn virtual_de(ks: u32) -> Option<(VIRTUAL_KEY, bool)> {
    let v = match ks {
        0xffe3 | 0xffe4 => (VK_CONTROL, false),
        0xffe1 | 0xffe2 => (VK_SHIFT, false),
        0xffe9 | 0xffea => (VK_MENU, false),
        0xffeb | 0xffec => (VK_LWIN, true),
        0xff09 => (VK_TAB, false),
        0xff0d => (VK_RETURN, false),
        0xff1b => (VK_ESCAPE, false),
        0xff08 => (VK_BACK, false),
        0xffff => (VK_DELETE, true),
        0xff50 => (VK_HOME, true),
        0xff57 => (VK_END, true),
        0xff55 => (VK_PRIOR, true),
        0xff56 => (VK_NEXT, true),
        0xff51 => (VK_LEFT, true),
        0xff52 => (VK_UP, true),
        0xff53 => (VK_RIGHT, true),
        0xff54 => (VK_DOWN, true),
        0xff61 => (VK_SNAPSHOT, true),
        0x20 => (VK_SPACE, false),
        f @ 0xffbe..=0xffc9 => (VIRTUAL_KEY(VK_F1.0 + (f - 0xffbe) as u16), false),
        c => {
            // letras y signos: la tecla que los produce en la distribución activa
            let c = char::from_u32(c)?.to_lowercase().next()?;
            let mut b = [0u16; 2];
            let u = *c.encode_utf16(&mut b).first()?;
            let r = unsafe { VkKeyScanW(u) };
            if r == -1 {
                return None;
            }
            (VIRTUAL_KEY((r & 0xff) as u16), false)
        }
    };
    Some(v)
}

/// Pulsa una combinación (keysyms) en la ventana activa: en orden y se sueltan al revés.
pub fn pulsar(ks: &[u32]) {
    esperar_modificadores();
    let teclas: Vec<_> = ks.iter().filter_map(|&k| virtual_de(k)).collect();
    if teclas.len() != ks.len() {
        eprintln!("combinación con teclas desconocidas en Windows: {ks:x?}");
        return;
    }
    let mut ev = Vec::new();
    for &(vk, ext) in &teclas {
        ev.push(tecla_virtual(vk, ext)[0]);
    }
    for &(vk, ext) in teclas.iter().rev() {
        ev.push(tecla_virtual(vk, ext)[1]);
    }
    enviar(&ev);
}
