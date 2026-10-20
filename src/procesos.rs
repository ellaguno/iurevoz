//! Procesos auxiliares (llama-server, servidores MCP de la suite) que deben morir con la app.
//!
//! `PR_SET_PDEATHSIG` avisa cuando muere el *hilo* que creó el proceso, no la app: si se
//! lanzan desde un hilo pasajero (p. ej. el que precalienta el LLM), mueren al terminar ese
//! hilo. Por eso todos se lanzan desde un hilo que vive lo mismo que la app.

use std::process::{Child, Command};
use std::sync::OnceLock;

use crossbeam_channel::{Sender, bounded, unbounded};

type Pedido = (Command, Sender<std::io::Result<Child>>);

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
                            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
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

/// Lanza `cmd` ligado a la vida de la app (si la app muere, el proceso recibe SIGTERM).
pub fn lanzar(cmd: Command) -> std::io::Result<Child> {
    let (tx, rx) = bounded(1);
    lanzador().send((cmd, tx)).map_err(|_| std::io::Error::other("lanzador detenido"))?;
    rx.recv().map_err(|_| std::io::Error::other("lanzador detenido"))?
}
