//! Síntesis de voz local (TTS) con sherpa-onnx y reproducción con cpal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Sender, unbounded};
use sherpa_onnx::{GenerationConfig, LinearResampler, OfflineTts, OfflineTtsConfig};

use crate::config::dir_datos;

const URL_BASE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/";

/// Voces disponibles: (clave, archivo del modelo, descripción).
pub const VOCES: &[(&str, &str, &str)] = &[
    ("piper-ald", "vits-piper-es_MX-ald-medium", "Piper · México, hombre (rápida)"),
    ("piper-claude", "vits-piper-es_MX-claude-high", "Piper · México, mujer"),
    ("piper-daniela", "vits-piper-es_AR-daniela-high", "Piper · Argentina, mujer"),
    ("kokoro-dora", "kokoro-int8-multi-lang-v1_0", "Kokoro · mujer (más natural)"),
    ("kokoro-alex", "kokoro-int8-multi-lang-v1_0", "Kokoro · hombre"),
    ("supertonic", "sherpa-onnx-supertonic-3-tts-int8-2026-05-11", "Supertonic 3 · multilingüe"),
];

fn dirs_busqueda() -> Vec<PathBuf> {
    let mut v = vec![dir_datos().join("voces")];
    if let Some(c) = dirs::cache_dir() {
        v.push(c.join("stt-bench").join("tts"));
    }
    v
}

fn modelo_de(clave: &str) -> Result<&'static str> {
    VOCES.iter().find(|v| v.0 == clave).map(|v| v.1).with_context(|| format!("voz desconocida: {clave}"))
}

pub fn falta(clave: &str) -> Option<&'static str> {
    let m = modelo_de(clave).ok()?;
    dirs_busqueda().iter().all(|d| !d.join(m).exists()).then_some(m)
}

pub fn descargar(clave: &str, progreso: impl Fn(f32)) -> Result<()> {
    let Some(m) = falta(clave) else { return Ok(()) };
    let destino = dir_datos().join("voces");
    std::fs::create_dir_all(&destino)?;
    let resp = ureq::get(&format!("{URL_BASE}{m}.tar.bz2")).call()?;
    let total: f32 = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok()).unwrap_or(0.0);
    let mut leido = 0u64;
    let lector = resp.into_body().into_reader();
    let lector = Contador { r: lector, leido: &mut leido, total, cb: &progreso };
    let tmp = destino.join(format!(".{m}.parcial"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    tar::Archive::new(bzip2::read::BzDecoder::new(lector)).unpack(&tmp)?;
    std::fs::rename(tmp.join(m), destino.join(m))?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

struct Contador<'a, R, F: Fn(f32)> {
    r: R,
    leido: &'a mut u64,
    total: f32,
    cb: &'a F,
}

impl<R: std::io::Read, F: Fn(f32)> std::io::Read for Contador<'_, R, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.r.read(buf)?;
        let antes = *self.leido;
        *self.leido += n as u64;
        if self.total > 0.0 && *self.leido >> 22 != antes >> 22 {
            (self.cb)(*self.leido as f32 / self.total);
        }
        Ok(n)
    }
}

fn archivo(dir: &Path, pred: impl Fn(&str) -> bool) -> Result<String> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if pred(&p.file_name().unwrap_or_default().to_string_lossy()) {
            return Ok(p.to_string_lossy().into_owned());
        }
    }
    bail!("archivo no encontrado en {}", dir.display())
}

pub struct Sintetizador {
    tts: OfflineTts,
    generacion: GenerationConfig,
}

