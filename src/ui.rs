//! Ventana de iurevoz (egui).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crossbeam_channel::Receiver;
use eframe::egui::{self, Color32, RichText};

use crate::config::{Config, Correccion, Inyeccion, ModeloAsr, Servicio};
use crate::control::{Control, Controlador, ParaUi};
use crate::correccion::Resultado;
use crate::motor::{Evento, Motor, Orden};
use crate::portal::{Aviso, Portal};
use crate::voz::{VOCES, Voz};

const PRUEBA: &str = "Prueba: áéíóú ÁÉÚ ñÑ ü ¿Sí? ¡Ya! \"ok\" (1) 2...";

#[derive(Default)]
struct PruebaTeclado {
    campo: String,
    inicio: Option<std::time::Instant>,
    enviada: bool,
    resultado: Option<(bool, String)>,
}

struct Frase {
    id: usize,
    texto: String,
    correccion: Option<Resultado>,
    error: Option<String>,
    /// Si es una instrucción del modo comando, no dictado.
    comando: Option<ComandoUi>,
}

#[derive(Default)]
struct ComandoUi {
    acciones: Vec<String>,
    respuesta: Option<String>,
    error: Option<String>,
}

pub struct App {
    cfg: Config,
    motor: Motor,
    ctl: Controlador,
    externos: Receiver<ParaUi>,
    parcial: String,
    frases: Vec<Frase>,
    estado: String,
    error: Option<String>,
    listo: bool,
    faltan: Vec<String>,
    descarga: Option<(String, f32)>,
    atajos: std::collections::HashMap<String, String>,
    ajustes: bool,
    glosario: String,
    portal: Option<Portal>,
    voz: Option<Voz>,
    esperando_comando: bool,
    prueba: PruebaTeclado,
}

impl App {
    pub fn new(cfg: Config, motor: Motor, ctl: Controlador, externos: Receiver<ParaUi>, portal: Option<Portal>, voz: Option<Voz>) -> Self {
        let glosario = cfg.correccion.glosario.join(", ");
        Self {
            cfg,
            motor,
            ctl,
            externos,
            parcial: String::new(),
            frases: Vec::new(),
            estado: "Iniciando…".into(),
            error: None,
            listo: false,
            faltan: Vec::new(),
            descarga: None,
            atajos: Default::default(),
            ajustes: false,
            glosario,
            portal,
            voz,
            esperando_comando: false,
            prueba: PruebaTeclado::default(),
        }
    }

    fn grabando(&self) -> bool {
        self.ctl.estado.grabando.load(Ordering::Relaxed)
    }

    fn alternar(&mut self) {
        self.ctl.enviar(Control::Alternar);
    }

    fn aplicar_config(&mut self) {
        self.cfg.correccion.glosario =
            self.glosario.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        // el permiso del portal lo guarda el hilo de control: no pisarlo con una copia vieja
        self.cfg.token_portal = Config::cargar().token_portal;
        if let Err(e) = self.cfg.guardar() {
            self.error = Some(format!("no pude guardar la configuración: {e}"));
        }
        let _ = self.motor.tx.send(Orden::Config(self.cfg.clone()));
        if let Some(v) = &self.voz {
            v.configurar(&self.cfg.voz.voz, self.cfg.voz.velocidad);
        }
    }

    fn texto_completo(&self) -> String {
        self.frases
            .iter()
            .filter(|f| f.comando.is_none())
            .map(|f| f.correccion.as_ref().map_or(f.texto.as_str(), |c| c.texto.as_str()).to_string())
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string()
    }

