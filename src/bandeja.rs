//! Ícono en la barra superior de GNOME (StatusNotifierItem; lo muestra la extensión
//! AppIndicator de Ubuntu) o en el área de notificación de Windows. El color dice qué hace
//! IureVoz: gris inactivo, verde con la oreja abierta a «oye compu», rojo dictando y azul
//! escuchando una instrucción.
//!
//! Con el ícono, cerrar la ventana la destruye y IureVoz sigue aquí; «Mostrar IureVoz» crea
//! otra (en Wayland una ventana no se puede esconder y volver a mostrar). «Salir» termina.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[cfg(target_os = "linux")]
use ksni::TrayMethods;

use crate::control::{Control, Controlador};

/// El ícono está en la barra: cerrar la ventana no termina IureVoz.
pub static ACTIVA: AtomicBool = AtomicBool::new(false);
/// Se pidió salir desde el menú: el cierre ya no se intercepta.
pub static SALIENDO: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Modo {
    Inactivo,
    Escuchando,
    Dictando,
    Comando,
}

impl Modo {
    fn de(ctl: &Controlador) -> Self {
        let e = &ctl.estado;
        if e.grabando.load(Ordering::Relaxed) {
            Modo::Dictando
        } else if e.modo_comando.load(Ordering::Relaxed)
            || e.pendiente.lock().unwrap().is_some()
            || e.captura.lock().unwrap().is_some()
        {
            Modo::Comando
        } else if e.vigilia.load(Ordering::Relaxed) {
            Modo::Escuchando
        } else {
            Modo::Inactivo
        }
    }

    pub fn texto(self) -> &'static str {
        match self {
            Modo::Inactivo => "Inactivo",
            Modo::Escuchando => "Esperando «oye compu»",
            Modo::Dictando => "Dictando",
            Modo::Comando => "Escuchando una instrucción",
        }
    }

    /// Color del círculo (r, g, b).
    fn color(self) -> (u8, u8, u8) {
        match self {
            Modo::Inactivo => (0x77, 0x76, 0x7b),
            Modo::Escuchando => (0x2e, 0xc2, 0x7e),
            Modo::Dictando => (0xe0, 0x1b, 0x24),
            Modo::Comando => (0x35, 0x84, 0xe4),
        }
    }
}

/// Distancia de `p` al segmento a–b.
fn segmento(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (px, py) = (p.0 - a.0, p.1 - a.1);
    let (bx, by) = (b.0 - a.0, b.1 - a.1);
    let h = ((px * bx + py * by) / (bx * bx + by * by)).clamp(0.0, 1.0);
    ((px - bx * h).powi(2) + (py - by * h).powi(2)).sqrt()
}

/// Imagen cuadrada en ARGB32 (orden de red), como la pide StatusNotifierItem.
pub struct Icono {
    pub width: i32,
    pub height: i32,
    pub data: Vec<u8>,
}

/// Círculo del color del modo con un micrófono blanco. Se dibuja con distancias, con bordes
/// suavizados.
pub fn icono(modo: Modo, lado: i32) -> Icono {
    let (r, g, b) = modo.color();
    let n = lado as f32;
    let cubre = |d: f32| (0.5 - d * n).clamp(0.0, 1.0);
    let mut data = Vec::with_capacity((lado * lado * 4) as usize);
    for y in 0..lado {
        for x in 0..lado {
            let p = ((x as f32 + 0.5) / n, (y as f32 + 0.5) / n);
            let fondo = cubre(((p.0 - 0.5).powi(2) + (p.1 - 0.5).powi(2)).sqrt() - 0.48);
            // cápsula, soporte en U, pie y base
            let capsula = segmento(p, (0.5, 0.30), (0.5, 0.47)) - 0.105;
            let radio = ((p.0 - 0.5).powi(2) + (p.1 - 0.47).powi(2)).sqrt();
            let u = if p.1 >= 0.47 { (radio - 0.19).abs() - 0.035 } else { 1.0 };
            let pie = segmento(p, (0.5, 0.66), (0.5, 0.76)) - 0.035;
            let base = segmento(p, (0.38, 0.77), (0.62, 0.77)) - 0.035;
            let mic = cubre(capsula.min(u).min(pie).min(base)).min(fondo);
            let mezcla = |c: u8| (c as f32 * (1.0 - mic) + 255.0 * mic).round() as u8;
            data.extend_from_slice(&[(fondo * 255.0).round() as u8, mezcla(r), mezcla(g), mezcla(b)]);
        }
    }
    Icono { width: lado, height: lado, data }
}

