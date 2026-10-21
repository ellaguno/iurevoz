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
    /// Igual, pero sin callar la voz: para escuchar la respuesta a una pregunta del asistente.
    Seguir,
    /// El motor ya recibió la instrucción: si el micrófono se abrió solo para ella, se cierra.
    FinComando,
    /// Leer la selección en voz alta, o callar si ya está hablando.
    Leer,
    /// Atajo global: (id, pulsado).
    Atajo(String, bool),
    /// Pedir permiso al usuario: descripción (ventana), versión hablada y a quién avisar.
    Permiso(String, String, Sender<bool>),
    /// Pregunta de sí o no de otro programa (F6b): igual que un permiso, pero se dice tal cual.
    Pregunta(String, Sender<bool>),
    /// Escuchar una respuesta libre (F6b): se dice la pregunta (si hay) y lo que el usuario
    /// diga hasta una pausa de `pausa` va a `tx` en lugar de al agente.
    Escuchar { pregunta: Option<String>, pausa: std::time::Duration, tx: Sender<String> },
    /// Decir un texto; `listo` recibe aviso cuando termina de hablar.
    Decir(String, Sender<()>),
    /// Respuesta del usuario (botones o voz) al permiso pendiente.
    Responder(bool),
    /// El permiso con este número venció sin respuesta.
    Caducar(u64),
    /// Se oyó la palabra clave (lo manda el motor).
    Despertar,
    /// Activa o desactiva el modo «siempre escuchando».
    Vigilia(bool),
    /// Pausa (true) o reanuda (false) la escucha sin cambiar la preferencia del usuario.
    PausarVigilia(bool),
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
    /// Permiso que espera respuesta: (número, descripción, a quién avisar).
    pub pendiente: Arc<std::sync::Mutex<Option<(u64, String, Sender<bool>)>>>,
    /// El audio va al reconocedor principal; si no, a la vigilia (palabra clave).
    pub atento: Arc<AtomicBool>,
    /// Modo «siempre escuchando» activo.
    pub vigilia: Arc<AtomicBool>,
    /// Respuesta hablada que espera otro programa (F6b): a quién mandarla y qué pausa la cierra.
    pub captura: Arc<std::sync::Mutex<Option<(Sender<String>, std::time::Duration)>>>,
}

