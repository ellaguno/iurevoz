//! Reconocimiento de voz con sherpa-onnx: Nemotron (streaming) o Moonshine (VAD + offline).

use anyhow::{Context, Result};
use sherpa_onnx::{
    OfflineMoonshineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OnlineRecognizer,
    OnlineRecognizerConfig, OnlineStream, SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};

use crate::config::{Config, ModeloAsr};
use crate::modelos;

pub const SR: i32 = 16000;
/// Bloque del modelo Nemotron 560 ms: el decodificador va un bloque atrás del audio.
const BLOQUE_NEMOTRON: f32 = 0.56;
/// Silencio que se le da a un stream nuevo: sin él la primera palabra a veces se pierde.
const RELLENO_INICIAL: usize = SR as usize / 4;
/// Tramos de voz de hasta esta duración (s) se rescatan con Moonshine si Nemotron no los vio.
const RESCATE_MAX_S: f64 = 1.6;
/// Margen (s) para que Nemotron, que va un bloque atrás, alcance a decodificar un tramo.
const RESCATE_ESPERA_S: f64 = 1.3;

#[derive(Debug, Clone, PartialEq)]
pub enum Salida {
    /// Texto provisional de la frase en curso.
    Parcial(String),
    /// Frase cerrada por silencio o por fin de dictado.
    Final(String),
}

pub enum Reconocedor {
    Nemotron(Box<Nemotron>),
    Moonshine(Moonshine),
}

impl Reconocedor {
    pub fn cargar(cfg: &Config) -> Result<Self> {
        Ok(match cfg.modelo_asr {
            ModeloAsr::Nemotron => Self::Nemotron(Box::new(Nemotron::cargar(cfg)?)),
            ModeloAsr::Moonshine => {
                let r = modelos::rutas_moonshine()?;
                let rec = crear_moonshine(&r, cfg.hilos)?;
                let vad = crear_vad(&r.vad, cfg.silencio)?;
                Self::Moonshine(Moonshine { rec, vad, resto: Vec::new(), hablando: false })
            }
        })
    }

    /// Alimenta audio a 16 kHz y devuelve lo que haya salido.
    pub fn alimentar(&mut self, muestras: &[f32]) -> Vec<Salida> {
        match self {
            Self::Nemotron(n) => n.alimentar(muestras),
            Self::Moonshine(m) => m.alimentar(muestras),
        }
    }

    /// Transcribe un tramo corto aparte (para revisar la palabra clave), sin tocar el
    /// estado del dictado. Usa Moonshine: el del rescate o el principal.
    pub fn transcribir_corto(&self, audio: &[f32]) -> String {
        let moon = match self {
            Self::Nemotron(n) => n.rescate.as_ref().map(|r| &r.moon),
            Self::Moonshine(m) => Some(&m.rec),
        };
        let Some(moon) = moon else { return String::new() };
        let st = moon.create_stream();
        st.accept_waveform(SR, audio);
        moon.decode(&st);
        texto_moonshine(&st)
    }

    /// Cierra el dictado: empuja silencio para que salga la última frase y reinicia.
    pub fn terminar(&mut self) -> Vec<Salida> {
        let mut out = match self {
            Self::Nemotron(n) => n.terminar(),
            Self::Moonshine(m) => m.terminar(),
        };
        out.push(Salida::Parcial(String::new()));
        out
    }
}

fn log(msg: impl FnOnce() -> String) {
    if std::env::var_os("IUREVOZ_LOG").is_some() {
        eprintln!("[iurevoz] asr: {}", msg());
    }
}

// ── Nemotron ─────────────────────────────────────────────────────────────────

