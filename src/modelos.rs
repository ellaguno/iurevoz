//! Localiza y descarga los modelos de sherpa-onnx.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::{ModeloAsr, dir_datos};

const URL_BASE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/";
const NEMOTRON: &str = "sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11";
const MOONSHINE: &str = "sherpa-onnx-moonshine-base-es-quantized-2026-02-27";
const VAD: &str = "silero_vad.onnx";

/// Lo que necesita cada modelo: directorios (.tar.bz2) y archivos sueltos.
fn piezas(modelo: ModeloAsr) -> &'static [&'static str] {
    match modelo {
        // Moonshine y el VAD rescatan las palabras cortas que Nemotron descarta
        ModeloAsr::Nemotron => &[NEMOTRON, MOONSHINE, VAD],
        ModeloAsr::Moonshine => &[MOONSHINE, VAD],
    }
}

/// Directorios donde buscar, en orden. El segundo reutiliza lo que bajó el benchmark.
fn dirs_busqueda() -> Vec<PathBuf> {
    let mut v = vec![dir_datos().join("modelos")];
    if let Some(cache) = dirs::cache_dir() {
        v.push(cache.join("stt-bench").join("models"));
    }
    v
}

fn buscar(pieza: &str) -> Option<PathBuf> {
    dirs_busqueda().into_iter().map(|d| d.join(pieza)).find(|p| p.exists())
}

pub fn faltantes(modelo: ModeloAsr) -> Vec<&'static str> {
    piezas(modelo).iter().copied().filter(|p| buscar(p).is_none()).collect()
}

pub struct RutasNemotron {
    pub encoder: String,
    pub decoder: String,
    pub joiner: String,
    pub tokens: String,
}

pub struct RutasMoonshine {
    pub encoder: String,
    pub decoder: String,
    pub tokens: String,
    pub vad: String,
}

fn archivo(dir: &Path, prefijo: &str, ext: &str) -> Result<String> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        let n = p.file_name().unwrap_or_default().to_string_lossy();
        if n.starts_with(prefijo) && n.ends_with(ext) {
            return Ok(p.to_string_lossy().into_owned());
        }
    }
    bail!("no encontré {prefijo}*{ext} en {}", dir.display())
}

fn dir_de(pieza: &str) -> Result<PathBuf> {
    buscar(pieza).with_context(|| format!("falta el modelo {pieza}"))
}

pub fn rutas_nemotron() -> Result<RutasNemotron> {
    let d = dir_de(NEMOTRON)?;
    Ok(RutasNemotron {
        encoder: archivo(&d, "encoder", ".onnx")?,
        decoder: archivo(&d, "decoder", ".onnx")?,
        joiner: archivo(&d, "joiner", ".onnx")?,
        tokens: archivo(&d, "tokens", ".txt")?,
    })
}

pub fn rutas_moonshine() -> Result<RutasMoonshine> {
    let d = dir_de(MOONSHINE)?;
    Ok(RutasMoonshine {
        encoder: archivo(&d, "encoder", ".ort")?,
        decoder: archivo(&d, "decoder", ".ort")?,
        tokens: archivo(&d, "tokens", ".txt")?,
        vad: dir_de(VAD)?.to_string_lossy().into_owned(),
    })
}

/// Baja lo que falte para `modelo`. `progreso` recibe (pieza, fracción 0..1).
pub fn descargar(modelo: ModeloAsr, progreso: impl Fn(&str, f32)) -> Result<()> {
    let destino = dir_datos().join("modelos");
    std::fs::create_dir_all(&destino)?;
    for pieza in faltantes(modelo) {
        let es_tar = !pieza.ends_with(".onnx");
        let nombre = if es_tar { format!("{pieza}.tar.bz2") } else { pieza.to_string() };
        let url = format!("{URL_BASE}{nombre}");
        let resp = ureq::get(&url).call().with_context(|| format!("descargando {url}"))?;
        let total: f32 = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok()?.parse().ok())
            .unwrap_or(0.0);
        let mut lector = Progreso { r: resp.into_body().into_reader(), leido: 0, total, pieza, cb: &progreso };
        if es_tar {
            // Se extrae a un directorio temporal y se renombra al final: una descarga
            // interrumpida no deja un modelo a medias que parezca válido.
            let tmp = destino.join(format!(".{pieza}.parcial"));
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp)?;
            tar::Archive::new(bzip2::read::BzDecoder::new(&mut lector)).unpack(&tmp)?;
            std::fs::rename(tmp.join(pieza), destino.join(pieza))?;
            let _ = std::fs::remove_dir_all(&tmp);
        } else {
            let tmp = destino.join(format!(".{pieza}.parcial"));
            std::io::copy(&mut lector, &mut std::fs::File::create(&tmp)?)?;
            std::fs::rename(&tmp, destino.join(pieza))?;
        }
        progreso(pieza, 1.0);
    }
    Ok(())
}

struct Progreso<'a, R, F: Fn(&str, f32)> {
    r: R,
    leido: u64,
    total: f32,
    pieza: &'a str,
    cb: &'a F,
}

impl<R: Read, F: Fn(&str, f32)> Read for Progreso<'_, R, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.r.read(buf)?;
        let antes = self.leido;
        self.leido += n as u64;
        // avisa cada ~4 MB para no saturar la interfaz
        if self.total > 0.0 && self.leido >> 22 != antes >> 22 {
            (self.cb)(self.pieza, self.leido as f32 / self.total);
        }
        Ok(n)
    }
}
