//! Orquestación: audio → ASR → puntuación dictada → (corrección LLM) → inyección.
//!
//! Tres hilos: el de ASR (latencia crítica, nunca espera a la red), el de salida, que
//! corrige e inyecta frase por frase en orden, y el del agente, que atiende instrucciones.

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};
use sherpa_onnx::LinearResampler;

use crate::agente::Agente;
use crate::asr::{Reconocedor, SR, Salida};
use crate::control::{Control, Estado};
use crate::config::{Config, Inyeccion};
use crate::correccion::{Corrector, Resultado};
use crate::modelos;
use crate::portal::Portal;
use crate::puntuacion::Puntuador;
use crate::voz::Voz;

pub enum Orden {
    Audio { muestras: Vec<f32>, tasa: u32 },
    /// Audio en modo «siempre escuchando»: solo se busca la palabra clave.
    Vigilia { muestras: Vec<f32>, tasa: u32 },
    /// Audio real mientras la app habla: solo se buscan «cállate», «silencio»…
    Interrumpir { muestras: Vec<f32>, tasa: u32 },
    /// Fin del dictado: sale la última frase y se reinicia la puntuación.
    Fin,
    /// Nueva configuración (recarga el modelo si cambió).
    Config(Config),
    /// Se entrenó un detector nuevo: volver a cargarlo.
    RecargarDetector,
    Descargar,
}

#[derive(Debug, Clone)]
pub enum Evento {
    Estado(String),
    Listo { modelo: String, carga_ms: u128 },
    FaltanModelos(Vec<String>),
    Descarga { pieza: String, fraccion: f32 },
    Parcial(String),
    Frase { id: usize, texto: String },
    Corregida { id: usize, res: Resultado },
    ErrorCorreccion { id: usize, msg: String },
    /// Instrucción recibida en modo comando.
    Comando { id: usize, instruccion: String },
    /// El modo comando espera la instrucción (se dijo solo el prefijo).
    EsperandoComando,
    /// Se oyó la palabra clave.
    Despierto,
    Accion { id: usize, texto: String },
    Respuesta { id: usize, texto: String },
    ErrorComando { id: usize, msg: String },
    Error(String),
}

pub struct Motor {
    pub tx: Sender<Orden>,
    pub eventos: Receiver<Evento>,
}

type Notificar = Arc<dyn Fn() + Send + Sync>;

impl Motor {
    pub fn iniciar(
        cfg: Config,
        portal: Option<Portal>,
        voz: Option<Voz>,
        estado: Estado,
        control: Sender<Control>,
        notificar: Notificar,
    ) -> Self {
        let (tx, rx) = unbounded::<Orden>();
        let (tx_ev, eventos) = unbounded::<Evento>();
        let (tx_sal, rx_sal) = unbounded::<(usize, String, Config)>();
        let (tx_ag, rx_ag) = unbounded::<(usize, String, Config)>();
        let control_ag = control.clone();

        let ev = Emisor { tx: tx_ev.clone(), notificar: notificar.clone() };
        let rutas = Rutas { salida: tx_sal, agente: tx_ag, estado, control };
        std::thread::Builder::new()
            .name("asr".into())
            .spawn(move || hilo_asr(cfg, rx, rutas, ev))
            .expect("hilo asr");

        let ev = Emisor { tx: tx_ev.clone(), notificar: notificar.clone() };
        let p = portal.clone();
        std::thread::Builder::new()
            .name("salida".into())
            .spawn(move || hilo_salida(rx_sal, p, ev))
            .expect("hilo salida");

        let ev = Emisor { tx: tx_ev, notificar };
        std::thread::Builder::new()
            .name("agente".into())
            .spawn(move || hilo_agente(rx_ag, portal, voz, control_ag, ev))
            .expect("hilo agente");

        Self { tx, eventos }
    }
}

#[derive(Clone)]
struct Emisor {
    tx: Sender<Evento>,
    notificar: Notificar,
}

impl Emisor {
    fn enviar(&self, e: Evento) {
        if std::env::var_os("IUREVOZ_LOG").is_some() && !matches!(e, Evento::Parcial(_) | Evento::Descarga { .. }) {
            eprintln!("[iurevoz] {e:?}");
        }
        let _ = self.tx.send(e);
        (self.notificar)();
    }
}

