# Plan: IureVoz dentro de la suite Iurefficient

Objetivo: preguntarle por voz a Iurefficient («¿qué tengo hoy?», «¿cómo va el expediente de Juárez?», «registra dos horas en el amparo de Pérez»), abrirlo donde haga falta y encadenarlo con las apps de escritorio de la suite, reutilizando lo que ya existe.

## Lo que ya existe (análisis, 2026-10-20)

| Pieza | Qué ofrece | Cómo se usa desde IureVoz |
|---|---|---|
| **Instancia Iurefficient** (`iur-lms-sso-wt`, Flask + Next.js) | Proyectos/expedientes, tareas, vencimientos, agenda (`calendar_events`), horas, notificaciones, documentos, tickets, CRM | — |
| **MCP remoto** `POST /api/mcp` (Streamable HTTP, protocolo 2025-06-18) | 8 herramientas **de solo lectura**: `listar_proyectos`, `detalle_proyecto`, `tareas_del_proyecto`, `proximos_vencimientos`, `buscar_en_expedientes`, `buscar_en_proyecto`, `listar_documentos`, `leer_documento` | Token `iurmcp_…` por usuario (hereda su visibilidad, revocable, 365 días) |
| **REST** `/api/*` | Escritura: `/api/tasks`, `/api/calendar-events`, `/api/deadlines`, `/api/.../time-entries`, `/api/timesheet`, `/api/notifications` | Sesión JWT en cookies + CSRF + TOTP |
| **iurefficient-connect** (crate Rust v0.7.1) | Cliente MCP (`mcp.rs`), servidor MCP stdio (`mcp_server.rs`, convención `<App> --mcp`), registro en Claude Desktop/VS Code (`agents.rs`), sesión REST con TOTP (`rest.rs`), operaciones tipadas (`api.rs`: horas, compromisos→tareas, documentos, CRM, crear token MCP), credenciales en el llavero **compartidas por todas las apps** (`secrets.rs`) | Dependencia directa: misma cuenta y sesión que IureTranscribe, IureDav, IureEditor, IureOCR |
| **IureTranscribe** `--mcp` | `transcribe_file`, `read_transcript`, `list_recordings`… | Cliente MCP stdio |
| **IureOCR** `--mcp` | `ocr_file`, `extract_text`, `pdf_info`… | Cliente MCP stdio |
| **Interfaz web** | Rutas `/dashboard/cases/<id>`, `/dashboard/tasks/<id>`, `/dashboard/calendar`, `/dashboard/deadlines`, `/dashboard/timesheet`, `/dashboard/notifications` | Enlaces directos para «ábrelo» |
| **Agenda local de GNOME** (Evolution Data Server) | Calendario «Personal» local, hoy casi vacío; sin cuentas en línea | Opcional, baja prioridad |

Huecos: el MCP no tiene «mis tareas», «agenda de hoy», «notificaciones» ni «horas»; no hay CalDAV en la suite (solo exportar ICS); el MCP no escribe.

## Apps de escritorio de la suite (instaladas aquí: IureTranscribe 0.8.0, IureOCR 0.5.1, IureEditor 1.11.2, IureDav 0.8.1)

Hay dos formas de hablarles, y cada una sirve para algo distinto:

- **MCP stdio (`<app> --mcp`)**: arranca un proceso nuevo y trabaja en segundo plano (sin ventana). Sirve para OCR, extraer texto o transcribir un archivo. **No controla la ventana abierta.**
- **Ventana abierta**: las cuatro usan instancia única con enlaces propios (`iuretranscribe://`, `iureditor://`, `iureocr://`, `iuredav://`): relanzar el binario con una ruta o un enlace le llega a la ventana que ya está abierta. Además IureVoz puede teclear en la ventana enfocada (ya funciona).

`iurefficient-connect` ya trae `apps::locate` (encuentra los binarios), `apps::launch` / `apps::open_with` (abrir con ruta o enlace) y `apps::iuredav_mounts()` (puntos de montaje de IureDav): no hay que reinventarlo.

