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

## Arquitectura propuesta

```
voz ─▶ agente IureVoz ─┬─ herramientas de escritorio (hoy)
                       ├─ «iurefficient»: sub-agente ─┬─ MCP remoto /api/mcp (lectura, descubierto dinámicamente)
                       │                              └─ REST vía connect (escrituras, siempre con confirmación)
                       ├─ MCP stdio de la suite: IureTranscribe, IureOCR (descubierto dinámicamente)
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
| **F6 · Suite de escritorio** | Cliente MCP stdio para IureTranscribe e IureOCR («transcribe la grabación de la junta», «pásale OCR a este PDF y súbelo al expediente»). Y `iurevoz --mcp` con el `Handler` de connect: `decir`, `preguntar_por_voz` (confirmación hablada), `dictar` — Claude Desktop/Code podrían hablar y escuchar a través de IureVoz. Registro en Claude Desktop con `agents.rs`. | iurevoz | M |
| **F7 · Agenda local (opcional)** | Leer/crear eventos del calendario de GNOME (Evolution Data Server por D-Bus) o suscribir el ICS de Iurefficient en GNOME Calendar. Solo si el usuario usa esa agenda. | iurevoz | S |

Orden sugerido: F0 → F1 → F3 → F2 → F5 → F4 → F6 → F7. F1+F3 ya dan valor con lo que el servidor ofrece hoy; F2 vuelve útil «mi día»; las escrituras van después, cuando la lectura sea confiable.

## Ejemplos de uso al terminar F1–F5

- «Oye compu, ¿qué tengo hoy?» → «Tienes 3 tareas para hoy, la audiencia de Pérez a las 10 y vence el plazo del amparo de Juárez el jueves.»
- «¿Cómo va el expediente de Juárez?» → resumen de tareas abiertas y últimos movimientos.
- «Ábrelo» (con memoria de conversación) → abre el expediente en el navegador.
- «Busca en los expedientes dónde se habló de la suspensión provisional» → resultados leídos en resumen.
- «Registra hora y media en Juárez por revisión de pruebas» → confirmación hablada → `time_entry`.
- «Crea una tarea para mañana: llamar al cliente Pérez» → confirmación → tarea.

## Riesgos y preguntas abiertas

- **Calidad del modelo local** con preguntas de varias vueltas: el sub-agente y descripciones de herramientas cortas lo mitigan; para consultas complejas se puede escalar a OpenRouter o Claude, con aviso de privacidad.
- **Sesión REST con TOTP**: el refresco dura 30 días; al vencer hay que pedir el código otra vez (por voz no; tarjeta en la ventana).
- **Instancias múltiples / Central SSO**: connect ya maneja cuentas; IureVoz usaría la cuenta activa de la suite.
- **Cambios en el servidor (F2)** requieren release de la instancia; mientras tanto F1 funciona con las 8 herramientas actuales.
- **Por decidir**: URL de la instancia a usar en pruebas, si F2 se implementa en `iur-lms-sso-wt` desde esta sesión, y si la privacidad «solo LLM local» va activada por omisión.
