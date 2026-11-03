//! Configuración persistente en ~/.config/iurevoz/config.toml.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "mx.iurefficient.IureVoz";

/// Ícono de la app (el que le dibuja iureapps), para la ventana y el lanzador de desarrollo.
pub const ICONO_PNG: &[u8] = include_bytes!("../packaging/icono/png/256.png");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModeloAsr {
    /// Nemotron 3.5 ASR Streaming 560 ms: texto en vivo y final en ~1.2 s.
    Nemotron,
    /// Moonshine base es: menos RAM, sin parciales, final más lento.
    Moonshine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Inyeccion {
    /// Teclea en la ventana activa vía el portal RemoteDesktop.
    Teclear,
    /// Solo deja el texto en la ventana de iurevoz.
    Ninguna,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Servicio {
    /// llama-server administrado por la app (local, sin instalar nada).
    Local,
    /// API nativa de Ollama (local).
    Ollama,
    /// OpenRouter (nube): el texto sale de la máquina.
    OpenRouter,
    /// Cualquier servidor OpenAI-compatible (llama-server, LM Studio…).
    OpenAi,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Correccion {
    pub activa: bool,
    pub servicio: Servicio,
    pub url: String,
    pub modelo: String,
    /// Variable de entorno con la API key (solo OpenRouter / OpenAI). Tiene prioridad.
    pub variable_clave: String,
    /// API key guardada (para cuando la app se abre desde el menú, sin el entorno del shell).
    pub clave: Option<String>,
    /// Términos del dominio que el modelo debe respetar.
    pub glosario: Vec<String>,
    /// Si el modelo cambia más de esta fracción de palabras, se conserva el original.
    pub max_cambio: f32,
}

impl Default for Correccion {
    fn default() -> Self {
        Self {
            activa: false,
            servicio: Servicio::Local,
            url: String::new(),
            modelo: "gemma4-e2b".into(),
            variable_clave: "OPENROUTER_API_KEY".into(),
            clave: None,
            glosario: vec![],
            max_cambio: 0.25,
        }
    }
}

impl Correccion {
    pub fn clave_efectiva(&self) -> Option<String> {
        std::env::var(&self.variable_clave).ok().filter(|c| !c.is_empty()).or_else(|| self.clave.clone().filter(|c| !c.is_empty()))
    }

    pub fn url_por_omision(servicio: Servicio) -> &'static str {
        match servicio {
            Servicio::Local => "",
            Servicio::Ollama => "http://localhost:11434",
            Servicio::OpenRouter => "https://openrouter.ai/api/v1",
            Servicio::OpenAi => "http://localhost:8080/v1",
        }
    }
}

/// Modo comando: instrucciones habladas que un LLM traduce a acciones de escritorio.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Comandos {
    pub activos: bool,
    /// LLM que interpreta (mismos campos que la corrección).
    pub llm: Correccion,
    /// Etapa 2: permitir delegar a Claude Code (cuenta del usuario), con confirmación.
    pub claude: bool,
}

impl Default for Comandos {
    fn default() -> Self {
        // para instrucciones conviene un modelo que razone mejor aunque sea más lento
        let llm = Correccion { activa: true, modelo: "qwen3.5-4b".into(), ..Correccion::default() };
        Self { activos: true, llm, claude: true }
    }
}

/// Modo «siempre escuchando» con palabra clave.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Escucha {
    pub siempre: bool,
    pub palabra: String,
    /// 0 = exacto; más alto acepta más variantes (y más falsas activaciones).
    pub tolerancia: f32,
}

impl Default for Escucha {
    fn default() -> Self {
        Self { siempre: false, palabra: "oye compu".into(), tolerancia: 0.34 }
    }
}

/// Iurefficient: resumen del día y avisos (F5).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct IureCfg {
    /// Decir «tu día» al abrir IureVoz.
    pub resumen_al_iniciar: bool,
    /// Sondear la instancia y avisar tareas que vencen y notificaciones nuevas.
    pub avisos: bool,
    /// Además de la notificación de escritorio, decirlos en voz alta.
    pub avisos_hablados: bool,
    pub cada_minutos: u32,
}

impl Default for IureCfg {
    fn default() -> Self {
        Self { resumen_al_iniciar: false, avisos: true, avisos_hablados: false, cada_minutos: 15 }
    }
}

/// Voz (TTS) local.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct VozCfg {
    /// Responder en voz alta a los comandos.
    pub responder: bool,
    pub voz: String,
    pub velocidad: f32,
}

impl Default for VozCfg {
    fn default() -> Self {
        Self { responder: true, voz: "piper-claude".into(), velocidad: 1.0 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub modelo_asr: ModeloAsr,
    pub hilos: i32,
    /// Silencio (s) que cierra una frase.
    pub silencio: f32,
    pub puntuacion_dictada: bool,
    /// Comandos de formato Markdown al dictar («título uno», «abre negritas»…), p. ej. en IureEditor.
    pub dictado_markdown: bool,
    /// Guardar un registro de diagnóstico en `~/.local/state/iurevoz/iurevoz.log` (incluye lo dictado).
    pub registro: bool,
    pub inyeccion: Inyeccion,
    pub correccion: Correccion,
    pub comandos: Comandos,
    pub voz: VozCfg,
    pub escucha: Escucha,
    pub iure: IureCfg,
    /// Token del portal RemoteDesktop para no volver a pedir permiso.
    pub token_portal: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            modelo_asr: ModeloAsr::Nemotron,
            hilos: 4,
            silencio: 0.5,
            puntuacion_dictada: true,
            dictado_markdown: false,
            registro: false,
            inyeccion: Inyeccion::Teclear,
            correccion: Correccion::default(),
            comandos: Comandos::default(),
            voz: VozCfg::default(),
            escucha: Escucha::default(),
            iure: IureCfg::default(),
            token_portal: None,
        }
    }
}

pub fn dir_config() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("iurevoz")
}

/// Modelos y datos grandes: en Windows van a `%LOCALAPPDATA%` (no viajan con el perfil).
pub fn dir_datos() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("iurevoz")
}

impl Config {
    fn ruta() -> PathBuf {
        dir_config().join("config.toml")
    }

    pub fn cargar() -> Self {
        match std::fs::read_to_string(Self::ruta()) {
            Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
                eprintln!("config.toml inválido, uso valores por omisión: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn guardar(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir_config())?;
        let ruta = Self::ruta();
        std::fs::write(&ruta, toml::to_string_pretty(self)?)?;
        // puede contener la API key (en Windows, %APPDATA% ya es solo del usuario)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ruta, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}