| App | Qué se podrá pedir por voz | Cómo | ¿Cambios en la app? |
|---|---|---|---|
| **IureEditor** (Tiptap) | Dictar dentro del documento; «abre en el editor el escrito de Juárez»; «documento nuevo»; formato con atajos («negritas», «título») | Dictado: ya funciona hoy (se teclea donde está el cursor). Abrir: `iureditor://open?path=…` o `iureditor://iurefficient/doc?case=&doc=`. Formato: herramienta `teclas` (Ctrl+B…) o un **modo dictado Markdown** en IureVoz («título uno» → `# `, «viñeta» → `- `) cuando la ventana activa es IureEditor | No; opcional `iureditor://new` |
| **IureDav** | «¿Qué archivos hay en el expediente Juárez?», «busca el contrato de arrendamiento», «ábrelo», «léemelo» | `apps::iuredav_mounts()` → montaje (aquí `~/Iurefficient-syncrum`); herramientas de IureVoz para listar/buscar en esa carpeta y abrir (`gio open` o en IureEditor); leer: texto directo, PDF vía IureOCR `extract_text`. Si no está montado, se avisa o se abre IureDav | No |
| **IureTranscribe** | «Graba la junta» / «detén la grabación» (con hablantes), «transcribe la última grabación», «¿qué se acordó?» | Grabar en la ventana: hoy `iuretranscribe://record` solo cambia de vista → **agregar acciones** `iuretranscribe://record/start?speakers=…` y `/stop` que llamen a `start_recording`/`stop_recording`. Transcribir y leer: MCP (`transcribe_file`, `transcription_status`, `list_recordings`, `read_transcript`), con progreso en la tarjeta de tareas como Claude | **Sí, pequeño**: dos acciones de enlace en `state.svelte.ts` (+ quizá `recording_status` por MCP) |
| **IureOCR** | «Pásale OCR a este PDF», «léeme la página 3», «gira la página 2», «quita las páginas 5 a 7», «¿cuántas páginas tiene?» | MCP directo: `ocr_file` (con progreso y cancelación), `extract_text`, `pdf_info`, `edit_pdf_pages` (con confirmación: crea un PDF nuevo junto al original). «Este PDF» = el seleccionado en Archivos o el último mencionado | No |

Detalle: mientras IureTranscribe graba, IureVoz sigue pudiendo usar el micrófono (PipeWire permite varias capturas), pero conviene pausar «siempre escuchando» para no transcribir la junta como instrucciones.

## Arquitectura propuesta

```
voz ─▶ agente IureVoz ─┬─ herramientas de escritorio (hoy)
                       ├─ «iurefficient»: sub-agente ─┬─ MCP remoto /api/mcp (lectura, descubierto dinámicamente)
                       │                              └─ REST vía connect (escrituras, siempre con confirmación)
                       ├─ MCP stdio de la suite: IureTranscribe, IureOCR (descubierto dinámicamente)
                       ├─ enlaces a ventanas abiertas: iureditor://, iuretranscribe://record/start, IureDav (montaje)
                       └─ Claude Code (etapa 2, ya existe)
IureVoz --mcp ◀─ Claude Desktop / Claude Code (decir en voz alta, pedir confirmación, dictar)
```

Decisiones de diseño:

1. **Depender de `iurefficient-connect`**, no reimplementar. Ganamos el inicio de sesión compartido: si el usuario ya entró desde IureTranscribe, IureVoz funciona sin pedir nada. Cualquier wrapper REST nuevo va en connect y lo aprovechan las demás apps.
2. **Puente MCP genérico**: al arrancar (y cada cierto tiempo) se piden `tools/list` al servidor remoto y a los MCP stdio configurados, y se ofrecen al modelo con prefijo (`iure_listar_proyectos`, `transcribe_read_transcript`). Una herramienta nueva en el servidor aparece en IureVoz sin cambiar código.
3. **Sub-agente «iurefficient»** en vez de mezclar 20+ herramientas en un solo prompt: el agente principal ve una sola herramienta `iurefficient(pregunta)`; esta corre un segundo ciclo con las herramientas de Iurefficient. Los modelos locales de 2–4B se degradan con muchas herramientas y el prompt crece (el precalentamiento de hoy ya tarda ~25 s con 12).
4. **Lectura libre, escritura con permiso**: todo lo que escribe (horas, tareas, eventos, marcar leídas) pasa por la misma confirmación de voz/tarjeta que ya usan los permisos de Claude, con un resumen hablado («Registro 2 horas en Amparo Pérez, hoy, “revisión de demanda”. ¿Lo hago?»).
5. **Privacidad**: los datos de expedientes van al LLM que elija el usuario. Con el LLM local integrado no salen de la máquina (salvo hacia la propia instancia). Opción «Iurefficient solo con LLM local» activada por omisión; si el modo comando usa OpenRouter, las preguntas de Iurefficient se resuelven con el local o se advierte.

## Fases

