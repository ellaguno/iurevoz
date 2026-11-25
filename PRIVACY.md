# Política de privacidad de IureVoz / IureVoz privacy policy

*Última actualización / Last updated: 2026-11-25* · [Español](#español) · [English](#english)

## Español

IureVoz (`iurevoz`) es una app de escritorio de dictado y órdenes por voz: escribe lo que dictas en
la ventana activa, entiende instrucciones habladas y lee texto en voz alta. Forma parte de la suite
Iurefficient y la publica Eduardo Llaguno Velasco («nosotros»). Esta política explica qué datos
maneja (incluida su línea de comandos).

**Resumen:** IureVoz no tiene telemetría, analítica ni reportes automáticos de fallos, y no nos
envía datos tuyos. La voz se reconoce **en tu equipo**: el audio del micrófono no sale de él ni se
guarda. Sólo sale texto, y sólo hacia los servicios que tú configures (un modelo de lenguaje en la
nube, Claude Code o tu instancia de Iurefficient).

### El micrófono y tu voz
- IureVoz usa el micrófono mientras dictas o das una orden y, si activas el modo «siempre
  escuchando», de forma continua para detectar la palabra clave. El reconocimiento de voz
  (Nemotron o Moonshine con sherpa-onnx) y el detector de la palabra clave corren localmente; el
  audio se procesa en memoria y se descarta.
- El audio no se graba en disco. La única excepción es un modo de diagnóstico que sólo se enciende
  a mano con la variable de entorno `IUREVOZ_GRABAR`: entonces guarda cada dictado como `.wav` en la
  carpeta de datos de la app, para entrenar el detector de la palabra clave en tu equipo.
- La voz con que IureVoz responde (Piper, Kokoro, Supertonic) también se genera localmente.

### Qué sale de tu equipo
1. **Descarga de modelos.** La primera vez que los necesitas, IureVoz descarga los modelos de voz
   desde GitHub (`github.com/k2-fsa/sherpa-onnx`) y, si usas el modelo de lenguaje local, el
   servidor `llama-server` de llama.cpp (`github.com/ggml-org/llama.cpp`) y el modelo desde Hugging
   Face (`huggingface.co`). No envía datos tuyos; esos sitios reciben, como en cualquier conexión,
   tu dirección IP.
2. **Modelo de lenguaje (opcional).** La corrección del dictado y el modo de órdenes usan un modelo
   de lenguaje (IA generativa). Por omisión es uno **local** (`llama-server` en tu equipo), y el
   texto no sale. Si eliges otro servicio en *Ajustes*, IureVoz le envía el texto dictado o la
   instrucción (con tu glosario y, en el modo de órdenes, el historial de la conversación):
   OpenRouter (`openrouter.ai`, en la nube) o el servidor compatible con OpenAI u Ollama cuya
   dirección configures. Su tratamiento lo rige la política de ese servicio.
3. **Claude Code (opcional).** Si delegas una tarea a Claude Code, IureVoz ejecuta el programa
   `claude` instalado en tu equipo con tu propia cuenta y le pasa la instrucción; lo que Claude
   Code envíe a Anthropic lo rige su política. Cada acción que no sea de sólo lectura se te
   pregunta antes. Si registras IureVoz como servidor MCP de Claude Code, Claude Code puede pedirle
   que hable, que te pregunte algo o que dictes.
4. **Iurefficient (opcional).** Si tu equipo tiene una cuenta de Iurefficient activa (la que dejan
   las demás apps de la suite), IureVoz se conecta a *esa* instancia para responder preguntas sobre
   tu agenda, tareas y notificaciones, darte el resumen del día y avisos periódicos, y, cuando lo
   pides por voz, registrar horas, tareas o eventos. Crea para ello un token MCP de IureVoz. El
   tratamiento de esos datos lo rige la política de privacidad de quien opera la instancia
   (normalmente tu organización).
5. **Páginas y búsquedas que pides.** Cuando le pides abrir un sitio o buscar algo, IureVoz abre tu
   navegador (las búsquedas, en DuckDuckGo). Cuando le pides leer una página o noticias, la
   descarga directamente para leerla en voz alta.
6. **Otras apps de la suite.** Puede abrir y usar en tu equipo IureDav, IureOCR, IureTranscribe e
   IureEditor si las tienes instaladas.

### Qué hace en tu equipo
- **Escribe en otras ventanas.** Para dictar, IureVoz teclea el texto en la ventana activa
  (`SendInput` en Windows) y registra atajos de teclado globales (Ctrl+Alt+Espacio, Ctrl+Alt+C y
  Ctrl+Alt+L). No registra lo que tecleas tú.
- **Portapapeles.** Para leer en voz alta el texto seleccionado lo copia con Ctrl+C, lo lee y
  restaura lo que tenías en el portapapeles.
- **Órdenes de escritorio.** Una lista cerrada de acciones: abrir apps, carpetas o sitios, cambiar
  el volumen, hacer cuentas, decir la hora. No ejecuta comandos de la terminal por su cuenta.

### Qué se guarda en tu equipo
- Las preferencias, el glosario y, si la configuras, la clave de API del servicio de modelo de
  lenguaje, en `config.toml` de la carpeta de configuración de tu usuario.
- Los modelos de voz y de lenguaje descargados y el detector de la palabra clave que entrenes, en
  la carpeta de datos de la app.
- Si activas el registro de diagnóstico: un archivo `iurevoz.log` que incluye lo que dictas y lo
  que oye el reconocedor. Está apagado por omisión y nunca sale de tu equipo.
- Si usas Iurefficient: el token MCP de IureVoz y la sesión compartida de la suite en el llavero
  del sistema (Administrador de credenciales de Windows, Llavero de macOS o Secret Service en
  Linux).
- En la versión de la Microsoft Store, desinstalar la app borra su configuración, el registro y
  los modelos; las credenciales del llavero, que comparte con las demás apps de la suite, quedan.

### Actualizaciones
IureVoz no se actualiza solo. En la versión de la Microsoft Store la actualiza la tienda.

### Niños
IureVoz es una herramienta de trabajo y no está dirigida a menores de 13 años.

### Tus opciones y contacto
Puedes usar IureVoz sin cuenta y sólo con modelos locales, de modo que nada de lo que dictas salga
de tu equipo; el permiso del micrófono se retira en la configuración de privacidad del sistema.
Para preguntas sobre esta política, abre un *issue* en <https://github.com/ellaguno/iurevoz/issues>.
Si la política cambia, la nueva versión se publica en esta misma página con su fecha.

## English

IureVoz (`iurevoz`) is a desktop app for voice dictation and voice commands: it types what you
dictate into the active window, understands spoken instructions and reads text aloud. It is part of
the Iurefficient suite and is published by Eduardo Llaguno Velasco ("we"). This policy explains
what data it handles (including its command line).

**Summary:** IureVoz has no telemetry, analytics or automatic crash reports, and sends no data
about you to us. Speech is recognized **on your computer**: microphone audio never leaves it and is
not stored. Only text leaves your computer, and only to the services you configure (a cloud
language model, Claude Code or your Iurefficient instance).

### The microphone and your voice
- IureVoz uses the microphone while you dictate or give a command and, if you turn on "always
  listening", continuously to detect the wake word. Speech recognition (Nemotron or Moonshine with
  sherpa-onnx) and the wake-word detector run locally; audio is processed in memory and discarded.