#[cfg(target_os = "linux")]
struct Bandeja {
    modo: Modo,
    ctl: Controlador,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for Bandeja {
    fn id(&self) -> String {
        "iurevoz".into()
    }

    fn title(&self) -> String {
        "IureVoz".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        [22, 32, 48]
            .map(|n| {
                let i = icono(self.modo, n);
                ksni::Icon { width: i.width, height: i.height, data: i.data }
            })
            .to_vec()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: "IureVoz".into(), description: self.modo.texto().into(), ..Default::default() }
    }

    // al iniciar sesión la extensión de indicadores puede llegar después que IureVoz
    fn watcher_online(&self) {
        ACTIVA.store(true, Ordering::Relaxed);
    }

    fn watcher_offline(&self, _: ksni::OfflineReason) -> bool {
        ACTIVA.store(false, Ordering::Relaxed);
        true
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        crate::ui::mostrar_ventana();
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, StandardItem};
        let dictando = self.modo == Modo::Dictando;
        let escuchando = self.ctl.estado.vigilia.load(Ordering::Relaxed);
        vec![
            StandardItem {
                label: self.modo.texto().into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Mostrar IureVoz".into(),
                activate: Box::new(|_: &mut Self| crate::ui::mostrar_ventana()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if dictando { "Detener el dictado".into() } else { "Dictar".into() },
                activate: Box::new(move |t: &mut Self| t.ctl.enviar(Control::Dictar(!dictando))),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Escuchar «oye compu»".into(),
                checked: escuchando,
                activate: Box::new(move |t: &mut Self| t.ctl.enviar(Control::Vigilia(!escuchando))),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Salir".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_: &mut Self| salir()),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Cierra la ventana y termina; si la ventana no procesa la orden, después de un momento
/// se termina el proceso (los auxiliares mueren con él).
fn salir() {
    crate::ui::salir();
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(2000));
        crate::llm_local::apagar();
        crate::suite::apagar();
        std::process::exit(0);
    });
}

/// Pone el ícono y lo mantiene al día con el estado. Sin extensión de indicadores no hay
/// ícono y la ventana se comporta como siempre; si la extensión llega tarde (al iniciar
/// sesión), el ícono aparece entonces.
#[cfg(target_os = "linux")]
pub fn iniciar(ctl: Controlador) {
    std::thread::Builder::new()
        .name("bandeja".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => return eprintln!("bandeja: {e}"),
            };
            rt.block_on(async move {
                let modo = Modo::de(&ctl);
                // si la barra aún no está, `watcher_offline` lo apaga y `watcher_online` lo prende
                ACTIVA.store(true, Ordering::Relaxed);
                let h = match (Bandeja { modo, ctl: ctl.clone() }).assume_sni_available(true).spawn().await {
                    Ok(h) => h,
                    Err(e) => {
                        ACTIVA.store(false, Ordering::Relaxed);
                        return eprintln!("bandeja: sin ícono ({e})");
                    }
                };
                let mut antes = (modo, ctl.estado.vigilia.load(Ordering::Relaxed));
                loop {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    let ahora = (Modo::de(&ctl), ctl.estado.vigilia.load(Ordering::Relaxed));
                    if ahora != antes {
                        antes = ahora;
                        if h.update(|t| t.modo = ahora.0).await.is_none() {
                            break;
                        }
                    }
                }
            });
        })
        .expect("hilo bandeja");
}

/// Windows: el ícono vive en su propio hilo, que atiende los mensajes de su ventana oculta
/// y lo actualiza cada 250 ms.
#[cfg(windows)]
pub fn iniciar(ctl: Controlador) {
    std::thread::Builder::new()
        .name("bandeja".into())
        .spawn(move || {
            if let Err(e) = bandeja_windows(ctl) {
                ACTIVA.store(false, Ordering::Relaxed);
                eprintln!("bandeja: sin ícono ({e})");
            }
        })
        .expect("hilo bandeja");
}