/// «Sí», «adelante», «permítelo» → true; «no», «cancela», «alto» → false; lo demás, None.
fn si_o_no(frase: &str) -> Option<bool> {
    let p = crate::correccion::palabras(frase);
    let primera = p.first()?.as_str();
    match primera {
        "sí" | "si" | "claro" | "adelante" | "dale" | "ok" | "okay" | "va" | "hazlo" | "permitido" | "permítelo" | "permitelo" | "acepto" | "correcto" | "afirmativo" | "órale" => Some(true),
        "no" | "cancela" | "cancelar" | "alto" | "detente" | "niega" | "nada" | "negativo" | "espera" => Some(false),
        "permite" | "permitir" => Some(true),
        _ => None,
    }
}

/// «Claude, revisa…» (y variantes que oye el ASR): devuelve la tarea.
fn para_claude(instruccion: &str) -> Option<String> {
    let p = crate::correccion::palabras(instruccion);
    let primera = p.first()?;
    if !matches!(primera.as_str(), "claude" | "cloud" | "clod" | "clode" | "claud" | "clau" | "glod" | "klod" | "clave" | "claudio") {
        return None;
    }
    let resto = instruccion.trim_start_matches(|c: char| !c.is_alphanumeric());
    let resto = resto.trim_start_matches(char::is_alphanumeric);
    let resto = resto.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    (!resto.is_empty()).then(|| resto.to_string())
}

/// Tiempo máximo esperando una instrucción que no empieza.
const ESPERA_INSTRUCCION: std::time::Duration = std::time::Duration::from_secs(8);

/// ¿El texto es solo (parte de) la palabra clave? «Oye», «Oye, compu», «compu».
fn solo_palabra_clave(texto: &str, palabra: &str) -> bool {
    let clave = crate::correccion::palabras(palabra);
    let t = crate::correccion::palabras(texto);
    !t.is_empty() && t.len() <= clave.len() && t.iter().all(|w| clave.contains(w))
}

/// ¿La respuesta espera que el usuario conteste?
fn invita_a_contestar(r: &str) -> bool {
    let r = r.trim();
    let bajo = r.to_lowercase();
    r.ends_with('?') || ["dime ", "cuéntame", "cuentame", "¿cuál", "¿qué", "¿quieres"].iter().any(|p| bajo.starts_with(p) || bajo.contains(&format!(". {p}")))
}

/// Tiempo máximo esperando la respuesta a una pregunta de otro programa (incluye oírla).
const ESPERA_RESPUESTA: std::time::Duration = std::time::Duration::from_secs(25);

/// Silencio tras la última frase que da por terminada una instrucción.
const FIN_INSTRUCCION: std::time::Duration = std::time::Duration::from_millis(1000);

struct EnCurso {
    texto: String,
    ultimo: std::time::Instant,
    hablando: bool,
}

impl EnCurso {
    fn new(t: &str) -> Self {
        Self { texto: t.trim().to_string(), ultimo: std::time::Instant::now(), hablando: false }
    }

    fn agregar(&mut self, t: &str) {
        // «¿Cuánto es el quince por ciento» + «de 200»: el signo de cierre sobra a media frase
        let base = self.texto.trim_end_matches(['.', '?', '!']).to_string();
        self.texto = format!("{base} {}", t.trim());
        self.ultimo = std::time::Instant::now();
        self.hablando = false;
    }
}

/// A dónde va cada frase final.
struct Rutas {
    salida: Sender<(usize, String, Config)>,
    agente: Sender<(usize, String, Config)>,
    estado: Estado,
    control: Sender<Control>,
}


