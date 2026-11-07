//! Conversion diagnostics and streaming output, using small synthetic files only.
use std::{fs, path::PathBuf, process::{Command, Output}, sync::atomic::{AtomicUsize, Ordering}};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!("wt-conversion-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_wt-unpack")).current_dir(&self.0).args(args).output().unwrap()
    }
}
impl Drop for Scratch { fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); } }

fn archive(files: &[(&[u8], &[u8])]) -> Vec<u8> {
    let n = files.len();
    let mut body = vec![0u8; 32 + 24 * n];
    body[0..4].copy_from_slice(&32u32.to_le_bytes());
    body[4..8].copy_from_slice(&(n as u32).to_le_bytes());
    body[16..20].copy_from_slice(&((32 + 8*n) as u32).to_le_bytes());
    body[20..24].copy_from_slice(&(n as u32).to_le_bytes());
    for (i, (name, data)) in files.iter().enumerate() {
        let ofs = body.len() as u64;
        body[32+8*i..40+8*i].copy_from_slice(&ofs.to_le_bytes());
        body.extend_from_slice(name);body.push(0);
        let ofs = body.len() as u32;
        let pos = 32 + 8*n + 16*i;
        body[pos..pos+4].copy_from_slice(&ofs.to_le_bytes());
        body[pos+4..pos+8].copy_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(data);
    }
    let mut out = b"VRFspc\0\0".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());out.extend_from_slice(&0u32.to_le_bytes());out.extend(body);out
}

#[test]
fn extraction_summarizes_raw_fallbacks_and_preserves_success_status() {
    let temp=Scratch::new();let bad=[1,2,1,0];
    fs::write(temp.0.join("test.vromfs.bin"), archive(&[(b"a.blk",&bad),(b"b.blk",&bad),(b"ok.txt",b"unchanged")])).unwrap();
    let result=temp.run(&["extract","test.vromfs.bin","--out","out","--blk","json"]);
    let stderr=String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(),"{stderr}");
    assert!(stderr.contains("wrote 3 files"),"{stderr}");
    assert!(stderr.contains("kept raw: 2 BLK files"),"{stderr}");
    assert!(stderr.contains("2 x name count cannot fit in its byte area: count=2, limit=1 bytes"),"{stderr}");
    assert_eq!(fs::read(temp.0.join("out/a.blk")).unwrap(),bad);
    assert_eq!(fs::read(temp.0.join("out/b.blk")).unwrap(),bad);
    assert_eq!(fs::read(temp.0.join("out/ok.txt")).unwrap(),b"unchanged");
    assert!(!temp.0.join("out/a.blk.json").exists());
}

#[test]
fn cat_rejection_reports_count_and_limit() {
    let temp=Scratch::new();fs::write(temp.0.join("bad.blk"),[1,2,1,0]).unwrap();
    let result=temp.run(&["cat","bad.blk"]);let stderr=String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success());assert!(result.stdout.is_empty());
    assert!(stderr.contains("count=2, limit=1 bytes"),"{stderr}");
}

#[test]
fn invalid_shared_names_abort_before_extraction_even_in_raw_mode() {
    let temp=Scratch::new();
    fs::write(temp.0.join("test.vromfs.bin"),archive(&[(b"ok.txt",b"unchanged"),(b"\xff?nm",b"x")])).unwrap();
    for format in ["text","json","raw"] {
        let result=temp.run(&["extract","test.vromfs.bin","--out","out","--blk",format]);
        assert!(!result.status.success());assert!(String::from_utf8_lossy(&result.stderr).contains("shared name map is too short"));
        assert!(!temp.0.join("out").exists());
    }
}

#[test]
fn write_errors_are_fatal() {
    let temp=Scratch::new();fs::write(temp.0.join("test.vromfs.bin"),archive(&[(b"a",b"data")])).unwrap();
    fs::write(temp.0.join("out"),b"file in place of directory").unwrap();
    assert!(!temp.run(&["extract","test.vromfs.bin","--out","out"]).status.success());
}
