//! Captura del micrófono con cpal. Entrega bloques mono a la tasa del dispositivo.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};

/// Mientras exista, el micrófono está abierto.
pub struct Captura {
    _stream: Stream,
    pub tasa: u32,

}

/// Abre el micrófono por omisión. `entregar` recibe (muestras mono, tasa); `nivel` el RMS
/// del último bloque (f32 en bits).
pub fn abrir(entregar: impl Fn(Vec<f32>, u32) + Send + 'static, nivel: Arc<AtomicU32>) -> Result<Captura> {
    let host = cpal::default_host();
    let dev = host.default_input_device().context("no hay micrófono")?;
    // Mejor pedir 16 kHz mono directo: PipeWire remuestrea con más calidad que nosotros.
    let preferida = dev.supported_input_configs().ok().and_then(|cs| {
        cs.filter(|c| matches!(c.sample_format(), SampleFormat::F32 | SampleFormat::I16))
            .filter_map(|c| {
                let canales = c.channels();
                c.try_with_sample_rate(16000).map(|c| (canales, c))
            })
            .min_by_key(|(canales, c)| (*canales, c.sample_format() != SampleFormat::F32))
            .map(|(_, c)| c)
    });
    let sup = match preferida {
        Some(c) => c,
        None => dev.default_input_config().context("el micrófono no da configuración")?,
    };
    let cfg = sup.config();
    let canales = cfg.channels as usize;
    let tasa = cfg.sample_rate;

    fn mono<T: Copy>(data: &[T], canales: usize, a_f32: impl Fn(T) -> f32) -> Vec<f32> {
        data.chunks(canales).map(|f| f.iter().map(|&s| a_f32(s)).sum::<f32>() / canales as f32).collect()
    }
    let enviar = move |m: Vec<f32>| {
        let rms = (m.iter().map(|x| x * x).sum::<f32>() / m.len().max(1) as f32).sqrt();
        nivel.store(rms.to_bits(), Ordering::Relaxed);
        entregar(m, tasa);
    };
    let error = |e| eprintln!("audio: {e}");

    let stream = match sup.sample_format() {
        SampleFormat::F32 => dev.build_input_stream::<f32, _, _>(cfg, move |d, _| enviar(mono(d, canales, |s| s)), error, None),
        SampleFormat::I16 => dev.build_input_stream::<i16, _, _>(
            cfg,
            move |d, _| enviar(mono(d, canales, |s| s as f32 / 32768.0)),
            error,
            None,
        ),
        SampleFormat::I32 => dev.build_input_stream::<i32, _, _>(
            cfg,
            move |d, _| enviar(mono(d, canales, |s| s as f32 / 2_147_483_648.0)),
            error,
            None,
        ),
        f => bail!("formato de audio no soportado: {f:?}"),
    }?;
    stream.play()?;
    Ok(Captura { _stream: stream, tasa })
}
