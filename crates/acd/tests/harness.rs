mod common;

#[test]
fn start_retries_when_a_port_is_taken_before_the_daemon_binds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = common::init_dir(tmp.path());
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = blocker.local_addr().unwrap().port();
    let mut picks = vec![
        (taken, common::free_port()),
        (common::free_port(), common::free_port()),
    ]
    .into_iter();
    let started = common::start_daemon_with(&dir, "", &[], || {
        picks.next().expect("at most two attempts")
    });
    assert_ne!(started.port, taken, "the second attempt must be used");
    assert!(
        common::acd(&dir).arg("status").status().unwrap().success(),
        "ready means status works"
    );
}

#[test]
fn a_stale_socket_is_not_readiness() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = common::init_dir(tmp.path());
    drop(std::os::unix::net::UnixListener::bind(dir.join("acd.sock")).unwrap());
    let started = common::start_daemon(&dir, "", &[]);
    assert!(common::acd(&dir).arg("status").status().unwrap().success());
    drop(started);
}

#[test]
fn every_command_is_isolated_from_the_user_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let cmd = common::acd(tmp.path());
    let envs: Vec<_> = cmd
        .get_envs()
        .map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned())))
        .collect();
    for k in common::ISOLATED {
        assert!(
            envs.iter().any(|(n, v)| n == k && v.is_none()),
            "{k} must be removed"
        );
    }
    assert!(
        envs.iter()
            .any(|(n, v)| n == "HOME" && v.as_deref() == Some(tmp.path().as_os_str()))
    );
}
