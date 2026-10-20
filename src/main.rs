//! iurevoz: dictado por voz local para Linux.

mod asr;
mod audio;
mod config;
mod control;
mod correccion;
mod modelos;
mod motor;
mod portal;
mod puntuacion;
mod teclas;
mod ui;

use std::time::Instant;

use anyhow::{Context, Result, bail};

use config::Config;

const AYUDA: &str = "\
iurevoz — dictado por voz local

Uso:
  iurevoz                       abre la ventana
  iurevoz alternar              empieza/detiene el dictado en la instancia abierta
  iurevoz transcribir A.wav [--corregir]
                                prueba sin interfaz: ASR + puntuación (+ corrección)
  iurevoz --sin-portal          abre la ventana sin teclear ni atajo global
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "ayuda") => print!("{AYUDA}"),
        Some("alternar") => {
            if !control::enviar_a_instancia("alternar") {
                bail!("iurevoz no está abierto");
            }
        }
        Some("transcribir") => {
            let ruta = args.get(1).context("falta el archivo .wav")?;
            transcribir(ruta, args.iter().any(|a| a == "--corregir"))?;
        }
        _ => {
            if control::enviar_a_instancia("alternar") {
                // ya hay una ventana abierta: el segundo arranque funciona como atajo
                return Ok(());
            }
            instalar_lanzador();
            let sin_portal = args.iter().any(|a| a == "--sin-portal");
            ui::ejecutar(Config::cargar(), sin_portal).map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }
    Ok(())
}

/// Los portales identifican a las apps fuera de Flatpak por su archivo .desktop.
fn instalar_lanzador() {
    let Some(dir) = dirs::data_dir().map(|d| d.join("applications")) else { return };
    let Ok(exe) = std::env::current_exe() else { return };
    let ruta = dir.join(format!("{}.desktop", config::APP_ID));
    let contenido = format!(
        "[Desktop Entry]\nType=Application\nName=IureVoz\nComment=Dictado por voz local\n\
         Exec={}\nIcon=audio-input-microphone\nCategories=Utility;Accessibility;\nTerminal=false\n",
        exe.display()
    );
    if std::fs::read_to_string(&ruta).ok().as_deref() != Some(contenido.as_str()) {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&ruta, contenido);
    }
}

/// Simula un dictado con un archivo: bloques de 100 ms como si vinieran del micrófono.
fn transcribir(ruta: &str, corregir: bool) -> Result<()> {
    let cfg = Config::cargar();
    let faltan = modelos::faltantes(cfg.modelo_asr);
    if !faltan.is_empty() {
        eprintln!("Descargando {faltan:?}…");
        modelos::descargar(cfg.modelo_asr, |p, f| eprint!("\r{p} {:.0}%   ", f * 100.0))?;
        eprintln!();
    }
    let wav = sherpa_onnx::Wave::read(ruta).context("no pude leer el WAV (16 kHz mono)")?;
    let t = Instant::now();
    let mut rec = asr::Reconocedor::cargar(&cfg)?;
    eprintln!("{:?} cargado en {:.1} s", cfg.modelo_asr, t.elapsed().as_secs_f32());

    let mut muestras = wav.samples().to_vec();
    if wav.sample_rate() != asr::SR {
        let rs = sherpa_onnx::LinearResampler::create(wav.sample_rate(), asr::SR).context("remuestreo")?;
        muestras = rs.resample(&muestras, true);
    }
    let corrector = corregir.then(|| correccion::Corrector::new(cfg.correccion.clone()));
    let mut punt = puntuacion::Puntuador::new();
    let mut texto = String::new();
    let t = Instant::now();
    let mut salidas = Vec::new();
    for bloque in muestras.chunks(asr::SR as usize / 10) {
        salidas.extend(rec.alimentar(bloque));
    }
    salidas.extend(rec.terminar());
    let computo = t.elapsed().as_secs_f32();
    for s in salidas {
        if let asr::Salida::Final(f) = s {
            let p = punt.procesar(&f, cfg.puntuacion_dictada);
            println!("ASR   {f}");
            let mut final_ = p.clone();
            if let Some(c) = &corrector {
                let inicio = p.len() - p.trim_start().len();
                let r = c.corregir(&p[inicio..], &texto)?;
                println!("LLM   {} ({} ms, {}){}", r.texto, r.ms, if r.aceptada { "aceptada" } else { "DESCARTADA" },
                    if r.cambios.is_empty() { String::new() } else { format!(" cambios: {:?}", r.cambios) });
                final_ = format!("{}{}", &p[..inicio], r.texto);
            }
            texto.push_str(&final_);
        }
    }
    let dur = muestras.len() as f32 / asr::SR as f32;
    println!("\n{}\n", texto.trim());
    eprintln!("audio {dur:.1} s · cómputo ASR {computo:.1} s · RTF {:.2}", computo / dur);
    Ok(())
}