fn hilo_asr(mut cfg: Config, rx: Receiver<Orden>, rutas: Rutas, ev: Emisor) {
    let mut rec = cargar(&cfg, &ev);
    let mut punt = Puntuador::new();
    let mut remuestreo: Option<(u32, LinearResampler)> = None;
    let mut id = 0usize;
    // IUREVOZ_GRABAR=1 guarda el audio de cada dictado (solo local) para depurar el ASR
    let grabar = std::env::var_os("IUREVOZ_GRABAR").is_some();
    let mut grabacion: Vec<f32> = Vec::new();

    // instrucción que se va armando con varias frases (el usuario puede pausar para pensar)
    let mut instr: Option<EnCurso> = None;
    // detector de palabra clave; se crea al primer audio de vigilia
    let mut vigilia: Option<crate::vigilia::Vigilia> = None;
    // audio real mientras la app habla, para oír «cállate»: se revisan los últimos 1.6 s cada segundo
    let mut barrera: Vec<f32> = Vec::new();
    let mut barrera_nuevo = 0usize;
    // desde cuándo se espera una instrucción que no llega
    let mut esperando_desde: Option<std::time::Instant> = None;
    let mut ultimo_envio: Option<std::time::Instant> = None;
    // última vez que el reconocedor oyó voz (texto parcial): una respuesta en curso no se corta
    let mut oyo_voz = std::time::Instant::now();
    let modo = |r: &Rutas| r.estado.modo_comando.load(std::sync::atomic::Ordering::Relaxed);

    let emitir = |salidas: Vec<Salida>, cfg: &Config, punt: &mut Puntuador, instr: &mut Option<EnCurso>, id: &mut usize| {
        for s in salidas {
            match s {
                Salida::Parcial(t) => {
                    if let Some(e) = instr.as_mut() {
                        e.hablando = !t.is_empty();
                        if e.hablando {
                            e.ultimo = std::time::Instant::now();
                        }
                    }
                    ev.enviar(Evento::Parcial(t))
                }
                Salida::Final(t) => {
                    if std::env::var_os("IUREVOZ_LOG").is_some() {
                        eprintln!("[iurevoz] asr: {t:?}");
                    }
                    // ¿respuesta a un permiso pendiente?
                    if rutas.estado.pendiente.lock().unwrap().is_some() {
                        if let Some(si) = si_o_no(&t) {
                            let _ = rutas.control.send(Control::Responder(si));
                        }
                        continue; // mientras se espera el permiso nada se dicta
                    }
                    // continuación de una instrucción en curso
                    if let Some(e) = instr.as_mut() {
                        e.agregar(&t);
                        continue;
                    }
                    // ¿respuesta que espera otro programa? (F6b) Se toma tal cual, sin palabra clave
                    if rutas.estado.captura.lock().unwrap().is_some() {
                        *instr = Some(EnCurso::new(&t));
                        continue;
                    }
                    // ¿empieza una instrucción?
                    let palabra = &cfg.escucha.palabra;
                    let inicio = if modo(&rutas) {
                        // Ya se sabe que viene una instrucción: la palabra clave se quita aunque el
                        // ASR la haya oído mal («Oye Ku»), con más tolerancia que al dictar.
                        let tol = cfg.escucha.tolerancia.max(0.5);
                        Some(crate::vigilia::quitar(&t, palabra, tol).unwrap_or_else(|| t.clone()))
                    } else if cfg.comandos.activos {
                        // al dictar, la misma palabra clave al inicio de una frase la vuelve instrucción
                        crate::vigilia::quitar(&t, palabra, cfg.escucha.tolerancia)
                    } else {
                        None
                    };
                    if let Some(i) = inicio {
                        rutas.estado.modo_comando.store(true, std::sync::atomic::Ordering::Relaxed);
                        // «Oye» o «compu» sueltos son restos de la palabra clave, no la instrucción
                        let i = if solo_palabra_clave(&i, palabra) { String::new() } else { i };
                        if i.is_empty() {
                            // solo se dijo el prefijo: se espera la instrucción
                            ev.enviar(Evento::EsperandoComando);
                        } else {
                            *instr = Some(EnCurso::new(&i));
                        }
                        continue;
                    }
                    // al dictar, «(oye compu,) termina el dictado» es una orden, no texto: el ASR oye
                    // la palabra clave mal («Oye Ku») y pedirla exacta dejaba escrito el «deja de dictar»
                    {
                        let resto = crate::vigilia::quitar(&t, palabra, cfg.escucha.tolerancia.max(0.5)).unwrap_or_else(|| t.clone());
                        if crate::correccion::palabras(&resto).len() <= 6 && crate::agente::es_fin_dictado(&resto) {
                            ev.enviar(Evento::Estado("Dictado terminado.".into()));
                            let _ = rutas.control.send(Control::Dictar(false));
                            continue;
                        }
                    }
                    punt.markdown = cfg.dictado_markdown;
                    let texto = punt.procesar(&t, cfg.puntuacion_dictada);
                    // «nuevo párrafo» solo produce saltos de línea: no se descarta
                    if texto.is_empty() {
                        continue;
                    }
                    *id += 1;
                    ev.enviar(Evento::Frase { id: *id, texto: texto.clone() });
                    let _ = rutas.salida.send((*id, texto, cfg.clone()));
                }
            }
        }
    };

    // Manda la instrucción armada al agente.
    let despachar = |instr: &mut Option<EnCurso>, cfg: &Config, id: &mut usize| {
        let Some(e) = instr.take() else { return };
        rutas.estado.modo_comando.store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = rutas.control.send(Control::FinComando);
        let i = e.texto.trim().to_string();
        if let Some((tx, _)) = rutas.estado.captura.lock().unwrap().take() {
            ev.enviar(Evento::Estado(format!("Respuesta: «{i}»")));
            let _ = tx.send(i);
            return;
        }
        if i.is_empty() {
            return;
        }
        *id += 1;
        ev.enviar(Evento::Comando { id: *id, instruccion: i.clone() });
        let _ = rutas.agente.send((*id, i, cfg.clone()));
    };

    fn a_16k(remuestreo: &mut Option<(u32, LinearResampler)>, muestras: Vec<f32>, tasa: u32) -> Option<Vec<f32>> {
        if tasa as i32 == SR {
            return Some(muestras);
        }
        if remuestreo.as_ref().is_none_or(|(t, _)| *t != tasa) {
            *remuestreo = LinearResampler::create(tasa as i32, SR).map(|r| (tasa, r));
        }
        remuestreo.as_ref().map(|(_, rs)| rs.resample(&muestras, false))
    }

    for orden in rx {
        match orden {
            Orden::Vigilia { muestras, tasa } => {
                let Some(r) = rec.as_mut() else { continue };
                let Some(m16) = a_16k(&mut remuestreo, muestras, tasa) else { continue };
                if vigilia.is_none() {
                    match modelos::rutas_moonshine().and_then(|m| crate::vigilia::Vigilia::new(&m.vad)) {
                        Ok(mut v) => {
                            let dedicado = v.cargar_detector(&cfg.escucha.palabra);
                            ev.enviar(Evento::Estado(format!(
                                "Esperando «{}» ({})",
                                cfg.escucha.palabra,
                                if dedicado { "detector dedicado" } else { "verificación con Moonshine" }
                            )));
                            vigilia = Some(v)
                        }
                        Err(e) => {
                            ev.enviar(Evento::Error(format!("siempre escuchando: {e:#}")));
                            continue;
                        }
                    }
                }
                let v = vigilia.as_mut().unwrap();
                let (palabra, tol) = (cfg.escucha.palabra.clone(), cfg.escucha.tolerancia);
                let despierta = v.alimentar(&m16, |tramo| {
                    let t = r.transcribir_corto(tramo);
                    let si = crate::vigilia::coincide(&t, &palabra, tol).is_some();
                    if std::env::var_os("IUREVOZ_LOG").is_some() {
                        eprintln!("[iurevoz] vigilia: {t:?}{}", if si { " → despierta" } else { "" });
                    }
                    si
                });
                if let Some(audio) = despierta {
                    // desde ya el audio va al reconocedor principal, en modo comando
                    rutas.estado.atento.store(true, std::sync::atomic::Ordering::Relaxed);
                    rutas.estado.modo_comando.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = rutas.control.send(Control::Despertar);
                    ev.enviar(Evento::Despierto);
                    esperando_desde = Some(std::time::Instant::now());
                    let s = r.alimentar(&audio);
                    emitir(s, &cfg, &mut punt, &mut instr, &mut id);
                }
            }
            Orden::Interrumpir { muestras, tasa } => {
                let Some(r) = rec.as_mut() else { continue };
                if tasa as i32 != SR {
                    continue; // (el micrófono va a 16 kHz; no se mezcla con el remuestreo del dictado)
                }
                barrera_nuevo += muestras.len();
                barrera.extend_from_slice(&muestras);
                let max = SR as usize * 3;
                if barrera.len() > max {
                    barrera.drain(..barrera.len() - max);
                }
                // sin cancelación de eco el micrófono oye a la app; «cállate» cae a media frase,
                // así que no sirve buscar inicios de voz: se transcribe una ventana corrida
                if barrera_nuevo >= SR as usize {
                    barrera_nuevo = 0;
                    let tramo = &barrera[barrera.len().saturating_sub(SR as usize * 8 / 5)..];
                    let t = r.transcribir_corto(tramo);
                    if !t.trim().is_empty() {
                        if std::env::var_os("IUREVOZ_LOG").is_some() {
                            eprintln!("[iurevoz] mientras habla: {t:?}");
                        }
                        let _ = rutas.control.send(Control::Interrupcion(t));
                    }
                }
            }
            Orden::Audio { muestras, tasa } => {
                let Some(r) = rec.as_mut() else { continue };
                let Some(m16) = a_16k(&mut remuestreo, muestras, tasa) else { continue };
                // mientras la app habla el micrófono llega en ceros: la espera de una
                // instrucción o una respuesta empieza cuando termina de hablar
                if m16.iter().all(|x| *x == 0.0) {
                    esperando_desde = None;
                } else {
                    // ya no habla: la ventana de «cállate» empieza de cero la próxima vez
                    barrera.clear();
                    barrera_nuevo = 0;
                }
                if let Some(v) = vigilia.as_mut() {
                    v.reiniciar(); // al volver a vigilar se empieza limpio
                }
                if grabar {
                    grabacion.extend_from_slice(&m16);
                }
                let s = r.alimentar(&m16);
                if s.iter().any(|x| matches!(x, Salida::Parcial(t) if !t.trim().is_empty())) {
                    oyo_voz = std::time::Instant::now();
                }
                emitir(s, &cfg, &mut punt, &mut instr, &mut id);
                // la instrucción termina tras ~1 s sin voz después de la última frase
                // (una respuesta para otro programa puede pedir una pausa más larga, p. ej. al dictar)
                let fin = rutas.estado.captura.lock().unwrap().as_ref().map_or(FIN_INSTRUCCION, |(_, p)| *p);
                if instr.as_ref().is_some_and(|e| !e.hablando && e.ultimo.elapsed() >= fin) {
                    despachar(&mut instr, &cfg, &mut id);
                    ultimo_envio = Some(std::time::Instant::now());
                    esperando_desde = None;
                }
                // modo comando sin instrucción: se cancela a los 8 s para no dejar el reconocedor andando
                if instr.is_none() && modo(&rutas) {
                    let desde = *esperando_desde.get_or_insert_with(std::time::Instant::now);
                    let capturando = rutas.estado.captura.lock().unwrap().is_some();
                    // a una pregunta se le da más tiempo: primero hay que oírla
                    let espera = if capturando { ESPERA_RESPUESTA } else { ESPERA_INSTRUCCION };
                    let hablando_ahora = oyo_voz.elapsed() < std::time::Duration::from_secs(2);
                    if desde.elapsed() >= espera && !hablando_ahora && rutas.estado.pendiente.lock().unwrap().is_none() {
                        if let Some((tx, _)) = rutas.estado.captura.lock().unwrap().take() {
                            // lo que el reconocedor tenga a medias ES la respuesta: antes se perdía
                            // y al cerrar el micrófono se tecleaba en la ventana activa
                            let resto: Vec<String> = r
                                .terminar()
                                .into_iter()
                                .filter_map(|x| if let Salida::Final(t) = x { Some(t) } else { None })
                                .collect();
                            let t = resto.join(" ").trim().to_string();
                            if !t.is_empty() {
                                ev.enviar(Evento::Estado(format!("Respuesta: «{t}»")));
                            }
                            let _ = tx.send(t);
                        }
                        // lo que quede al cerrar el micrófono no se teclea
                        ultimo_envio = Some(std::time::Instant::now());
                        rutas.estado.modo_comando.store(false, std::sync::atomic::Ordering::Relaxed);
                        let _ = rutas.control.send(Control::FinComando);
                        esperando_desde = None;
                        ev.enviar(Evento::Estado("No oí ninguna instrucción.".into()));
                    }
                } else if instr.is_some() {
                    esperando_desde = None;
                }
            }
            Orden::Fin => {
                if grabar && !grabacion.is_empty() {
                    guardar_grabacion(&grabacion, &ev);
                    grabacion.clear();
                }
                if let Some(r) = rec.as_mut() {
                    let s = r.terminar();
                    if instr.is_some() {
                        // se cerró el micrófono a media instrucción: lo último también cuenta
                        emitir(s, &cfg, &mut punt, &mut instr, &mut id);
                        despachar(&mut instr, &cfg, &mut id);
                    } else if ultimo_envio.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3)) {
                        // restos al cerrar el micrófono tras una instrucción: nunca se teclean
                        if std::env::var_os("IUREVOZ_LOG").is_some() {
                            eprintln!("[iurevoz] asr: se descartan restos tras la instrucción");
                        }
                    } else {
                        emitir(s, &cfg, &mut punt, &mut instr, &mut id);
                    }
                }
                punt.reiniciar();
                if let Some((_, rs)) = &remuestreo {
                    rs.reset();
                }
            }
            Orden::RecargarDetector => {
                if let Some(v) = vigilia.as_mut() {
                    let dedicado = v.cargar_detector(&cfg.escucha.palabra);
                    ev.enviar(Evento::Estado(format!("Detector de «{}»: {}", cfg.escucha.palabra, if dedicado { "cargado" } else { "no entrenado" })));
                }
            }
            Orden::Config(nueva) => {
                // otra palabra clave: otro detector
                if nueva.escucha.palabra != cfg.escucha.palabra {
                    if let Some(v) = vigilia.as_mut() {
                        v.cargar_detector(&nueva.escucha.palabra);
                    }
                }
                let recargar = nueva.modelo_asr != cfg.modelo_asr || nueva.hilos != cfg.hilos || nueva.silencio != cfg.silencio;
                cfg = nueva;
                if recargar || rec.is_none() {
                    drop(rec.take()); // libera la RAM del modelo anterior antes de cargar el nuevo
                    rec = cargar(&cfg, &ev);
                }
            }
            Orden::Descargar => {
                let ev2 = ev.clone();
                let r = modelos::descargar(cfg.modelo_asr, |pieza, fraccion| {
                    ev2.enviar(Evento::Descarga { pieza: pieza.to_string(), fraccion })
                });
                match r {
                    Ok(()) => rec = cargar(&cfg, &ev),
                    Err(e) => ev.enviar(Evento::Error(format!("descarga: {e:#}"))),
                }
            }
        }
    }
}