- Audio is not written to disk. The only exception is a diagnostic mode that is turned on manually
  with the `IUREVOZ_GRABAR` environment variable: it then saves each dictation as a `.wav` file in
  the app's data folder, to train the wake-word detector on your computer.
- The voice IureVoz answers with (Piper, Kokoro, Supertonic) is also generated locally.

### What leaves your computer
1. **Model downloads.** The first time they are needed, IureVoz downloads the speech models from
   GitHub (`github.com/k2-fsa/sherpa-onnx`) and, if you use the local language model, llama.cpp's
   `llama-server` (`github.com/ggml-org/llama.cpp`) and the model from Hugging Face
   (`huggingface.co`). It sends no data about you; as with any connection, those sites receive your
   IP address.
2. **Language model (optional).** Dictation correction and command mode use a language model
   (generative AI). By default it is a **local** one (`llama-server` on your computer) and the text
   stays there. If you choose another service in *Settings*, IureVoz sends it the dictated text or
   the instruction (with your glossary and, in command mode, the conversation history): OpenRouter
   (`openrouter.ai`, in the cloud) or the OpenAI-compatible or Ollama server whose address you set.
   That service's policy governs how it handles them.
3. **Claude Code (optional).** If you delegate a task to Claude Code, IureVoz runs the `claude`
   program installed on your computer with your own account and passes it the instruction; what
   Claude Code sends to Anthropic is governed by its policy. Every action that is not read-only is
   asked of you first. If you register IureVoz as a Claude Code MCP server, Claude Code can ask it
   to speak, to ask you something or to let you dictate.
4. **Iurefficient (optional).** If your computer has an active Iurefficient account (the one the
   other suite apps leave), IureVoz connects to *that* instance to answer questions about your
   calendar, tasks and notifications, give you a daily summary and periodic reminders, and, when
   you ask by voice, log hours, tasks or events. For this it creates an IureVoz MCP token. Those
   data are governed by the privacy policy of whoever runs that instance (usually your
   organization).
5. **Pages and searches you ask for.** When you ask it to open a site or search for something,
   IureVoz opens your browser (searches go to DuckDuckGo). When you ask it to read a page or the
   news, it downloads the page directly to read it aloud.
6. **Other suite apps.** It can open and use IureDav, IureOCR, IureTranscribe and IureEditor on
   your computer if you have them installed.

### What it does on your computer
- **Types into other windows.** To dictate, IureVoz types the text into the active window
  (`SendInput` on Windows) and registers global keyboard shortcuts (Ctrl+Alt+Space, Ctrl+Alt+C and
  Ctrl+Alt+L). It does not record what you type.
- **Clipboard.** To read the selected text aloud it copies it with Ctrl+C, reads it and restores
  what was on your clipboard.
- **Desktop commands.** A closed list of actions: open apps, folders or sites, change the volume,
  do arithmetic, tell the time. It does not run terminal commands on its own.

### What is stored on your computer
- Preferences, the glossary and, if you set one, the language-model service API key, in
  `config.toml` in your user's configuration folder.
- Downloaded speech and language models and any wake-word detector you train, in the app's data
  folder.
- If you turn on the diagnostic log: an `iurevoz.log` file that includes what you dictate and what
  the recognizer hears. It is off by default and never leaves your computer.
- If you use Iurefficient: the IureVoz MCP token and the suite's shared session in the system
  keychain (Windows Credential Manager, macOS Keychain or Secret Service on Linux).
- In the Microsoft Store version, uninstalling the app deletes its settings, log and models; the
  keychain credentials, shared with the other suite apps, remain.

### Updates
IureVoz does not update itself. In the Microsoft Store version the Store updates it.

### Children
IureVoz is a work tool and is not directed to children under 13.

### Your choices and contact
You can use IureVoz without an account and with local models only, so nothing you dictate leaves
your computer; microphone permission can be withdrawn in the system's privacy settings. For
questions about this policy, open an issue at <https://github.com/ellaguno/iurevoz/issues>. If the
policy changes, the new version is published on this same page with its date.