pub struct Nemotron {
    rec: OnlineRecognizer,
    st: OnlineStream,
    ultimo: String,
    cierre: f32,
    /// Últimos ~cierre segundos de audio, para volver a alimentarlos tras un reset.
    reciente: Vec<f32>,
    /// Segundos de audio recibidos desde que se cargó.
    t: f64,
    /// Tiempo absoluto que corresponde al instante 0 del stream actual.
    origen: f64,
    /// Tiempos absolutos de los tokens de frases ya cerradas (últimos segundos) y de la actual.
    tokens_previos: Vec<f64>,
    tokens_actuales: Vec<f64>,
    rescate: Option<Rescate>,
}

/// Nemotron descarta a menudo palabras cortas dichas solas entre pausas («uno», «coma»).
/// Un VAD aparte detecta esos tramos y, si Nemotron no produjo ningún token en ellos, se
/// transcriben con Moonshine.
struct Rescate {
    moon: OfflineRecognizer,
    vad: VoiceActivityDetector,
    resto: Vec<f32>,
    /// Tiempo absoluto del instante 0 del VAD.
    base: f64,
    /// Tramos cortos esperando a ver si Nemotron los cubre: (inicio, fin, audio).
    pendientes: Vec<(f64, f64, Vec<f32>)>,
}

impl Nemotron {
    fn cargar(cfg: &Config) -> Result<Self> {
        let r = modelos::rutas_nemotron()?;
        // Si el silencio de cierre no cubre también el bloque, el endpoint se
        // dispara antes de que salgan las últimas palabras.
        let cierre = cfg.silencio + BLOQUE_NEMOTRON;
        let mut c = OnlineRecognizerConfig::default();
        c.model_config.transducer.encoder = Some(r.encoder);
        c.model_config.transducer.decoder = Some(r.decoder);
        c.model_config.transducer.joiner = Some(r.joiner);
        c.model_config.tokens = Some(r.tokens);
        c.model_config.num_threads = cfg.hilos;
        c.decoding_method = Some("greedy_search".into());
        c.enable_endpoint = true;
        c.rule1_min_trailing_silence = 2.4;
        c.rule2_min_trailing_silence = cierre;
        // corte forzoso tras hablar sin pausa; a 30 s cortaba a media palabra al leer de corrido
        c.rule3_min_utterance_length = 60.0;
        let rec = OnlineRecognizer::create(&c).context("no pude cargar Nemotron")?;

        // El rescate es opcional: sin Moonshine, Nemotron funciona solo.
        let rescate = match modelos::rutas_moonshine() {
            Ok(m) => {
                let moon = crear_moonshine(&m, 2)?;
                let vad = crear_vad(&m.vad, 0.3)?;
                Some(Rescate { moon, vad, resto: Vec::new(), base: 0.0, pendientes: Vec::new() })
            }
            Err(e) => {
                log(|| format!("sin rescate de palabras cortas: {e}"));
                None
            }
        };
        let st = nuevo_stream(&rec);
        Ok(Self {
            rec,
            st,
            ultimo: String::new(),
            cierre,
            reciente: Vec::new(),
            t: 0.0,
            origen: -(RELLENO_INICIAL as f64 / SR as f64),
            tokens_previos: Vec::new(),
            tokens_actuales: Vec::new(),
            rescate,
        })
    }

    fn decodificar(&mut self) {
        while self.rec.is_ready(&self.st) {
            self.rec.decode(&self.st);
        }
    }

