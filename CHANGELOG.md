# Changelog

Versiones de IureVoz. El flujo `.github/workflows/release.yml` toma las notas de la
sección `## vX.Y.Z` al subir la etiqueta.

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