impl Estado {
    pub fn new() -> Self {
        Self {
            grabando: Arc::new(AtomicBool::new(false)),
            nivel: Arc::new(AtomicU32::new(0)),
            modo_comando: Arc::new(AtomicBool::new(false)),
            pendiente: Default::default(),
            atento: Arc::new(AtomicBool::new(false)),
            vigilia: Arc::new(AtomicBool::new(false)),
            captura: Default::default(),
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
        let tx_propio = tx.clone();
        std::thread::Builder::new()
            .name("control".into())
            .spawn(move || {
                // cpal::Stream no es Send: se crea y se suelta siempre en este hilo
                let mut captura: Option<audio::Captura> = None;
                // por qué está abierto el micrófono
                let mut dictando = false;
                let mut por_comando = false; // instrucción, permiso o palabra clave
                let mut numero = 0u64;
                // «siempre escuchando» quedó en pausa (p. ej. mientras se graba una junta)
                let mut pausada = false;
                // número del permiso pendiente que en realidad es una pregunta de sí o no
                let mut pregunta_pendiente: Option<u64> = None;
                let abrir = |e: &Estado| {
                    let m = motor.clone();
                    let hablando = voz.as_ref().map(|v| v.hablando.clone());
                    let atento = e.atento.clone();
                    audio::abrir(
                        move |mut muestras, tasa| {
                            // mientras la app habla, el micrófono oye silencio: no se transcribe a sí misma
                            if hablando.as_ref().is_some_and(|h| h.load(Ordering::Relaxed)) {
                                muestras.iter_mut().for_each(|x| *x = 0.0);
                            }
                            let o = if atento.load(Ordering::Relaxed) {
                                Orden::Audio { muestras, tasa }
                            } else {
                                Orden::Vigilia { muestras, tasa }
                            };
                            let _ = m.send(o);
                        },
                        e.nivel.clone(),
                    )
                };
                // Cierra la parte de comando; si no hay dictado, el reconocedor se vacía.
                let fin_comando = |por_comando: &mut bool, dictando: bool| {
                    if *por_comando {
                        *por_comando = false;
                        if !dictando {
                            let _ = motor.send(Orden::Fin);
                        }
                    }
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
                            dictando = !dictando;
                            log(if dictando { "inicia dictado" } else { "detiene dictado" });
                            if !dictando {
                                e.modo_comando.store(false, Ordering::Relaxed);
                                por_comando = false;
                                let _ = motor.send(Orden::Fin);
                            }
                        }
                        Control::Comando | Control::Seguir => {
                            log("escuchando instrucción");
                            if matches!(orden, Control::Comando) {
                                if let Some(v) = &voz {
                                    v.callar();
                                }
                            }
                            e.modo_comando.store(true, Ordering::Relaxed);
                            por_comando = true;
                        }
                        Control::Despertar => {
                            log("palabra clave: escuchando instrucción");
                            por_comando = true;
                            if let Some(v) = &voz {
                                v.pitido();
                            }
                        }
                        Control::FinComando => fin_comando(&mut por_comando, dictando),
                        Control::PausarVigilia(si) => {
                            if si && e.vigilia.load(Ordering::Relaxed) {
                                log("siempre escuchando: en pausa");
                                e.vigilia.store(false, Ordering::Relaxed);
                                pausada = true;
                            } else if !si && pausada {
                                log("siempre escuchando: reanudado");
                                e.vigilia.store(true, Ordering::Relaxed);
                                pausada = false;
                            }
                        }
                        Control::Vigilia(si) => {
                            pausada = false;
                            log(if si { "siempre escuchando: activo" } else { "siempre escuchando: inactivo" });
                            e.vigilia.store(si, Ordering::Relaxed);
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
                        Control::Permiso(..) | Control::Pregunta(..) => {
                            let (desc, hablada, tx, es_pregunta) = match orden {
                                Control::Permiso(d, h, tx) => (d, format!("{h} ¿Lo permito?"), tx, false),
                                Control::Pregunta(p, tx) => (p.clone(), p, tx, true),
                                _ => unreachable!(),
                            };
                            numero += 1;
                            pregunta_pendiente = es_pregunta.then_some(numero);
                            log(&format!("permiso #{numero}: {desc}"));
                            // un permiso anterior sin responder se niega
                            if let Some((_, _, viejo)) = e.pendiente.lock().unwrap().replace((numero, desc.clone(), tx)) {
                                let _ = viejo.send(false);
                            }
                            if let Some(v) = &voz {
                                v.decir(hablada);
                            }
                            por_comando = true;
                            // vence a los 100 s (el servidor de permisos espera 120)
                            let tx = tx_propio.clone();
                            let n = numero;
                            std::thread::spawn(move || {
                                std::thread::sleep(std::time::Duration::from_secs(100));
                                let _ = tx.send(Control::Caducar(n));
                            });
                        }
                        Control::Responder(si) => {
                            if let Some((n, desc, tx)) = e.pendiente.lock().unwrap().take() {
                                log(&format!("permiso #{n} {}: {desc}", if si { "concedido" } else { "negado" }));
                                let _ = tx.send(si);
                                if let Some(v) = &voz {
                                    if pregunta_pendiente.take() == Some(n) {
                                        v.decir("Entendido.");
                                    } else {
                                        v.decir(if si { "Permitido." } else { "No lo permito." });
                                    }
                                }
                            }
                            if !e.modo_comando.load(Ordering::Relaxed) {
                                fin_comando(&mut por_comando, dictando);
                            }
                        }
                        Control::Caducar(n) => {
                            let mut p = e.pendiente.lock().unwrap();
                            if p.as_ref().is_some_and(|(m, ..)| *m == n) {
                                if let Some((_, desc, tx)) = p.take() {
                                    log(&format!("permiso #{n} vencido: {desc}"));
                                    let _ = tx.send(false);
                                }
                                drop(p);
                                if !e.modo_comando.load(Ordering::Relaxed) {
                                    fin_comando(&mut por_comando, dictando);
                                }
                            }
                        }
                        Control::Escuchar { pregunta, pausa, tx } => {
                            log(&format!("escuchando una respuesta para otro programa: {pregunta:?}"));
                            // una espera anterior se queda sin respuesta
                            if let Some((viejo, _)) = e.captura.lock().unwrap().replace((tx, pausa)) {
                                let _ = viejo.send(String::new());
                            }
                            if let (Some(v), Some(p)) = (&voz, pregunta) {
                                v.decir(p);
                            } else if let Some(v) = &voz {
                                v.pitido();
                            }
                            e.modo_comando.store(true, Ordering::Relaxed);
                            por_comando = true;
                        }
                        Control::Decir(texto, listo) => match &voz {
                            Some(v) => {
                                v.decir(texto);
                                let h = v.hablando.clone();
                                std::thread::spawn(move || {
                                    // la síntesis tarda en empezar: se espera a que hable y luego a que calle
                                    let t = std::time::Instant::now();
                                    while !h.load(Ordering::Relaxed) && t.elapsed() < std::time::Duration::from_secs(5) {
                                        std::thread::sleep(std::time::Duration::from_millis(50));
                                    }
                                    while h.load(Ordering::Relaxed) && t.elapsed() < std::time::Duration::from_secs(300) {
                                        std::thread::sleep(std::time::Duration::from_millis(100));
                                    }
                                    let _ = listo.send(());
                                });
                            }
                            None => drop(listo.send(())),
                        },
                        Control::Atajo(..) => unreachable!(),
                    }
                    // el audio va al reconocedor si algo lo necesita; si no, a la vigilia
                    let pendiente = e.pendiente.lock().unwrap().is_some();
                    e.atento.store(dictando || por_comando || pendiente || e.modo_comando.load(Ordering::Relaxed), Ordering::Relaxed);
                    let quiere = dictando || por_comando || pendiente || e.vigilia.load(Ordering::Relaxed);
                    if quiere && captura.is_none() {
                        match abrir(&e) {
                            Ok(c) => {
                                log(&format!("micrófono a {} Hz", c.tasa));
                                captura = Some(c);
                            }
                            Err(err) => drop(ui.send(ParaUi::Error(format!("micrófono: {err:#}")))),
                        }
                    } else if !quiere && captura.is_some() {
                        captura = None;
                        e.nivel.store(0, Ordering::Relaxed);
                    }
                    e.grabando.store(dictando, Ordering::Relaxed);
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
        if !matches!(a, Aviso::TokenTeclado(_)) {
            log(&format!("portal: {a:?}"));
        }
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

/// Envía una orden y espera la respuesta (hasta `espera`). `None` si la app no está abierta.
pub fn preguntar_a_instancia(orden: &str, espera: std::time::Duration) -> Option<String> {
    let mut s = UnixStream::connect(ruta_socket()).ok()?;
    let _ = s.set_read_timeout(Some(espera));
    s.write_all(orden.as_bytes()).ok()?;
    let _ = s.shutdown(std::net::Shutdown::Write);
    let mut r = String::new();
    let _ = s.read_to_string(&mut r);
    Some(r)
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
                let _ = (&c).take(16 * 1024).read_to_string(&mut orden);
                if let Some(json) = orden.trim().strip_prefix("permiso ") {
                    // petición de `iurevoz mcp-permisos`: se responde «si» o «no» por el mismo socket
                    let v = serde_json::from_str::<serde_json::Value>(json).unwrap_or_default();
                    let desc = v["descripcion"].as_str().unwrap_or("hacer una acción").to_string();
                    let hablada = format!("Claude quiere {}.", v["hablada"].as_str().unwrap_or(&desc));
                    // (el «¿Lo permito?» lo agrega el control)
                    let ctl = ctl.clone();
                    std::thread::spawn(move || {
                        let (tx, rx) = crossbeam_channel::bounded(1);
                        ctl.enviar(Control::Permiso(desc, hablada, tx));
                        let si = rx.recv_timeout(std::time::Duration::from_secs(110)).unwrap_or(false);
                        let mut c = c;
                        let _ = c.write_all(if si { b"si" } else { b"no" });
                    });
                    continue;
                }
                // F6b: `iurevoz --mcp` pide hablar o escuchar y espera la respuesta por el socket
                if let Some((cmd, json)) = orden.trim().split_once(' ').filter(|(c, _)| ["decir", "si_no", "escuchar"].contains(c)) {
                    let v = serde_json::from_str::<serde_json::Value>(json).unwrap_or_default();
                    let ctl = ctl.clone();
                    let cmd = cmd.to_string();
                    std::thread::spawn(move || {
                        let texto = |k: &str| v[k].as_str().map(str::trim).filter(|t| !t.is_empty()).map(String::from);
                        let respuesta = match cmd.as_str() {
                            "decir" => {
                                let (tx, rx) = crossbeam_channel::bounded(1);
                                ctl.enviar(Control::Decir(texto("texto").unwrap_or_default(), tx));
                                let _ = rx.recv_timeout(std::time::Duration::from_secs(310));
                                "ok".to_string()
                            }
                            "si_no" => {
                                let (tx, rx) = crossbeam_channel::bounded(1);
                                ctl.enviar(Control::Pregunta(texto("pregunta").unwrap_or_else(|| "¿Sí o no?".into()), tx));
                                match rx.recv_timeout(std::time::Duration::from_secs(110)) {
                                    Ok(true) => "si".into(),
                                    Ok(false) => "no".into(),
                                    Err(_) => String::new(),
                                }
                            }
                            _ => {
                                let (tx, rx) = crossbeam_channel::bounded(1);
                                let pausa = v["pausa"].as_f64().unwrap_or(1.0).clamp(0.6, 6.0);
                                ctl.enviar(Control::Escuchar {
                                    pregunta: texto("pregunta"),
                                    pausa: std::time::Duration::from_secs_f64(pausa),
                                    tx,
                                });
                                rx.recv_timeout(std::time::Duration::from_secs(600)).unwrap_or_default()
                            }
                        };
                        let mut c = c;
                        let _ = c.write_all(respuesta.as_bytes());
                    });
                    continue;
                }
                match orden.trim() {
                    "alternar" => ctl.enviar(Control::Alternar),
                    "comando" => ctl.enviar(Control::Comando),
                    "leer" | "callar" => ctl.enviar(Control::Leer),
                    // solo se puede negar desde fuera: aprobar exige al usuario (voz o botón)
                    "rechazar" => ctl.enviar(Control::Responder(false)),
                    _ => {}
                }
            }
        })
        .expect("hilo de socket");
}