    fn procesar_eventos(&mut self) {
        while let Ok(e) = self.motor.eventos.try_recv() {
            match e {
                Evento::Estado(s) => self.estado = s,
                Evento::Listo { modelo, carga_ms } => {
                    self.listo = true;
                    self.faltan.clear();
                    self.descarga = None;
                    self.estado = format!("{modelo} listo ({:.1} s)", carga_ms as f32 / 1000.0);
                }
                Evento::FaltanModelos(f) => {
                    self.listo = false;
                    self.faltan = f;
                    self.estado = "Faltan modelos".into();
                }
                Evento::Descarga { pieza, fraccion } => self.descarga = Some((pieza, fraccion)),
                Evento::Parcial(t) => self.parcial = t,
                Evento::Frase { id, texto } => {
                    self.parcial.clear();
                    self.frases.push(Frase { id, texto, correccion: None, error: None, comando: None });
                }
                Evento::EsperandoComando => self.esperando_comando = true,
                Evento::Comando { id, instruccion } => {
                    self.parcial.clear();
                    self.esperando_comando = false;
                    self.frases.push(Frase { id, texto: instruccion, correccion: None, error: None, comando: Some(ComandoUi::default()) });
                }
                Evento::Accion { id, texto } => {
                    if let Some(c) = self.frases.iter_mut().find(|f| f.id == id).and_then(|f| f.comando.as_mut()) {
                        c.acciones.push(texto);
                    }
                }
                Evento::Respuesta { id, texto } => {
                    if let Some(c) = self.frases.iter_mut().find(|f| f.id == id).and_then(|f| f.comando.as_mut()) {
                        c.respuesta = Some(texto);
                    }
                }
                Evento::ErrorComando { id, msg } => {
                    if let Some(c) = self.frases.iter_mut().find(|f| f.id == id).and_then(|f| f.comando.as_mut()) {
                        c.error = Some(msg);
                    }
                }
                Evento::Corregida { id, res } => {
                    if let Some(f) = self.frases.iter_mut().find(|f| f.id == id) {
                        f.correccion = Some(res);
                    }
                }
                Evento::ErrorCorreccion { id, msg } => {
                    if let Some(f) = self.frases.iter_mut().find(|f| f.id == id) {
                        f.error = Some(msg);
                    }
                }
                Evento::Error(s) => {
                    self.error = Some(s);
                    self.descarga = None;
                }
            }
        }
        while let Ok(x) = self.externos.try_recv() {
            match x {
                ParaUi::AtajoAsignado(id, a) => drop(self.atajos.insert(id, a)),
                ParaUi::Error(e) => self.error = Some(e),
            }
        }
    }