    fn alimentar(&mut self, muestras: &[f32]) -> Vec<Salida> {
        let mut out = Vec::new();
        self.st.accept_waveform(SR, muestras);
        self.t += muestras.len() as f64 / SR as f64;
        let max = (self.cierre * 0.95 * SR as f32) as usize;
        self.reciente.extend_from_slice(muestras);
        if self.reciente.len() > max {
            self.reciente.drain(..self.reciente.len() - max);
        }
        self.decodificar();

        let res = self.rec.get_result(&self.st);
        let texto = res.as_ref().map(|r| r.text.trim().to_string()).unwrap_or_default();
        if let Some(ts) = res.as_ref().and_then(|r| r.timestamps.as_ref()) {
            self.tokens_actuales = ts.iter().map(|&x| self.origen + x as f64).collect();
        }

        if self.rec.is_endpoint(&self.st) {
            if !texto.is_empty() {
                out.push(Salida::Final(texto));
            }
            self.tokens_previos.append(&mut self.tokens_actuales);
            let limite = self.t - 30.0;
            self.tokens_previos.retain(|&x| x > limite);
            self.rec.reset(&self.st);
            // El decodificador va un bloque atrás: al detectar el fin de frase ya recibió el
            // inicio de la palabra siguiente, y reset() lo descarta. Se vuelve a alimentar el
            // audio reciente, que por definición del endpoint es casi todo silencio.
            self.st.accept_waveform(SR, &self.reciente);
            self.origen = self.t - self.reciente.len() as f64 / SR as f64;
            self.decodificar();
            self.ultimo.clear();
        } else if texto != self.ultimo {
            out.push(Salida::Parcial(texto.clone()));
            self.ultimo = texto;
        }

        out.extend(self.rescatar(muestras, false));
        out
    }

    /// Pasa el audio al VAD de rescate y transcribe los tramos cortos que Nemotron no cubrió.
    fn rescatar(&mut self, muestras: &[f32], forzar: bool) -> Vec<Salida> {
        let Some(r) = self.rescate.as_mut() else { return vec![] };
        r.resto.extend_from_slice(muestras);
        let mut i = 0;
        while i + 512 <= r.resto.len() {
            r.vad.accept_waveform(&r.resto[i..i + 512]);
            i += 512;
        }
        r.resto.drain(..i);
        if forzar {
            r.vad.flush();
        }
        while let Some(seg) = r.vad.front() {
            let ini = r.base + seg.start() as f64 / SR as f64;
            let dur = seg.samples().len() as f64 / SR as f64;
            if dur <= RESCATE_MAX_S {
                r.pendientes.push((ini, ini + dur, seg.samples().to_vec()));
            }
            r.vad.pop();
        }

        let mut out = Vec::new();
        let ahora = self.t;
        let tokens = self.tokens_previos.iter().chain(&self.tokens_actuales);
        let tokens: Vec<f64> = tokens.copied().collect();
        let mut quedan = Vec::new();
        for (ini, fin, audio) in r.pendientes.drain(..) {
            if !forzar && ahora < fin + RESCATE_ESPERA_S {
                quedan.push((ini, fin, audio));
                continue;
            }
            if tokens.iter().any(|&x| x >= ini - 0.2 && x <= fin + 0.3) {
                continue; // Nemotron sí lo transcribió
            }
            let st = r.moon.create_stream();
            st.accept_waveform(SR, &audio);
            r.moon.decode(&st);
            let texto = texto_moonshine(&st);
            let palabras = texto.split_whitespace().count();
            log(|| format!("rescate {ini:.1}–{fin:.1} s: {texto:?}"));
            // un tramo corto con mucho texto suele ser una alucinación (tos, ruido)
            if palabras > 0 && palabras <= 4 && texto.chars().any(char::is_alphanumeric) {
                out.push(Salida::Final(texto));
            }
        }
        r.pendientes = quedan;
        out
    }

    fn terminar(&mut self) -> Vec<Salida> {
        let mut out = Vec::new();
        let silencio = vec![0.0f32; ((self.cierre + 0.6) * SR as f32) as usize];
        self.st.accept_waveform(SR, &silencio);
        self.decodificar();
        let res = self.rec.get_result(&self.st);
        if let Some(ts) = res.as_ref().and_then(|r| r.timestamps.as_ref()) {
            self.tokens_actuales = ts.iter().map(|&x| self.origen + x as f64).collect();
        }
        // primero lo que el rescate tenga pendiente: es anterior a la última frase o está aislado
        let rescatadas = self.rescatar(&[], true);
        let texto = res.map(|r| r.text.trim().to_string()).unwrap_or_default();
        out.extend(rescatadas);
        if !texto.is_empty() {
            out.push(Salida::Final(texto));
        }
        self.st = nuevo_stream(&self.rec);
        self.origen = self.t - RELLENO_INICIAL as f64 / SR as f64;
        self.ultimo.clear();
        self.reciente.clear();
        self.tokens_previos.clear();
        self.tokens_actuales.clear();
        if let Some(r) = self.rescate.as_mut() {
            r.vad.reset();
            r.resto.clear();
            r.pendientes.clear();
            r.base = self.t;
        }
        out
    }
}

