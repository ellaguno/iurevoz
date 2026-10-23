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
    /// Los atajos personalizados de GNOME ya existen.
    atajos_gnome: bool,
    /// Pedir que se recalcule el panorama de Iurefficient (tras conectar).
    renovar_iure: bool,
    /// Formulario para entrar a Iurefficient: (contraseña, código, pide código, visible).
    entrada_iure: (String, String, bool, bool),
    /// Hay entrada en ~/.config/autostart.
    con_la_sesion: bool,
    /// F6b: ¿IureVoz está registrado como servidor MCP en Claude Code? y registro en curso.
    mcp_claude: (bool, Option<crossbeam_channel::Receiver<Result<String, String>>>),
    /// Mensajes del entrenamiento del detector en curso.
    entrenando: Option<crossbeam_channel::Receiver<Result<String, String>>>,
    progreso_entrenamiento: String,
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
            atajos_gnome: atajos_gnome_instalados(),
            renovar_iure: false,
            con_la_sesion: crate::bandeja::arranque::activo(),
            mcp_claude: (crate::mcp_voz::registrado_en_claude_code(), None),
            entrada_iure: Default::default(),
            entrenando: None,
            progreso_entrenamiento: String::new(),
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
        // progreso del entrenamiento: Ok = paso intermedio, Err = mensaje final
        if let Some(rx) = &self.entrenando {
            let mut terminado = None;
            while let Ok(m) = rx.try_recv() {
                match m {
                    Ok(p) => self.progreso_entrenamiento = p,
                    Err(fin) => terminado = Some(fin),
                }
            }
            if let Some(fin) = terminado {
                self.entrenando = None;
                self.estado = fin;
                let _ = self.motor.tx.send(Orden::RecargarDetector);
            }
        }
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
                Evento::Despierto => self.esperando_comando = true,
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
            let b = egui::Button::new(RichText::new(if escuchando { "… te escucho" } else { "💬 Comando" }).size(15.0))
                .min_size(egui::vec2(110.0, 40.0));
            if ui.add_enabled(self.listo && self.cfg.comandos.activos, b).clicked() {
                self.ctl.enviar(Control::Comando);
            }
            let vigilando = self.cfg.escucha.siempre;
            let b = egui::Button::new(RichText::new("👂").size(18.0))
                .selected(vigilando)
                .min_size(egui::vec2(40.0, 40.0));
            let ayuda = format!("Siempre escuchando: di «{}» para dar una instrucción", self.cfg.escucha.palabra);
            if ui.add_enabled(self.listo, b).on_hover_text(ayuda).clicked() {
                self.cfg.escucha.siempre = !vigilando;
                self.ctl.enviar(Control::Vigilia(self.cfg.escucha.siempre));
                self.aplicar_config();
            }
            let hablando = self.voz.as_ref().is_some_and(|v| v.hablando.load(Ordering::Relaxed));
            let b = egui::Button::new(RichText::new(if hablando { "🔇 Callar" } else { "🔊 Leer" }).size(15.0)).min_size(egui::vec2(90.0, 40.0));
            if ui.add_enabled(self.voz.is_some(), b).on_hover_text("Lee en voz alta el texto seleccionado").clicked() {
                self.ctl.enviar(Control::Leer);
            }
            ui.vertical(|ui| {
                if self.cfg.escucha.siempre && !self.grabando() {
                    ui.label(RichText::new(format!("Escuchando «{}»…", self.cfg.escucha.palabra)).color(Color32::from_rgb(60, 140, 200)));
                }
                ui.label(&self.estado);
                let nivel = f32::from_bits(self.ctl.estado.nivel.load(Ordering::Relaxed));
                let barra = egui::ProgressBar::new((nivel * 8.0).sqrt().min(1.0)).desired_width(160.0).desired_height(6.0);
                ui.add(barra);
                let texto = if !self.atajos.is_empty() {
                    let a = |id: &str| self.atajos.get(id).cloned().unwrap_or_else(|| "—".into());
                    format!("Dictar {} · Comando {} · Leer {}", a("dictar"), a("comando"), a("leer"))
                } else if self.atajos_gnome {
                    "Dictar Ctrl+Alt+Espacio · Comando Ctrl+Alt+C · Leer Ctrl+Alt+L".into()
                } else {
                    "Atajos: F9 en esta ventana (instálalos en Ajustes)".into()
                };
                ui.label(RichText::new(texto).small().weak());
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
            ui.label("Formato Markdown");
            cambio |= ui
                .checkbox(&mut self.cfg.dictado_markdown, "«título uno», «abre negritas», «casilla»… (IureEditor)")
                .changed();
            ui.end_row();

            ui.label("Escribir en la ventana activa");
            let mut teclear = self.cfg.inyeccion == Inyeccion::Teclear;
            let como = if cfg!(windows) { "teclear el texto" } else { "teclear el texto (portal de GNOME)" };
            if ui.checkbox(&mut teclear, como).changed() {
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
                ui.label("");
                ui.label(RichText::new(format!("Al dictar, una frase que empiece con «{}» también es instrucción.", self.cfg.escucha.palabra)).small().weak());
                ui.end_row();
                cambio |= ajustes_llm(ui, "cmd", &mut self.cfg.comandos.llm);
                ui.label("Claude Code");
                cambio |= ui
                    .checkbox(&mut self.cfg.comandos.claude, "delegar tareas del sistema («Claude, …»), con permiso por acción")
                    .changed();
                ui.end_row();
            }
            ui.label("Inicio");
            if ui
                .checkbox(&mut self.con_la_sesion, if cfg!(windows) { "abrir IureVoz al iniciar la sesión (solo el ícono junto al reloj, sin ventana)" } else { "abrir IureVoz al iniciar la sesión (solo el ícono arriba, sin ventana)" })
                .changed()
            {
                if let Err(e) = crate::bandeja::arranque::fijar(self.con_la_sesion) {
                    self.error = Some(format!("arranque con la sesión: {e:#}"));
                    self.con_la_sesion = crate::bandeja::arranque::activo();
                }
            }
            ui.end_row();
            ui.label("Registro");
            ui.horizontal(|ui| {
                cambio |= ui
                    .checkbox(&mut self.cfg.registro, "guardar un registro de diagnóstico")
                    .on_hover_text(if cfg!(windows) { "En %LOCALAPPDATA%\\iurevoz\\iurevoz.log. Incluye lo que dictas y lo que oye el reconocedor; no sale de tu equipo. Vale desde el próximo arranque." } else { "En ~/.local/state/iurevoz/iurevoz.log. Incluye lo que dictas y lo que oye el reconocedor; no sale de tu equipo. Vale desde el próximo arranque." })
                    .changed();
                if self.cfg.registro && ui.small_button("abrir carpeta").clicked() {
                    let dir = dirs::state_dir().or_else(dirs::data_local_dir).unwrap_or_else(crate::config::dir_datos).join("iurevoz");
                    let _ = crate::plataforma::abrir(dir);
                }
            });
            ui.end_row();
            ui.label("Voz para Claude");
            self.fila_mcp_claude(ui);
            ui.end_row();

            ui.label(RichText::new("Siempre escuchando").strong());
            if ui.checkbox(&mut self.cfg.escucha.siempre, "esperar la palabra clave").changed() {
                self.ctl.enviar(Control::Vigilia(self.cfg.escucha.siempre));
                cambio = true;
            }
            ui.end_row();
            ui.label("Palabra clave");
            cambio |= ui.add(egui::TextEdit::singleline(&mut self.cfg.escucha.palabra).hint_text("oye compu")).lost_focus();
            ui.end_row();
            ui.label("Tolerancia");
            cambio |= ui
                .add(egui::Slider::new(&mut self.cfg.escucha.tolerancia, 0.1..=0.5))
                .on_hover_text("Más alta: entiende más variantes, pero se activa más por error")
                .drag_stopped();
            ui.end_row();
            ui.label("Detector dedicado");
            ui.vertical(|ui| {
                let meta = std::fs::read_to_string(crate::detector::rutas(&self.cfg.escucha.palabra).1)
                    .ok()
                    .and_then(|t| serde_json::from_str::<crate::detector::Meta>(&t).ok());
                match &meta {
                    Some(m) => drop(ui.label(RichText::new(format!("entrenado {} · detecta {:.0} % en voces nuevas", m.fecha, m.recall * 100.0)).small())),
                    None => drop(ui.label(RichText::new("sin entrenar: se usa Moonshine (más CPU)").small().weak())),
                }
                if self.entrenando.is_some() {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label(RichText::new(&self.progreso_entrenamiento).small());
                    });
                } else if ui
                    .button(if meta.is_some() { "Volver a entrenar" } else { "Entrenar detector" })
                    .on_hover_text("Genera ejemplos con las voces locales y tus grabaciones, y entrena una red pequeña (unos minutos)")
                    .clicked()
                {
                    let (tx, rx) = crossbeam_channel::unbounded();
                    let palabra = self.cfg.escucha.palabra.clone();
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let t2 = tx.clone();
                        let c2 = ctx.clone();
                        let r = crate::entrenamiento::entrenar(&palabra, &move |m| {
                            let _ = t2.send(Ok(m));
                            c2.request_repaint();
                        });
                        let _ = tx.send(Err(match r {
                            Ok(m) => format!("listo: detecta {:.0} % en voces nuevas", m.recall * 100.0),
                            Err(e) => format!("falló el entrenamiento: {e:#}"),
                        }));
                        ctx.request_repaint();
                    });
                    self.entrenando = Some(rx);
                    self.progreso_entrenamiento = "empezando…".into();
                }
            });
            ui.end_row();

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
        ui.separator();
        self.panel_suite(ui);
        ui.separator();
        #[cfg(target_os = "linux")]
        ui.horizontal(|ui| {
            let t = if self.atajos_gnome { "Reinstalar atajos de GNOME" } else { "Instalar atajos de GNOME" };
            if ui.button(t).on_hover_text("Crea en Configuración → Teclado los atajos Ctrl+Alt+Espacio (dictar), Ctrl+Alt+C (instrucción) y Ctrl+Alt+L (leer)").clicked() {
                match crate::atajos_gnome::instalar() {
                    Ok(m) => {
                        self.atajos_gnome = true;
                        self.estado = m;
                    }
                    Err(e) => self.error = Some(format!("{e:#}")),
                }
            }
            if self.atajos_gnome {
                ui.label(RichText::new("instalados").small().color(Color32::from_rgb(60, 160, 60)));
            }
        });
        self.panel_prueba(ui);
        if self.cfg.correccion.activa && self.cfg.correccion.servicio == Servicio::OpenRouter {
            ui.label(RichText::new("Con OpenRouter cada frase dictada sale de esta computadora.").small().color(Color32::from_rgb(200, 150, 0)));
        }
        if self.cfg.comandos.activos && self.cfg.comandos.claude {
            ui.label(RichText::new("Con Claude Code la tarea y lo que Claude lea salen a los servidores de Anthropic, con tu cuenta.").small().color(Color32::from_rgb(200, 150, 0)));
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
            Servicio::Local => "Local (integrado)",
            Servicio::Ollama => "Ollama (local)",
            Servicio::OpenRouter => "OpenRouter (nube)",
            Servicio::OpenAi => "OpenAI-compatible (llama-server…)",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut c.servicio, Servicio::Local, "Local (integrado)");
            ui.selectable_value(&mut c.servicio, Servicio::Ollama, "Ollama (local)");
            ui.selectable_value(&mut c.servicio, Servicio::OpenRouter, "OpenRouter (nube)");
            ui.selectable_value(&mut c.servicio, Servicio::OpenAi, "OpenAI-compatible (llama-server…)");
        });
    if c.servicio != antes {
        c.url = Correccion::url_por_omision(c.servicio).into();
        c.modelo = match c.servicio {
            Servicio::Local => "gemma4-e2b",
            Servicio::Ollama => "gemma4:e2b",
            Servicio::OpenRouter => "deepseek/deepseek-v4.1-flash",
            Servicio::OpenAi => "local",
        }
        .into();
        cambio = true;
    }
    ui.end_row();
    if c.servicio == Servicio::Local {
        use crate::llm_local::MODELOS;
        ui.label("Modelo");
        let actual = MODELOS.iter().find(|m| m.0 == c.modelo).map_or(c.modelo.as_str(), |m| m.3).to_string();
        egui::ComboBox::from_id_salt(format!("local-{id}")).selected_text(actual).show_ui(ui, |ui| {
            for (clave, _, _, desc) in MODELOS {
                cambio |= ui.selectable_value(&mut c.modelo, clave.to_string(), *desc).changed();
            }
        });
        ui.end_row();
        ui.label("");
        ui.horizontal(|ui| match crate::llm_local::preparando() {
            Some((que, f)) => {
                ui.add(egui::ProgressBar::new(f).text(format!("{que} {:.0}%", f * 100.0)).desired_width(220.0));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
            None => match crate::llm_local::estado(&c.modelo) {
                (true, true) => drop(ui.label(RichText::new("listo · se arranca solo al usarlo").small().color(Color32::from_rgb(60, 160, 60)))),
                _ => {
                    if ui.button("Descargar").on_hover_text("llama.cpp (35 MB) y el modelo, una sola vez").clicked() {
                        let ctx = ui.ctx().clone();
                        crate::llm_local::preparar_en_fondo(c.modelo.clone(), move || ctx.request_repaint());
                    }
                    ui.label(RichText::new("falta descargar").small().weak());
                }
            },
        });
        ui.end_row();
        return cambio;
    }
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
    /// Qué ve IureVoz de la suite Iurefficient: apps instaladas, unidades de IureDav y cuenta.
    /// Los datos se calculan en otro hilo (`iure::panorama`): aquí solo se dibujan.
    /// F6b: registrar `iurevoz --mcp` en Claude Code (corre `claude mcp add` aparte).
    fn fila_mcp_claude(&mut self, ui: &mut egui::Ui) {
        if let Some(rx) = &self.mcp_claude.1 {
            match rx.try_recv() {
                Ok(Ok(m)) => {
                    self.estado = m;
                    self.mcp_claude = (crate::mcp_voz::registrado_en_claude_code(), None);
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.mcp_claude.1 = None;
                }
                Err(_) => {
                    ui.spinner();
                    return;
                }
            }
        }
        ui.horizontal(|ui| {
            if self.mcp_claude.0 {
                ui.label("✔ Claude Code puede hablarte, preguntarte y tomarte dictado");
            } else if ui
                .button("Registrar en Claude Code")
                .on_hover_text("Agrega IureVoz como servidor MCP (decir, preguntar sí/no, preguntar por voz, dictar) con `claude mcp add --scope user`")
                .clicked()
            {
                let (tx, rx) = crossbeam_channel::bounded(1);
                std::thread::spawn(move || drop(tx.send(crate::mcp_voz::registrar_en_claude_code().map_err(|e| format!("{e:#}")))));
                self.mcp_claude.1 = Some(rx);
            }
        });
    }

    fn panel_suite(&mut self, ui: &mut egui::Ui) {
        let verde = Color32::from_rgb(60, 160, 60);
        egui::CollapsingHeader::new(RichText::new("Iurefficient").strong()).default_open(false).show(ui, |ui| {
            let ctx = ui.ctx().clone();
            let renovar = std::mem::take(&mut self.renovar_iure);
            let Some(p) = crate::iure::panorama(renovar, move || ctx.request_repaint()) else {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new("consultando apps, unidades y cuenta…").small());
                });
                return;
            };
            ui.label(RichText::new("Apps de escritorio (se detectan solas, sin configurar)").small().weak());
            for a in &p.apps {
                ui.horizontal(|ui| {
                    if a.installed {
                        ui.label(RichText::new(format!("✔ {}", a.name)).color(verde));
                        ui.label(RichText::new(a.path.clone().unwrap_or_default()).small().weak());
                    } else {
                        ui.label(format!("✖ {} no instalada", a.name));
                        if ui.small_button("Descargar").clicked() {
                            let _ = crate::plataforma::abrir(&a.download_url);
                        }
                    }
                });
            }
            ui.add_space(4.0);
            if p.unidades.is_empty() {
                ui.label(RichText::new("IureDav: sin unidades configuradas").small());
            }
            for m in &p.unidades {
                let estado = if m.mounted { "montada" } else { "sin montar: ábrela en IureDav" };
                ui.label(RichText::new(format!("Unidad «{}» en {} · {estado}", m.name, m.mount_point)).small());
            }
            ui.add_space(4.0);
            if let Some(c) = &p.cuenta {
                ui.label(RichText::new(format!("Cuenta de la suite: {} en {} (guardada por {})", c.email, c.domain, c.app)).small());
            }
            use crate::iure::Estado;
            match p.estado.clone() {
                Estado::Conectado(q) => {
                    ui.label(RichText::new(format!("✔ IureVoz conectado a la instancia ({q})")).color(verde));
                    let mut cambio = false;
                    cambio |= ui
                        .checkbox(&mut self.cfg.iure.resumen_al_iniciar, "decir «tu día» al abrir IureVoz")
                        .on_hover_text("Citas, entregas de hoy, tareas atrasadas y notificaciones. También con «¿qué tengo hoy?»")
                        .changed();
                    ui.horizontal(|ui| {
                        cambio |= ui.checkbox(&mut self.cfg.iure.avisos, "avisar vencimientos y notificaciones").changed();
                        ui.add_enabled_ui(self.cfg.iure.avisos, |ui| {
                            cambio |= ui.checkbox(&mut self.cfg.iure.avisos_hablados, "también en voz alta").changed();
                            ui.label("cada");
                            cambio |= ui.add(egui::DragValue::new(&mut self.cfg.iure.cada_minutos).range(5..=120).suffix(" min")).changed();
                        });
                    });
                    if cambio {
                        self.aplicar_config();
                    }
                }
                Estado::PorConectar(q) if !self.entrada_iure.3 => {
                    ui.horizontal(|ui| {
                        if ui.button("Conectar IureVoz").on_hover_text("Crea un token de solo lectura «IureVoz» (un año, revocable en Iurefficient)").clicked() {
                            match crate::iure::conectar() {
                                Ok(m) => self.estado = m,
                                Err(e) => {
                                    // sesión vencida: se ofrece entrar con contraseña
                                    self.error = Some(format!("{e:#}"));
                                    self.entrada_iure.3 = true;
                                }
                            }
                            self.renovar_iure = true;
                        }
                        ui.label(RichText::new(format!("con la sesión de la suite ({q})")).small());
                    });
                }
                Estado::PorConectar(q) | Estado::SinCredenciales(q) => {
                    ui.label(RichText::new(format!("Entra a Iurefficient ({q}). La contraseña no se guarda; IureVoz crea un token de solo lectura.")).small());
                    let (clave, codigo, pide_codigo, _) = &mut self.entrada_iure;
                    let mut resultado = None;
                    ui.horizontal(|ui| {
                        if !*pide_codigo {
                            let r = ui.add(egui::TextEdit::singleline(clave).password(true).hint_text("contraseña").desired_width(180.0));
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if (ui.button("Entrar").clicked() || enter) && !clave.is_empty() {
                                resultado = Some(crate::iure::entrar(clave).map(|e| match e {
                                    crate::iure::Entrada::Listo(m) => Some(m),
                                    crate::iure::Entrada::PideCodigo => None,
                                }));
                                clave.clear();
                            }
                        } else {
                            let r = ui.add(egui::TextEdit::singleline(codigo).hint_text("código de verificación").desired_width(140.0));
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if (ui.button("Verificar").clicked() || enter) && !codigo.is_empty() {
                                resultado = Some(crate::iure::verificar_codigo(codigo).map(Some));
                                codigo.clear();
                            }
                        }
                    });
                    match resultado {
                        Some(Ok(Some(m))) => {
                            self.estado = m;
                            self.entrada_iure = Default::default();
                            self.renovar_iure = true;
                            self.error = None;
                        }
                        Some(Ok(None)) => self.entrada_iure.2 = true,
                        Some(Err(e)) => {
                            self.error = Some(format!("{e:#}"));
                            self.entrada_iure.2 = false;
                        }
                        None => {}
                    }
                }
                Estado::SinCuenta => drop(ui.label(RichText::new("Sin cuenta de Iurefficient en este equipo: entra desde IureTranscribe o IureDav.").small())),
            }
        });
    }

    /// Tareas de Claude corriendo en segundo plano.
    fn tareas_claude(&mut self, ui: &mut egui::Ui) {
        let tareas = crate::claude::tareas();
        if tareas.is_empty() {
            return;
        }
        for t in tareas {
            egui::Frame::group(ui.style()).fill(Color32::from_rgba_unmultiplied(60, 140, 200, 25)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    let s = t.inicio.elapsed().as_secs();
                    ui.label(RichText::new(format!("Claude trabajando · {}:{:02}", s / 60, s % 60)).strong());
                    if ui.button("■ Detener").on_hover_text("Termina la tarea y lo que Claude haya lanzado").clicked() {
                        crate::claude::detener(t.num);
                    }
                });
                ui.label(RichText::new(&t.resumen).small());
                let ultimo: String = t.ultimo.chars().take(140).collect();
                ui.label(RichText::new(format!("↳ {ultimo}")).small().weak());
            });
        }
        ui.add_space(6.0);
        // el reloj y el indicador se actualizan aunque no lleguen eventos
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    }

    /// Permiso pendiente (Claude o una acción que crea archivos): también se responde con «sí» o «no».
    fn permiso(&mut self, ui: &mut egui::Ui) {
        let desc = self.ctl.estado.pendiente.lock().unwrap().as_ref().map(|(_, d, _)| d.clone());
        let Some(desc) = desc else { return };
        egui::Frame::group(ui.style()).fill(Color32::from_rgba_unmultiplied(200, 150, 0, 30)).show(ui, |ui| {
            ui.label(RichText::new("Se pide tu permiso").strong());
            ui.label(RichText::new(&desc).size(15.0));
            ui.horizontal(|ui| {
                if ui.button(RichText::new("✔ Permitir").size(15.0)).clicked() {
                    self.ctl.enviar(Control::Responder(true));
                }
                if ui.button(RichText::new("✖ Rechazar").size(15.0)).clicked() {
                    self.ctl.enviar(Control::Responder(false));
                }
                ui.label(RichText::new("o di «sí» / «no»").small().weak());
            });
        });
        ui.add_space(6.0);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
    }

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
                None if p.inicio.is_some() && cfg!(windows) => drop(ui.label("escribiendo…")),
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
        // Esc calla la voz (con el micrófono en silencio mientras habla, no oiría «cállate»)
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Some(v) = &self.voz {
                v.callar();
            }
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
                // con muchas opciones el panel no cabe en ventanas chicas: se desplaza
                let alto = ui.ctx().content_rect().height() * 0.65;
                egui::ScrollArea::vertical().max_height(alto).auto_shrink([false, true]).show(ui, |ui| {
                    self.panel_ajustes(ui);
                });
            }
            if let Some(e) = &self.error {
                ui.label(RichText::new(e).small().color(Color32::from_rgb(200, 80, 80)));
            }
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.faltantes(ui);
            self.tareas_claude(ui);
            self.permiso(ui);
            self.transcripcion(ui);
        });
        if self.grabando() || self.voz.as_ref().is_some_and(|v| v.hablando.load(Ordering::Relaxed)) {
            // el medidor y el botón de la voz se animan mientras hay actividad
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
        }
    }
}

