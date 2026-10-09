#![allow(dead_code)]
use acd_client::{Timing, conn::*, endpoint::Endpoint};
use acd_core::{Auth, Core, ServerOptions, router, testing::*, tls};
use acd_protocol::ClientInfo;
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;

pub const TOKEN: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
pub struct Daemon {
    pub core: Arc<Core>,
    pub auth: Arc<Auth>,
    pub fake: Arc<FakeAdapter>,
    pub port: u16,
    pub pin: String,
    pub files: tls::KeyFiles,
    pub kill: tls::KillSwitch,
    pub task: tokio::task::JoinHandle<()>,
    pub config_tx: watch::Sender<Arc<rustls::ServerConfig>>,
}
pub async fn daemon() -> Daemon {
    daemon_with(FakeAdapter::new()).await
}
pub async fn daemon_with(fake: Arc<FakeAdapter>) -> Daemon {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    serve_on(tcp, port, fake, tls::generate("test")).await
}
pub async fn serve_on(
    tcp: tokio::net::TcpListener,
    port: u16,
    fake: Arc<FakeAdapter>,
    files: tls::KeyFiles,
) -> Daemon {
    let pin = tls::check_pair(&files).unwrap();
    let (config_tx, rx) = watch::channel(tls::server_config(&files).unwrap());
    let listener = tls::TlsListener::new(
        tcp,
        rx,
        tls::Admission::new(tls::MAX_UNAUTHENTICATED_TCP),
        tls::Limits::default(),
    );
    let kill = listener.kill_switch();
    let core = Core::new(test_machine(), vec![fake.clone()]);
    let auth = Arc::new(Auth::new(TOKEN.into()));
    let app = router(core.clone(), auth.clone(), ServerOptions::default());
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<tls::Admit>(),
        )
        .await
        .unwrap()
    });
    Daemon {
        core,
        auth,
        fake,
        port,
        pin,
        files,
        kill,
        task,
        config_tx,
    }
}

/// Restarts acd on the same port with the same TLS key and token, creating a
/// fresh Core and epoch after `delay`.
pub async fn restart_after(d: &mut Daemon, delay: Duration) {
    d.kill.kill();
    d.task.abort();
    let _ = (&mut d.task).await;
    tokio::time::sleep(delay).await;
    let tcp = tokio::net::TcpListener::bind(("127.0.0.1", d.port))
        .await
        .unwrap();
    let files = tls::KeyFiles {
        key_pem: d.files.key_pem.clone(),
        cert_pem: d.files.cert_pem.clone(),
    };
    *d = serve_on(tcp, d.port, FakeAdapter::new(), files).await;
}

pub async fn restart(d: &mut Daemon) {
    restart_after(d, Duration::ZERO).await;
}

pub fn fast() -> Timing {
    let ms = Duration::from_millis;
    Timing {
        backoff_base: ms(20),
        backoff_max: ms(100),
        identity_retry: ms(200),
        idle_ping: ms(500),
        ping_timeout: ms(300),
        undo: ms(200),
        not_ready_interval: ms(50),
        connect_total: ms(3000),
        ..Timing::default()
    }
}
pub fn client() -> ClientInfo {
    ClientInfo {
        id: "11111111-1111-4111-8111-111111111111".into(),
        name: "test".into(),
        version: "0".into(),
    }
}
pub fn tcp(d: &Daemon, token: &str) -> MachineConfig {
    MachineConfig {
        key: "m1".into(),
        instance: 1,
        endpoint: Endpoint::Tcp {
            host: "127.0.0.1".into(),
            port: d.port,
            pin: d.pin.clone(),
        },
        credential: Credential::Token(token.into()),
        client: client(),
        timing: fast(),
    }
}
pub async fn wait_for(
    rx: &mut watch::Receiver<MachineState>,
    what: &str,
    f: impl Fn(&MachineState) -> bool,
) -> MachineState {
    tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|s| f(s)))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap()
        .clone()
}

pub fn local_dir(token: &str) -> tempfile::TempDir {
    use std::{fs, os::unix::fs::PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    write_token(&dir, token);
    dir
}
pub fn write_token(dir: &tempfile::TempDir, token: &str) {
    use std::{fs, os::unix::fs::PermissionsExt};
    let path = dir.path().join("client.token");
    fs::write(&path, format!("{token}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
pub async fn serve_unix(
    dir: &tempfile::TempDir,
    core: Arc<Core>,
    auth: Arc<Auth>,
) -> tokio::task::JoinHandle<()> {
    use tokio::net::UnixListener;
    let path = dir.path().join("acd.sock");
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(path).unwrap();
    let app = router(core, auth, ServerOptions::default());
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let app = app.clone();
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let service = hyper_util::service::TowerToHyperService::new(app);
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await;
            });
        }
    })
}
pub fn local_cfg(dir: &tempfile::TempDir) -> MachineConfig {
    MachineConfig {
        key: "local".into(),
        instance: 1,
        endpoint: Endpoint::Local {
            config_dir: dir.path().to_owned(),
        },
        credential: Credential::LocalFile(dir.path().to_owned()),
        client: client(),
        timing: fast(),
    }
}