    fn barra(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (texto, color) = if self.grabando() {
                ("■  Detener", Color32::from_rgb(200, 60, 60))
            } else {
                ("●  Dictar", Color32::from_rgb(40, 120, 200))
            };
            let boton = egui::Button::new(RichText::new(texto).size(18.0).color(Color32::WHITE))
                .fill(color)
                .min_size(egui::vec2(140.0, 40.0));
            if ui.add_enabled(self.listo, boton).clicked() {
                self.alternar();
            }
            let escuchando = self.ctl.estado.modo_comando.load(Ordering::Relaxed);
            let b = egui::Button::new(RichText::new(if escuchando { "… te escucho" } else { "⌘ Comando" }).size(15.0))
                .min_size(egui::vec2(110.0, 40.0));
            if ui.add_enabled(self.listo && self.cfg.comandos.activos, b).clicked() {
                self.ctl.enviar(Control::Comando);
            }
            let hablando = self.voz.as_ref().is_some_and(|v| v.hablando.load(Ordering::Relaxed));
            let b = egui::Button::new(RichText::new(if hablando { "🔇 Callar" } else { "🔊 Leer" }).size(15.0)).min_size(egui::vec2(90.0, 40.0));
            if ui.add_enabled(self.voz.is_some(), b).on_hover_text("Lee en voz alta el texto seleccionado").clicked() {
                self.ctl.enviar(Control::Leer);
            }
            ui.vertical(|ui| {
                ui.label(&self.estado);
                let nivel = f32::from_bits(self.ctl.estado.nivel.load(Ordering::Relaxed));
                let barra = egui::ProgressBar::new((nivel * 8.0).sqrt().min(1.0)).desired_width(160.0).desired_height(6.0);
                ui.add(barra);
                let a = |id: &str, def: &str| self.atajos.get(id).cloned().unwrap_or_else(|| def.to_string());
                ui.label(RichText::new(format!("Dictar {} · Comando {} · Leer {}", a("dictar", "F9"), a("comando", "—"), a("leer", "—"))).small().weak());
            });
        });
    }

    fn faltantes(&mut self, ui: &mut egui::Ui) {
        if self.faltan.is_empty() {
            return;
        }
        ui.group(|ui| {
            ui.label(format!("Faltan modelos: {}", self.faltan.join(", ")));
            match &self.descarga {
                Some((pieza, f)) => {
                    ui.add(egui::ProgressBar::new(*f).text(format!("{pieza} {:.0}%", f * 100.0)));
                }
                None => {
                    if ui.button("Descargar (~0.7–1 GB)").clicked() {
                        self.descarga = Some(("…".into(), 0.0));
                        let _ = self.motor.tx.send(Orden::Descargar);
                    }
                }
            }
        });
    }

    fn transcripcion(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for f in &self.frases {
                if let Some(c) = &f.comando {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.label(RichText::new(format!("⌘ {}", f.texto.trim())).size(15.0).strong());
                        for a in &c.acciones {
                            ui.label(RichText::new(format!("  • {a}")).small().weak());
                        }
                        match (&c.respuesta, &c.error) {
                            (_, Some(e)) => drop(ui.label(RichText::new(e).small().color(Color32::from_rgb(200, 80, 80)))),
                            (Some(r), _) => drop(ui.label(RichText::new(r).color(Color32::from_rgb(60, 140, 200)))),
                            _ => drop(ui.label(RichText::new("pensando…").small().italics().weak())),
                        }
                    });
                    continue;
                }
                let texto = f.correccion.as_ref().map_or(f.texto.trim(), |c| c.texto.trim());
                ui.label(RichText::new(texto).size(16.0));
                if let Some(c) = &f.correccion {
                    let mut nota = String::new();
                    if !c.cambios.is_empty() {
                        let lista: Vec<String> = c
                            .cambios
                            .iter()
                            .map(|(a, b)| format!("{} → {}", if a.is_empty() { "∅" } else { a }, if b.is_empty() { "∅" } else { b }))
                            .collect();
                        nota = lista.join(" · ");
                    }
                    let (pref, color) = if c.aceptada {
                        ("corregida", Color32::from_rgb(200, 150, 0))
                    } else {
                        ("descartada, cambiaba demasiado", Color32::from_rgb(200, 80, 80))
                    };
                    if !nota.is_empty() || !c.aceptada {
                        ui.label(RichText::new(format!("{pref} ({} ms): {nota}", c.ms)).small().color(color));
                    }
                }
                if let Some(e) = &f.error {
                    ui.label(RichText::new(format!("sin corregir: {e}")).small().color(Color32::from_rgb(200, 80, 80)));
                }
            }
            if self.esperando_comando || self.ctl.estado.modo_comando.load(Ordering::Relaxed) {
                ui.label(RichText::new("⌘ di tu instrucción…").italics().color(Color32::from_rgb(60, 140, 200)));
            }
            if !self.parcial.is_empty() {
                ui.label(RichText::new(&self.parcial).size(16.0).italics().weak());
            }
        });
    }

    fn panel_ajustes(&mut self, ui: &mut egui::Ui) {
        let mut cambio = false;
        egui::Grid::new("ajustes").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Modelo de voz");
            egui::ComboBox::from_id_salt("asr")
                .selected_text(match self.cfg.modelo_asr {
                    ModeloAsr::Nemotron => "Nemotron 560 ms (en vivo)",
                    ModeloAsr::Moonshine => "Moonshine (ligero)",
                })
                .show_ui(ui, |ui| {
                    cambio |= ui.selectable_value(&mut self.cfg.modelo_asr, ModeloAsr::Nemotron, "Nemotron 560 ms (en vivo)").changed();
                    cambio |= ui.selectable_value(&mut self.cfg.modelo_asr, ModeloAsr::Moonshine, "Moonshine (ligero)").changed();
                });
            ui.end_row();

            ui.label("Silencio que cierra frase");
            cambio |= ui.add(egui::Slider::new(&mut self.cfg.silencio, 0.3..=1.5).suffix(" s")).drag_stopped();
            ui.end_row();

            ui.label("Puntuación dictada");
            cambio |= ui.checkbox(&mut self.cfg.puntuacion_dictada, "«coma», «punto», «punto y aparte»…").changed();
            ui.end_row();

            ui.label("Escribir en la ventana activa");
            let mut teclear = self.cfg.inyeccion == Inyeccion::Teclear;
            if ui.checkbox(&mut teclear, "teclear el texto (portal de GNOME)").changed() {
                self.cfg.inyeccion = if teclear { Inyeccion::Teclear } else { Inyeccion::Ninguna };
                cambio = true;
            }
            ui.end_row();

            ui.label("Corrección con LLM");
            cambio |= ui.checkbox(&mut self.cfg.correccion.activa, "corregir cada frase").changed();
            ui.end_row();

            if self.cfg.correccion.activa {
                cambio |= ajustes_llm(ui, "corr", &mut self.cfg.correccion);
                ui.label("Glosario");
                cambio |= ui
                    .add(egui::TextEdit::singleline(&mut self.glosario).hint_text("amparo, desahogar, kernel…"))
                    .lost_focus();
                ui.end_row();
                ui.label("Máx. palabras cambiadas");
                cambio |= ui.add(egui::Slider::new(&mut self.cfg.correccion.max_cambio, 0.05..=0.6)).drag_stopped();
                ui.end_row();
            }

            ui.label(RichText::new("Modo comando").strong());
            cambio |= ui.checkbox(&mut self.cfg.comandos.activos, "entender instrucciones (sin shell)").changed();
            ui.end_row();
            if self.cfg.comandos.activos {
                ui.label("Prefijo hablado");
                cambio |= ui.add(egui::TextEdit::singleline(&mut self.cfg.comandos.prefijo).hint_text("computadora")).lost_focus();
                ui.end_row();
                cambio |= ajustes_llm(ui, "cmd", &mut self.cfg.comandos.llm);
            }

            ui.label(RichText::new("Voz").strong());
            cambio |= ui.checkbox(&mut self.cfg.voz.responder, "responder en voz alta").changed();
            ui.end_row();
            ui.label("Voz");
            let actual = VOCES.iter().find(|v| v.0 == self.cfg.voz.voz).map_or(self.cfg.voz.voz.as_str(), |v| v.2).to_string();
            egui::ComboBox::from_id_salt("voz").selected_text(actual).show_ui(ui, |ui| {
                for (clave, _, desc) in VOCES {
                    cambio |= ui.selectable_value(&mut self.cfg.voz.voz, clave.to_string(), *desc).changed();
                }
            });
            ui.end_row();
            ui.label("Velocidad");
            ui.horizontal(|ui| {
                cambio |= ui.add(egui::Slider::new(&mut self.cfg.voz.velocidad, 0.7..=1.6)).drag_stopped();
                if ui.button("Probar").clicked() {
                    if let Some(v) = &self.voz {
                        v.configurar(&self.cfg.voz.voz, self.cfg.voz.velocidad);
                        v.decir("Hola, soy IureVoz. Así sueno con esta voz.");
                    }
                }
            });
            ui.end_row();
        });
        self.panel_prueba(ui);
        if self.cfg.correccion.activa && self.cfg.correccion.servicio == Servicio::OpenRouter {
            ui.label(RichText::new("Con OpenRouter cada frase dictada sale de esta computadora.").small().color(Color32::from_rgb(200, 150, 0)));
        }
        if self.cfg.comandos.activos && self.cfg.comandos.llm.servicio == Servicio::OpenRouter {
            ui.label(RichText::new("Con OpenRouter cada instrucción sale de esta computadora.").small().color(Color32::from_rgb(200, 150, 0)));
        }
        if cambio {
            self.aplicar_config();
        }
    }
}

