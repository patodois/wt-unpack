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

#[cfg(unix)]
#[test]
fn a_write_failing_midway_names_the_file_and_is_fatal() {
    let temp=Scratch::new();
    fs::write(temp.0.join("test.vromfs.bin"),archive(&[(b"a",&vec![7u8;1<<20])])).unwrap();
    // A file-size limit makes the write fail after the file was created.
    let out=Command::new("sh").current_dir(&temp.0).arg("-c")
        .arg(format!("trap '' XFSZ; ulimit -f 8; exec '{}' extract test.vromfs.bin --out out",env!("CARGO_BIN_EXE_wt-unpack")))
        .output().unwrap();
    assert!(!out.status.success());
    let err=String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("writing out"),"{err}");
}

/// A sound bank with one PCM16 FSB5 holding a one-frame mono sample per name
fn bank(names: &[&[u8]]) -> Vec<u8> {
    let n = names.len();
    let (mut headers, mut table, mut strings) = (Vec::new(), Vec::new(), Vec::new());
    for (i, name) in names.iter().enumerate() {
        // rate index 8 (44,100 Hz), mono, data offset in units of 32 bytes, one frame
        headers.extend_from_slice(&(8u64 << 1 | (i as u64) << 7 | 1u64 << 34).to_le_bytes());
        table.extend_from_slice(&((4 * n + strings.len()) as u32).to_le_bytes());
        strings.extend_from_slice(name);
        strings.push(0);
    }
    table.extend(strings);
    let mut fsb = b"FSB5".to_vec();
    for v in [1, n as u32, headers.len() as u32, table.len() as u32, 32 * n as u32, 2, 0, 0] {
        fsb.extend_from_slice(&v.to_le_bytes());
    }
    fsb.resize(0x3C, 0);
    fsb.extend(headers);
    fsb.extend(table);
    fsb.resize(fsb.len() + 32 * n, 0);
    // RIFF "FEV " with one SND chunk; the FSB5 starts at the chunk's first offset that is a multiple of 32
    let mut body = b"FEV SND ".to_vec();
    body.extend_from_slice(&((12 + fsb.len()) as u32).to_le_bytes());
    body.extend_from_slice(&[0; 12]);
    body.extend(fsb);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

/// The files under `dir`, relative to it with `/`, sorted
fn files_under(dir: &std::path::Path) -> Vec<String> {
    fn walk(dir: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let name = format!("{prefix}{}", e.file_name().to_string_lossy());
            if e.path().is_dir() { walk(&e.path(), &format!("{name}/"), out) } else { out.push(name) }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort();
    out
}

#[test]
fn sound_extract_makes_sample_names_safe_and_leaves_ordinary_ones_alone() {
    let temp = Scratch::new();
    fs::create_dir_all(temp.0.join("work/deep")).unwrap();
    // A drive, a parent folder in both spellings and a stream; then ordinary names, three of which are devices
    // on Windows or end in a dot or a space
    let names: [&[u8]; 9] = [b"c:boom", b"../../up", b"..\\..\\back", b"shot:1", b"dshk_close", b"NUL", b"aux", b"ends.", b"two words"];
    fs::write(temp.0.join("work/deep/x.assets.bank"), bank(&names)).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_wt-unpack")).current_dir(temp.0.join("work/deep")).args(["sound", "extract", "x.assets.bank", "-o", "out"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(stderr.contains("wrote 9 samples"), "{stderr}");
    // Ordinary names give `<name>.wav`, as they always did; only Windows, where `NUL` and `aux` are devices,
    // puts a `_` in front of those two
    let (nul, aux) = if cfg!(windows) { ("_NUL", "_aux") } else { ("NUL", "aux") };
    let mut expected: Vec<String> = [".._.._back", ".._.._up", "c_boom", "shot_1", "dshk_close", nul, aux, "ends.", "two words"].iter().map(|n| format!("work/deep/out/{n}.wav")).collect();
    expected.push("work/deep/x.assets.bank".into());
    expected.sort();
    assert_eq!(files_under(&temp.0), expected);
}

#[test]
fn extract_keeps_refusing_paths_that_leave_the_output_folder() {
    // `extract` is unchanged: it writes what the system's own path rules call a plain relative path
    let temp = Scratch::new();
    fs::create_dir_all(temp.0.join("work/deep")).unwrap();
    let members: &[(&[u8], &[u8])] = &[(b"ok/a.txt", b"1"), (b"../up.txt", b"2"), (b"/abs.txt", b"3"), (b"ok/../../back.txt", b"4"), (b"ok/b c.txt", b"5")];
    fs::write(temp.0.join("work/deep/test.vromfs.bin"), archive(members)).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_wt-unpack")).current_dir(temp.0.join("work/deep")).args(["extract", "test.vromfs.bin", "--out", "out"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert_eq!(stderr.matches("skipping unsafe path").count(), 3, "{stderr}");
    assert_eq!(files_under(&temp.0), ["work/deep/out/ok/a.txt", "work/deep/out/ok/b c.txt", "work/deep/test.vromfs.bin"]);
}

/// Runs the program, reads `take` bytes of its standard output and closes the pipe, as `| head` does
fn run_and_stop_reading(temp: &Scratch, args: &[&str], take: usize) -> (std::process::ExitStatus, Vec<u8>, String) {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_wt-unpack")).current_dir(&temp.0).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut first = vec![0; take];
    stdout.read_exact(&mut first).unwrap();
    drop(stdout);
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    (child.wait().unwrap(), first, stderr)
}

#[test]
fn a_reader_that_stops_early_ends_list_and_cat_quietly_with_success() {
    // The README's first commands are `list ... | head` and `cat ... | head`. Both outputs here are far larger
    // than a pipe holds (20,000 lines, 2 MB), so the program is still writing when the reader goes away.
    let temp = Scratch::new();
    let names: Vec<String> = (0..20_000).map(|i| format!("gamedata/some/folder/file_number_{i:05}.txt")).collect();
    let text = "a line of a large text file\n".repeat(80_000);
    let mut files: Vec<(&[u8], &[u8])> = names.iter().map(|n| (n.as_bytes(), &b"x"[..])).collect();
    files.push((b"big.txt", text.as_bytes()));
    fs::write(temp.0.join("test.vromfs.bin"), archive(&files)).unwrap();

    let (status, first, stderr) = run_and_stop_reading(&temp, &["list", "test.vromfs.bin"], 8);
    assert_eq!((first.as_slice(), stderr.as_str()), (&b"target: "[..], ""), "list: {status}");
    assert!(status.success(), "list: {status}");

    let (status, first, stderr) = run_and_stop_reading(&temp, &["cat", "test.vromfs.bin:big.txt"], 6);
    assert_eq!((first.as_slice(), stderr.as_str()), (&b"a line"[..], ""), "cat: {status}");
    assert!(status.success(), "cat: {status}");

    // Read to the end, nothing changes: the whole listing, and success
    let all = temp.run(&["list", "test.vromfs.bin"]);
    assert!(all.status.success());
    assert_eq!(String::from_utf8_lossy(&all.stdout).lines().count(), 4 + 20_001);
}

#[test]
fn the_regression_commands_exit_with_1_when_something_fails() {
    let temp = Scratch::new();
    for dir in ["good", "bad", "empty"] {
        fs::create_dir(temp.0.join(dir)).unwrap();
    }
    // Good: a bank whose samples decode. Bad: files of the right names that are not what they say.
    fs::write(temp.0.join("good/a.assets.bank"), bank(&[b"one", b"two"])).unwrap();
    for name in ["x.grp", "x.bin", "x.bank"] {
        fs::write(temp.0.join("bad").join(name), b"not a game file, long enough to be read as a header ........").unwrap();
    }
    fs::write(temp.0.join("bad/a.assets.bank"), bank(&[b"one"])).unwrap();

    let run = |args: &[&str]| {
        let out = temp.run(args);
        (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    // Nothing to check, or everything fine: 0
    for args in [&["check", "empty"][..], &["check-levels", "empty"], &["sound", "check", "empty"], &["sound", "check", "good"], &["sound", "check", "good", "--decode"]] {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, Some(0), "{args:?}: {stdout}{stderr}");
    }
    // A failure: the summary still comes out in full, then status 1 with one line on standard error
    for (args, summary, error) in [
        (&["check", "bad"][..], "0/0 DynModels parsed", "1 pack or model failed"),
        (&["check-levels", "bad"], "0/1 levels parse fully", "1 of 1 levels failed"),
        (&["sound", "check", "bad"], "2 banks, 1 with sample data", "1 bank or sample failed"),
        (&["sound", "check", "bad", "--decode"], "decoded 1 samples", "1 bank or sample failed"),
    ] {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, Some(1), "{args:?}: {stdout}{stderr}");
        assert!(stdout.contains(summary), "{args:?}: {stdout}");
        assert!(stderr.contains(error), "{args:?}: {stderr}");
    }
    // A folder that cannot be read is an error as before (1), and arguments that are not understood are 2
    assert_eq!(run(&["check-levels", "absent"]).0, Some(1));
    assert_eq!(run(&["check-levels"]).0, Some(2));
}
