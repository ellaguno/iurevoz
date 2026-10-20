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

use crossbeam_channel::{Receiver, Sender};

use crate::audio;
use crate::config::Config;
use crate::motor::Orden;
use crate::portal::Aviso;
use crate::voz::Voz;

pub enum Control {
    Alternar,
    /// Escuchar una instrucción (abre el micrófono si hace falta).
    Comando,
    /// El motor ya recibió la instrucción: si el micrófono se abrió solo para ella, se cierra.
    FinComando,
    /// Leer la selección en voz alta, o callar si ya está hablando.
    Leer,
    /// Atajo global: (id, pulsado).
    Atajo(String, bool),
}

/// Avisos para la interfaz que no son del motor.
pub enum ParaUi {
    AtajoAsignado(String, String),
    Error(String),
}

/// Estado compartido con la interfaz y el motor.
#[derive(Clone)]
pub struct Estado {
    pub grabando: Arc<AtomicBool>,
    pub nivel: Arc<AtomicU32>,
    /// La próxima frase es una instrucción, no dictado.
    pub modo_comando: Arc<AtomicBool>,
}

impl Estado {
    pub fn new() -> Self {
        Self {
            grabando: Arc::new(AtomicBool::new(false)),
            nivel: Arc::new(AtomicU32::new(0)),
            modo_comando: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[derive(Clone)]
pub struct Controlador {
    pub tx: Sender<Control>,
    pub estado: Estado,
}

fn log(m: &str) {
    if std::env::var_os("IUREVOZ_LOG").is_some() {
        eprintln!("[iurevoz] {m}");
    }
}

impl Controlador {
    /// `tx`/`rx` se crean antes para que el motor también pueda mandar órdenes.
    pub fn iniciar(
        tx: Sender<Control>,
        rx: Receiver<Control>,
        estado: Estado,
        motor: Sender<Orden>,
        voz: Option<Voz>,
        ui: Sender<ParaUi>,
        repintar: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let e = estado.clone();
        std::thread::Builder::new()
            .name("control".into())
            .spawn(move || {
                // cpal::Stream no es Send: se crea y se suelta siempre en este hilo
                let mut captura = None;
                // el micrófono se abrió solo para una instrucción
                let mut solo_comando = false;
                let abrir = |e: &Estado| {
                    let m = motor.clone();
                    let hablando = voz.as_ref().map(|v| v.hablando.clone());
                    audio::abrir(
                        move |mut muestras, tasa| {
                            // mientras la app habla, el micrófono oye silencio: no se transcribe a sí misma
                            if hablando.as_ref().is_some_and(|h| h.load(Ordering::Relaxed)) {
                                muestras.iter_mut().for_each(|x| *x = 0.0);
                            }
                            let _ = m.send(Orden::Audio { muestras, tasa });
                        },
                        e.nivel.clone(),
                    )
                };
                for orden in rx {
                    let orden = match orden {
                        Control::Atajo(id, true) => match id.as_str() {
                            "comando" => Control::Comando,
                            "leer" => Control::Leer,
                            _ => Control::Alternar,
                        },
                        Control::Atajo(_, false) => continue, // soltar la tecla no hace nada
                        o => o,
                    };
                    match orden {
                        Control::Alternar => {
                            if captura.is_none() {
                                log("inicia dictado");
                                match abrir(&e) {
                                    Ok(c) => {
                                        log(&format!("micrófono a {} Hz", c.tasa));
                                        captura = Some(c)
                                    }
                                    Err(err) => drop(ui.send(ParaUi::Error(format!("micrófono: {err:#}")))),
                                }
                                solo_comando = false;
                            } else {
                                log("detiene dictado");
                                captura = None;
                                e.modo_comando.store(false, Ordering::Relaxed);
                                let _ = motor.send(Orden::Fin);
                                e.nivel.store(0, Ordering::Relaxed);
                            }
                        }
                        Control::Comando => {
                            log("escuchando instrucción");
                            if let Some(v) = &voz {
                                v.callar();
                            }
                            e.modo_comando.store(true, Ordering::Relaxed);
                            if captura.is_none() {
                                match abrir(&e) {
                                    Ok(c) => {
                                        log(&format!("micrófono a {} Hz", c.tasa));
                                        captura = Some(c);
                                        solo_comando = true;
                                    }
                                    Err(err) => drop(ui.send(ParaUi::Error(format!("micrófono: {err:#}")))),
                                }
                            }
                        }
                        Control::FinComando => {
                            if solo_comando && captura.is_some() {
                                captura = None;
                                solo_comando = false;
                                let _ = motor.send(Orden::Fin);
                                e.nivel.store(0, Ordering::Relaxed);
                            }
                        }
                        Control::Leer => match &voz {
                            Some(v) if v.hablando.load(Ordering::Relaxed) => v.callar(),
                            Some(v) => {
                                let (v, ui) = (v.clone(), ui.clone());
                                std::thread::spawn(move || match crate::escritorio::seleccion() {
                                    Ok(t) => v.decir(t),
                                    Err(err) => drop(ui.send(ParaUi::Error(format!("{err:#}")))),
                                });
                            }
                            None => drop(ui.send(ParaUi::Error("la voz está desactivada".into()))),
                        },
                        Control::Atajo(..) => unreachable!(),
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

    /// Atiende los avisos del portal: los atajos controlan el dictado; el resto va a la interfaz.
    pub fn aviso_portal(&self, a: Aviso, ui: &Sender<ParaUi>) {
        match a {
            Aviso::Atajo { id, pulsado } => self.enviar(Control::Atajo(id, pulsado)),
            Aviso::AtajoAsignado(id, s) => drop(ui.send(ParaUi::AtajoAsignado(id, s))),
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
                match orden.trim() {
                    "alternar" => ctl.enviar(Control::Alternar),
                    "comando" => ctl.enviar(Control::Comando),
                    "leer" | "callar" => ctl.enviar(Control::Leer),
                    _ => {}
                }
            }
        })
        .expect("hilo de socket");
}