/// Construye todo y abre la ventana.
/// Contexto de la ventana abierta (ninguno mientras está cerrada y solo queda el ícono).
static VENTANA: std::sync::Mutex<Option<egui::Context>> = std::sync::Mutex::new(None);
/// Avisos al hilo principal mientras no hay ventana: abrirla (o salir, si `bandeja::SALIENDO`).
static ABRIR: std::sync::OnceLock<(crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>)> = std::sync::OnceLock::new();

fn canal_abrir() -> &'static (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>) {
    ABRIR.get_or_init(crossbeam_channel::unbounded)
}

/// «Mostrar IureVoz» (ícono o un segundo arranque): abre la ventana, o la trae al frente si ya
/// está (en Wayland GNOME puede responder con el aviso «IureVoz está listo»).
pub fn mostrar_ventana() {
    if let Some(ctx) = VENTANA.lock().unwrap().as_ref() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    } else {
        let _ = canal_abrir().0.send(());
    }
}

/// «Salir» del ícono: cierra la ventana (si la hay) y termina.
pub fn salir() {
    crate::bandeja::SALIENDO.store(true, Ordering::Relaxed);
    if let Some(ctx) = VENTANA.lock().unwrap().as_ref() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        ctx.request_repaint();
    }
    let _ = canal_abrir().0.send(());
}