/// Servicio, modelo, URL y clave de un LLM. Devuelve true si algo cambió.
fn ajustes_llm(ui: &mut egui::Ui, id: &str, c: &mut Correccion) -> bool {
    let mut cambio = false;
    ui.label("Servicio");
    let antes = c.servicio;
    egui::ComboBox::from_id_salt(format!("servicio-{id}"))
        .selected_text(match antes {
            Servicio::Ollama => "Ollama (local)",
            Servicio::OpenRouter => "OpenRouter (nube)",
            Servicio::OpenAi => "OpenAI-compatible (llama-server…)",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut c.servicio, Servicio::Ollama, "Ollama (local)");
            ui.selectable_value(&mut c.servicio, Servicio::OpenRouter, "OpenRouter (nube)");
            ui.selectable_value(&mut c.servicio, Servicio::OpenAi, "OpenAI-compatible (llama-server…)");
        });
    if c.servicio != antes {
        c.url = Correccion::url_por_omision(c.servicio).into();
        c.modelo = match c.servicio {
            Servicio::Ollama => "gemma4:e2b",
            Servicio::OpenRouter => "deepseek/deepseek-v4.1-flash",
            Servicio::OpenAi => "local",
        }
        .into();
        cambio = true;
    }
    ui.end_row();
    ui.label("Modelo");
    cambio |= ui.text_edit_singleline(&mut c.modelo).lost_focus();
    ui.end_row();
    ui.label("URL");
    cambio |= ui.text_edit_singleline(&mut c.url).lost_focus();
    ui.end_row();
    if c.servicio != Servicio::Ollama {
        ui.label("API key");
        if std::env::var(&c.variable_clave).is_ok_and(|v| !v.is_empty()) {
            ui.label(RichText::new(format!("desde ${}", c.variable_clave)).weak());
        } else {
            let clave = c.clave.get_or_insert_with(String::new);
            cambio |= ui.add(egui::TextEdit::singleline(clave).password(true)).lost_focus();
        }
        ui.end_row();
    }
    cambio
}

