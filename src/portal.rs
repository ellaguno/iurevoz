//! Portales de escritorio (Wayland/GNOME): teclear texto en la ventana activa
//! (RemoteDesktop) y atajo de teclado global (GlobalShortcuts).
//!
//! Todo corre en un solo runtime de tokio para compartir la conexión D-Bus de ashpd.

use std::time::Duration;

use anyhow::{Context, Result};
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::{PersistMode, Session};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::config::APP_ID;

/// Atajos globales: (id, descripción, combinación sugerida). GNOME deja cambiarlos.
pub const ATAJOS: &[(&str, &str, &str)] = &[
    ("dictar", "Dictar / detener", "<Control><Alt>space"),
    ("comando", "Dar una instrucción", "<Control><Alt>c"),
    ("leer", "Leer la selección en voz alta / callar", "<Control><Alt>l"),
];

#[derive(Debug)]
pub enum Aviso {
    /// Un atajo global se activó (pulsado) o se soltó.
    Atajo { id: String, pulsado: bool },
    /// (id, combinación asignada por el usuario en GNOME).
    AtajoAsignado(String, String),
    /// Token para que el permiso de teclear no se vuelva a pedir.
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
    tx: mpsc::UnboundedSender<Pedido>,
}

impl Portal {
    /// Arranca el hilo de portales. `avisar` recibe los eventos para la interfaz.
    pub fn iniciar(token: Option<String>, atajo: bool, avisar: impl Fn(Aviso) + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<Pedido>();
        std::thread::Builder::new()
            .name("portal".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build();
                let rt = match rt {
                    Ok(rt) => rt,
                    Err(e) => return avisar(Aviso::Error(format!("tokio: {e}"))),
                };
                rt.block_on(async move {
                    // Las apps fuera de Flatpak deben registrar su id antes de usar portales.
                    if let Err(e) = ashpd::register_host_app(APP_ID.try_into().expect("app id válido")).await {
                        eprintln!("registro en portal: {e}");
                    }
                    let avisar = std::sync::Arc::new(avisar);
                    if atajo {
                        let a = avisar.clone();
                        tokio::spawn(async move {
                            if let Err(e) = atajo_global(&*a).await {
                                a(Aviso::Error(format!("atajo global: {e:#}")));
                            }
                        });
                    }
                    teclado(rx, token, &*avisar).await;
                });
            })
            .expect("hilo de portal");
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

async fn atajo_global(avisar: &(dyn Fn(Aviso) + Send + Sync)) -> Result<()> {
    let gs = GlobalShortcuts::new().await?;
    let sesion = gs.create_session(Default::default()).await?;
    if std::env::var_os("IUREVOZ_LOG").is_some() {
        eprintln!("[iurevoz] portal: sesión de atajos creada (versión {})", gs.version());
    }
    let atajos: Vec<NewShortcut> = ATAJOS.iter().map(|(id, d, t)| NewShortcut::new(*id, *d).preferred_trigger(*t)).collect();
    // En GNOME 50 de Ubuntu el portal a veces nunca responde: se avisa en vez de esperar para siempre.
    let pedido = gs.bind_shortcuts(&sesion, &atajos, None, Default::default());
    let r = match tokio::time::timeout(Duration::from_secs(60), pedido).await {
        Ok(r) => r?.response()?,
        Err(_) => anyhow::bail!("el portal de GNOME no respondió; usa «Instalar atajos de GNOME» en Ajustes"),
    };
    for s in r.shortcuts() {
        avisar(Aviso::AtajoAsignado(s.id().to_string(), s.trigger_description().to_string()));
    }
    let mut act = gs.receive_activated().await?;
    let mut desact = gs.receive_deactivated().await?;
    loop {
        tokio::select! {
            Some(a) = act.next() => avisar(Aviso::Atajo { id: a.shortcut_id().to_string(), pulsado: true }),
            Some(d) = desact.next() => avisar(Aviso::Atajo { id: d.shortcut_id().to_string(), pulsado: false }),
            else => break,
        }
    }
    Ok(())
}

struct Teclado {
    rd: RemoteDesktop,
    sesion: Session<RemoteDesktop>,
    teclas: crate::teclas::Teclas,
}

async fn abrir_teclado(token: Option<&str>, avisar: &(dyn Fn(Aviso) + Send + Sync)) -> Result<Teclado> {
    let rd = RemoteDesktop::new().await?;
    let sesion = rd.create_session(Default::default()).await?;
    rd.select_devices(
        &sesion,
        SelectDevicesOptions::default()
            .set_devices(ashpd::enumflags2::BitFlags::from(DeviceType::Keyboard))
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(token),
    )
    .await?;
    let r = rd.start(&sesion, None, Default::default()).await?.response().context("permiso denegado")?;
    if let Some(t) = r.restore_token() {
        avisar(Aviso::TokenTeclado(t.to_string()));
    }
    Ok(Teclado { rd, sesion, teclas: crate::teclas::Teclas::de_gnome() })
}

async fn teclado(mut rx: mpsc::UnboundedReceiver<Pedido>, mut token: Option<String>, avisar: &(dyn Fn(Aviso) + Send + Sync)) {
    let mut tec: Option<Teclado> = None;
    while let Some(pedido) = rx.recv().await {
        if tec.is_none() {
            match abrir_teclado(token.as_deref(), avisar).await {
                Ok(t) => tec = Some(t),
                Err(e) => {
                    avisar(Aviso::Error(format!("no pude abrir el teclado virtual: {e:#}")));
                    token = None; // un token vencido hace fallar; el siguiente intento pide permiso
                    continue;
                }
            }
        }
        let t = tec.as_ref().unwrap();
        let r = match &pedido {
            Pedido::Escribir(texto) => teclear(t, texto).await,
            Pedido::Teclas(ks) => combinacion(t, ks).await,
        };
        if let Err(e) = r {
            avisar(Aviso::Error(format!("al teclear: {e:#}")));
            tec = None; // la sesión pudo cerrarse (permiso revocado): se reabre la próxima vez
        }
    }
}

async fn combinacion(t: &Teclado, ks: &[u32]) -> Result<()> {
    for &k in ks {
        t.rd.notify_keyboard_keysym(&t.sesion, k as i32, KeyState::Pressed, Default::default()).await?;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for &k in ks.iter().rev() {
        t.rd.notify_keyboard_keysym(&t.sesion, k as i32, KeyState::Released, Default::default()).await?;
    }
    Ok(())
}

async fn teclear(t: &Teclado, texto: &str) -> Result<()> {
    for c in texto.chars() {
        for ks in t.teclas.secuencia(c) {
            let ks = ks as i32;
            t.rd.notify_keyboard_keysym(&t.sesion, ks, KeyState::Pressed, Default::default()).await?;
            t.rd.notify_keyboard_keysym(&t.sesion, ks, KeyState::Released, Default::default()).await?;
            // sin pausa algunas apps (terminales, Electron) pierden o reordenan teclas
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    Ok(())
}