/// La ventana se crea y se destruye; el estado (frases, ajustes, tareas) vive en `App`.
struct Ventana(std::rc::Rc<std::cell::RefCell<App>>);

impl eframe::App for Ventana {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut *self.0.borrow_mut(), ui, frame);
    }
}

/// ¿Ya están los atajos personalizados de GNOME? (En Windows los registra la app.)
fn atajos_gnome_instalados() -> bool {
    #[cfg(target_os = "linux")]
    return crate::atajos_gnome::instalados();
    #[cfg(windows)]
    false
}

/// Arranca motor, control, voz e ícono y abre la ventana. En Wayland una ventana no se puede
/// esconder: con el ícono en la barra, cerrarla la destruye y «Mostrar IureVoz» crea otra,
/// como IureDav. `oculta` (arranque con la sesión) empieza solo con el ícono.
pub fn ejecutar(cfg: Config, sin_portal: bool, oculta: bool) -> eframe::Result {
    let repintar: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
        if let Some(ctx) = VENTANA.lock().unwrap().as_ref() {
            ctx.request_repaint();
        }
    });
    let (tx_ui, rx_ui) = crossbeam_channel::unbounded::<ParaUi>();
    let (tx_aviso, rx_aviso) = crossbeam_channel::unbounded::<Aviso>();
    let portal = (!sin_portal).then(|| {
        // con los atajos de GNOME instalados no hace falta el portal (que en GNOME 50 no responde)
        let atajo_portal = !atajos_gnome_instalados();
        Portal::iniciar(cfg.token_portal.clone(), atajo_portal, move |a| drop(tx_aviso.send(a)))
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
    let ctl_estado = estado.clone();
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
    if cfg.escucha.siempre {
        ctl.enviar(Control::Vigilia(true));
    }
    // F5: resumen del día al abrir y avisos de Iurefficient
    {
        let e = ctl_estado.clone();
        crate::resumen::arrancar(voz.clone(), move || {
            use std::sync::atomic::Ordering::Relaxed;
            // no hablar encima de un dictado o de una instrucción
            e.grabando.load(Relaxed) || e.atento.load(Relaxed) || e.pendiente.lock().unwrap().is_some()
        });
    }
    // índice de nombres de la unidad de IureDav (recorrerla es lento)
    crate::suite::indexar_en_fondo();
    // LLM local: se arranca y precalienta en segundo plano para que la primera instrucción sea rápida
    if cfg.comandos.activos && cfg.comandos.llm.servicio == Servicio::Local {
        let c = cfg.clone();
        std::thread::spawn(move || {
            let mut ag = crate::agente::Agente::new(c.comandos.llm.clone(), None, None);
            if c.comandos.claude {
                // mismas herramientas que en uso real, para que el prefijo en caché coincida
                ag = ag.con_claude(Box::new(|_| Ok(String::new())));
            }
            if let Err(e) = ag.precalentar() {
                eprintln!("precalentar LLM local: {e:#}");
            }
        });
    }
    // ícono en la barra superior, con el estado (dictando, instrucción, «oye compu»)
    crate::bandeja::iniciar(ctl.clone());
    let app = std::rc::Rc::new(std::cell::RefCell::new(App::new(cfg, motor, ctl, rx_ui, portal, voz)));
    let inicio = std::time::Instant::now();
    let mut abrir = !oculta;
    let mut abierta_alguna_vez = false;
    loop {
        if abrir {
            let a = app.clone();
            let opciones = eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_title("IureVoz")
                    .with_app_id(crate::config::APP_ID)
                    .with_inner_size([560.0, 640.0])
                    .with_min_inner_size([380.0, 320.0]),
                run_and_return: true,
                ..Default::default()
            };
            eframe::run_native(
                "IureVoz",
                opciones,
                Box::new(move |cc| {
                    *VENTANA.lock().unwrap() = Some(cc.egui_ctx.clone());
                    Ok(Box::new(Ventana(a)))
                }),
            )?;
            *VENTANA.lock().unwrap() = None;
            abrir = false;
            abierta_alguna_vez = true;
        }
        // sin ícono no habría cómo volver a abrirla: cerrar la ventana es salir
        if crate::bandeja::SALIENDO.load(Ordering::Relaxed) || (abierta_alguna_vez && !crate::bandeja::ACTIVA.load(Ordering::Relaxed)) {
            return Ok(());
        }
        // sin ventana: los eventos del motor se siguen atendiendo hasta que pidan abrirla
        match canal_abrir().1.recv_timeout(std::time::Duration::from_millis(400)) {
            Ok(()) => abrir = !crate::bandeja::SALIENDO.load(Ordering::Relaxed),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                app.borrow_mut().procesar_eventos();
                // arranque oculto y el ícono no aparece (sin extensión de indicadores, o tarda
                // al iniciar sesión): se abre una vez para que IureVoz no quede invisible
                if !abierta_alguna_vez && !crate::bandeja::ACTIVA.load(Ordering::Relaxed) && inicio.elapsed() > std::time::Duration::from_secs(30) {
                    abrir = true;
                }
            }
            Err(_) => return Ok(()),
        }
    }
}