| Fase | Entrega | Dónde | Tamaño |
|---|---|---|---|
| **F0 · Cuenta** | «Conectar Iurefficient» en Ajustes: URL de instancia, usuario, contraseña y TOTP con `rest.rs`; crea el token MCP (`create_mcp_token`) y lo guarda en el llavero compartido. Si ya existe la cuenta de otra app de la suite, la reutiliza. | iurevoz | S |
| **F1 · Preguntar** | Cliente MCP remoto, puente genérico y sub-agente. Preguntas: proyectos, detalle y tareas de un proyecto, vencimientos próximos, búsqueda en expedientes y documentos («¿qué dice el último escrito de Juárez?» → resumen hablado). | iurevoz | M |
| **F2 · Mi día** | Herramientas MCP nuevas de solo lectura en el servidor: `mis_tareas` (asignadas, vencidas/hoy/semana), `agenda` (eventos + vencimientos + entregas de tareas en un rango), `notificaciones` (no leídas, menciones), `mis_horas` (semana, por aprobar), `buscar_cliente`. Con F1 aparecen solas en IureVoz. | iur-lms (instancia) | M |
| **F3 · Ábrelo** | `abrir_en_iurefficient(tipo, id/nombre)`: resuelve el id con MCP y abre `/dashboard/cases/<id>`, `/tasks/<id>`, `/calendar`, `/timesheet`… en el navegador. | iurevoz | S |
| **F4 · Escribir** | Con confirmación: registrar horas (ya en `api.rs`), crear tarea, crear evento de agenda, marcar notificaciones leídas, dictar una nota o comentario a una tarea. Wrappers nuevos en connect (`tasks`, `calendar_events`, `notifications`). | connect + iurevoz | M |
| **F5 · Resumen y avisos** | «¿Qué tengo hoy?» como comando rápido; resumen hablado opcional al iniciar sesión; aviso por voz/notificación de vencimientos y menciones (sondeo local cada 15 min, sin servidor nuevo). | iurevoz | S |
| **F6a · Apps de escritorio** | Cliente MCP stdio (IureOCR, IureTranscribe) con progreso; IureDav (montaje, listar/buscar/abrir/leer); IureEditor (abrir por enlace, modo dictado Markdown al detectar su ventana); IureTranscribe grabar/detener por enlace | iurevoz + acciones `record/start|stop` en iuretranscribe | M |
| **F6b · IureVoz como MCP** | `iurevoz --mcp` con el `Handler` de connect: `decir`, `preguntar_por_voz` (confirmación hablada), `dictar` — Claude Desktop/Code podrían hablar y escuchar a través de IureVoz. Registro en Claude Desktop con `agents.rs`. | iurevoz | S |
| **F7 · Agenda local (opcional)** | Leer/crear eventos del calendario de GNOME (Evolution Data Server por D-Bus) o suscribir el ICS de Iurefficient en GNOME Calendar. Solo si el usuario usa esa agenda. | iurevoz | S |

Orden sugerido: F0 → F1 → F3 → F6a → F2 → F5 → F4 → F6b → F7. F1+F3 ya dan valor con lo que el servidor ofrece hoy; F6a casi no necesita la instancia (IureOCR, IureDav e IureEditor funcionan con archivos locales) y puede ir incluso antes de F0; F2 vuelve útil «mi día»; las escrituras van después, cuando la lectura sea confiable.

## Ejemplos de uso al terminar F1–F5

- «Oye compu, ¿qué tengo hoy?» → «Tienes 3 tareas para hoy, la audiencia de Pérez a las 10 y vence el plazo del amparo de Juárez el jueves.»
- «¿Cómo va el expediente de Juárez?» → resumen de tareas abiertas y últimos movimientos.
- «Ábrelo» (con memoria de conversación) → abre el expediente en el navegador.
- «Busca en los expedientes dónde se habló de la suspensión provisional» → resultados leídos en resumen.
- «Registra hora y media en Juárez por revisión de pruebas» → confirmación hablada → `time_entry`.
- «Crea una tarea para mañana: llamar al cliente Pérez» → confirmación → tarea.
- «Abre en el editor el escrito de contestación» → IureEditor se abre ahí; luego dictas con «título uno», «viñeta», «punto y aparte».
- «¿Qué hay en la carpeta de Juárez?» → lista hablada de la unidad de IureDav; «léeme el contrato» → texto vía IureOCR.
- «Graba la junta con Pérez y García» → IureTranscribe empieza a grabar; «detén la grabación… ¿qué se acordó?» → transcripción y resumen.
- «Pásale OCR al PDF que acabo de escanear y gira la página 2» → IureOCR en segundo plano, con progreso y confirmación para editar.

## Riesgos y preguntas abiertas

- **Calidad del modelo local** con preguntas de varias vueltas: el sub-agente y descripciones de herramientas cortas lo mitigan; para consultas complejas se puede escalar a OpenRouter o Claude, con aviso de privacidad.
- **Sesión REST con TOTP**: el refresco dura 30 días; al vencer hay que pedir el código otra vez (por voz no; tarjeta en la ventana).
- **Instancias múltiples / Central SSO**: connect ya maneja cuentas; IureVoz usaría la cuenta activa de la suite.
- **Cambios en el servidor (F2)** requieren release de la instancia; mientras tanto F1 funciona con las 8 herramientas actuales. **Estado (2026-10-20)**: las cinco herramientas están hechas en la rama `feat/mcp-mi-dia` del repo de la instancia (smoke 93/93), sin liberar. **F5 hecho** (resumen en código, avisos por sondeo, opciones en Ajustes). IureVoz ya enruta «qué tengo hoy», «cuántas horas llevo», «tengo avisos», «busca al cliente…» al sub-agente de Iurefficient.
- **Por decidir**: URL de la instancia a usar en pruebas, si F2 se implementa en `iur-lms-sso-wt` desde esta sesión, y si la privacidad «solo LLM local» va activada por omisión.