fn guardar_grabacion(muestras: &[f32], ev: &Emisor) {
    let dir = crate::config::dir_datos().join("depuracion");
    let _ = std::fs::create_dir_all(&dir);
    let seg = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let ruta = dir.join(format!("dictado-{seg}.wav"));
    if sherpa_onnx::write(&ruta.to_string_lossy(), muestras, SR) {
        ev.enviar(Evento::Estado(format!("Audio guardado en {}", ruta.display())));
    }
}

fn cargar(cfg: &Config, ev: &Emisor) -> Option<Reconocedor> {
    let falta = modelos::faltantes(cfg.modelo_asr);
    if !falta.is_empty() {
        ev.enviar(Evento::FaltanModelos(falta.iter().map(|s| s.to_string()).collect()));
        return None;
    }
    ev.enviar(Evento::Estado(format!("Cargando {:?}…", cfg.modelo_asr)));
    let t = std::time::Instant::now();
    match Reconocedor::cargar(cfg) {
        Ok(r) => {
            ev.enviar(Evento::Listo { modelo: format!("{:?}", cfg.modelo_asr), carga_ms: t.elapsed().as_millis() });
            Some(r)
        }
        Err(e) => {
            ev.enviar(Evento::Error(format!("{e:#}")));
            None
        }
    }
}

