# Changelog

Versiones de IureVoz. El flujo `.github/workflows/release.yml` toma las notas de la
sección `## vX.Y.Z` al subir la etiqueta.

## v0.4.0 — 2026-11-13

### Agregado
- **Versión para macOS** (alfa), universal para Apple Silicon e Intel, macOS 11 o
  posterior. Teclea con eventos de Quartz (pide el permiso de Accesibilidad una vez),
  registra los atajos ⌃⌥Espacio, ⌃⌥C y ⌃⌥L, pone su ícono en la barra de menús, guarda
  las credenciales de la suite en el Llavero, abre las apps por su nombre en español y
  descarga el `llama-server` de Mac, que usa la GPU con Metal. Cerrar la ventana esconde
  la app y IureVoz sigue escuchando. En el modo comando, Ctrl se pulsa como ⌘.
- **Instaladores de macOS:** `IureVoz-<versión>-macos-universal.dmg` (se arrastra a
  Aplicaciones) y `-macos-universal.app.tar.gz`, el que usa **iureapps** para
  instalarla, abrirla y desinstalarla en la Mac.

## v0.3.0 — 2026-11-03

### Agregado
- **Versión para Windows** (alfa). Teclea con `SendInput`, registra los atajos
  Ctrl+Alt+Espacio, Ctrl+Alt+C y Ctrl+Alt+L, pone su ícono junto al reloj, guarda las
  credenciales de la suite en el Administrador de credenciales y descarga el
  `llama-server` de Windows con Vulkan. Con el ícono junto al reloj, cerrar la ventana
  la esconde y IureVoz sigue escuchando.
- **Instaladores nuevos:** `.rpm` para Fedora y openSUSE, y en Windows un `.exe` que
  instala para el usuario y un `.msi` para todo el equipo (políticas, Intune), además
  del `.zip`. La versión publica un `SHA256SUMS`.
- **Ícono propio**, el que le dibuja iureapps, en la ventana, el menú, las
  notificaciones, los paquetes y el `.exe`.
- Se instala y actualiza desde **iureapps** en Linux y en Windows.
- La voz lee de las rutas de Windows solo el nombre final, como con las de Linux.

### Cambiado
- **Código abierto** con licencia Apache-2.0.
- Los paquetes se llaman `IureVoz-<versión>-linux-x64.deb`, `-linux-x64.rpm`,
  `-windows-x64.exe`, etc.

## v0.2.2 — 2026-10-23

### Corregido
- **Al iniciar la sesión ya no se abre la ventana ni falta el ícono.** Si la extensión
  de indicadores llegaba después que IureVoz, el ícono no se ponía, la ventana se abría
  sola y al cerrarla volvía a abrirse. Ahora el ícono aparece en cuanto la barra está
  lista; solo si no aparece en 30 s se abre la ventana, una vez, y cerrarla sin ícono
  sale de IureVoz.

### Cambiado
- **Enrutamiento con el modelo.** Antes de actuar, una consulta corta con el catálogo
  de lo que IureVoz sabe hacer decide a quién le toca: Iurefficient, las apps de
  escritorio, Claude (sistema), el escritorio o la charla. Las palabras clave quedan de
  respaldo. «Transcribe la siguiente junta» ya va a IureTranscribe (graba y transcribe
  al detener) y no a la agenda de Iurefficient.
- «¿Qué puedes hacer?» responde con el catálogo.

## v0.2.1 — 2026-10-22

### Cambiado
- **La ventana solo se ve cuando se pide**, como en IureDav: cerrarla la quita y
  IureVoz sigue con su ícono; «Mostrar IureVoz» o abrirlo otra vez desde el menú la
  vuelve a crear con todo lo dictado. Al iniciar la sesión arranca solo con el ícono.
- Ajustes se desplaza con scroll cuando no cabe en la ventana.

## v0.2.0 — 2026-10-21

### Añadido
- **Ícono en la barra superior** con el estado: gris inactivo, verde esperando «oye
  compu», rojo dictando y azul escuchando una instrucción. Su menú muestra la ventana,
  empieza o detiene el dictado, enciende o apaga «oye compu» y sale.
- **Abrir al iniciar la sesión** (*Ajustes → Inicio*), minimizado y con su ícono.

### Cambiado
- Con el ícono en la barra, cerrar la ventana la minimiza: IureVoz sigue escuchando.
  «Salir» del menú del ícono lo termina.

## v0.1.0 — 2026-10-21

Primera versión publicada, para Linux con GNOME en Wayland (paquete .deb).

### Dictado
- Reconocimiento local con Nemotron (o Moonshine sin GPU) vía sherpa-onnx; teclea en
  cualquier ventana por el portal RemoteDesktop, con acentos en cualquier distribución.
- Puntuación dictada («coma», «punto y aparte»…), viñetas y listas numeradas, Markdown
  opcional y corrección con un LLM.
- Edición por voz: «borra la palabra», «borra la línea», «deshaz».
- En IureEditor 1.13.0+ el dictado entra directo al documento aunque la ventana esté detrás.

### Órdenes
- «Oye compu» siempre escuchando; «silencio» o «cállate» callan la voz mientras habla.
- Agente con herramientas básicas del escritorio y LLM local (llama-server) u OpenRouter.
- Tareas de sistema (scripts, comandos, instalar, mover archivos) a Claude Code, con permiso.
- Voz local (Piper, Kokoro, Supertonic).

### Suite Iurefficient
- Proyectos, tareas, vencimientos, documentos, CRM (oportunidades y prospectos) y «¿qué
  tengo hoy?» de la instancia; registrar horas, crear tareas y eventos con confirmación.
- IureDav, IureOCR, IureTranscribe e IureEditor desde la voz.
- `iurevoz --mcp`: Claude Code puede hablar, preguntar y tomar dictado a través de IureVoz.