impl Sintetizador {
    pub fn cargar(clave: &str, velocidad: f32) -> Result<Self> {
        let m = modelo_de(clave)?;
        let dir = dirs_busqueda().into_iter().map(|d| d.join(m)).find(|d| d.exists()).with_context(|| format!("falta la voz {m}"))?;
        let s = |p: &Path| Some(p.to_string_lossy().into_owned());
        let mut c = OfflineTtsConfig::default();
        c.model.num_threads = 2;
        let mut generacion = GenerationConfig { speed: velocidad, ..Default::default() };
        if clave.starts_with("piper") {
            c.model.vits.model = Some(archivo(&dir, |n| n.ends_with(".onnx"))?);
            c.model.vits.tokens = s(&dir.join("tokens.txt"));
            c.model.vits.data_dir = s(&dir.join("espeak-ng-data"));
        } else if clave.starts_with("kokoro") {
            c.model.kokoro.model = s(&dir.join("model.int8.onnx"));
            c.model.kokoro.voices = s(&dir.join("voices.bin"));
            c.model.kokoro.tokens = s(&dir.join("tokens.txt"));
            c.model.kokoro.data_dir = s(&dir.join("espeak-ng-data"));
            c.model.kokoro.lang = Some("es".into());
            // ids de Kokoro v1.0: ef_dora = 28, em_alex = 29
            generacion.sid = if clave == "kokoro-alex" { 29 } else { 28 };
        } else {
            let t = &mut c.model.supertonic;
            t.duration_predictor = Some(archivo(&dir, |n| n.starts_with("duration_predictor"))?);
            t.text_encoder = Some(archivo(&dir, |n| n.starts_with("text_encoder"))?);
            t.vector_estimator = Some(archivo(&dir, |n| n.starts_with("vector_estimator"))?);
            t.vocoder = Some(archivo(&dir, |n| n.starts_with("vocoder"))?);
            t.tts_json = s(&dir.join("tts.json"));
            t.unicode_indexer = s(&dir.join("unicode_indexer.bin"));
            t.voice_style = s(&dir.join("voice.bin"));
            generacion.extra = Some(HashMap::from([("lang".to_string(), serde_json::json!("es"))]));
        }
        let tts = OfflineTts::create(&c).context("no pude cargar la voz")?;
        Ok(Self { tts, generacion })
    }

    /// Texto → (muestras, tasa).
    pub fn generar(&self, texto: &str) -> Result<(Vec<f32>, i32)> {
        let a = self
            .tts
            .generate_with_config(texto, &self.generacion, None::<fn(&[f32], f32) -> bool>)
            .context("la voz no generó audio")?;
        Ok((a.samples().to_vec(), a.sample_rate()))
    }
}

/// Divide en oraciones para empezar a hablar pronto y poder interrumpir entre ellas.
pub fn oraciones(texto: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut actual = String::new();
    for c in texto.chars() {
        actual.push(c);
        if matches!(c, '.' | '?' | '!' | '\n' | ';') && actual.trim().chars().count() > 1 {
            v.push(std::mem::take(&mut actual).trim().to_string());
        }
    }
    if !actual.trim().is_empty() {
        v.push(actual.trim().to_string());
    }
    v.retain(|s| s.chars().any(char::is_alphanumeric));
    v
}

enum Orden {
    Decir(String),
    Configurar { voz: String, velocidad: f32 },
}

/// Voz en un hilo propio: sintetiza oración por oración y reproduce. `hablando` sirve
/// para que el micrófono ignore lo que dice la propia app.
#[derive(Clone)]
pub struct Voz {
    tx: Sender<Orden>,
    pub hablando: Arc<AtomicBool>,
    callar: Arc<AtomicBool>,
}

