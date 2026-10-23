//! Procesos auxiliares (llama-server, servidores MCP de la suite) que deben morir con la app.
//!
//! `PR_SET_PDEATHSIG` avisa cuando muere el *hilo* que creó el proceso, no la app: si se
//! lanzan desde un hilo pasajero (p. ej. el que precalienta el LLM), mueren al terminar ese
//! hilo. Por eso todos se lanzan desde un hilo que vive lo mismo que la app.
//!
//! En Windows van a un Job Object que los termina cuando se cierra su último handle, es
//! decir, cuando la app termina por la razón que sea.

use std::process::{Child, Command};
#[cfg(target_os = "linux")]
use std::sync::OnceLock;

#[cfg(target_os = "linux")]
use crossbeam_channel::{Sender, bounded, unbounded};

#[cfg(target_os = "linux")]

type Pedido = (Command, Sender<std::io::Result<Child>>);

#[cfg(target_os = "linux")]
fn lanzador() -> &'static Sender<Pedido> {
    static TX: OnceLock<Sender<Pedido>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = unbounded::<Pedido>();
        std::thread::Builder::new()
            .name("lanzador".into())
            .spawn(move || {
                for (mut cmd, resp) in rx {
                    unsafe {
                        use std::os::unix::process::CommandExt;
                        cmd.pre_exec(|| {
                            // SIGKILL y no SIGTERM: llama-server atrapa SIGTERM para cerrar en orden
                            // y, con el modelo dormido, no llega a salir
                            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                            Ok(())
                        });
                    }
                    let _ = resp.send(cmd.spawn());
                }
            })
            .expect("hilo lanzador");
        tx
    })
}

/// Lanza `cmd` ligado a la vida de la app (si la app muere, el proceso recibe SIGKILL).
#[cfg(target_os = "linux")]
pub fn lanzar(cmd: Command) -> std::io::Result<Child> {
    let (tx, rx) = bounded(1);
    lanzador().send((cmd, tx)).map_err(|_| std::io::Error::other("lanzador detenido"))?;
    rx.recv().map_err(|_| std::io::Error::other("lanzador detenido"))?
}

/// Lanza `cmd` ligado a la vida de la app (sin ventana de consola).
#[cfg(windows)]
pub fn lanzar(mut cmd: Command) -> std::io::Result<Child> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::AssignProcessToJobObject;
    crate::plataforma::sin_consola(&mut cmd);
    let hijo = cmd.spawn()?;
    if let Some(t) = trabajo() {
        if let Err(e) = unsafe { AssignProcessToJobObject(HANDLE(t as _), HANDLE(hijo.as_raw_handle())) } {
            eprintln!("job object: {e}");
        }
    }
    Ok(hijo)
}

/// Job Object de la app (como dirección, porque `HANDLE` no se comparte entre hilos).
#[cfg(windows)]
fn trabajo() -> Option<usize> {
    use std::sync::OnceLock;
    use windows::Win32::System::JobObjects::*;
    static J: OnceLock<Option<usize>> = OnceLock::new();
    *J.get_or_init(|| unsafe {
        let j = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            j,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .ok()?;
        // el handle queda abierto hasta que termine la app
        Some(j.0 as usize)
    })
}