impl App {
    /// Teclea un texto conocido en un campo de esta ventana y compara lo que llegó.
    fn panel_prueba(&mut self, ui: &mut egui::Ui) {
        let Some(portal) = self.portal.clone() else { return };
        ui.separator();
        let p = &mut self.prueba;
        let campo = ui.add(egui::TextEdit::singleline(&mut p.campo).hint_text("aquí escribe la prueba").desired_width(f32::INFINITY));
        ui.horizontal(|ui| {
            if ui.add_enabled(p.inicio.is_none(), egui::Button::new("Probar escritura")).clicked() {
                p.campo.clear();
                p.resultado = None;
                p.enviada = false;
                p.inicio = Some(std::time::Instant::now());
                campo.request_focus();
            }
            match &p.resultado {
                Some((true, _)) => drop(ui.label(RichText::new("✔ el texto llegó completo").color(Color32::from_rgb(60, 160, 60)))),
                Some((false, m)) => drop(ui.label(RichText::new(m).color(Color32::from_rgb(200, 80, 80)))),
                None if p.inicio.is_some() => drop(ui.label("escribiendo… (acepta el permiso de GNOME si aparece)")),
                None => {}
            }
        });
        if let Some(t) = p.inicio {
            let s = t.elapsed().as_secs_f32();
            if !p.enviada && s > 0.8 {
                p.enviada = true;
                portal.escribir(PRUEBA.to_string());
            }
            // 30 s de margen para aceptar el diálogo del portal la primera vez
            if p.enviada && (p.campo == PRUEBA || s > 30.0) {
                let ok = p.campo == PRUEBA;
                let msg = format!("llegó «{}», esperaba «{PRUEBA}»", p.campo);
                eprintln!("[iurevoz] prueba de teclado: {}", if ok { "OK".to_string() } else { msg.clone() });
                p.resultado = Some((ok, msg));
                p.inicio = None;
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.procesar_eventos();
        if ui.input(|i| i.key_pressed(egui::Key::F9)) {
            self.alternar();
        }
        egui::Panel::top("barra").show(ui, |ui| {
            ui.add_space(6.0);
            self.barra(ui);
            ui.add_space(6.0);
        });
        egui::Panel::bottom("pie").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Copiar todo").clicked() {
                    ui.ctx().copy_text(self.texto_completo());
                }
                if ui.button("Limpiar").clicked() {
                    self.frases.clear();
                    self.parcial.clear();
                }
                ui.toggle_value(&mut self.ajustes, "Ajustes");
            });
            if self.ajustes {
                ui.separator();
                self.panel_ajustes(ui);
            }
            if let Some(e) = &self.error {
                ui.label(RichText::new(e).small().color(Color32::from_rgb(200, 80, 80)));
            }
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.faltantes(ui);
            self.transcripcion(ui);
        });
        if self.grabando() || self.voz.as_ref().is_some_and(|v| v.hablando.load(Ordering::Relaxed)) {
            // el medidor y el botón de la voz se animan mientras hay actividad
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
        }
    }
}

/// Construye todo y abre la ventana.
pub fn ejecutar(cfg: Config, sin_portal: bool) -> eframe::Result {
    let opciones = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("IureVoz")
            .with_app_id(crate::config::APP_ID)
            .with_inner_size([560.0, 640.0])
            .with_min_inner_size([380.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        "IureVoz",
        opciones,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let repintar: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
            let (tx_ui, rx_ui) = crossbeam_channel::unbounded::<ParaUi>();
            let (tx_aviso, rx_aviso) = crossbeam_channel::unbounded::<Aviso>();
            let portal = (!sin_portal).then(|| {
                Portal::iniciar(cfg.token_portal.clone(), true, move |a| drop(tx_aviso.send(a)))
            });
            let voz = {
                let tx = tx_ui.clone();
                let r = repintar.clone();
                Some(Voz::iniciar(cfg.voz.voz.clone(), cfg.voz.velocidad, move |m| {
                    let _ = tx.send(ParaUi::Error(m));
                    r();
                }))
            };
            let estado = crate::control::Estado::new();
            let (tx_ctl, rx_ctl) = crossbeam_channel::unbounded::<Control>();
            let motor = Motor::iniciar(cfg.clone(), portal.clone(), voz.clone(), estado.clone(), tx_ctl.clone(), repintar.clone());
            let ctl = Controlador::iniciar(tx_ctl, rx_ctl, estado, motor.tx.clone(), voz.clone(), tx_ui.clone(), repintar.clone());
            {
                let ctl = ctl.clone();
                let r = repintar.clone();
                std::thread::spawn(move || {
                    for a in rx_aviso {
                        ctl.aviso_portal(a, &tx_ui);
                        r();
                    }
                });
            }
            crate::control::escuchar_socket(ctl.clone());
            Ok(Box::new(App::new(cfg, motor, ctl, rx_ui, portal, voz)))
        }),
    )
}
