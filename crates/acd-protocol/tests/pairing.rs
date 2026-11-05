use acd_protocol::pairing::*;

const TOKEN: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
const PIN: &str = "sha256:ICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj8";

#[test]
fn round_trip() {
    let p = Pairing {
        host: "devbox.tail1234.ts.net".into(),
        port: 47800,
        pin: PIN.into(),
        token: TOKEN.into(),
    };
    let line = p.to_line();
    assert_eq!(
        line,
        format!("acd://devbox.tail1234.ts.net:47800?pin={PIN}#{TOKEN}")
    );
    assert_eq!(Pairing::parse(&line).unwrap(), p);
    assert_eq!(Pairing::parse(&format!("  {line}\n")).unwrap(), p);
}

#[test]
fn ipv6_hosts_are_bracketed() {
    let p = Pairing {
        host: "fd7a:115c:a1e0::1".into(),
        port: 47800,
        pin: PIN.into(),
        token: TOKEN.into(),
    };
    assert!(p.to_line().starts_with("acd://[fd7a:115c:a1e0::1]:47800?"));
    assert_eq!(Pairing::parse(&p.to_line()).unwrap(), p);
}

#[test]
fn rejects_malformed_lines_without_echoing_the_token() {
    let ok = format!("acd://h:47800?pin={PIN}#{TOKEN}");
    assert_eq!(
        Pairing::parse(&ok.replace("acd://", "http://")),
        Err(PairingError::Scheme)
    );
    assert_eq!(
        Pairing::parse(&ok.replace(":47800", ":0")),
        Err(PairingError::Port)
    );
    assert_eq!(
        Pairing::parse(&ok.replace(":47800", "")),
        Err(PairingError::Port)
    );
    assert_eq!(
        Pairing::parse(&ok.replace("sha256:", "md5:")),
        Err(PairingError::Pin)
    );
    assert_eq!(
        Pairing::parse(&ok.replace(&format!("#{TOKEN}"), "#short")),
        Err(PairingError::Token)
    );
    assert_eq!(
        Pairing::parse(&ok.replace("acd://h:", "acd://:")),
        Err(PairingError::Host)
    );
    let bad = Pairing::parse(&ok.replace(&format!("#{TOKEN}"), "#short")).unwrap_err();
    assert!(!format!("{bad} {bad:?}").contains("short"));
    assert!(
        !format!("{:?}", Pairing::parse(&ok).unwrap()).contains(TOKEN),
        "Debug redacts the token"
    );
}

#[test]
fn pin_is_sha256_of_spki() {
    assert_eq!(
        spki_pin(b"abc"),
        "sha256:ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0"
    );
    assert!(pins_equal(PIN, PIN) && !pins_equal(PIN, &spki_pin(b"x")));
}

fn line(address: &str) -> String {
    format!("acd://{address}?pin={PIN}#{TOKEN}")
}

#[test]
fn accepts_hostnames_and_ip_literals() {
    for host in [
        "h",
        "devbox.tail1234.ts.net",
        "Dev-Box.example.com",
        "127.0.0.1",
        "100.101.102.103",
        "[fd7a:115c:a1e0::1]",
        "[::1]",
    ] {
        assert!(Pairing::parse(&line(&format!("{host}:47800"))).is_ok(), "{host}");
    }
    let longest = format!("{0}.{0}.{0}.{1}", "a".repeat(63), "a".repeat(61));
    assert_eq!(longest.len(), 253);
    assert!(Pairing::parse(&line(&format!("{longest}:1"))).is_ok());
}

#[test]
fn rejects_anything_that_is_not_a_hostname_or_ip_literal() {
    let too_long = format!("{}.com", "a".repeat(250));
    let long_label = "a".repeat(64);
    for host in [
        "\u{202e}moc.live",
        "ho\0st",
        "host\x7f",
        "ho st",
        "host\t",
        "127.0.0.1\u{200b}",
        "ｈｏｓｔ",
        "münchen.de",
        "user@host",
        "user:pw@host",
        "host/path",
        "host?x=1",
        "-host",
        "host-",
        "a..b",
        ".host",
        "host.",
        "ho_st",
        "a:b",
        "[1.2.3.4]",
        "[host]",
        "[fe80::1%eth0]",
        "[fd7a::1\u{200b}]",
        &too_long,
        &long_label,
    ] {
        assert_eq!(
            Pairing::parse(&line(&format!("{host}:47800"))),
            Err(PairingError::Host),
            "{host:?}"
        );
    }
    assert_eq!(
        Pairing::parse(&line(&format!("{}:47800", "a".repeat(100_000)))),
        Err(PairingError::Host)
    );
}