/// Turnos que se recuerdan y cuánto dura la conversación sin actividad.
const MEMORIA_TURNOS: usize = 6;
const MEMORIA_DURA: std::time::Duration = std::time::Duration::from_secs(180);

fn hilo_agente(rx: Receiver<(usize, String, Config)>, portal: Option<Portal>, voz: Option<Voz>, control: Sender<Control>, ev: Emisor) {
    let mut historial: Vec<serde_json::Value> = Vec::new();
    let mut ultima = std::time::Instant::now();
    for (id, instruccion, cfg) in rx {
        if ultima.elapsed() > MEMORIA_DURA {
            historial.clear(); // conversación nueva
        }
        ultima = std::time::Instant::now();
        let voz_resp = voz.clone().filter(|_| cfg.voz.responder);
        // «Claude, …»: directo a Claude Code, sin pasar por el modelo local
        if cfg.comandos.claude {
            if let Some(tarea) = para_claude(&instruccion) {
                if let Some(v) = &voz_resp {
                    v.decir("Se lo paso a Claude.");
                }
                lanzar_claude(id, tarea, voz_resp.clone(), ev.clone(), control.clone());
                continue;
            }
        }
        let ganchos = {
            let (c1, c2, c3) = (control.clone(), control.clone(), control.clone());
            std::sync::Arc::new(crate::agente::Ganchos {
                confirmar: Box::new(move |desc: &str| {
                    let (tx, rx) = crossbeam_channel::bounded(1);
                    // los resúmenes de F4 ya son una frase («Registro una hora en…»); los demás, una acción
                    let hablada = if desc.chars().next().is_some_and(char::is_uppercase) { desc.to_string() } else { format!("Voy a {desc}.") };
                    let _ = c1.send(Control::Permiso(desc.to_string(), hablada, tx));
                    rx.recv_timeout(std::time::Duration::from_secs(110)).unwrap_or(false)
                }),
                pausar_escucha: Box::new(move |si| drop(c2.send(Control::PausarVigilia(si)))),
                dictar: Box::new(move |si| drop(c3.send(Control::Dictar(si)))),
            })
        };
        let mut ag = Agente::new(cfg.comandos.llm.clone(), portal.clone(), voz.clone()).con_ganchos(ganchos);
        if cfg.comandos.claude {
            let (control, ev, v, ctl_claude) = (control.clone(), ev.clone(), voz_resp.clone(), control.clone());
            ag = ag.con_claude(Box::new(move |tarea: &str| {
                // el modelo local decidió delegar: se confirma antes de mandar nada fuera
                let (tx, rx) = crossbeam_channel::bounded(1);
                let resumen: String = tarea.split(['.', ',', '(']).next().unwrap_or(tarea).trim().to_string();
                let _ = control.send(Control::Permiso(format!("encargarse de: {tarea}"), format!("Claude quiere encargarse de {resumen}."), tx));
                if rx.recv_timeout(std::time::Duration::from_secs(110)).unwrap_or(false) {
                    lanzar_claude(id, tarea.to_string(), v.clone(), ev.clone(), ctl_claude.clone());
                    Ok("le pasé la tarea a Claude; avisaré cuando termine".into())
                } else {
                    Ok("el usuario no quiso pasarle la tarea a Claude".into())
                }
            }));
        }
        let r = ag.ejecutar(&instruccion, &historial, |accion| ev.enviar(Evento::Accion { id, texto: accion.to_string() }));
        match r {
            Ok(inf) => {
                if let Some(v) = &voz_resp {
                    if !inf.respuesta.is_empty() {
                        v.decir(inf.respuesta.clone());
                    }
                }
                // se recuerda lo pedido, lo hecho y lo respondido
                let hecho = if inf.acciones.is_empty() { String::new() } else { format!(" [acciones: {}]", inf.acciones.join("; ")) };
                historial.push(serde_json::json!({"role": "user", "content": instruccion}));
                historial.push(serde_json::json!({"role": "assistant", "content": format!("{}{hecho}", inf.respuesta)}));
                let sobran = historial.len().saturating_sub(MEMORIA_TURNOS * 2);
                historial.drain(..sobran);
                // si el asistente preguntó algo («¿…?», «Dime cuál…»), se queda escuchando la respuesta
                if invita_a_contestar(&inf.respuesta) {
                    let _ = control.send(Control::Seguir);
                }
                ultima = std::time::Instant::now();
                ev.enviar(Evento::Respuesta { id, texto: inf.respuesta });
            }
            Err(e) => {
                if let Some(v) = &voz_resp {
                    v.decir("No pude hacerlo.");
                }
                ev.enviar(Evento::ErrorComando { id, msg: format!("{e:#}") });
            }
        }
    }
}