#[cfg(windows)]
fn bandeja_windows(ctl: Controlador) -> anyhow::Result<()> {
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage};

    // Windows pide RGBA; el dibujo es ARGB
    let imagen = |m: Modo| -> anyhow::Result<tray_icon::Icon> {
        let i = icono(m, 32);
        let rgba = i.data.chunks(4).flat_map(|p| [p[1], p[2], p[3], p[0]]).collect();
        Ok(tray_icon::Icon::from_rgba(rgba, i.width as u32, i.height as u32)?)
    };
    let estado = MenuItem::new(Modo::Inactivo.texto(), false, None);
    let mostrar = MenuItem::new("Mostrar IureVoz", true, None);
    let dictar = MenuItem::new("Dictar", true, None);
    let oir = CheckMenuItem::new("Escuchar «oye compu»", true, false, None);
    let fin = MenuItem::new("Salir", true, None);
    let menu = Menu::new();
    menu.append_items(&[&estado, &PredefinedMenuItem::separator(), &mostrar, &dictar, &oir, &PredefinedMenuItem::separator(), &fin])?;
    let modo = Modo::de(&ctl);
    let tray = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("IureVoz").with_icon(imagen(modo)?).build()?;
    ACTIVA.store(true, Ordering::Relaxed);

    let mut antes = None;
    loop {
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let dictando = Modo::de(&ctl) == Modo::Dictando;
            if ev.id == *mostrar.id() {
                crate::ui::mostrar_ventana();
            } else if ev.id == *dictar.id() {
                ctl.enviar(Control::Dictar(!dictando));
            } else if ev.id == *oir.id() {
                ctl.enviar(Control::Vigilia(!ctl.estado.vigilia.load(Ordering::Relaxed)));
            } else if ev.id == *fin.id() {
                salir();
            }
        }
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                crate::ui::mostrar_ventana();
            }
        }
        let ahora = (Modo::de(&ctl), ctl.estado.vigilia.load(Ordering::Relaxed));
        if antes != Some(ahora) {
            antes = Some(ahora);
            let _ = tray.set_icon(Some(imagen(ahora.0)?));
            let _ = tray.set_tooltip(Some(format!("IureVoz · {}", ahora.0.texto())));
            estado.set_text(ahora.0.texto());
            dictar.set_text(if ahora.0 == Modo::Dictando { "Detener el dictado" } else { "Dictar" });
            oir.set_checked(ahora.1);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Arranque con la sesión, en Windows: valor `IureVoz` en `HKCU\…\Run`.
#[cfg(windows)]
pub mod arranque {
    use std::process::Command;

    const CLAVE: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

    fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
        crate::plataforma::sin_consola(&mut Command::new("reg")).args(args).output()
    }

    pub fn activo() -> bool {
        reg(&["query", CLAVE, "/v", "IureVoz"]).is_ok_and(|o| o.status.success())
    }

    /// Crea o quita la entrada. Arranca sin ventana, solo con el ícono (`--autoarranque`).
    pub fn fijar(si: bool) -> anyhow::Result<()> {
        let out = if si {
            let exe = std::env::current_exe()?;
            reg(&["add", CLAVE, "/v", "IureVoz", "/t", "REG_SZ", "/d", &format!("\"{}\" --autoarranque", exe.display()), "/f"])?
        } else if activo() {
            reg(&["delete", CLAVE, "/v", "IureVoz", "/f"])?
        } else {
            return Ok(());
        };
        if !out.status.success() {
            anyhow::bail!("reg: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }
}

/// Arranque con la sesión: `~/.config/autostart/mx.iurefficient.IureVoz.desktop`.
#[cfg(target_os = "linux")]
pub mod arranque {
    use std::path::PathBuf;

    fn ruta() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("autostart").join(format!("{}.desktop", crate::config::APP_ID)))
    }

    pub fn activo() -> bool {
        ruta().is_some_and(|r| r.exists())
    }

    /// Crea o quita la entrada. Arranca sin ventana, solo con el ícono (`--autoarranque`).
    pub fn fijar(si: bool) -> anyhow::Result<()> {
        let r = ruta().ok_or_else(|| anyhow::anyhow!("no encuentro ~/.config"))?;
        if !si {
            if r.exists() {
                std::fs::remove_file(&r)?;
            }
            return Ok(());
        }
        let exe = std::env::current_exe()?;
        if let Some(d) = r.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(
            &r,
            format!(
                "[Desktop Entry]\nType=Application\nName=IureVoz\nComment=Dictado y órdenes por voz\n\
                 Exec={} --autoarranque\nIcon=audio-input-microphone\nTerminal=false\n\
                 X-GNOME-Autostart-enabled=true\nX-GNOME-Autostart-Delay=5\n",
                exe.display()
            ),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    /// `IUREVOZ_ICONOS=dir cargo test -- --ignored volcar_iconos` deja los íconos en crudo (ARGB).
    #[test]
    #[ignore]
    fn volcar_iconos() {
        let dir = std::env::var("IUREVOZ_ICONOS").unwrap();
        for (n, m) in [("inactivo", Modo::Inactivo), ("escuchando", Modo::Escuchando), ("dictando", Modo::Dictando), ("comando", Modo::Comando)] {
            std::fs::write(format!("{dir}/{n}.argb"), icono(m, 48).data).unwrap();
        }
    }

    #[test]
    fn iconos_del_tamano_pedido() {
        for m in [Modo::Inactivo, Modo::Escuchando, Modo::Dictando, Modo::Comando] {
            let i = icono(m, 22);
            assert_eq!(i.data.len(), 22 * 22 * 4);
            // esquina transparente, centro opaco
            assert_eq!(i.data[0], 0);
            let c = ((11 * 22 + 11) * 4) as usize;
            assert_eq!(i.data[c], 255);
        }
    }
}