impl Voz {
    pub fn iniciar(voz: String, velocidad: f32, avisar: impl Fn(String) + Send + 'static) -> Self {
        let (tx, rx) = unbounded::<Orden>();
        let hablando = Arc::new(AtomicBool::new(false));
        let callar = Arc::new(AtomicBool::new(false));
        let (h, c) = (hablando.clone(), callar.clone());
        std::thread::Builder::new()
            .name("voz".into())
            .spawn(move || {
                let mut clave = voz;
                let mut vel = velocidad;
                let mut sint: Option<Sintetizador> = None;
                for orden in rx {
                    match orden {
                        Orden::Configurar { voz, velocidad } => {
                            if voz != clave || velocidad != vel {
                                sint = None;
                            }
                            clave = voz;
                            vel = velocidad;
                        }
                        Orden::Decir(texto) => {
                            if sint.is_none() {
                                if let Some(m) = falta(&clave) {
                                    avisar(format!("Descargando la voz {m}…"));
                                    if let Err(e) = descargar(&clave, |_| {}) {
                                        avisar(format!("no pude bajar la voz: {e:#}"));
                                        continue;
                                    }
                                }
                                match Sintetizador::cargar(&clave, vel) {
                                    Ok(s) => sint = Some(s),
                                    Err(e) => {
                                        avisar(format!("voz: {e:#}"));
                                        continue;
                                    }
                                }
                            }
                            c.store(false, Ordering::Relaxed);
                            h.store(true, Ordering::Relaxed);
                            for o in oraciones(&texto) {
                                if c.load(Ordering::Relaxed) {
                                    break;
                                }
                                match sint.as_ref().unwrap().generar(&o) {
                                    Ok((m, sr)) => {
                                        if let Err(e) = reproducir(&m, sr, &c) {
                                            avisar(format!("altavoz: {e:#}"));
                                            break;
                                        }
                                    }
                                    Err(e) => avisar(format!("voz: {e:#}")),
                                }
                            }
                            h.store(false, Ordering::Relaxed);
                        }
                    }
                }
            })
            .expect("hilo de voz");
        Self { tx, hablando, callar }
    }

    pub fn decir(&self, texto: impl Into<String>) {
        let _ = self.tx.send(Orden::Decir(texto.into()));
    }

    pub fn callar(&self) {
        self.callar.store(true, Ordering::Relaxed);
    }

    pub fn configurar(&self, voz: &str, velocidad: f32) {
        let _ = self.tx.send(Orden::Configurar { voz: voz.to_string(), velocidad });
    }
}

/// Reproduce y bloquea hasta terminar (o hasta que `callar` se active).
pub fn reproducir(muestras: &[f32], sr: i32, callar: &AtomicBool) -> Result<()> {
    let dev = cpal::default_host().default_output_device().context("no hay altavoz")?;
    let sup = dev.default_output_config()?;
    let cfg = sup.config();
    let tasa = cfg.sample_rate as i32;
    let canales = cfg.channels as usize;
    let datos: Vec<f32> = if tasa == sr {
        muestras.to_vec()
    } else {
        LinearResampler::create(sr, tasa).context("remuestreo")?.resample(muestras, true)
    };
    let total = datos.len();
    let datos = Arc::new(datos);
    let pos = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (d, p) = (datos.clone(), pos.clone());
    let escribir = move |out: &mut [f32]| {
        for marco in out.chunks_mut(canales) {
            let i = p.fetch_add(1, Ordering::Relaxed);
            let v = d.get(i).copied().unwrap_or(0.0);
            marco.iter_mut().for_each(|s| *s = v);
        }
    };
    let stream = match sup.sample_format() {
        cpal::SampleFormat::F32 => dev.build_output_stream::<f32, _, _>(cfg, move |o, _| escribir(o), |e| eprintln!("altavoz: {e}"), None)?,
        cpal::SampleFormat::I16 => {
            let mut tmp = Vec::new();
            dev.build_output_stream::<i16, _, _>(
                cfg,
                move |o, _| {
                    tmp.resize(o.len(), 0.0);
                    escribir(&mut tmp);
                    o.iter_mut().zip(&tmp).for_each(|(s, v)| *s = (v.clamp(-1.0, 1.0) * 32767.0) as i16);
                },
                |e| eprintln!("altavoz: {e}"),
                None,
            )?
        }
        f => bail!("formato de salida no soportado: {f:?}"),
    };
    stream.play()?;
    while pos.load(Ordering::Relaxed) < total + tasa as usize / 10 {
        if callar.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(())
}
