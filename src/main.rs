//! iurevoz: dictado por voz local para Linux.

mod agente;
mod asr;
mod atajos_gnome;
mod audio;
mod config;
mod control;
mod correccion;
mod escritorio;
mod modelos;
mod motor;
mod portal;
mod puntuacion;
mod teclas;
mod ui;
mod voz;

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
  iurevoz comando \"instrucción\"   ejecuta una instrucción sin interfaz (prueba del modo comando)
  iurevoz instalar-atajos       crea atajos de GNOME (Ctrl+Alt+Espacio, Ctrl+Alt+C, Ctrl+Alt+L)
  iurevoz leer | callar         lee la selección en voz alta en la instancia abierta
  iurevoz decir \"texto\" [--voz CLAVE] [--guardar A.wav]
                                habla con la voz local (claves: piper-ald, piper-claude,
                                piper-daniela, kokoro-dora, kokoro-alex, supertonic)
  iurevoz --sin-portal          abre la ventana sin teclear ni atajo global
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "ayuda") => print!("{AYUDA}"),
        Some(o @ ("alternar" | "leer" | "callar")) if args.len() == 1 => {
            if !control::enviar_a_instancia(o) {
                bail!("iurevoz no está abierto");
            }
        }
        Some("instalar-atajos") => println!("{}", atajos_gnome::instalar()?),
        Some("comando") if args.len() == 1 => {
            if !control::enviar_a_instancia("comando") {
                bail!("iurevoz no está abierto");
            }
        }
        Some("comando") => {
            let cfg = Config::cargar();
            let mudo = args.iter().any(|a| a == "--mudo");
            let instruccion = args[1..].iter().filter(|a| *a != "--mudo").cloned().collect::<Vec<_>>().join(" ");
            let voz = (cfg.voz.responder && !mudo).then(|| voz::Voz::iniciar(cfg.voz.voz.clone(), cfg.voz.velocidad, |m| eprintln!("{m}")));
            let portal = portal::Portal::iniciar(cfg.token_portal.clone(), false, |a| {
                if let portal::Aviso::TokenTeclado(t) = a {
                    let mut c = Config::cargar();
                    c.token_portal = Some(t);
                    let _ = c.guardar();
                }
            });
            let t = Instant::now();
            let ag = agente::Agente::new(cfg.comandos.llm.clone(), Some(portal), voz.clone());
            let inf = ag.ejecutar(&instruccion, |a| println!("  · {a}"))?;
            println!("{}  ({:.1} s)", inf.respuesta, t.elapsed().as_secs_f32());
            if let Some(v) = voz {
                v.decir(inf.respuesta);
                // espera a que termine de hablar
                std::thread::sleep(std::time::Duration::from_millis(300));
                while v.hablando.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            // da tiempo a que el portal termine de teclear
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Some("decir") => {
            let texto = args.get(1).context("falta el texto")?;
            let opt = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
            let clave = opt("--voz").unwrap_or_else(|| "piper-ald".into());
            if voz::falta(&clave).is_some() {
                eprintln!("Descargando la voz {clave}…");
                voz::descargar(&clave, |f| eprint!("\r{:.0}%   ", f * 100.0))?;
                eprintln!();
            }
            let t = Instant::now();
            let s = voz::Sintetizador::cargar(&clave, 1.0)?;
            let carga = t.elapsed().as_secs_f32();
            let t = Instant::now();
            let (m, sr) = s.generar(texto)?;
            let tgen = t.elapsed().as_secs_f32();
            let dur = m.len() as f32 / sr as f32;
            eprintln!("{clave}: carga {carga:.1} s · {dur:.1} s de audio en {tgen:.2} s (RTF {:.2})", tgen / dur);
            match opt("--guardar") {
                Some(r) => drop(sherpa_onnx::write(&r, &m, sr)),
                None => voz::reproducir(&m, sr, &std::sync::atomic::AtomicBool::new(false))?,
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
