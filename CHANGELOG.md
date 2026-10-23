# Changelog

Versiones de IureVoz. El flujo `.github/workflows/release.yml` toma las notas de la
sección `## vX.Y.Z` al subir la etiqueta.

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
