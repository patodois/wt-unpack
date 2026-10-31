//! `tools/verify-release.sh` on a synthetic release signed with a throwaway key (no network, no game files).
//!
//! OpenSSL 3.0 cannot verify an Ed25519 signature whose message comes from a pipe ("Could not allocate 0 bytes
//! for oneshot sign/verify buffer"), which broke the script on Ubuntu 22.04/24.04 and Debian 12.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn openssl3() -> bool {
    Command::new("openssl")
        .arg("version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).starts_with("OpenSSL 3"))
        .unwrap_or(false)
}

fn openssl(dir: &Path, args: &[&str]) -> Output {
    let out = Command::new("openssl").current_dir(dir).args(args).stderr(Stdio::piped()).output().unwrap();
    assert!(out.status.success(), "openssl {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

#[test]
fn verify_release_accepts_a_signed_release_and_rejects_a_changed_one() {
    if !openssl3() {
        eprintln!("skipped: needs OpenSSL 3 on PATH");
        return;
    }
    let dir: PathBuf = std::env::temp_dir().join(format!("wt-unpack-verify-release-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("dl")).unwrap();

    // A throwaway key; the script under test gets its public half in place of the real release key
    openssl(&dir, &["genpkey", "-algorithm", "ed25519", "-out", "priv.pem"]);
    let public = String::from_utf8(openssl(&dir, &["pkey", "-in", "priv.pem", "-pubout"]).stdout).unwrap();
    let script = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tools/verify-release.sh")).unwrap();
    let begin = script.find("key='-----BEGIN PUBLIC KEY-----").expect("the script carries the key");
    let end = begin + script[begin..].find("-----END PUBLIC KEY-----'").unwrap() + "-----END PUBLIC KEY-----'".len();
    let patched = format!("{}key='{}'{}", &script[..begin], public.trim_end(), &script[end..]);
    fs::write(dir.join("verify.sh"), patched).unwrap();

    // A release of one archive, signed the way the script expects: "osp release patodois/<repo> <tag>\n" + SHA256SUMS
    let archive = b"not a real archive";
    fs::write(dir.join("dl/demo.tar.gz"), archive).unwrap();
    let hash = String::from_utf8(openssl(&dir.join("dl"), &["dgst", "-sha256", "-r", "demo.tar.gz"]).stdout).unwrap();
    let sums = format!("{}  demo.tar.gz\n", hash.split_whitespace().next().unwrap());
    fs::write(dir.join("dl/SHA256SUMS"), sums.as_bytes()).unwrap();
    fs::write(dir.join("msg"), format!("osp release patodois/demo v1.0.0\n{sums}")).unwrap();
    openssl(
        &dir,
        &["pkeyutl", "-sign", "-rawin", "-inkey", "priv.pem", "-in", "msg", "-out", "dl/SHA256SUMS.sig"],
    );

    let verify = |tag: &str| {
        Command::new("sh")
            .current_dir(&dir)
            .args(["verify.sh", "demo", tag, "dl"])
            .output()
            .unwrap()
    };
    let ok = verify("v1.0.0");
    assert!(
        ok.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ok.stdout),
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(String::from_utf8_lossy(&ok.stdout).contains("OK: SHA256SUMS signed by the release key; 1 file(s) match"));

    // another tag does not verify, and a changed archive does not match its checksum
    assert!(!verify("v1.0.1").status.success());
    fs::write(dir.join("dl/demo.tar.gz"), b"changed").unwrap();
    assert!(!verify("v1.0.0").status.success());

    fs::remove_dir_all(&dir).unwrap();
}