/// Notificación de escritorio: llega aunque la ventana esté oculta.
fn notificar(titulo: &str, cuerpo: &str) {
    let cuerpo: String = cuerpo.chars().take(300).collect();
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=IureVoz", "--icon=audio-input-microphone", titulo, &cuerpo])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Corre la tarea con Claude Code en su propio hilo: puede tardar minutos.
fn lanzar_claude(id: usize, tarea: String, voz: Option<Voz>, ev: Emisor, control: Sender<Control>) {
    std::thread::Builder::new()
        .name("claude".into())
        .spawn(move || {
            ev.enviar(Evento::Accion { id, texto: format!("Claude: {tarea}") });
            notificar("Claude está trabajando", &tarea);
            let r = crate::claude::ejecutar(&tarea, |p| {
                let texto = match p {
                    crate::claude::Progreso::Texto(t) if !t.is_empty() => format!("Claude: {t}"),
                    crate::claude::Progreso::Herramienta(h) => format!("Claude va a {h}"),
                    _ => return,
                };
                ev.enviar(Evento::Accion { id, texto });
            });
            match r {
                Ok(resp) => {
                    notificar("Claude terminó", &crate::voz::para_voz(&resp));
                    if let Some(v) = &voz {
                        v.decir(resp.clone());
                    }
                    // si Claude pregunta algo, se queda escuchando la respuesta
                    if resp.trim_end().ends_with('?') {
                        let _ = control.send(Control::Seguir);
                    }
                    ev.enviar(Evento::Respuesta { id, texto: resp });
                }
                Err(e) => {
                    notificar("Claude no terminó la tarea", &format!("{e:#}"));
                    if let Some(v) = &voz {
                        v.decir("Claude no pudo terminar la tarea.");
                    }
                    ev.enviar(Evento::ErrorComando { id, msg: format!("{e:#}") });
                }
            }
        })
        .expect("hilo claude");
}

fn hilo_salida(rx: Receiver<(usize, String, Config)>, portal: Option<Portal>, ev: Emisor) {
    let mut corrector: Option<(crate::config::Correccion, Corrector)> = None;
    // lo ya entregado, como contexto para la siguiente corrección
    let mut contexto = String::new();
    for (id, texto, cfg) in rx {
        let mut final_ = texto.clone();
        if cfg.correccion.activa {
            let c = &cfg.correccion;
            let reusar = corrector.as_ref().is_some_and(|(prev, _)| {
                prev.servicio == c.servicio && prev.url == c.url && prev.clave == c.clave && prev.modelo == c.modelo && prev.glosario == c.glosario && prev.max_cambio == c.max_cambio
            });
            if !reusar {
                corrector = Some((c.clone(), Corrector::new(c.clone())));
            }
            let (_, corr) = corrector.as_ref().unwrap();
            // se corrige sin el espacio o salto inicial, que se repone después
            let inicio = texto.len() - texto.trim_start().len();
            let (prefijo, cuerpo) = texto.split_at(inicio);
            match corr.corregir(cuerpo, &contexto) {
                Ok(res) => {
                    final_ = format!("{prefijo}{}", res.texto);
                    ev.enviar(Evento::Corregida { id, res });
                }
                Err(e) => ev.enviar(Evento::ErrorCorreccion { id, msg: format!("{e:#}") }),
            }
        }
        contexto.push_str(&final_);
        if contexto.len() > 2000 {
            let mut corte = contexto.len() - 1000;
            while !contexto.is_char_boundary(corte) {
                corte += 1;
            }
            contexto.drain(..corte);
        }
        if cfg.inyeccion == Inyeccion::Teclear {
            if let Some(p) = &portal {
                p.escribir(final_);
            }
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn instruccion_en_varias_frases() {
        let mut e = EnCurso::new("¿Cuánto es el quince por ciento?");
        e.agregar("de 200");
        assert_eq!(e.texto, "¿Cuánto es el quince por ciento de 200");
    }

    #[test]
    fn respuestas_y_claude() {
        assert_eq!(si_o_no("Sí, adelante"), Some(true));
        assert_eq!(si_o_no("No."), Some(false));
        assert_eq!(si_o_no("Permite"), Some(true));
        assert_eq!(si_o_no("el clima de hoy"), None);
        assert!(solo_palabra_clave("Oye", "oye compu"));
        assert!(invita_a_contestar("Dime cuál es tu duda y con gusto te ayudo."));
        assert!(invita_a_contestar("No pude leerlo. ¿Quieres que intente con otro?"));
        assert!(!invita_a_contestar("Benito Juárez fue presidente de México."));
        assert!(solo_palabra_clave("Oye, compu.", "oye compu"));
        assert!(!solo_palabra_clave("Oye, te voy a dictar", "oye compu"));
        assert!(!solo_palabra_clave("", "oye compu"));
        assert_eq!(para_claude("Claude, revisa mis descargas").as_deref(), Some("revisa mis descargas"));
        assert_eq!(para_claude("Cloud busca los PDF del expediente").as_deref(), Some("busca los PDF del expediente"));
        assert_eq!(para_claude("abre la calculadora"), None);
        assert_eq!(para_claude("Clave, instala htop").as_deref(), Some("instala htop"));
    }

}
