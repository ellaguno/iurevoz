//! Control del dictado, independiente de la ventana.
//!
//! En Wayland la interfaz solo corre cuando se dibuja, y no se dibuja si está oculta o
//! en otro escritorio. Por eso el micrófono y las órdenes (botón, atajo global, socket)
//! viven en su propio hilo.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crossbeam_channel::{Sender, unbounded};

use crate::audio;
use crate::config::Config;
use crate::motor::Orden;
use crate::portal::Aviso;

pub enum Control {
    Alternar,
    /// Atajo en modo pulsar-para-hablar: true al pulsar, false al soltar.
    Atajo(bool),
}

/// Avisos para la interfaz que no son del motor.
pub enum ParaUi {
    AtajoAsignado(String),
    Error(String),
}

/// Estado compartido con la interfaz.
#[derive(Clone)]
pub struct Estado {
    pub grabando: Arc<AtomicBool>,
    pub nivel: Arc<AtomicU32>,
}

#[derive(Clone)]
pub struct Controlador {
    pub tx: Sender<Control>,
    pub estado: Estado,
}

impl Controlador {
    pub fn iniciar(motor: Sender<Orden>, ui: Sender<ParaUi>, repintar: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = unbounded::<Control>();
        let estado = Estado { grabando: Arc::new(AtomicBool::new(false)), nivel: Arc::new(AtomicU32::new(0)) };
        let e = estado.clone();
        std::thread::Builder::new()
            .name("control".into())
            .spawn(move || {
                // cpal::Stream no es Send: se crea y se suelta siempre en este hilo
                let mut captura = None;
                for orden in rx {
                    if let Control::Atajo(false) = orden {
                        continue; // modo alternar: soltar la tecla no hace nada
                    }
                    let quiere = captura.is_none();
                    if std::env::var_os("IUREVOZ_LOG").is_some() {
                        eprintln!("[iurevoz] {}", if quiere { "inicia dictado" } else { "detiene dictado" });
                    }
                    if quiere {
                        let m = motor.clone();
                        match audio::abrir(move |muestras, tasa| drop(m.send(Orden::Audio { muestras, tasa })), e.nivel.clone()) {
                            Ok(c) => {
                                if std::env::var_os("IUREVOZ_LOG").is_some() {
                                    eprintln!("[iurevoz] micrófono a {} Hz", c.tasa);
                                }
                                captura = Some(c)
                            }
                            Err(err) => {
                                eprintln!("micrófono: {err:#}");
                                let _ = ui.send(ParaUi::Error(format!("micrófono: {err:#}")));
                            }
                        }
                    } else {
                        captura = None;
                        let _ = motor.send(Orden::Fin);
                        e.nivel.store(0, Ordering::Relaxed);
                    }
                    e.grabando.store(captura.is_some(), Ordering::Relaxed);
                    repintar();
                }
            })
            .expect("hilo de control");
        Self { tx, estado }
    }

    pub fn enviar(&self, c: Control) {
        let _ = self.tx.send(c);
    }

    /// Atiende los avisos del portal: el atajo controla el dictado; el resto va a la interfaz.
    pub fn aviso_portal(&self, a: Aviso, ui: &Sender<ParaUi>) {
        match a {
            Aviso::Atajo { pulsado } => self.enviar(Control::Atajo(pulsado)),
            Aviso::AtajoAsignado(s) => drop(ui.send(ParaUi::AtajoAsignado(s))),
            Aviso::TokenTeclado(t) => {
                // se guarda aquí y no en la interfaz, que puede no estar corriendo
                let mut cfg = Config::cargar();
                cfg.token_portal = Some(t);
                if let Err(e) = cfg.guardar() {
                    let _ = ui.send(ParaUi::Error(format!("no pude guardar el permiso: {e}")));
                }
            }
            Aviso::Error(e) => drop(ui.send(ParaUi::Error(e))),
        }
    }
}

fn ruta_socket() -> PathBuf {
    dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join("iurevoz.sock")
}

/// Envía una orden a la instancia en marcha. Devuelve false si no hay ninguna.
pub fn enviar_a_instancia(orden: &str) -> bool {
    match UnixStream::connect(ruta_socket()) {
        Ok(mut s) => s.write_all(orden.as_bytes()).is_ok(),
        Err(_) => false,
    }
}

/// Socket de control: `iurevoz alternar` desde un atajo de GNOME o un script.
pub fn escuchar_socket(ctl: Controlador) {
    let ruta = ruta_socket();
    let _ = std::fs::remove_file(&ruta); // socket huérfano de una ejecución anterior
    let l = match UnixListener::bind(&ruta) {
        Ok(l) => l,
        Err(e) => return eprintln!("socket de control {}: {e}", ruta.display()),
    };
    std::thread::Builder::new()
        .name("socket".into())
        .spawn(move || {
            for c in l.incoming().flatten() {
                let mut orden = String::new();
                let _ = (&c).take(64).read_to_string(&mut orden);
                if orden.trim() == "alternar" {
                    ctl.enviar(Control::Alternar);
                }
            }
        })
        .expect("hilo de socket");
}