// ── Moonshine ────────────────────────────────────────────────────────────────

pub struct Moonshine {
    rec: OfflineRecognizer,
    vad: VoiceActivityDetector,
    resto: Vec<f32>,
    hablando: bool,
}

impl Moonshine {
    fn alimentar(&mut self, muestras: &[f32]) -> Vec<Salida> {
        let mut out = Vec::new();
        self.resto.extend_from_slice(muestras);
        let mut i = 0;
        while i + 512 <= self.resto.len() {
            self.vad.accept_waveform(&self.resto[i..i + 512]);
            i += 512;
        }
        self.resto.drain(..i);
        let detectado = self.vad.detected();
        if detectado != self.hablando {
            self.hablando = detectado;
            // Moonshine no da parciales: solo avisamos que hay voz.
            out.push(Salida::Parcial(if detectado { "…".into() } else { String::new() }));
        }
        out.extend(vaciar_vad(&self.rec, &self.vad));
        out
    }

    fn terminar(&mut self) -> Vec<Salida> {
        self.resto.clear();
        self.vad.flush();
        let out = vaciar_vad(&self.rec, &self.vad);
        self.vad.reset();
        self.hablando = false;
        out
    }
}

fn crear_moonshine(r: &modelos::RutasMoonshine, hilos: i32) -> Result<OfflineRecognizer> {
    let mut c = OfflineRecognizerConfig::default();
    c.model_config.moonshine = OfflineMoonshineModelConfig {
        encoder: Some(r.encoder.clone()),
        merged_decoder: Some(r.decoder.clone()),
        ..Default::default()
    };
    c.model_config.tokens = Some(r.tokens.clone());
    c.model_config.num_threads = hilos;
    OfflineRecognizer::create(&c).context("no pude cargar Moonshine")
}

fn crear_vad(modelo: &str, silencio: f32) -> Result<VoiceActivityDetector> {
    let vc = VadModelConfig {
        silero_vad: SileroVadModelConfig {
            model: Some(modelo.to_string()),
            threshold: 0.5,
            min_silence_duration: silencio,
            min_speech_duration: 0.15,
            window_size: 512,
            max_speech_duration: 20.0,
        },
        sample_rate: SR,
        num_threads: 1,
        ..Default::default()
    };
    VoiceActivityDetector::create(&vc, 60.0).context("no pude cargar el VAD")
}

fn nuevo_stream(rec: &OnlineRecognizer) -> OnlineStream {
    let st = rec.create_stream();
    st.set_option("language", "es");
    st.accept_waveform(SR, &[0.0; RELLENO_INICIAL]);
    st
}

/// Moonshine a veces deja el marcador de palabra de SentencePiece («▁4.»).
fn texto_moonshine(st: &sherpa_onnx::OfflineStream) -> String {
    st.get_result().map(|r| r.text.replace('▁', " ").split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default()
}

fn vaciar_vad(rec: &OfflineRecognizer, vad: &VoiceActivityDetector) -> Vec<Salida> {
    let mut out = Vec::new();
    while let Some(seg) = vad.front() {
        let st = rec.create_stream();
        st.accept_waveform(SR, seg.samples());
        rec.decode(&st);
        let texto = texto_moonshine(&st);
        vad.pop();
        if !texto.is_empty() {
            out.push(Salida::Final(texto));
        }
    }
    out
}
