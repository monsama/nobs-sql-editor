// NOBS SQL Editor - a cross-platform MySQL/MariaDB client.
// Copyright (C) 2026 Viktor Ljuca <https://monsama.ch>
//
// This program is free software; you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation; either version 2 of the License, or (at your option)
// any later version.
//
// This program is distributed in the hope that it will be useful, but
// WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY
// or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License
// for more details.
//
// You should have received a copy of the GNU General Public License along
// with this program; if not, see <https://www.gnu.org/licenses/>. A copy is
// in the LICENSE file at the root of this repository.

// NOBS SQL Editor - Tauri (Rust) backend
// Cross-platform desktop app. Uses the `mysql` driver for typed results
// (real NULL, proper bit/binary handling) and shells out to mysql/mysqldump
// only for dump-style export/import (which need DELIMITER handling).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use mysql::prelude::*;
use mysql::{Conn, Opts, OptsBuilder, SslOpts, Value as MyValue, Column, Row};
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};
use tauri::Manager;
use tauri::Emitter;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
static EXPORT_CANCEL: AtomicBool = AtomicBool::new(false);
// Marks a run() that ended because the user pressed Cancel, so the caller can log it as a
// cancellation rather than a failure. A NUL cannot appear in a mysqldump message.
const RUN_CANCELLED: &str = "\u{0}cancelled";

// ---------- cancellable Export / Import jobs ----------
// One "job" is a single Export or Import run started from the UI, identified by the jobId the
// UI generates and passes in. Such a run shells out to mysqldump/mysql once per database, table
// or file, so cancelling has to do two things: stop the loop before it starts the next child,
// and kill the child already running. Stopping only between children would leave a large single
// dump writing for minutes after the user pressed Cancel, which is what the Cancel button
// appeared to do before this existed. Mirrors the PowerShell backend's Api-CancelJob.
struct Job {
    cancelled: AtomicBool,
    // The child currently running for this job, if any. Held in its own lock so the cancelling
    // thread can reach it while the worker thread is polling it.
    child: Mutex<Option<std::process::Child>>,
}
static JOBS: OnceLock<Mutex<std::collections::HashMap<String, std::sync::Arc<Job>>>> = OnceLock::new();
// What a failed or cancelled mysqldump left behind is moved aside to "<file>.partial". It used to
// stay under the export's own name - with the timestamp off, in place of the last good backup - and
// looked like a finished dump to anyone restoring from the folder later. The per-table export's
// own temporary file is removed by its caller instead.
fn set_aside_partial(file: &str) -> String {
    if file.ends_with(".tmp") || !std::path::Path::new(file).exists() { return String::new(); }
    let p = format!("{}.partial", file);
    match std::fs::rename(file, &p) { Ok(_) => format!(" (the incomplete file was kept as {})", p), Err(_) => String::new() }
}

// The warnings in what mysql printed with --show-warnings, less the ones about the dump's own
// spelling that change no data: deprecated syntax (1287), the utf8/utf8mb3 and NATIONAL aliases
// (3719, 3720, 3778), and the integer display width (1681) - MySQL 8 raises one of those for every
// table in a dump that says "utf8" - a value given for a generated column (1906), which the
// server computes again anyway; MariaDB's mysqldump writes one into every such row - and MySQL
// 5.7's note that NO_AUTO_CREATE_USER is deprecated (3090), raised by every 5.7 dump's sql_mode.
fn import_warnings(out: &str) -> Vec<String> {
    const HARMLESS: &[&str] = &["1287", "1681", "1906", "3090", "3719", "3720", "3778"];
    out.lines().map(|l| l.trim())
        .filter(|l| l.starts_with("Warning (Code "))
        .filter(|l| { let code: String = l["Warning (Code ".len()..].chars().take_while(|c| c.is_ascii_digit()).collect(); !HARMLESS.contains(&code.as_str()) })
        .map(String::from).collect()
}

// Whether a dump file is a view's: mysqldump heads a view's section "Temporary view structure for
// view" (MySQL 8), "Temporary table structure for view" (MariaDB and older) or "Final view
// structure for view", and a per-table file starts with its object's section.
fn dump_starts_with_sandbox_line(path: &str) -> bool {
    use std::io::Read;
    let mut buf = [0u8; 64];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    buf[..n].starts_with(b"/*M!999999\\- enable the sandbox mode */")
}

fn dump_file_is_view(path: &str) -> bool {
    use std::io::Read;
    let mut buf = vec![0u8; 65536];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    let head = String::from_utf8_lossy(&buf[..n]);
    let first_section = head.lines().find(|l| l.starts_with("-- Table structure for table") || l.starts_with("-- Dumping data for table")
        || l.starts_with("-- Temporary view structure for view") || l.starts_with("-- Temporary table structure for view") || l.starts_with("-- Final view structure for view"));
    first_section.map(|l| l.contains("for view")).unwrap_or(false)
}

// DEFINER=`user`@`host` as mysqldump writes it into views, triggers, routines and events.
fn definer_regex() -> regex::bytes::Regex {
    regex::bytes::Regex::new(r"DEFINER=`(?:[^`]|``)*`@`(?:[^`]|``)*`\s*").unwrap()
}

// Leaves DEFINER out of a dump's object definitions, and nothing else. The whole file used to go
// through the pattern as one string: a row whose text held "DEFINER=`root`@`localhost`" - a
// table that keeps DDL, an audit log - was changed in the backup, a dump that was not UTF-8
// (latin1, binary data without --hex-blob) was left as it was without a word, and the whole dump
// was held in memory twice. It is now read as bytes, a line at a time, into a file beside it
// that then takes its place. Rows are never touched: mysqldump writes each INSERT on a line of
// its own and escapes line breaks inside values, so a line that starts with INSERT is data.
fn strip_definers(file: &str, re: &regex::bytes::Regex) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, BufWriter, Write};
    let tmp = format!("{}.definer.tmp", file);
    let res = (|| -> std::io::Result<()> {
        let mut r = BufReader::new(std::fs::File::open(file)?);
        let mut w = BufWriter::new(std::fs::File::create(&tmp)?);
        let mut line = Vec::new();
        loop {
            line.clear();
            if r.read_until(b'\n', &mut line)? == 0 { break; }
            if line.starts_with(b"INSERT ") || line.starts_with(b"REPLACE ") { w.write_all(&line)?; }
            else { w.write_all(&re.replace_all(&line, &b""[..]))?; }
        }
        w.flush()?;
        drop(w);
        std::fs::rename(&tmp, file)
    })();
    if res.is_err() { let _ = std::fs::remove_file(&tmp); }
    res
}

#[cfg(test)]
mod definer_tests {
    use super::*;

    #[test]
    fn a_path_mysqldump_cannot_take_is_explained() {
        let e = friendly_dump_err("mysqldump: Can't create/write to file 'C:\\x\\??????\\d.sql' (OS errno 22 - Invalid argument)");
        assert!(e.contains("system code page") && e.contains("MariaDB tools"), "{e}");
        assert_eq!(friendly_dump_err("ERROR 1045 (28000): Access denied"), "ERROR 1045 (28000): Access denied");
    }

    #[test]
    fn option_file_values_are_quoted_so_hash_spaces_and_quotes_survive() {
        // Measured against both clients' --print-defaults: each of these comes back exactly.
        assert_eq!(cnf_quote("ab#cd"), "\"ab#cd\"");
        assert_eq!(cnf_quote(" sp "), "\" sp \"");
        assert_eq!(cnf_quote("x\"y\\z"), "\"x\\\"y\\\\z\"");
        assert_eq!(cnf_quote("a\r\nb"), "\"ab\"");
    }

    #[test]
    fn definer_goes_from_definitions_and_rows_keep_every_byte() {
        let dir = std::env::temp_dir().join(format!("nobs-definer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("d.sql");
        let mut dump: Vec<u8> = Vec::new();
        dump.extend_from_slice(b"/*!50013 DEFINER=`root`@`localhost` SQL SECURITY DEFINER */\r\n");
        dump.extend_from_slice(b"CREATE DEFINER=`a``b`@`%` PROCEDURE `p`()\nBEGIN SELECT 1; END\n");
        dump.extend_from_slice(b"INSERT INTO `log` VALUES (1,'CREATE DEFINER=`root`@`localhost` VIEW v',");
        dump.extend_from_slice(&[b'\'', 0xE9, 0xFF, b'\'', b')', b';', b'\n']);
        dump.extend_from_slice(b"-- end");
        std::fs::write(&f, &dump).unwrap();
        strip_definers(f.to_str().unwrap(), &definer_regex()).unwrap();
        let out = std::fs::read(&f).unwrap();
        let mut want: Vec<u8> = Vec::new();
        want.extend_from_slice(b"/*!50013 SQL SECURITY DEFINER */\r\n");
        want.extend_from_slice(b"CREATE PROCEDURE `p`()\nBEGIN SELECT 1; END\n");
        want.extend_from_slice(b"INSERT INTO `log` VALUES (1,'CREATE DEFINER=`root`@`localhost` VIEW v',");
        want.extend_from_slice(&[b'\'', 0xE9, 0xFF, b'\'', b')', b';', b'\n']);
        want.extend_from_slice(b"-- end");
        assert_eq!(out, want);
        assert!(!dir.join("d.sql.definer.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// The most a page of rows holds, as text, before the rest is left for the next fetch.
const PAGE_BYTES: usize = 32 * 1024 * 1024;

fn jobs() -> &'static Mutex<std::collections::HashMap<String, std::sync::Arc<Job>>> {
    JOBS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
fn job_start(id: &str) -> Option<std::sync::Arc<Job>> {
    if id.is_empty() { return None; }
    let j = std::sync::Arc::new(Job { cancelled: AtomicBool::new(false), child: Mutex::new(None) });
    jobs().lock().ok()?.insert(id.to_string(), j.clone());
    Some(j)
}
fn job_is_cancelled(job: &Option<std::sync::Arc<Job>>) -> bool {
    job.as_ref().map(|j| j.cancelled.load(Ordering::SeqCst)).unwrap_or(false)
}
// Deregisters on every exit path, including the `?` early returns inside the worker closure -
// a job left in the map would make a later cancel look like it succeeded against a dead run.
struct JobGuard(String);
impl Drop for JobGuard {
    fn drop(&mut self) {
        if !self.0.is_empty() { if let Ok(mut m) = jobs().lock() { m.remove(&self.0); } }
    }
}

// Runs one child process as part of `job`, returning what Command::output() would have.
// Two differences from output(), both required here:
//   - the Child is parked in the job so cancel_job can kill it mid-run;
//   - the wait is a poll rather than a blocking wait(), because holding the child's lock across
//     a blocking wait would make the cancelling thread wait for the process it wants to kill.
// stderr is drained on its own thread for the reason output() does the same internally: a child
// that fills the stderr pipe buffer blocks forever if nobody is reading it.
// Stops a child export/import process.
//
// On Windows this delegates to taskkill instead of Child::kill(). Calling TerminateProcess on
// our own child handle, while mysqldump was actively writing a large dump, took the entire
// application down - no window, no process, and no Rust panic message, so an abort rather than
// a panic. Confirmed by A/B: a build with the kill removed survives the same cancel, one with
// it does not. Cancelling between tables was always fine, because no kill happens there; only
// interrupting a dump in flight reaches this.
//
// taskkill runs the terminate in its own process, and /T takes any grandchildren with it.
// CREATE_NO_WINDOW keeps a console from flashing over the app on every cancel.
#[cfg(windows)]
fn kill_child(c: &mut std::process::Child) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let pid = c.id();
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
#[cfg(not(windows))]
fn kill_child(c: &mut std::process::Child) { let _ = c.kill(); }

fn run_job_child(job: Option<&std::sync::Arc<Job>>, cmd: &mut Command) -> std::io::Result<std::process::Output> {
    run_job_child_fed(job, cmd, None)
}

// A writer for the child's stdin, run on its own thread while the child is watched for cancel.
type StdinFeed = Box<dyn FnOnce(std::process::ChildStdin) + Send>;

fn run_job_child_fed(job: Option<&std::sync::Arc<Job>>, cmd: &mut Command, feed: Option<StdinFeed>) -> std::io::Result<std::process::Output> {
    if feed.is_some() { cmd.stdin(Stdio::piped()); }
    let mut child = cmd.stderr(Stdio::piped()).spawn()?;
    // The feeder ends when the file does or when the child stops reading (a failed statement
    // exits it), whichever comes first; dropping stdin is what tells the client it has everything.
    if let (Some(f), Some(stdin)) = (feed, child.stdin.take()) { std::thread::spawn(move || f(stdin)); }
    let mut pipe = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = pipe.as_mut() { use std::io::Read; let _ = p.read_to_end(&mut buf); }
        buf
    });
    // A caller that pipes stdout gets the warnings mysql prints there with --show-warnings, and only
    // those (at most a thousand): anything else a script prints could be any size.
    let out_reader = child.stdout.take().map(|out| std::thread::spawn(move || {
        use std::io::BufRead;
        let mut keep = Vec::new(); let mut n = 0usize;
        for line in std::io::BufReader::new(out).split(b'\n') {
            let Ok(line) = line else { break };
            if line.starts_with(b"Warning (Code ") && n < 1000 { keep.extend_from_slice(&line); keep.push(b'\n'); n += 1; }
        }
        keep
    }));
    let status = match job {
        None => child.wait()?,
        Some(j) => {
            if let Ok(mut slot) = j.child.lock() { *slot = Some(child); }
            loop {
                let mut finished = None;
                if let Ok(mut slot) = j.child.lock() {
                    if let Some(c) = slot.as_mut() {
                        if j.cancelled.load(Ordering::SeqCst) { kill_child(c); }
                        finished = c.try_wait()?;
                    } else {
                        // Nothing parked: treat as finished rather than spinning forever.
                        break std::process::ExitStatus::default();
                    }
                }
                if let Some(st) = finished {
                    if let Ok(mut slot) = j.child.lock() { *slot = None; }
                    break st;
                }
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        }
    };
    // After a kill the child's stderr is of no interest, and anything it spawned that inherited
    // the pipe can hold it open long after the kill - joining then would block for exactly as
    // long as cancelling was meant to save. Leave that reader to finish on its own.
    let cancelled = job.map(|j| j.cancelled.load(Ordering::SeqCst)).unwrap_or(false);
    let stderr = if cancelled { Vec::new() } else { reader.join().unwrap_or_default() };
    let stdout = match out_reader { Some(r) if !cancelled => r.join().unwrap_or_default(), _ => Vec::new() };
    Ok(std::process::Output { status, stdout, stderr })
}
// Tracks in-flight SELECT queries so Cancel can stop them server-side, the same way MySQL
// Workbench does it: keep the query's own MySQL CONNECTION_ID(), and to cancel, open a brand
// new connection and run KILL QUERY <id> on it (you can't cancel over the same connection
// that's busy running the query - it has to come from elsewhere).
fn running_queries() -> &'static Mutex<std::collections::HashMap<String, (u64, Value)>> {
    static MAP: OnceLock<Mutex<std::collections::HashMap<String, (u64, Value)>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

// A simple thread-safe set of requestIds the user has asked to cancel. Compare operations run
// MANY sequential queries (one per table/chunk) rather than one big one, so instead of trying to
// kill whichever single sub-query happens to be in flight, each loop just checks this set
// between iterations and stops cleanly if its requestId shows up here.
fn cancelled_compares() -> &'static Mutex<std::collections::HashSet<String>> {
    static SET: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}
// Request ids whose query was actually killed by a Cancel click. The PowerShell build keeps
// this as a Cancelled flag on its RunningQueries entry; this port dropped it and inferred
// cancellation from "a requestId was supplied", which is true of EVERY query the editor runs -
// so every failure was reported as a cancel and the real error was thrown away.
fn cancelled_queries() -> &'static Mutex<std::collections::HashSet<String>> {
    static C: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}
fn mark_query_cancelled(rid: &str) { if let Ok(mut s) = cancelled_queries().lock() { s.insert(rid.to_string()); } }
// Consumes the marker: a later query reusing the id (ids are per-run uuids, so this is really
// just hygiene) must not inherit it.
fn take_query_cancelled(rid: &str) -> bool {
    cancelled_queries().lock().map(|mut s| s.remove(rid)).unwrap_or(false)
}
fn is_compare_cancelled(rid: &str) -> bool { cancelled_compares().lock().unwrap().contains(rid) }
fn clear_compare_cancel(rid: &str) { cancelled_compares().lock().unwrap().remove(rid); }

// Mirrors running_queries() above, but a compare step can have TWO connections (source + target)
// live under the same requestId at once, so this holds a connection_id + conn-info pair PER
// connection rather than one. Without this, Cancel only ever set the cooperative flag above,
// which a single un-chunked SELECT (get_rows_by_pk's fetch, or either side's plain PK scan) can't
// see until it finishes on its own - so Stop looked like it did nothing on any table big enough
// for that one query to take a while, and closing the Compare Databases dialog mid-scan left it
// running in the background for the same reason.
type CompareConnMap = std::collections::HashMap<String, Vec<(u64, Value)>>;
fn running_compare_conns() -> &'static Mutex<CompareConnMap> {
    static MAP: OnceLock<Mutex<CompareConnMap>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
fn register_compare_conn(rid: &Option<String>, conn: &mut Conn, connj: &Value) {
    let Some(r) = rid else { return };
    if let Ok((_c, rows)) = run_select(conn, "SELECT CONNECTION_ID()") {
        if let Some(cid) = rows.first().and_then(|row| row.first()).cloned().flatten().and_then(|s| s.parse::<u64>().ok()) {
            running_compare_conns().lock().unwrap().entry(r.clone()).or_default().push((cid, connj.clone()));
        }
    }
}
fn unregister_compare_conns(rid: &Option<String>) {
    if let Some(r) = rid { running_compare_conns().lock().unwrap().remove(r); }
}
// RAII guard so the registration above is always cleaned up - compare_rows/compare_rows_diff
// have several early `?`/return paths, and leaving a stale entry behind would let a LATER,
// unrelated request that happens to reuse this id inherit connections that no longer exist.
struct CompareConnGuard(Option<String>);
impl Drop for CompareConnGuard {
    fn drop(&mut self) { unregister_compare_conns(&self.0); }
}

#[tauri::command]
async fn compare_cancel(req: Value) -> R {
    if let Some(rid) = req["requestId"].as_str() {
        if !rid.is_empty() {
            cancelled_compares().lock().unwrap().insert(rid.to_string());
            // Set BEFORE the kill lands, same ordering as compare_rows/compare_rows_diff's own
            // checkpoints, so a query that dies from the KILL below is seen as "cancelled" by
            // whichever loop is waiting on it rather than surfacing as a real error.
            let conns = running_compare_conns().lock().unwrap().get(rid).cloned();
            if let Some(conns) = conns {
                tokio::task::spawn_blocking(move || {
                    for (cid, connj) in conns {
                        if let Ok(mut kc) = build_conn(&connj) { let _ = kc.query_drop(format!("KILL QUERY {}", cid)); }
                    }
                }).await.ok();
            }
        }
    }
    Ok(json!({"ok":true}))
}

type R = Result<Value, String>;

// ---------- connection ----------
// Browsing in another character set, for when a value's encoding is in doubt. The server transcodes
// every text column into the session's charset before it is sent, so what the app receives depends
// on what the session asked for: a column of UTF-8 bytes stored in a latin1 column reads as mojibake
// in utf8mb4 and as itself in latin1, which is how you tell a storage problem from a display one.
// "binary" asks for no transcoding at all - every text column then arrives marked charset 63 and is
// shown as hex bytes (see is_binaryish), which is the truth about what is stored.
//
// A list, not a pattern: this ends up in SET NAMES, which takes no placeholder, so the value is
// interpolated into SQL. Everything here is a charset MySQL or MariaDB ships, and anything else -
// including anything with a quote or a semicolon in it - is not a charset and is ignored.
const BROWSE_CHARSETS: &[&str] = &[
    // No ucs2, utf16 or utf32: MySQL refuses those as a client's character set, so offering one
    // would be offering a connection error.
    "binary", "ascii", "latin1", "latin2", "latin5", "latin7", "utf8mb3", "utf8mb4",
    "cp1250", "cp1251", "cp1256", "cp1257", "cp850", "cp852", "cp866", "cp932", "koi8r", "koi8u",
    "greek", "hebrew", "tis620", "big5", "gbk", "gb2312", "sjis", "ujis", "euckr", "macroman",
];

// The charset a connection asks to browse in, if it asks for one this app will hand to the server.
// None means the driver's default (utf8mb4), which is every ordinary connection.
fn browse_charset(connj: &Value) -> Option<String> {
    let want = connj["charset"].as_str()?.trim().to_ascii_lowercase();
    if want.is_empty() || want == "default" { return None; }
    BROWSE_CHARSETS.iter().find(|c| **c == want).map(|c| (*c).to_string())
}

// Read-only for this request. Either the connection is marked so by its owner, or it is browsing in
// another character set - which is a diagnostic, and a write from it would be interpreted in that
// session's charset and stored as different bytes than the ones on screen. The UI disables writing
// in that mode as well; this is the half that does not depend on the UI being right.
fn ro_mode(req: &Value) -> bool {
    ro_flag(req) || browse_charset(&req["conn"]).is_some()
}

// Marked read-only by the page, or by the saved connections themselves: when every saved connection
// to this account (host, port, user, SSH host) is read-only, so is the request, whatever the page
// sent - the page is not the only thing standing between a read-only connection and a write. A
// second saved connection to the same account that is not read-only leaves it to the page.
fn ro_flag(req: &Value) -> bool {
    req["ro"].as_bool().unwrap_or(false) || saved_ro(&req["conn"])
}
fn saved_ro(connj: &Value) -> bool {
    if !connj.is_object() { return false; }
    let s = |v: &Value| v.as_str().map(|x| x.trim().to_lowercase()).or_else(|| v.as_u64().map(|n| n.to_string())).unwrap_or_default();
    let key = |c: &Value| (s(&c["host"]), s(&c["port"]), c["user"].as_str().unwrap_or("").to_string(), s(&c["sshHost"]));
    let k = key(connj);
    let same: Vec<Value> = load_profiles().into_iter().filter(|c| key(c) == k).collect();
    !same.is_empty() && same.iter().all(|c| c["readonly"].as_bool().unwrap_or(false))
}

// ---------- SSH tunnels ----------
// A connection with an SSH host reaches its database through `ssh -L` - the system's OpenSSH
// client - listening on a free local port, and every connection to it (build_conn, and the option
// files of the command-line tools) goes to that port instead. One tunnel per SSH login and database
// address, kept for whatever connects to it next, until the app exits. Signing in is the client's
// own business: an agent, a key file, or ~/.ssh/config, which brings host aliases and ProxyJump
// with it. BatchMode means it never waits on a password prompt nobody can see; a login that would
// need one fails, with ssh's own reason.
struct Tunnel { child: std::process::Child, port: u16 }
fn tunnels() -> &'static Mutex<std::collections::HashMap<String, Tunnel>> {
    static MAP: OnceLock<Mutex<std::collections::HashMap<String, Tunnel>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
// A saved connection's SSH password, kept in the OS keychain beside its database password.
const SSH_KEYRING: &str = "NOBSSQL-Desktop-SSH";
fn ssh_pw_get(name: &str) -> String { keyring::Entry::new(SSH_KEYRING, name).ok().and_then(|e| e.get_password().ok()).unwrap_or_default() }
fn ssh_pw_set(name: &str, pw: &str) {
    if let Ok(e) = keyring::Entry::new(SSH_KEYRING, name) { if pw.is_empty() { let _ = e.delete_credential(); } else { let _ = e.set_password(pw); } }
}
// The page never holds a saved connection's passwords: it names the connection (savedName) and the
// password and SSH password are filled in here - but only for the address they were saved for. A
// page that could send savedName "prod" with another host would otherwise have prod's password sent
// to that host. When they are filled in, the connection's own SSL settings go with them, so the
// request cannot ask for them over less than the connection was saved with.
fn endpoint_key(c: &Value) -> (String, u16, String, String, u16, String) {
    let t = |v: &Value| v.as_str().unwrap_or("").trim().to_lowercase();
    (t(&c["host"]), port_of(&c["port"], 3306), c["user"].as_str().unwrap_or("").to_string(),
     t(&c["sshHost"]), port_of(&c["sshPort"], 22), c["sshUser"].as_str().unwrap_or("").trim().to_string())
}
fn profile_named(name: &str) -> Option<Value> { load_profiles().into_iter().find(|c| c["name"].as_str() == Some(name)) }
fn db_pw_get(name: &str) -> String { keyring::Entry::new("NOBSSQL-Desktop", name).ok().and_then(|e| e.get_password().ok()).unwrap_or_default() }
fn db_pw_set(name: &str, pw: &str) {
    if let Ok(e) = keyring::Entry::new("NOBSSQL-Desktop", name) { if pw.is_empty() { let _ = e.delete_credential(); } else { let _ = e.set_password(pw); } }
}
fn resolve_saved(connj: &Value) -> Value {
    let name = connj["savedName"].as_str().unwrap_or("");
    if name.is_empty() { return connj.clone(); }
    resolve_with(connj, profile_named(name).as_ref(), || db_pw_get(name), || ssh_pw_get(name))
}
// The decision, apart from where profiles and passwords are kept (so a test can drive it).
fn resolve_with(connj: &Value, profile: Option<&Value>, db_pw: impl Fn() -> String, ssh_pw: impl Fn() -> String) -> Value {
    let Some(p) = profile else { return connj.clone() };
    if endpoint_key(p) != endpoint_key(connj) { return connj.clone(); }
    let mut c = connj.clone();
    let mut filled = false;
    if c["password"].as_str().unwrap_or("").is_empty() {
        let pw = db_pw();
        if !pw.is_empty() { c["password"] = json!(pw); filled = true; }
    }
    if c["sshPassword"].as_str().unwrap_or("").is_empty() {
        let pw = ssh_pw();
        if !pw.is_empty() { c["sshPassword"] = json!(pw); filled = true; }
    }
    if filled {
        c["ssl"] = p["ssl"].clone(); c["sslCa"] = p["sslCa"].clone(); c["sshKey"] = p["sshKey"].clone();
        c["clearPw"] = json!(p["clearPw"].as_bool().unwrap_or(false));
    }
    c
}
fn close_tunnels() {
    if let Ok(mut m) = tunnels().lock() { for (_, mut t) in m.drain() { let _ = t.child.kill(); let _ = t.child.wait(); } }
}
fn port_of(v: &Value, default: u16) -> u16 {
    v.as_str().and_then(|s| s.trim().parse().ok()).or_else(|| v.as_u64().map(|p| p as u16)).unwrap_or(default)
}
fn ssh_exe() -> std::path::PathBuf {
    // The OpenSSH client Windows ships, when it is installed; otherwise whatever ssh is on the PATH.
    #[cfg(windows)]
    {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let p = std::path::Path::new(&root).join("System32").join("OpenSSH").join("ssh.exe");
        if p.exists() { return p; }
    }
    std::path::PathBuf::from("ssh")
}
// Where to connect for this database: the tunnel's local end if the connection has an SSH host,
// otherwise its own host and port.
fn endpoint(connj: &Value) -> Result<(String, u16), String> {
    let resolved = resolve_saved(connj);
    let connj = &resolved;
    let host = connj["host"].as_str().unwrap_or("127.0.0.1").trim().to_string();
    let port = port_of(&connj["port"], 3306);
    let ssh_host = connj["sshHost"].as_str().unwrap_or("").trim().to_string();
    if ssh_host.is_empty() { return Ok((host, port)); }
    if connj["ssl"].as_str() == Some("verify") {
        return Err("SSL mode \"verify\" cannot check the server's host name through an SSH tunnel, because the connection is made to 127.0.0.1. Use \"verify-ca\" instead: the certificate chain is still validated.".into());
    }
    let ssh_port = port_of(&connj["sshPort"], 22);
    let ssh_user = connj["sshUser"].as_str().unwrap_or("").trim().to_string();
    let key = connj["sshKey"].as_str().unwrap_or("").trim().to_string();
    let password = connj["sshPassword"].as_str().unwrap_or("").to_string();
    let id = format!("{}@{}:{}|{}|{}:{}", ssh_user, ssh_host, ssh_port, key, host, port);
    // Held while a tunnel opens, so two connections at once do not open two.
    let mut m = tunnels().lock().unwrap();
    if let Some(t) = m.get_mut(&id) {
        if matches!(t.child.try_wait(), Ok(None)) { return Ok(("127.0.0.1".into(), t.port)); }
        m.remove(&id);
    }
    let t = open_tunnel(&ssh_host, ssh_port, &ssh_user, &key, &password, &host, port)?;
    let local = t.port;
    m.insert(id, t);
    Ok(("127.0.0.1".into(), local))
}
// The local port is found free, let go of and handed to ssh, and something else can take it in
// between. ssh then gives up on the forward (ExitOnForwardFailure), but a program listening on that
// port answered the check below all the same, and the connection - password and all - would have
// gone to it. So ssh has to be still running after the port answers, and a port lost that way is
// tried again with another, up to three times.
fn open_tunnel(ssh_host: &str, ssh_port: u16, ssh_user: &str, key: &str, password: &str, host: &str, port: u16) -> Result<Tunnel, String> {
    let mut last = String::new();
    for _ in 0..3 {
        let local = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).map_err(|e| e.to_string())?;
        match open_tunnel_on(local, ssh_host, ssh_port, ssh_user, key, password, host, port) {
            Ok(t) => return Ok(t),
            Err((why, true)) => last = why,
            Err((why, false)) => return Err(why),
        }
    }
    Err(last)
}
// One try on one local port. The error says whether it was the port - worth another try - or not.
#[allow(clippy::too_many_arguments)]
fn open_tunnel_on(local: u16, ssh_host: &str, ssh_port: u16, ssh_user: &str, key: &str, password: &str, host: &str, port: u16) -> Result<Tunnel, (String, bool)> {
    let target = if host.contains(':') { format!("[{}]", host) } else { host.to_string() };
    let mut cmd = Command::new(ssh_exe());
    cmd.args(["-N", "-T", "-o", "ExitOnForwardFailure=yes", "-o", "StrictHostKeyChecking=accept-new",
              "-o", "ServerAliveInterval=30", "-o", "ConnectTimeout=15"]);
    cmd.arg("-L").arg(format!("127.0.0.1:{}:{}:{}", local, target, port)).arg("-p").arg(ssh_port.to_string());
    if !key.is_empty() { cmd.arg("-i").arg(key).args(["-o", "IdentitiesOnly=yes"]); }
    // Kept until this function returns - the tunnel is up, or it has failed.
    let _pw_file: Option<tempfile::NamedTempFile> = if password.is_empty() {
        // Nobody can answer a prompt, so there is none: a login that would need one fails.
        cmd.args(["-o", "BatchMode=yes"]);
        None
    } else {
        // The password goes to ssh the one way it takes one without a terminal: a program it runs
        // and reads it from - this app again (see main). The password is in a file of the user's own
        // that lasts until the tunnel is up or has failed (this function), not in the environment,
        // where it stayed readable for as long as ssh ran. One try, so a wrong one fails at once.
        cmd.args(["-o", "NumberOfPasswordPrompts=1"]);
        let mut f = tempfile::Builder::new().prefix("nobs-ssh-").tempfile().map_err(|e| (format!("Could not prepare the SSH password: {e}"), false))?;
        { use std::io::Write; f.write_all(password.as_bytes()).and_then(|_| f.flush()).map_err(|e| (format!("Could not prepare the SSH password: {e}"), false))?; }
        if let Ok(me) = std::env::current_exe() {
            cmd.env("SSH_ASKPASS", me).env("SSH_ASKPASS_REQUIRE", "force").env("NOBS_SSH_ASKPASS", "1").env("NOBS_SSH_PWFILE", f.path());
        }
        Some(f)
    };
    // After "--" the destination cannot be read as an option, whatever it starts with.
    cmd.arg("--").arg(if ssh_user.is_empty() { ssh_host.to_string() } else { format!("{}@{}", ssh_user, ssh_host) });
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; cmd.creation_flags(0x0800_0000); }
    let mut child = cmd.spawn().map_err(|e| (format!("Could not start ssh ({}). SSH tunnels use the OpenSSH client; on Windows it is the optional feature \"OpenSSH Client\".", e), false))?;
    // What ssh says, read as it comes so a chatty tunnel never fills the pipe and stalls; the last
    // line is the reason when it gives up.
    let said = std::sync::Arc::new(Mutex::new(String::new()));
    if let Some(err) = child.stderr.take() {
        let said = said.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                let l = line.trim();
                if !l.is_empty() && !l.starts_with("Warning: Permanently added") { *said.lock().unwrap() = l.to_string(); }
            }
        });
    }
    // ssh listens on the local port once it has signed in, so a connection there means the tunnel is
    // up - if ssh is still running a moment later. Had the port been taken, ssh has exited by then.
    let port_lost = |why: &str| { let w = why.to_ascii_lowercase(); w.contains("address already in use") || w.contains("cannot listen") || w.contains("local forwarding") || w.contains("bind") };
    let exited = |said: &std::sync::Arc<Mutex<String>>| { std::thread::sleep(std::time::Duration::from_millis(150)); said.lock().unwrap().clone() };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            let why = exited(&said);
            let lost = port_lost(&why);
            return Err((format!("Could not establish SSH tunnel to {}: {}", ssh_host, if why.is_empty() { "ssh exited.".to_string() } else { why }), lost));
        }
        if std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], local)), std::time::Duration::from_millis(200)).is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(400));
            if let Ok(Some(_)) = child.try_wait() {
                let why = exited(&said);
                return Err((format!("Could not establish SSH tunnel to {}: another program took its local port ({})", ssh_host, if why.is_empty() { "ssh exited" } else { &why }), true));
            }
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill(); let _ = child.wait();
            return Err((format!("SSH tunnel to {} timed out after 25 seconds.", ssh_host), false));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(Tunnel { child, port: local })
}

fn build_conn(connj: &Value) -> Result<Conn, String> {
    let resolved = resolve_saved(connj);
    let connj = &resolved;
    let (host, port) = endpoint(connj)?;
    let user = connj["user"].as_str().unwrap_or("root").to_string();
    let pass = connj["password"].as_str().unwrap_or("").to_string();
    let ssl = connj["ssl"].as_str().unwrap_or("default");
    let mut ob = OptsBuilder::new()
        .ip_or_hostname(Some(host)).tcp_port(port)
        .user(Some(user)).pass(Some(pass))
        .tcp_connect_timeout(Some(std::time::Duration::from_secs(10)));
    let ca = connj["sslCa"].as_str().filter(|s| !s.is_empty());
    ob = ob.ssl_opts(ssl_opts_for(ssl, ca));
    let mut init: Vec<String> = Vec::new();
    if connj["utc"].as_bool().unwrap_or(false) { init.push("SET time_zone='+00:00'".into()); }
    if let Some(cs) = browse_charset(connj) {
        init.push(format!("SET NAMES {}", cs));
        // And the server refuses to write on this connection at all. The endpoints below block a
        // write statement before it is sent, but they can only block the ones they are asked to
        // run; this covers every path that builds a connection, including any added later without
        // this in mind. Reads, temporary tables and the app's own session settings are unaffected.
        init.push("SET SESSION TRANSACTION READ ONLY".into());
    }
    if !init.is_empty() { ob = ob.init(init); }
    // "default" is what the MySQL and MariaDB clients call PREFERRED: encrypted whenever the server
    // offers TLS, the certificate not checked. It used to mean plaintext here, always - passwords
    // and rows went over the wire in the clear to servers that offered TLS, while Export and
    // Import, which run the client tools, did use it. Plaintext is the fallback only when TLS
    // itself is what failed (the server has none, or the handshake did not work); a refused
    // login is not tried a second time, which could count twice against an account lockout.
    //
    // mysql_clear_password - how a PAM or LDAP account signs in (MariaDB with
    // pam_use_cleartext_plugin=ON, MySQL Enterprise) - sends the password as it is typed, so it is
    // answered only on a connection that is encrypted: "required" and the verify modes always are,
    // and "default" on its TLS attempt. The plaintext fallback, and "disabled", refuse it.
    // And where the server's certificate is not checked ("required", "default"), only when the
    // connection says so (clearPw): TLS that checks nothing lets whoever sits in between pose as the
    // server, ask for the password as typed, and read it.
    let clear_ok = connj["clearPw"].as_bool().unwrap_or(false);
    if ssl_mode_verifies(ssl) || (ssl == "required" && clear_ok) { ob = ob.enable_cleartext_plugin(true); }
    if ssl == "default" {
        let tls = ob.clone().ssl_opts(Some(SslOpts::default().with_danger_accept_invalid_certs(true))).enable_cleartext_plugin(clear_ok);
        match Conn::new(Opts::from(tls)) {
            Ok(c) => return Ok(c),
            Err(e) if tls_was_the_problem(&e) => {}
            Err(e) => return Err(explain_conn_error(ssl, ca.is_some(), &e.to_string())),
        }
    }
    Conn::new(Opts::from(ob)).map_err(|e| explain_conn_error(ssl, ca.is_some(), &e.to_string()))
}

// Whether a connection failed at TLS rather than anywhere after it: the server offers none, or the
// handshake itself did not complete.
fn tls_was_the_problem(e: &mysql::Error) -> bool {
    match e {
        mysql::Error::DriverError(mysql::DriverError::TlsNotSupported) => true,
        mysql::Error::TlsError(_) => true,
        // An answer from the server is never TLS failing: "Access denied for user 'ssl_admin'" -
        // or any message that happens to say ssl - would otherwise be retried without encryption.
        mysql::Error::MySqlError(_) => false,
        _ => { let m = e.to_string().to_lowercase(); m.contains("tls") || m.contains("ssl") || m.contains("handshake") }
    }
}

// The ssl setting -> what the connection actually does. Split out from build_conn so a test can
// assert the mapping directly: the dangerous regression here is "verify" quietly picking up
// with_danger_accept_invalid_certs, which would still negotiate TLS and so still look correct
// from the outside while checking nothing.
//
// `ca` is an optional path to a PEM certificate to trust as the root. Without one, "verify"
// validates against the OS trust store, which no self-signed certificate can satisfy - and both
// MariaDB and MySQL generate exactly that when none is configured, so the common case for a
// private server is that "verify" is unusable until you point it at the server's own CA.
fn with_ca(opts: SslOpts, ca: Option<&str>) -> SslOpts {
    match ca {
        Some(p) => {
            let owned: std::borrow::Cow<'static, std::path::Path> =
                std::borrow::Cow::Owned(std::path::PathBuf::from(p));
            opts.with_root_cert_path(Some(owned))
        }
        None => opts,
    }
}

fn ssl_opts_for(ssl: &str, ca: Option<&str>) -> Option<SslOpts> {
    match ssl {
        // Encrypt the wire, but do not check who is on the other end of it. A CA is meaningless
        // here by definition - nothing is being verified - so it is deliberately not applied,
        // rather than set and silently ignored.
        "required" => Some(SslOpts::default().with_danger_accept_invalid_certs(true)),
        // Full validation - the chain AND that the certificate names the host being connected to -
        // against the given CA if there is one, the OS trust store if not.
        "verify"    => Some(with_ca(SslOpts::default(), ca)),
        // The chain, but not the host name. This is what makes a CA usable against the certificate
        // MariaDB and MySQL generate for themselves: MySQL 8's reads
        // CN=MySQL_Server_8.0.46_Auto_Generated_Server_Certificate with no subjectAltName, which no
        // host name will ever match, so "verify" refuses it even with exactly the right CA -
        // measured: "The certificate's CN name does not match the passed value." Only the host
        // name check is relaxed. The chain is still validated, so a certificate the CA did not
        // sign is still refused.
        "verify-ca" => Some(with_ca(SslOpts::default(), ca).with_danger_skip_domain_validation(true)),
        // No TLS. The crate already defaults to plaintext, but naming "disabled" here keeps it a
        // decision rather than a fall-through that a change of default would silently reverse.
        "disabled" => None,
        _ => None,
    }
}

// A failed TLS handshake under "verify" arrives as a raw debug-formatted Rust error - one real
// example, verbatim:
//
//   TlsError { A certificate chain processed, but terminated in a root certificate which is not
//   trusted by the trust provider. (os error -2146762487) }
//
// That is accurate and useless: it does not say which setting caused it, and the usual cause is
// not a broken server but a perfectly normal one. MariaDB and MySQL both auto-generate a
// self-signed certificate when none is configured, so "verify" rejects the default install of
// either. The note says which knob to turn instead of leaving the user guessing at the server.
// What to say when COMMIT itself fails.
//
// A statement failing BEFORE the commit is genuinely undone, either by the ROLLBACK we send or,
// if the connection is already gone, by the server discarding the open transaction when it
// notices - so telling the user nothing was applied is true.
//
// COMMIT is not like that. If it failed because the connection broke while it was in flight, the
// server may have committed and simply had nowhere to send the acknowledgement; we cannot tell
// that apart from a commit that never happened. This used to say "No changes were applied", which
// is a coin flip stated as a fact - and the reassuring side of it, which is the wrong way round
// for a message someone will act on by re-applying their edits.
fn commit_failure_message(err: &str) -> String {
    format!("Could not commit: {err}\n\nThe changes may or may not have been saved - if the \
connection dropped while the commit was in flight, the server may have completed it anyway. \
Check the table before applying these changes again.")
}

// The three ways a verifying connection actually fails, each with the error text Windows really
// produced for it against MySQL 8's auto-generated certificate:
//
//   no CA          "...terminated in a root certificate which is not trusted by the trust provider"
//   wrong CA       the same text - the chain still ends somewhere untrusted
//   right CA,      "The certificate's CN name does not match the passed value."
//   wrong name
//
// The last is the one worth singling out. The CA was right and the chain checked out; what failed
// is a host name that the auto-generated certificate could never have matched. The fix is a
// different mode, not a different file, and without saying so the natural next move is to keep
// swapping CA files that were never the problem.
fn explain_conn_error(ssl: &str, has_ca: bool, err: &str) -> String {
    // PAM and LDAP accounts. The server asked for the password as typed, and this connection is not
    // encrypted (build_conn answers that only over TLS) - or it asked through MariaDB's "dialog"
    // plugin, which only the client tools can answer.
    // The driver words it one way on the first request and another on a switch to it.
    if err.contains("mysql_clear_password must be enabled") || err.contains("Unknown authentication protocol: `mysql_clear_password`") {
        return format!("{err}\n\nThis account signs in with its password sent as typed (PAM or LDAP), and \
this connection is not encrypted, or does not check the server's certificate, so the password was not \
sent. Set SSL to a verify mode, or - on a network you trust - to \"required\" with \"PAM / LDAP sign-in\" \
ticked in the saved connection.");
    }
    if err.contains("Unknown authentication protocol: `dialog`") {
        return format!("{err}\n\nThis account signs in through PAM with MariaDB's dialog plugin, which \
this app cannot answer. On the server, SET GLOBAL pam_use_cleartext_plugin=ON (and in its config file) \
makes PAM ask for the password in a way the app can answer - over an encrypted connection, so set SSL \
to a verify mode, or to \"required\" with \"PAM / LDAP sign-in\" ticked in the saved connection.");
    }
    let tls_related = err.contains("TlsError") || err.contains("certificate") || err.contains("Certificate");
    if !ssl_mode_verifies(ssl) || !tls_related { return err.to_string(); }
    let name_mismatch = err.contains("CN name does not match") || err.contains("name mismatch")
        || err.contains("hostname") || err.contains("host name");
    if name_mismatch {
        return format!("{err}\n\nThe certificate chain checked out, but the certificate does not name \
the host you connected to. The certificates MariaDB and MySQL generate for themselves never do - \
MySQL's is issued to \"MySQL_Server_<version>_Auto_Generated_Server_Certificate\". Use SSL mode \
\"verify-ca\", which checks the certificate against your CA but not the host name, or connect using \
the exact name the certificate was issued to.");
    }
    if has_ca {
        format!("{err}\n\nA CA certificate was supplied, but the server's certificate was not signed \
by it. Check that the CA file is the one belonging to this server - for a MySQL server with an \
auto-generated certificate, that is ca.pem in its data directory.")
    } else {
        format!("{err}\n\nSSL mode \"{ssl}\" needs the server's certificate to be signed by a CA your \
machine already trusts, and the self-signed certificate MariaDB and MySQL generate by default never \
is. Set \"CA certificate\" on this connection to the server's CA file, or use \"required\" to \
encrypt without verifying.")
    }
}

// ---------- value conversion (typed -> Option<String>) ----------
fn is_binaryish(c: &Column) -> bool {
    use mysql::consts::ColumnType::*;
    match c.column_type() {
        // BIT is always rendered as a hex literal
        MYSQL_TYPE_BIT => true,
        // MySQL 9's VECTOR: 4-byte floats, read and written as their bytes.
        MYSQL_TYPE_VECTOR => true,
        // BLOB/TEXT and CHAR/VARCHAR share type codes; only charset 63 (binary) is real binary data.
        // TEXT columns (e.g. the "OK" status from CHECK/ANALYZE TABLE) have a real charset -> keep as text.
        MYSQL_TYPE_BLOB | MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB
        | MYSQL_TYPE_STRING | MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_VARCHAR | MYSQL_TYPE_GEOMETRY
            => c.character_set() == 63,
        _ => false,
    }
}
// is_binaryish() above answers "does this column display/round-trip as a 0x.. hex literal" -
// true for BOTH BIT and real binary (BLOB/VARBINARY/etc) columns, which is all display needs to
// know. But the two have different VALID WRITE syntax in MySQL: a bare integer literal
// (`INSERT ... VALUES (8)`) is a correct, unambiguous way to set a BIT column - MySQL treats it
// as the bit pattern, not as text - while the same bare integer sent to a true binary/BLOB column
// would store the bytes of the digit character itself. The editor's write-validation needs this
// finer distinction (see applyChanges() client-side, and the bitCols field in query()'s
// response) so it can accept a plain "0"/"1" for a BIT flag column - the overwhelmingly common
// case - without also opening the door to that byte-corruption mistake on a real binary column.
fn is_bit_col(c: &Column) -> bool {
    matches!(c.column_type(), mysql::consts::ColumnType::MYSQL_TYPE_BIT)
}
fn val_to_opt(v: &MyValue, binaryish: bool) -> Option<String> {
    match v {
        MyValue::NULL => None,
        MyValue::Bytes(b) => {
            if binaryish { return Some(format!("0x{}", hex::encode(b))); }
            match std::str::from_utf8(b) {
                Ok(s) => Some(s.to_string()),
                Err(_) => Some(format!("0x{}", hex::encode(b))),
            }
        }
        MyValue::Int(i) => Some(i.to_string()),
        MyValue::UInt(u) => Some(u.to_string()),
        MyValue::Float(f) => Some(f.to_string()),
        MyValue::Double(d) => Some(d.to_string()),
        MyValue::Date(y, mo, d, h, mi, s, us) => Some(if *us > 0 {
            format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:06}", y, mo, d, h, mi, s, us)
        } else { format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s) }),
        MyValue::Time(neg, d, h, mi, s, us) => {
            let sign = if *neg { "-" } else { "" };
            let hh = (*d) * 24 + (*h as u32);
            Some(if *us > 0 { format!("{}{}:{:02}:{:02}.{:06}", sign, hh, mi, s, us) }
                 else { format!("{}{}:{:02}:{:02}", sign, hh, mi, s) })
        }
    }
}

// The mysql crate's Display for a server error is its Debug-ish wrapper,
// "MySqlError { ERROR 1146 (42S02): Table 'x' doesn't exist }". Users see these now that real
// errors are no longer masked as cancellations, and the wrapper is noise - the PowerShell build
// shows the bare "ERROR 1146 (42S02): ..." line. Strip it, leaving anything unrecognised alone.
fn db_err(e: impl std::string::ToString) -> String {
    let s = e.to_string();
    s.strip_prefix("MySqlError { ").and_then(|r| r.strip_suffix(" }"))
        .map(|r| r.to_string()).unwrap_or(s)
}

// A result read into memory: its column names and its rows, a value per column (None for NULL).
type Rows = Vec<Vec<Option<String>>>;
type Table = (Vec<String>, Rows);
// What opening a cursor answers: column names, which are binary, and which are BIT.
type CursorOpen = Result<(Vec<String>, Vec<bool>, Vec<bool>), String>;
// What open_cursor answers: cursor id, columns, binary and BIT flags, the first rows, has_more.
type CursorFirstPage = (String, Vec<String>, Vec<bool>, Vec<bool>, Rows, bool);

// Which columns are binary/BIT is decided here for display encoding; run_select_bin also hands
// it back so the grid can refuse to write a decimal into one. Typing 8 into a BIT(8) cell stored
// 56 - the byte value of the character '8' - with no error at all.
fn run_select_bin(conn: &mut Conn, sql: &str) -> Result<(Vec<String>, Rows, Vec<bool>), String> {
    let mut result = conn.query_iter(sql).map_err(db_err)?;
    let cols: Vec<String> = result.columns().as_ref().iter().map(|c| c.name_str().to_string()).collect();
    let bin: Vec<bool> = result.columns().as_ref().iter().map(is_binaryish).collect();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    for r in result.by_ref() {
        let row = r.map_err(db_err)?;
        rows.push(decode_row(&row, &bin));
    }
    Ok((cols, rows, bin))
}

fn run_select(conn: &mut Conn, sql: &str) -> Result<Table, String> {
    let (c, r, _) = run_select_bin(conn, sql)?;
    Ok((c, r))
}

// Decodes one already-fetched row into the grid's Option<String> cell format. Shared by
// run_select_bin (whole-result-set reads used everywhere except ad-hoc queries) and the cursor
// thread below (which reads a bounded batch at a time from a query it keeps open across
// multiple Tauri command calls). `bin` is computed once per query, not per row/batch.
fn decode_row(row: &Row, bin: &[bool]) -> Vec<Option<String>> {
    let mut cells = Vec::with_capacity(bin.len());
    for i in 0..bin.len() {
        let v = row.as_ref(i).cloned().unwrap_or(MyValue::NULL);
        cells.push(val_to_opt(&v, *bin.get(i).unwrap_or(&false)));
    }
    cells
}

// ---------- ad-hoc query result cursors ----------
// Ad-hoc queries from the editor (the "Run" button and "Fetch next N rows") are streamed through
// a cursor rather than materialized in one shot: without this, a query with a high or missing
// LIMIT against a large table gets fully read into memory (once in the driver's row buffers,
// again in a Vec, again in the serde_json::Value tree, again in the serialized IPC payload) - on
// a multi-GB table that exhausts the process's memory and the allocator aborts the whole app
// rather than returning an error. A cursor only ever pulls one page's worth of rows into memory
// at a time, regardless of how many total rows the query matches or what LIMIT (if any) the user
// wrote, and - unlike re-running the query with an increasing OFFSET - it never makes MySQL
// rescan and discard everything before the current page: the same open result set is read
// incrementally across calls.
//
// A cursor is a live mysql::Conn + its still-open mysql::QueryResult, which cannot be split
// across two separate Tauri command invocations without becoming a self-referential struct
// (QueryResult borrows &mut Conn). Instead both live together in the stack frame of one
// dedicated OS thread that outlives any single command call: the thread opens the query once,
// then blocks on a channel between command calls, fetching another page whenever asked and
// exiting (dropping QueryResult then Conn, closing the connection) once the result set is
// exhausted, it's told to close, or nobody has asked for more in a while.
enum CursorCmd {
    Fetch { n: usize, reply: std::sync::mpsc::Sender<Result<CursorBatch, String>> },
    Close,
}
struct CursorBatch {
    rows: Vec<Vec<Option<String>>>,
    has_more: bool,
}
// Registry of open cursors, keyed by a server-generated cursorId: the frontend only ever holds
// the id, never the sender itself, and looks it up here on every subsequent fetch/close. Same
// OnceLock<Mutex<HashMap<...>>> idiom as running_queries()/cancelled_queries() above.
fn cursors() -> &'static Mutex<std::collections::HashMap<String, std::sync::mpsc::Sender<CursorCmd>>> {
    static MAP: OnceLock<Mutex<std::collections::HashMap<String, std::sync::mpsc::Sender<CursorCmd>>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
// No uuid crate in this project's Cargo.toml, and nothing here needs global uniqueness beyond
// "never collides with another cursor this process has open" - a monotonic counter plus the
// wall-clock time it was minted is simpler than pulling in a dependency for it.
fn next_cursor_id() -> String {
    static COUNTER: OnceLock<std::sync::atomic::AtomicU64> = OnceLock::new();
    let n = COUNTER.get_or_init(|| std::sync::atomic::AtomicU64::new(0)).fetch_add(1, Ordering::SeqCst);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("cur{}_{}", now, n)
}

// ---------- Transactions a tab keeps open ----------
// With auto-commit off, a tab's statements all run on one connection of its own, kept here under
// the id the tab made up for it, with autocommit=0, until Commit or Rollback. Every other command
// still opens a connection of its own. The connection is taken out while a statement runs (or a
// cursor reads from it) and put back after, so two runs from one tab never share it at once.
enum SessSlot { Idle(Conn), Busy }
struct Session { slot: SessSlot }
fn sessions() -> &'static Mutex<std::collections::HashMap<String, Session>> {
    static MAP: OnceLock<Mutex<std::collections::HashMap<String, Session>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}
// A connection borrowed from a session; dropping it puts it back. If the session was closed in the
// meantime there is nowhere to put it, and dropping it lets the server roll back what it held.
struct SessConn { id: String, c: Option<Conn> }
impl std::ops::Deref for SessConn { type Target = Conn; fn deref(&self) -> &Conn { self.c.as_ref().unwrap() } }
impl std::ops::DerefMut for SessConn { fn deref_mut(&mut self) -> &mut Conn { self.c.as_mut().unwrap() } }
impl Drop for SessConn { fn drop(&mut self) { if let Some(c) = self.c.take() { sess_return(&self.id, c); } } }
fn sess_return(id: &str, c: Conn) {
    if let Some(s) = sessions().lock().unwrap().get_mut(id) { s.slot = SessSlot::Idle(c); }
}
const SESS_LOST: &str = "The connection holding this tab's transaction was lost, so the server rolled back everything it had not committed. The next run starts a new transaction.";
// The tab's connection: opened with autocommit=0 the first time, and after that the same one - once
// the statement or cursor still using it hands it back.
fn sess_checkout(id: &str, connj: &Value) -> Result<SessConn, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        {
            let mut m = sessions().lock().unwrap();
            match m.get_mut(id) {
                None => {
                    m.insert(id.to_string(), Session { slot: SessSlot::Busy });
                    drop(m);
                    let opened = build_conn(connj).and_then(|mut c| c.query_drop("SET autocommit=0").map(|_| c).map_err(db_err));
                    return match opened {
                        Ok(c) => Ok(SessConn { id: id.to_string(), c: Some(c) }),
                        Err(e) => { sessions().lock().unwrap().remove(id); Err(e) }
                    };
                }
                Some(s) => {
                    if let SessSlot::Idle(_) = s.slot {
                        let SessSlot::Idle(mut c) = std::mem::replace(&mut s.slot, SessSlot::Busy) else { unreachable!() };
                        drop(m);
                        if c.ping().is_err() {
                            sessions().lock().unwrap().remove(id);
                            return Err(SESS_LOST.to_string());
                        }
                        return Ok(SessConn { id: id.to_string(), c: Some(c) });
                    }
                }
            }
        }
        if std::time::Instant::now() > deadline {
            return Err("This tab's transaction is still busy with the statement before. Wait for it, or cancel it, and run again.".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
// Where a command's connection comes from: the tab's session when it names one, else a new one.
enum Db { Own(Conn), Sess(SessConn) }
impl std::ops::Deref for Db { type Target = Conn; fn deref(&self) -> &Conn { match self { Db::Own(c) => c, Db::Sess(s) => s } } }
impl std::ops::DerefMut for Db { fn deref_mut(&mut self) -> &mut Conn { match self { Db::Own(c) => c, Db::Sess(s) => s } } }
impl Db {
    fn in_session(&self) -> bool { matches!(self, Db::Sess(_)) }
    // The connection itself, for a cursor thread to own, and the session to hand it back to.
    fn into_parts(self) -> (Conn, Option<String>) {
        match self { Db::Own(c) => (c, None), Db::Sess(mut s) => { let c = s.c.take().unwrap(); (c, Some(s.id.clone())) } }
    }
}
fn db_for(req: &Value) -> Result<Db, String> {
    match req["session"].as_str().filter(|s| !s.is_empty()) {
        Some(id) => Ok(Db::Sess(sess_checkout(id, &req["conn"])?)),
        None => Ok(Db::Own(build_conn(&req["conn"])?)),
    }
}
// Commit, Rollback, or Close (roll back and let the connection go) for a tab's session. A session
// that was never opened has nothing to commit or roll back, which is not an error.
#[tauri::command]
async fn session_end(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let id = req["session"].as_str().unwrap_or("").to_string();
        let action = req["action"].as_str().unwrap_or("rollback").to_string();
        let exists = sessions().lock().unwrap().contains_key(&id);
        if id.is_empty() || !exists { return Ok(json!({"ok":true,"open":false})); }
        if action == "close" {
            let slot = sessions().lock().unwrap().remove(&id);
            // A busy connection comes back to no session and is dropped there; an idle one is
            // rolled back here rather than left to the server to notice.
            if let Some(Session { slot: SessSlot::Idle(mut c) }) = slot { let _ = c.query_drop("ROLLBACK"); }
            return Ok(json!({"ok":true,"open":false}));
        }
        let mut c = match sess_checkout(&id, &req["conn"]) { Ok(c) => c, Err(e) => return Ok(json!({"ok":false,"error":e,"lost":e == SESS_LOST})) };
        let stmt = if action == "commit" { "COMMIT" } else { "ROLLBACK" };
        match c.query_drop(stmt) {
            Ok(_) => Ok(json!({"ok":true,"open":true})),
            Err(e) => {
                let e = db_err(e);
                Ok(json!({"ok":false,"error": if action == "commit" { commit_failure_message(&e) } else { e }}))
            }
        }
    }).await.map_err(|e| e.to_string())?
}

// Spawns the cursor's dedicated thread. `conn` is moved in and never touched again outside it.
// Returns a receiver that fires exactly once with the result of OPENING the query (columns +
// which are binary, or the error `conn.query_iter` failed with), and the sender used for every
// Fetch/Close for the lifetime of the cursor.
fn spawn_cursor_thread(conn: Conn, sql: String, cursor_id: String, request_id: Option<String>, home: Option<String>) -> (
    std::sync::mpsc::Receiver<CursorOpen>,
    std::sync::mpsc::Sender<CursorCmd>,
) {
    let (open_tx, open_rx) = std::sync::mpsc::channel::<CursorOpen>();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<CursorCmd>();
    std::thread::spawn(move || {
        let mut conn = conn;
        // Deregisters this cursor from both cursors() and, if the caller supplied one,
        // running_queries() - called from every exit path below. query()'s own registration of
        // requestId -> connection id (made before the cursor ever opens, so Cancel can find it
        // immediately) is deliberately NOT removed by query() itself once a cursor is going to
        // outlive that call: ownership of "when is this requestId no longer cancellable" passes
        // to the cursor thread, so Cancel keeps working against the same connection for as long
        // as the cursor stays open - including while idle between "fetch next" calls, and while
        // a slow fetch is in flight - not just during the very first page.
        let cleanup = |request_id: &Option<String>| {
            if let Ok(mut m) = cursors().lock() { m.remove(&cursor_id); }
            if let Some(rid) = request_id { running_queries().lock().unwrap().remove(rid); }
        };
        // Conn and its still-open QueryResult live together in this one stack frame for the
        // entire life of the cursor - the only way to avoid QueryResult's self-referential
        // borrow of Conn across separate command invocations.
        // A cursor on a tab's transaction gives the connection back to it at the end, whichever
        // way this block is left.
        'body: {
        let mut result = match conn.query_iter(&sql) {
            Ok(r) => r,
            Err(e) => { let _ = open_tx.send(Err(db_err(e))); cleanup(&request_id); break 'body; }
        };
        let cols: Vec<String> = result.columns().as_ref().iter().map(|c| c.name_str().to_string()).collect();
        let bin: Vec<bool> = result.columns().as_ref().iter().map(is_binaryish).collect();
        let bit: Vec<bool> = result.columns().as_ref().iter().map(is_bit_col).collect();
        if open_tx.send(Ok((cols.clone(), bin.clone(), bit.clone()))).is_err() { cleanup(&request_id); break 'body; } // caller went away

        // The look-ahead row from the previous Fetch. `result` is a forward-only iterator, so a
        // row read to answer "is there more?" cannot be un-read - it has to be held here and
        // emitted as the first row of the next page. Dropping it instead silently lost exactly
        // one row at every page boundary (100k rows at the default pageSize surfaced as 99,901,
        // with has_more=false, so nothing indicated the loss).
        let mut carry: Option<Vec<Option<String>>> = None;
        loop {
            // Idle safety net: an abandoned cursor (tab closed without the UI's cleanup call
            // reaching the backend, app crashed, etc.) can't hold its DB connection open forever.
            match cmd_rx.recv_timeout(std::time::Duration::from_secs(600)) {
                Ok(CursorCmd::Fetch { n, reply }) => {
                    // Fill the page (starting with whatever the last look-ahead held back), then
                    // read exactly one row beyond it so "is there more?" is still answered from
                    // this same fetch with no extra round trip - but keep that row for next time.
                    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
                    let mut err: Option<String> = None;
                    // A page also ends at PAGE_BYTES, whatever n says. A table of large BLOBs sent a
                    // thousand of them at once, hex-encoded and so twice their size: a few MB each
                    // came to gigabytes in one answer, and the app hung or ran out of memory. The
                    // rest arrives as the grid scrolls, as it does for any long result; a page
                    // always holds at least one row, however large.
                    let mut bytes = 0usize;
                    let size = |r: &Vec<Option<String>>| r.iter().map(|v| v.as_ref().map(|s| s.len()).unwrap_or(0)).sum::<usize>();
                    if let Some(r) = carry.take() { bytes += size(&r); rows.push(r); }
                    while rows.len() < n && bytes < PAGE_BYTES {
                        match result.next() {
                            Some(Ok(row)) => { let r = decode_row(&row, &bin); bytes += size(&r); rows.push(r) }
                            Some(Err(e)) => { err = Some(db_err(e)); break; }
                            None => break,
                        }
                    }
                    // Only look ahead when the page actually filled (by rows or by size): a short
                    // page already means the result set is exhausted, and asking for another row
                    // would be pointless.
                    if err.is_none() && (rows.len() == n || bytes >= PAGE_BYTES) {
                        match result.next() {
                            Some(Ok(row)) => carry = Some(decode_row(&row, &bin)),
                            Some(Err(e)) => { err = Some(db_err(e)); }
                            None => {}
                        }
                    }
                    let has_more = carry.is_some();
                    let done = err.is_some() || !has_more;
                    let resp = match err { Some(e) => Err(e), None => Ok(CursorBatch { rows, has_more }) };
                    let _ = reply.send(resp);
                    if done { break; }
                }
                Ok(CursorCmd::Close) => break,
                Err(_) => break, // idle timeout, or every Sender (incl. the registry's) was dropped
            }
        }
        // Exhaustion, Close, and timeout all land here: deregister first (a stale entry would
        // make a later fetch/close look like it's still talking to a live cursor), then let
        // `result` and `conn` drop, closing the connection.
        cleanup(&request_id);
        }
        if let Some(id) = home { sess_return(&id, conn); }
    });
    (open_rx, cmd_tx)
}

// Opens a new cursor and fetches its first page in one call - the common path for the "Run"
// button, whether or not the result ends up needing more than one page. `conn` is consumed: it
// belongs to the cursor thread from here on, not to the caller. Returns the cursorId (only
// meaningful if has_more is also true - see below), columns, binary-column flags, the first
// page of rows, and has_more. `request_id`, if supplied, is the same id query() registered in
// running_queries() before calling this - handed to the cursor thread so it (not this function's
// caller) can eventually clear that registration once the cursor genuinely closes.
fn open_cursor(conn: Conn, sql: String, first_n: usize, request_id: Option<String>, home: Option<String>) -> Result<CursorFirstPage, String> {
    let cursor_id = next_cursor_id();
    let (open_rx, cmd_tx) = spawn_cursor_thread(conn, sql, cursor_id.clone(), request_id, home);
    let (cols, bin, bit) = match open_rx.recv() {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err("Cursor thread failed to start.".to_string()),
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmd_tx.send(CursorCmd::Fetch { n: first_n, reply: reply_tx }).is_err() {
        return Err("Cursor closed unexpectedly.".to_string());
    }
    let batch = match reply_rx.recv() {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err("Cursor closed unexpectedly.".to_string()),
    };
    // Whole result fit in one page (or there were no columns at all, e.g. a statement with no
    // result set): the thread has already exhausted it, deregistered itself, and exited - so
    // there is nothing left to register here, and no cursorId the frontend should hold onto.
    if !cols.is_empty() && batch.has_more {
        cursors().lock().unwrap().insert(cursor_id.clone(), cmd_tx);
    }
    Ok((cursor_id, cols, bin, bit, batch.rows, batch.has_more))
}

// ---------- SQL text helpers (identifier + literal, Workbench-style + hex rule) ----------
fn sql_id(name: &str) -> String { format!("`{}`", name.replace('`', "``")) }
// Mirrors the PowerShell version's Test-SqlReadOnly: strips /* */, --, and # comments, then
// every statement's leading keyword must be on the allow-list for the SQL to be read-only.
// This is the server-side enforcement backing a connection's "read-only / safe mode" flag.
// Removes every balanced (...) group, tracking nesting depth so this is correct for parentheses
// nested inside parentheses (unlike a regex, which can't do that). Used by sql_is_readonly to see
// past a CTE's own body (or a subquery's) to the keyword actually driving the statement. Each
// removed group leaves a single space behind so words on either side don't get glued together.
fn strip_parens(s: &str, backslash_escapes: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    // A ')' (or, for that matter, a keyword) inside a quoted string/identifier isn't a real
    // paren/keyword at all - "WHERE a=')SELECT('" is one string literal, not three tokens. Without
    // tracking quote state, that stray ')' closed the depth early and let the literal SELECT it
    // contains leak out as an exposed depth-0 token, which sql_is_readonly's "first verb wins"
    // check then picked over the CTE's real (dangerous) trailing statement.
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if escaped { escaped = false; }
            else if c == '\\' && backslash_escapes && q != '`' { escaped = true; }
            else if c == q {
                // A doubled quote ('' or "" or ``) is an escaped literal quote, not the closer -
                // consume the pair and stay inside the string.
                if chars.peek() == Some(&q) { chars.next(); }
                else { quote = None; }
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => { quote = Some(c); }
            '(' => { if depth == 0 { out.push(' '); } depth += 1; }
            ')' => {
                // Two separate checks, not one if/else-if: decrement first, THEN test the new
                // depth - closing the outermost paren needs both to run in that order.
                if depth > 0 { depth -= 1; }
                if depth == 0 { out.push(' '); }
            }
            _ => { if depth == 0 { out.push(c); } }
        }
    }
    out
}
// Returns whatever follows the first top-level occurrence of `kw`, or None if it never appears
// outside a quoted string. Used to unwrap MariaDB's "SET STATEMENT <assignments> FOR <statement>",
// where the part after FOR is a whole statement that really executes. A FOR inside a string
// literal - SET STATEMENT x='FOR' FOR SELECT 1 - is not the separator and must not be taken for one.
fn split_off_keyword(s: &str, kw: &str, backslash_escapes: bool) -> Option<String> {
    let raw: Vec<char> = s.chars().collect();
    let up: Vec<char> = s.to_uppercase().chars().collect();
    let k: Vec<char> = kw.to_uppercase().chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut i = 0usize;
    while i < raw.len() {
        let c = raw[i];
        if let Some(q) = quote {
            if escaped { escaped = false; }
            else if c == '\\' && backslash_escapes && q != '`' { escaped = true; }
            else if c == q { quote = None; }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' || c == '`' { quote = Some(c); i += 1; continue; }
        // Match on word boundaries, so FORMAT - or a column named for_id - is not read as FOR.
        let before_ok = i == 0 || !is_word(raw[i - 1]);
        if before_ok && i + k.len() <= up.len() && up[i..i + k.len()] == k[..] {
            let after = i + k.len();
            if after >= up.len() || !is_word(raw[after]) {
                return Some(raw[after..].iter().collect::<String>().trim().to_string());
            }
        }
        i += 1;
    }
    None
}

// The words of a statement outside its quoted strings and identifiers, upper-cased: letters, digits
// and _ @ $ . - so @@GLOBAL.x is one word. The quotes follow the server's rules, as above.
fn unquoted_words(s: &str, backslash_escapes: bool) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in s.chars() {
        if let Some(q) = quote {
            if escaped { escaped = false; }
            else if c == '\\' && backslash_escapes && q != '`' { escaped = true; }
            else if c == q { quote = None; }
            continue;
        }
        if c.is_alphanumeric() || matches!(c, '_' | '@' | '$' | '.') { cur.extend(c.to_uppercase()); continue; }
        if !cur.is_empty() { words.push(std::mem::take(&mut cur)); }
        if c == '\'' || c == '"' || c == '`' { quote = Some(c); }
    }
    if !cur.is_empty() { words.push(cur); }
    words
}

// The text with its comments taken out the way the server reads them, strings left as they are.
// "--" is a comment only when a space or control character follows it - "SELECT 1--1" is one
// minus minus one - and nothing inside quotes is a comment. Patterns that ignored both let
// "SELECT 1--1; DELETE FROM t", "SELECT '#'; DELETE FROM t" and "SELECT '/*'; DELETE FROM t;
// SELECT '*/'" through as read-only, the DELETE hidden in what was taken for a comment.
// /*! ... */ and /*M! ... */ are not comments at all: the server runs what they hold, so their
// contents stay. Whether a backslash escapes a quote depends on the server's sql_mode, which is
// why the caller asks both ways.
fn strip_sql_comments(sql: &str, backslash_escapes: bool) -> String {
    let c: Vec<char> = sql.chars().collect();
    let n = c.len();
    let mut out = String::with_capacity(sql.len());
    let mut q: Option<char> = None;
    let mut i = 0;
    while i < n {
        let ch = c[i];
        if let Some(qc) = q {
            out.push(ch);
            if ch == '\\' && backslash_escapes && qc != '`' && i + 1 < n { out.push(c[i + 1]); i += 2; continue; }
            if ch == qc { q = None; }
            i += 1;
            continue;
        }
        if ch == '\'' || ch == '"' || ch == '`' { q = Some(ch); out.push(ch); i += 1; continue; }
        if ch == '#' || (ch == '-' && i + 1 < n && c[i + 1] == '-' && (i + 2 >= n || c[i + 2].is_whitespace() || c[i + 2].is_control())) {
            while i < n && c[i] != '\n' { i += 1; }
            out.push(' ');
            continue;
        }
        if ch == '/' && i + 1 < n && c[i + 1] == '*' {
            let mut j = i + 2;
            while j + 1 < n && !(c[j] == '*' && c[j + 1] == '/') { j += 1; }
            let (body_end, end) = if j + 1 < n { (j, j + 2) } else { (n, n) };
            let mut k = i + 2;
            if k < body_end && c[k] == 'M' && k + 1 < body_end && c[k + 1] == '!' { k += 1; }
            if k < body_end && c[k] == '!' {
                k += 1;
                while k < body_end && c[k].is_ascii_digit() { k += 1; }
                out.push(' ');
                out.extend(&c[k..body_end]);
            }
            out.push(' ');
            i = end;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

// Read-only only if it is read-only however a backslash is read: 'a\'; DELETE ...' is one string
// where a backslash escapes and a string followed by a DELETE where it does not (sql_mode
// NO_BACKSLASH_ESCAPES), and the other way round - so a text that would run a write under either
// reading is refused.
fn sql_is_readonly(sql: &str) -> bool {
    sql_is_readonly_as(sql, true) && sql_is_readonly_as(sql, false)
}

fn sql_is_readonly_as(sql: &str, backslash_escapes: bool) -> bool {
    if sql.trim().is_empty() { return true; }
    // MariaDB's ANALYZE [FORMAT=JSON] <statement> form (distinct from ANALYZE TABLE) actually
    // EXECUTES the wrapped statement while profiling it - bare "ANALYZE" was allow-listed for the
    // genuinely read-only ANALYZE TABLE form, which also let "ANALYZE DELETE FROM t" straight
    // through untouched. This strips a leading FORMAT=JSON clause so the wrapped statement's own
    // keyword is what's left to check.
    let re_analyze_fmt = regex::Regex::new(r"(?i)^FORMAT\s*=\s*JSON\s+").unwrap();
    let re_explain_fmt = regex::Regex::new(r"(?i)^FORMAT\s*=\s*\w+\s+").unwrap();
    let s = strip_sql_comments(sql, backslash_escapes);
    const ALLOW: &[&str] = &["SELECT","SHOW","DESCRIBE","DESC","EXPLAIN","USE","WITH","SET","HELP","VALUES","TABLE","ANALYZE","CHECK","CHECKSUM"];
    for stmt in s.split(';') {
        let t = stmt.trim();
        if t.is_empty() { continue; }
        let w = t.split_whitespace().next().unwrap_or("").to_uppercase();
        if !ALLOW.contains(&w.as_str()) { return false; }
        // SELECT ... INTO OUTFILE / INTO DUMPFILE writes a file on the DATABASE SERVER's
        // filesystem, as the mysqld user. It changes no table data, which is presumably why it
        // was never considered here - but a connection the user marked "read-only / safe mode"
        // being able to drop files on the server is not read-only. Verified against a live
        // MariaDB whose secure_file_priv was empty: the statement was reported as read-only and
        // wrote the file. Only the OUTFILE/DUMPFILE forms are refused; SELECT ... INTO @var is an
        // ordinary variable assignment and stays allowed, and an INTO inside a string literal is
        // not a clause at all - which is why this looks for the keyword outside quotes.
        // Every INTO, not only the first: "SELECT a INTO @x FROM t UNION SELECT b INTO OUTFILE ..."
        // has its file behind the second.
        let words = unquoted_words(t, backslash_escapes);
        if words.windows(2).any(|p| p[0] == "INTO" && (p[1] == "OUTFILE" || p[1] == "DUMPFILE")) { return false; }
        // SET is allowed because a session variable is harmless, but SET GLOBAL / SET PERSIST -
        // and their @@GLOBAL. / @@PERSIST. spellings - reconfigure the server for every
        // connection, which is not something a read-only connection should be able to do.
        if w == "SET" {
            let up = t.to_uppercase();
            let second = up.split_whitespace().nth(1).unwrap_or("");
            // In any of the assignments, not only the first: "SET @a = 1, GLOBAL x = 1" is two.
            if words.iter().any(|w| w == "GLOBAL" || w == "PERSIST" || w == "PERSIST_ONLY" || w.starts_with("@@GLOBAL") || w.starts_with("@@PERSIST")) { return false; }
            // SET RESOURCE GROUP g FOR <thread> moves another connection's thread (MySQL).
            if second == "RESOURCE" { return false; }
            // SET is allow-listed for session variables, but several SET forms are not variable
            // assignments at all. These three write, and were reaching the server on a connection
            // the user had marked read-only:
            //   SET PASSWORD FOR 'u'@'%' = ...   changes any account's credentials, root included
            //   SET DEFAULT ROLE admin FOR ...   grants a role to an account
            //   SET STATEMENT x=1 FOR <stmt>     MariaDB: EXECUTES the statement it wraps, so
            //                                    "... FOR DELETE FROM t" really does delete
            // The last is the same shape as the ANALYZE wrapper handled above - a read-only
            // looking prefix carrying an arbitrary statement - so it gets the same treatment:
            // unwrap it and judge the statement that is actually going to run.
            if second == "PASSWORD" { return false; }
            if second == "DEFAULT" && up.split_whitespace().nth(2).unwrap_or("") == "ROLE" { return false; }
            if second == "STATEMENT" {
                match split_off_keyword(t, "FOR", backslash_escapes) {
                    // Recurse: the wrapped statement is judged exactly as if it had been typed on
                    // its own, so "FOR SELECT 1" stays allowed and "FOR DROP TABLE t" does not.
                    Some(inner) => { if !sql_is_readonly(&inner) { return false; } }
                    // No FOR at all is not a form we recognise; refuse rather than guess.
                    None => return false,
                }
            }
        }
        // A CTE only stays read-only if it's actually prefixing a SELECT/TABLE/VALUES - MySQL
        // 8.0.19+/MariaDB also allow "WITH x AS (...) DELETE/UPDATE FROM t ...", which the leading
        // "WITH" alone can't reveal. Strip every CTE's own (possibly nested) body via
        // strip_parens, leaving roughly "WITH cte1 AS , cte2 AS  DELETE FROM t ..." - the first
        // remaining recognizable verb after that is the statement actually being run.
        if w == "WITH" {
            let stripped = strip_parens(t, backslash_escapes);
            const VERBS: &[&str] = &["SELECT","INSERT","UPDATE","DELETE","REPLACE","TABLE","VALUES"];
            let verb = stripped.split_whitespace().map(|w| w.to_uppercase()).find(|w| VERBS.contains(&w.as_str()));
            match verb.as_deref() {
                Some("SELECT") | Some("TABLE") | Some("VALUES") => {}
                _ => return false,
            }
        }
        // EXPLAIN ANALYZE (MySQL 8.0.18+, and DESCRIBE/DESC ANALYZE with it) runs the statement it
        // profiles, and from 8.0.19 that can be a multi-table UPDATE or DELETE - which then changes
        // the data. What follows ANALYZE (and a FORMAT=) is judged as a statement of its own, and
        // has to be a query. EXPLAIN without ANALYZE runs nothing.
        if matches!(w.as_str(), "EXPLAIN" | "DESCRIBE" | "DESC") && words.get(1).map(|x| x == "ANALYZE").unwrap_or(false) {
            let rest = t.split_once(char::is_whitespace).map(|x| x.1).unwrap_or("").trim_start();
            let rest = rest.split_once(char::is_whitespace).map(|x| x.1).unwrap_or("").trim_start();
            let inner = re_explain_fmt.replace(rest, "").to_string();
            let inner_w = inner.split_whitespace().next().unwrap_or("").to_uppercase();
            if !matches!(inner_w.as_str(), "SELECT" | "WITH" | "TABLE" | "VALUES") && !inner.starts_with('(') { return false; }
            if !sql_is_readonly_as(&inner, backslash_escapes) { return false; }
        }
        if w == "ANALYZE" {
            let rest = t.split_once(char::is_whitespace).map(|x| x.1).unwrap_or("").trim_start();
            let is_analyze_table = rest.split_whitespace().next().map(|f| f.eq_ignore_ascii_case("TABLE")).unwrap_or(false);
            if !is_analyze_table {
                let inner = re_analyze_fmt.replace(rest, "");
                let inner_w = inner.split_whitespace().next().unwrap_or("").to_uppercase();
                if inner_w != "SELECT" { return false; }
            }
        }
    }
    true
}
// The values written here escape a backslash as "\\" (sql_str_lit). On a server whose sql_mode has
// NO_BACKSLASH_ESCAPES a backslash is an ordinary character, and "C:\\temp" was stored as it was
// written, one backslash too many - a CSV import, a synced row, a saved cell. The connections that
// write them are the app's own and last for one job, so the mode is taken off there rather than
// every literal being written two ways. A server that refuses the SET keeps its mode; nothing
// else changes.
fn backslash_escapes_on(c: &mut Conn) {
    let _ = c.query_drop("SET SESSION sql_mode = TRIM(BOTH ',' FROM REPLACE(CONCAT(',', @@SESSION.sql_mode, ','), ',NO_BACKSLASH_ESCAPES,', ','))");
}

// The actual escaping: backslash first (so a literal backslash never combines with the quote
// doubling below to re-open the string), then the quote itself. Used directly wherever a plain
// string literal is needed (schema/table names from information_schema, usernames, ...) - unlike
// sql_lit() below, this has no hex-literal special case, so it's the right one for a NAME, which
// should never be reinterpreted as a raw hex value just because it happens to look like one.
// CR and NUL are written as escapes: mysql.exe reading a script turns every CR LF into LF, so a raw
// CR before a line feed was dropped whenever this SQL went through the client (an import, or a
// saved script), and a raw NUL makes it refuse the statement without --binary-mode.
fn sql_str_lit(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''").replace('\r', "\\r").replace('\0', "\\0"))
}
fn sql_lit(s: &str) -> String {
    if !s.is_empty() && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit()) && s.len() > 2 {
        return s.to_string(); // hex literal (bit/binary)
    }
    sql_str_lit(s)
}
// ---------- mysql / mysqldump CLI resolution ----------
fn config_file() -> std::path::PathBuf {
    let mut p = dirs::config_dir().unwrap_or(std::env::temp_dir());
    p.push("NOBSSQL-Desktop"); std::fs::create_dir_all(&p).ok(); p.push("config.json"); p
}
fn log_line(msg: &str) {
    let mut p = dirs::config_dir().unwrap_or(std::env::temp_dir());
    p.push("NOBSSQL-Desktop"); let _ = std::fs::create_dir_all(&p); p.push("log.txt");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        use std::io::Write as _;
        let _ = writeln!(f, "{}  {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), msg);
    }
}
fn load_cfg() -> Value {
    std::fs::read_to_string(config_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}))
}
// Downloaded client tools go to the local AppData: the roaming one is copied to the server at every
// sign-in and sign-out on a network with roaming profiles, and a hundred megabytes of programs has
// no business there. Settings stay in the roaming one, where settings belong.
fn tools_dir() -> std::path::PathBuf {
    let mut p = dirs::data_local_dir().or_else(dirs::config_dir).unwrap_or(std::env::temp_dir());
    p.push("NOBSSQL-Desktop"); p.push("bin"); p
}
// Where downloads went before - tools already there keep working from the paths saved for them.
fn tools_dir_old() -> std::path::PathBuf {
    let mut p = dirs::config_dir().unwrap_or(std::env::temp_dir());
    p.push("NOBSSQL-Desktop"); p.push("bin"); p
}
fn ver_key(s: &str) -> Vec<u64> {
    s.split('.').map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0)).collect()
}

fn resolve_tool(app: &tauri::AppHandle, base: &str, names: &[&str], env_key: &str) -> Result<String, String> {
    // 0) user-configured path (Settings / downloaded tools) wins
    let cfg = load_cfg();
    if let Some(pth) = cfg.get(format!("{}_bin", base)).and_then(|v| v.as_str()) {
        if !pth.is_empty() && std::path::Path::new(pth).exists() { return Ok(pth.to_string()); }
    }
    // 1) prefer a binary bundled with the app (src-tauri/binaries)
    if let Ok(dir) = app.path().resource_dir() {
        let mut p = dir.clone();
        #[cfg(windows)]
        p.push(format!("binaries/{}.exe", base));
        #[cfg(not(windows))]
        p.push(format!("binaries/{}", base));
        if p.exists() { return Ok(p.to_string_lossy().to_string()); }
    }
    // 2) fall back to env var / common install dirs / PATH
    resolve_bin(names, env_key)
}
// Directories to search for a client-tool executable, in order, under each of `bases`: any child
// named MariaDB*/MySQL* contributes its own bin, then each of ITS children's bin. Both layouts
// are needed and the old code only handled the second: MariaDB installs to
// "Program Files\MariaDB 11.4\bin" (version in the folder name, bin directly inside) while
// MySQL installs to "Program Files\MySQL\MySQL Server 8.0\bin" (one level deeper). Scanning
// only <root>\<child>\bin from a "Program Files\MariaDB" root missed every real MariaDB
// install, and the same shape missed XAMPP, whose bin sits directly at "xampp\mysql\bin" -
// both of which the Settings dialog claimed were checked. `direct` covers that last case.
// Kept separate from resolve_bin so it can be exercised against a temporary tree in a test
// rather than only on a machine that happens to have these products installed.
fn tool_search_dirs(bases: &[std::path::PathBuf], direct: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    for base in bases {
        let rd = match std::fs::read_dir(base) { Ok(r) => r, Err(_) => continue };
        // Sorted so the order is deterministic rather than whatever the filesystem returns.
        let mut kids: Vec<std::path::PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        kids.sort();
        for kid in kids {
            let name = kid.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !(name.starts_with("mariadb") || name.starts_with("mysql")) { continue; }
            out.push(kid.join("bin"));
            if let Ok(sub) = std::fs::read_dir(&kid) {
                let mut subs: Vec<std::path::PathBuf> = sub.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
                subs.sort();
                for s in subs { out.push(s.join("bin")); }
            }
        }
    }
    out.extend(direct.iter().cloned());
    out
}

fn resolve_bin(names: &[&str], env_key: &str) -> Result<String, String> {
    if let Ok(p) = std::env::var(env_key) {
        if !p.is_empty() && std::path::Path::new(&p).exists() { return Ok(p); }
    }
    {
        #[cfg(windows)]
        let (bases, direct) = (
            // Not C:\wamp64 or C:\xampp: any user of the computer can create those, and a mysql.exe
            // put there would be run as whoever uses the app. Their tools are used when picked in
            // Settings.
            vec![std::path::PathBuf::from("C:\\Program Files"),
                 std::path::PathBuf::from("C:\\Program Files (x86)")],
            Vec::<std::path::PathBuf>::new());
        #[cfg(not(windows))]
        let (bases, direct): (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) = (vec![], vec![]);
        for d in tool_search_dirs(&bases, &direct) {
            for n in names {
                let f = d.join(format!("{}.exe", n));
                if f.exists() { return Ok(f.to_string_lossy().to_string()); }
            }
        }
    }
    // Last resort: search PATH directories for the executable directly, rather than spawning it
    // with --version just to confirm it runs. All that's actually needed here is "does this file
    // exist and is it presumably runnable" - a plain existence check answers that without paying
    // for process creation (and, in practice, whatever a fresh/unscanned .exe costs to launch
    // under Windows Defender's real-time scanning) on every uncached tools_status call. Returns
    // the bare name on a match, same as the old --version-spawn check did (not the resolved full
    // path) - describe()'s "found on PATH" vs "found on system" label depends on that; a bare
    // name still spawns fine later since Command::new() resolves it via PATH itself.
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for n in names {
                #[cfg(windows)]
                let candidate = dir.join(format!("{}.exe", n));
                #[cfg(not(windows))]
                let candidate = dir.join(n);
                if candidate.exists() { return Ok(n.to_string()); }
            }
        }
    }
    let name = names[0];
    // Names the Settings dialog first: it is the fix inside the app (pick the path, or download
    // the MariaDB client tools), whereas PATH and the environment variable both need a restart
    // to take effect. The "Could not find '<tool>'" prefix is matched by showToolError() in the
    // UI to decide whether to offer an Open Settings button - keep it if this text changes.
    Err(format!("Could not find '{}'. Open Settings in the app to select {}.exe or download the MariaDB client tools. Alternatively add its bin folder to PATH, or set the {} environment variable to its full path.", name, name, env_key))
}

// ---------- which tools for which server ----------
// MariaDB's and MySQL's client tools are not interchangeable against the other's server. MariaDB's
// mysqldump writes values into a MySQL generated column, so the dump does not restore, and only
// MySQL's client can check a CA without the host name. So the configured (or downloaded) pair
// stays the default, and a MySQL server gets MySQL's own tools when they are available: set in
// Settings (mysql_bin_mysql / mysqldump_bin_mysql), or found in a MySQL Server installation.
//
// The "bin" folders of MySQL Server installations under `bases`, newest version first:
// <base>\MySQL\MySQL Server 8.4\bin, then 8.0, and so on.
fn mysql_server_bin_dirs(bases: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut found: Vec<(Vec<u64>, std::path::PathBuf)> = Vec::new();
    for base in bases {
        let Ok(rd) = std::fs::read_dir(base.join("MySQL")) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() { continue; }
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let Some(ver) = name.to_lowercase().strip_prefix("mysql server").map(|v| v.trim().to_string()) else { continue };
            found.push((ver_key(&ver), p.join("bin")));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().map(|(_, p)| p).collect()
}
fn program_files_dirs() -> Vec<std::path::PathBuf> {
    #[cfg(windows)]
    { vec![std::path::PathBuf::from("C:\\Program Files"), std::path::PathBuf::from("C:\\Program Files (x86)")] }
    #[cfg(not(windows))]
    { Vec::new() }
}
// The tool to use for a MySQL server, with where it came from - or None, meaning the default
// pair is used for MySQL servers as well.
fn mysql_flavor_tool(base: &str) -> Option<(String, String)> {
    let cfg = load_cfg();
    if let Some(p) = cfg.get(format!("{}_bin_mysql", base)).and_then(|v| v.as_str()) {
        if !p.is_empty() && std::path::Path::new(p).exists() { return Some((p.to_string(), "configured".into())); }
    }
    for d in mysql_server_bin_dirs(&program_files_dirs()) {
        let f = d.join(format!("{}.exe", base));
        if f.exists() { return Some((f.to_string_lossy().to_string(), "found in a MySQL Server installation".into())); }
    }
    None
}
// Some(true) for MariaDB, Some(false) for MySQL, None if the server could not be asked.
fn server_is_mariadb(connj: &Value) -> Option<bool> {
    let mut c = build_conn(connj).ok()?;
    let v: String = c.query_first("SELECT VERSION()").ok().flatten()?;
    Some(v.to_lowercase().contains("mariadb"))
}
// resolve_tool, but for a particular server: MySQL's own tools for a MySQL server when there are
// any, the default pair otherwise.
fn resolve_tool_for(app: &tauri::AppHandle, base: &str, names: &[&str], env_key: &str, connj: &Value) -> Result<String, String> {
    choose_tool(server_is_mariadb(connj), || mysql_flavor_tool(base).map(|x| x.0), || resolve_tool(app, base, names, env_key))
}
// Only a server known to be MySQL switches tools; MariaDB, or a server that could not be asked,
// keeps the default pair - which is also the fallback when no MySQL tools exist.
fn choose_tool(server_is_mariadb: Option<bool>, mysql_tool: impl FnOnce() -> Option<String>,
               default: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
    if server_is_mariadb == Some(false) {
        if let Some(p) = mysql_tool() { return Ok(p); }
    }
    default()
}

fn first_err(s: &str) -> String {
    // prefer the real "ERROR NNNN ..." line if present (mysql may echo the statement first)
    if let Some(l) = s.lines().map(|l| l.trim()).find(|l| l.starts_with("ERROR") || l.contains("ERROR ")) {
        return l.to_string();
    }
    s.lines().map(|l| l.trim()).find(|l| !l.is_empty() && !l.chars().all(|c| c == '-'))
        .unwrap_or("").to_string()
}

// mysqldump/mysql print exactly this wording (no "ERROR NNNN" prefix, so first_err() returns it
// verbatim) when a flag the binary doesn't recognise is passed - which happens whenever an
// export/import option only supported by one dump-tool flavor (MySQL vs MariaDB, or an older
// version of either) is used against the other. Name the likely cause instead of leaving a bare
// "unknown variable" for the user to puzzle over.
fn friendly_dump_err(raw: &str) -> String {
    // MySQL's own mysqldump reads its arguments in the Windows code page: a folder named in another
    // script - Отчёты, 報告 - reaches it as question marks, and it cannot create the file (measured,
    // 8.4). MariaDB's tools take the path as it is.
    if raw.contains("Can't create/write to file") && raw.contains('?') {
        return format!("{} - the folder's name has characters MySQL's mysqldump cannot take on this Windows (it reads its arguments in the system code page). Export to a folder whose path has none, or use the MariaDB tools (Settings).", raw);
    }
    if let Some(opt) = raw.split("unknown variable '").nth(1).and_then(|s| s.split('\'').next()) {
        format!("{} - '{}' isn't supported by this build of the tool (MySQL and MariaDB's client tools, and different versions of each, support different flag sets). Uncheck the matching export/import option, or point Settings at the other flavor's .exe.", raw, opt)
    } else {
        raw.to_string()
    }
}

// ---------- commands ----------
#[tauri::command]
async fn connect(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let mut c = match build_conn(&req["conn"]) { Ok(c) => c, Err(e) => return Ok(json!({"ok":false,"error":format!("Connection failed: {}", e)})) };
        match c.query_first::<String, _>("SELECT VERSION()") {
            Ok(Some(v)) => Ok(json!({"ok":true,"version":v,"mariadb":v.contains("MariaDB")})),
            Ok(None) => Ok(json!({"ok":true,"version":"","mariadb":false})),
            Err(e) => Ok(json!({"ok":false,"error":e.to_string()})),
        }
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
// Mirrors the PowerShell version's Api-Schemas: each entry is {name, size} (not a bare
// string) - the sidebar and export dialog both read .name, and the sidebar also shows the
// per-database size badge computed from information_schema.TABLES.
async fn schemas(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let mut c = build_conn(&req["conn"])?;
        let (_c, rows) = run_select(&mut c, "SHOW DATABASES")?;
        let mut names: Vec<String> = rows.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect();
        names.sort();
        let schemas: Vec<Value> = names.into_iter().map(|db| {
            let sql = format!(
                "SELECT SUM(DATA_LENGTH + INDEX_LENGTH) FROM information_schema.TABLES WHERE TABLE_SCHEMA={} AND TABLE_TYPE IN ('BASE TABLE','SYSTEM VERSIONED')",
                sql_lit(&db)
            );
            let size = run_select(&mut c, &sql).ok()
                .and_then(|(_cols, rows)| rows.into_iter().next())
                .and_then(|r| r.into_iter().next().flatten())
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            json!({"name": db, "size": size})
        }).collect();
        Ok(json!({"ok":true,"schemas":schemas}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn objects(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let db = sql_str_lit(req["db"].as_str().unwrap_or(""));
        let mut c = build_conn(&req["conn"])?;
        let sql = format!(
            "SELECT 'table' t,TABLE_NAME n,ENGINE e FROM information_schema.TABLES WHERE TABLE_SCHEMA={d} AND TABLE_TYPE IN ('BASE TABLE','SYSTEM VERSIONED') \
             UNION ALL SELECT 'view',TABLE_NAME,NULL FROM information_schema.TABLES WHERE TABLE_SCHEMA={d} AND TABLE_TYPE IN ('VIEW','SYSTEM VIEW') \
             UNION ALL SELECT 'sequence',TABLE_NAME,NULL FROM information_schema.TABLES WHERE TABLE_SCHEMA={d} AND TABLE_TYPE='SEQUENCE' \
             UNION ALL SELECT IF(ROUTINE_TYPE='PROCEDURE','procedure','function'),ROUTINE_NAME,NULL FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA={d} \
             UNION ALL SELECT 'trigger',TRIGGER_NAME,NULL FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA={d} \
             UNION ALL SELECT 'event',EVENT_NAME,NULL FROM information_schema.EVENTS WHERE EVENT_SCHEMA={d} ORDER BY 1,2", d = db);
        let (_c, rows) = run_select(&mut c, &sql)?;
        let (mut tables, mut views, mut procedures, mut functions, mut triggers, mut events) =
            (vec![], vec![], vec![], vec![], vec![], vec![]);
        // MariaDB's sequences (10.3 on); MySQL has none, and the list is simply empty there.
        let mut sequences: Vec<String> = vec![];
        // Each table's storage engine, for what its menu offers: REPAIR TABLE works on MyISAM,
        // Aria, CSV and Archive, and InnoDB only answers that it does not support it.
        let mut table_engines = serde_json::Map::new();
        for r in rows {
            let t = r.first().cloned().flatten().unwrap_or_default();
            let n = r.get(1).cloned().flatten().unwrap_or_default();
            match t.as_str() {
                "table" => {
                    if let Some(e) = r.get(2).cloned().flatten() { table_engines.insert(n.clone(), json!(e)); }
                    tables.push(n)
                }
                "view" => views.push(n),
                "sequence" => sequences.push(n),
                "procedure" => procedures.push(n), "function" => functions.push(n),
                "trigger" => triggers.push(n), "event" => events.push(n), _ => {}
            }
        }
        // Which table each trigger belongs to, so a table's own right-click menu can offer its
        // EXISTING triggers directly, not just the flat "Triggers" list elsewhere in the tree.
        // A separate lookup (rather than adding a column to the UNION above) so the shape of
        // the existing flat trigger-name array - which other code already relies on - never changes.
        let mut trigger_tables: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        if !triggers.is_empty() {
            if let Ok((_tc, trows)) = run_select(&mut c, &format!("SELECT TRIGGER_NAME, EVENT_OBJECT_TABLE FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA={}", db)) {
                for tr in trows {
                    let tname = tr.first().cloned().flatten().unwrap_or_default();
                    let ttable = tr.get(1).cloned().flatten().unwrap_or_default();
                    if !tname.is_empty() { trigger_tables.insert(tname, ttable); }
                }
            }
        }
        Ok(json!({"ok":true,"tables":tables,"views":views,"procedures":procedures,"functions":functions,"triggers":triggers,"events":events,"sequences":sequences,"triggerTables":trigger_tables,"tableEngines":table_engines}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn ddl(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let (db, name, ty) = (req["db"].as_str().unwrap_or(""), req["name"].as_str().unwrap_or(""), req["type"].as_str().unwrap_or(""));
        let obj = format!("{}.{}", sql_id(db), sql_id(name));
        let sql = match ty {
            "table" => format!("SHOW CREATE TABLE {}", obj),
            "view" => format!("SHOW CREATE VIEW {}", obj),
            "procedure" => format!("SHOW CREATE PROCEDURE {}", obj),
            "function" => format!("SHOW CREATE FUNCTION {}", obj),
            "trigger" => format!("SHOW CREATE TRIGGER {}", obj),
            "event" => format!("SHOW CREATE EVENT {}", obj),
            _ => return Ok(json!({"ok":false,"error":"unknown type"})),
        };
        let mut c = build_conn(&req["conn"])?;
        let (cols, rows) = run_select(&mut c, &sql)?;
        if rows.is_empty() { return Ok(json!({"ok":false,"error":"no DDL returned"})); }
        let idx = cols.iter().position(|h| { let l = h.to_lowercase(); l.contains("create") || l.contains("statement") })
            .unwrap_or(cols.len().saturating_sub(1));
        // The sql_mode a routine, trigger or event was created under, which decides how its body is
        // read - recreating it under another one changes what it does.
        let mode = cols.iter().position(|h| h.eq_ignore_ascii_case("sql_mode")).and_then(|i| rows[0].get(i).cloned().flatten());
        Ok(json!({"ok":true,"ddl":rows[0].get(idx).cloned().flatten().unwrap_or_default(),"sqlMode":mode}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn pk(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let db = sql_str_lit(req["db"].as_str().unwrap_or(""));
        let table = sql_str_lit(req["table"].as_str().unwrap_or(""));
        let mut c = build_conn(&req["conn"])?;
        let sql = format!("SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA={} AND TABLE_NAME={} AND CONSTRAINT_NAME='PRIMARY' ORDER BY ORDINAL_POSITION", db, table);
        let (_c, rows) = run_select(&mut c, &sql)?;
        let list: Vec<String> = rows.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect();
        Ok(json!({"ok":true,"pk":list}))
    }).await.map_err(|e| e.to_string())?
}

// Same idea as pk(), but for foreign-key columns - used so the grid can highlight FK columns
// the same way it already highlights the primary key.
#[tauri::command]
async fn fk(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let db = sql_str_lit(req["db"].as_str().unwrap_or(""));
        let table = sql_str_lit(req["table"].as_str().unwrap_or(""));
        let mut c = build_conn(&req["conn"])?;
        let sql = format!("SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA={} AND TABLE_NAME={} AND REFERENCED_TABLE_NAME IS NOT NULL ORDER BY ORDINAL_POSITION", db, table);
        let (_c, rows) = run_select(&mut c, &sql)?;
        let list: Vec<String> = rows.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect();
        // Full FK detail (which table/column each FK column actually references) - a SEPARATE
        // field from "fk" above, which stays just the local column-name list other callers
        // (PK/FK badge display) already rely on. This powers "go to referenced row" navigation.
        // The referenced table's database and the constraint come too: a key into another database
        // was looked up in this one, and a key over two columns was followed on one of them.
        let detail_sql = format!("SELECT COLUMN_NAME, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME, REFERENCED_TABLE_SCHEMA, CONSTRAINT_NAME FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA={} AND TABLE_NAME={} AND REFERENCED_TABLE_NAME IS NOT NULL ORDER BY ORDINAL_POSITION", db, table);
        let fk_details: Vec<Vec<Option<String>>> = run_select(&mut c, &detail_sql).map(|(_c, r)| r).unwrap_or_default();
        Ok(json!({"ok":true,"fk":list,"fkDetails":fk_details}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn process_list(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let mut c = build_conn(&req["conn"])?;
        let (cols, rows) = run_select(&mut c, "SHOW FULL PROCESSLIST")?;
        Ok(json!({"ok":true,"columns":cols,"rows":rows}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn kill_process(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        if ro_flag(&req) {
            return Ok(json!({"ok":false,"error":"Read-only mode: statement blocked."}));
        }
        let pid = req["pid"].as_str().unwrap_or("");
        if pid.is_empty() || !pid.chars().all(|c| c.is_ascii_digit()) {
            return Ok(json!({"ok":false,"error":"Invalid process id."}));
        }
        let mut c = build_conn(&req["conn"])?;
        match c.query_drop(format!("KILL {}", pid)) {
            Ok(_) => Ok(json!({"ok":true})),
            Err(e) => Ok(json!({"ok":false,"error":e.to_string()})),
        }
    }).await.map_err(|e| e.to_string())?
}

// Column list (with PK flags) plus every FK relationship for a schema - the raw data an ER
// diagram is drawn from. Layout/rendering happens entirely client-side (this just supplies the
// facts: which tables, which columns, which are primary keys, and which columns reference which).
#[tauri::command]
async fn schema_erd(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let db = sql_str_lit(req["db"].as_str().unwrap_or(""));
        let mut c = build_conn(&req["conn"])?;
        let (_c1, columns) = run_select(&mut c, &format!("SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA={} ORDER BY TABLE_NAME, ORDINAL_POSITION", db))?;
        // PK detection deliberately matches get_table_pk_cols's approach (CONSTRAINT_NAME='PRIMARY'),
        // NOT information_schema.COLUMNS.COLUMN_KEY='PRI'. COLUMN_KEY has a documented MySQL edge
        // case: a table with NO actual primary key but a UNIQUE NOT NULL index will still show that
        // index's column as 'PRI', since it behaves like one. Using the same precise method as the
        // grid means the ER diagram can never highlight a column as PK that the grid itself
        // disagrees is one.
        let (_c2, pks) = run_select(&mut c, &format!("SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA={} AND CONSTRAINT_NAME='PRIMARY'", db))?;
        let (_c3, fks) = run_select(&mut c, &format!("SELECT TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA={} AND REFERENCED_TABLE_NAME IS NOT NULL", db))?;
        Ok(json!({"ok":true,"columns":columns,"pks":pks,"fks":fks}))
    }).await.map_err(|e| e.to_string())?
}

// If the incoming SQL is one or more leading "USE <schema>;" statements followed by a final
// statement (this is exactly what the frontend sends for "USE db;\nSELECT ...;" - both when a
// user types it directly, and previously when "+ New Query" auto-inserted it before this
// session's earlier fix), extract the LAST USE's schema name and reduce sql down to just the
// final statement. This sidesteps needing real multi-statement protocol support (CLIENT_MULTI_
// STATEMENTS) entirely: the USE effect is applied via a plain, separate query_drop on this SAME
// connection - exactly the same mechanism already used for the ordinary db parameter below -
// and run_select only ever sees a genuinely single statement, unchanged from every other caller.
// Only matches a well-formed "USE <plain-identifier-or-`backtick-quoted`>;" at the very start,
// repeated as many times as it matches - it never touches anything after the last such match, so
// a semicolon inside the ACTUAL query's own string literals is never at risk of being split on.
fn strip_leading_use_statements(sql: &str) -> (Option<String>, String) {
    let use_re = regex::Regex::new(r"(?is)^\s*use\s+(`[^`]+`|[A-Za-z0-9_$]+)\s*;\s*").unwrap();
    let mut remaining = sql.to_string();
    let mut last_db: Option<String> = None;
    while let Some(caps) = use_re.captures(&remaining) {
        let raw = caps.get(1).unwrap().as_str();
        last_db = Some(raw.trim_matches('`').to_string());
        let matched_len = caps.get(0).unwrap().end();
        remaining = remaining[matched_len..].to_string();
    }
    (last_db, remaining)
}

#[tauri::command]
async fn query(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let sql = req["sql"].as_str().unwrap_or("").to_string();
        if sql.trim().is_empty() { return Ok(json!({"ok":false,"error":"Empty query."})); }
        if ro_mode(&req) && !sql_is_readonly(&sql) {
            return Ok(json!({"ok":false,"error":"Read-only mode: statement blocked."}));
        }
        let mut c = db_for(&req)?;
        // A failed USE is the answer, not something to step over. Dropping the schema the app is
        // pointed at made every query after it run with no database at all, so the server replied
        // "No database selected" - a puzzle about the statement instead of the plain truth, which
        // is that the database is gone. The UI reads exactly that wording to notice and refresh.
        if let Some(db) = req["db"].as_str() {
            if !db.is_empty() {
                if let Err(e) = c.query_drop(format!("USE {}", sql_id(db))) {
                    return Ok(json!({"ok":false,"error":e.to_string()}));
                }
            }
        }
        // A USE explicitly written in the query text itself takes priority over the ambient db
        // parameter above - the user may be deliberately switching schemas mid-query - and only
        // the statement AFTER it actually needs to run through run_select.
        let (explicit_db, sql) = strip_leading_use_statements(&sql);
        if let Some(db) = explicit_db {
            if let Err(e) = c.query_drop(format!("USE {}", sql_id(&db))) {
                return Ok(json!({"ok":false,"error":e.to_string()}));
            }
        }
        // If the frontend gave us a requestId, register this connection's own MySQL
        // CONNECTION_ID() so a Cancel click can look it up and KILL QUERY it from elsewhere.
        let request_id = req["requestId"].as_str().filter(|s| !s.is_empty()).map(String::from);
        if let Some(rid) = &request_id {
            if let Ok((_cols, rows)) = run_select(&mut c, "SELECT CONNECTION_ID()") {
                if let Some(cid) = rows.first().and_then(|r| r.first()).cloned().flatten().and_then(|s| s.parse::<u64>().ok()) {
                    running_queries().lock().unwrap().insert(rid.clone(), (cid, req["conn"].clone()));
                }
            }
        }
        // Cursor-based streaming (Workbench-style "fetch next batch"): every ad-hoc query - with
        // or without its own LIMIT - opens a cursor and pulls just its first page (pageSize rows,
        // default 1000) through it. This replaces the earlier OFFSET/LIMIT rewrite, which (a)
        // only ever kicked in for a query with no LIMIT of its own, leaving one written by the
        // user still hitting the old hard row cap with no way to see the rest, and (b) cost a
        // full rescan-and-discard of everything before OFFSET on every "fetch next" against a
        // large table. A cursor sidesteps both: it applies uniformly regardless of the query's
        // own LIMIT, and paging forward just keeps reading the same already-open result set.
        let page_size = req.get("pageSize").and_then(|v| v.as_u64()).unwrap_or(1000).max(1) as usize;

        let t = std::time::Instant::now();
        // Whether the requestId's running_queries() registration should outlive this call - true
        // only when a cursor is left open afterward, so Cancel keeps being able to KILL QUERY the
        // same connection while the user is idle between "fetch next" calls or waiting on a slow
        // one. False in every other case (no result set, single-page result, or an error), where
        // the cursor thread (if one ever ran) has already deregistered itself by the time this
        // returns, and the removal below is what clears it otherwise.
        let mut cursor_persists = false;
        let (conn, home) = c.into_parts();
        let result = match open_cursor(conn, sql, page_size, request_id.clone(), home) {
            Ok((cursor_id, cols, bin, bit, rows, has_more)) => {
                cursor_persists = has_more;
                if cols.is_empty() {
                    Ok(json!({"ok":true,"columns":[],"rows":[],"elapsedMs":t.elapsed().as_millis() as u64,"message":"Query OK. No result set."}))
                } else {
                    let cursor_id_for_response = if has_more { Some(cursor_id) } else { None };
                    Ok(json!({"ok":true,"columns":cols,"rows":rows,"binaryCols":bin,"bitCols":bit,"hasMore":has_more,"cursorId":cursor_id_for_response,"elapsedMs":t.elapsed().as_millis() as u64}))
                }
            }
            Err(e) => {
                // A cancelled query surfaces here as a MySQL error ("Query execution was
                // interrupted"), so it has to be told apart from an ordinary failure. Ask
                // whether THIS request was actually killed, rather than whether it merely had
                // a requestId - the editor sends one with every run, so the old check turned
                // every syntax error, missing table and permission failure into
                // "Query cancelled." and discarded what really went wrong.
                let was_cancelled = request_id.as_deref().map(take_query_cancelled).unwrap_or(false);
                if was_cancelled { Ok(json!({"ok":false,"error":"Query cancelled.","cancelled":true})) }
                else { Ok(json!({"ok":false,"error":e})) }
            }
        };
        if !cursor_persists {
            if let Some(rid) = &request_id { running_queries().lock().unwrap().remove(rid); let _ = take_query_cancelled(rid); }
        }
        result
    }).await.map_err(|e| e.to_string())?
}

// Fetches the next page (default 1000 rows) from a cursor previously opened by `query`. Returns
// an error if the cursorId is unknown - already exhausted, explicitly closed, or timed out from
// 10 minutes of inactivity - which the frontend surfaces and treats as "nothing more to fetch"
// rather than a hard failure, since all three are ordinary end states, not corruption.
#[tauri::command]
async fn fetch_cursor_batch(req: Value) -> R {
    let cursor_id = req["cursorId"].as_str().unwrap_or("").to_string();
    let page_size = req.get("pageSize").and_then(|v| v.as_u64()).unwrap_or(1000).max(1) as usize;
    // The same requestId query() registered under running_queries() when this cursor was opened
    // - still valid for the cursor's whole life (see query()'s tail / the cursor thread's own
    // cleanup), so a Cancel click reaches this fetch exactly like it reaches the first page.
    let request_id = req["requestId"].as_str().filter(|s| !s.is_empty()).map(String::from);
    tokio::task::spawn_blocking(move || {
        let tx = cursors().lock().unwrap().get(&cursor_id).cloned();
        let Some(tx) = tx else {
            return Ok(json!({"ok":false,"error":"Cursor not found or already closed."}));
        };
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if tx.send(CursorCmd::Fetch { n: page_size, reply: reply_tx }).is_err() {
            cursors().lock().unwrap().remove(&cursor_id);
            return Ok(json!({"ok":false,"error":"Cursor not found or already closed."}));
        }
        match reply_rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(Ok(batch)) => {
                // has_more false means the cursor thread already exhausted, deregistered, and
                // exited on its own - nothing left here to clean up in that case.
                if !batch.has_more { cursors().lock().unwrap().remove(&cursor_id); }
                Ok(json!({"ok":true,"rows":batch.rows,"hasMore":batch.has_more}))
            }
            Ok(Err(e)) => {
                cursors().lock().unwrap().remove(&cursor_id);
                // A Cancel click KILLs the connection this fetch is blocked on, which surfaces
                // here as an ordinary MySQL error ("Query execution was interrupted") - tell it
                // apart from a real failure the same way query()'s own first-page fetch does.
                let was_cancelled = request_id.as_deref().map(take_query_cancelled).unwrap_or(false);
                if was_cancelled { Ok(json!({"ok":false,"error":"Query cancelled.","cancelled":true})) }
                else { Ok(json!({"ok":false,"error":e})) }
            }
            // The fetch did not answer in 30s. The cursor was found and is very much alive - it is
            // still working - so the "not found" message this used to give was misleading in the
            // one case where the user most needs to know what actually happened.
            //
            // Dropping the registry entry alone also stranded the thread: its reply goes out
            // through `let _ = reply.send(..)`, so a receiver that has gone away is ignored and
            // the loop goes back to waiting for a command that can no longer reach it, holding
            // its connection and its open result set until the 600s idle timeout collects it.
            // Send Close so the connection is released now.
            Err(_) => {
                if let Some(tx) = cursors().lock().unwrap().remove(&cursor_id) {
                    let _ = tx.send(CursorCmd::Close);
                }
                Ok(json!({"ok":false,"error":"Timed out waiting for the next page of results (30s). \
The query is still running on the server; this cursor has been closed. Try a smaller page size, \
or narrow the query."}))
            }
        }
    }).await.map_err(|e| e.to_string())?
}

// Tears down an idle-but-open cursor: sent whenever the frontend is done with one before it ran
// out on its own (a tab closed, a new query replacing the previous one in the same tab, the app
// quitting with results still on screen). Always succeeds, including when the cursorId is
// already gone (already exhausted, already closed, idle-timed-out) - callers fire this
// defensively without checking whether there is anything left to close.
#[tauri::command]
async fn close_cursor(req: Value) -> R {
    let cursor_id = req["cursorId"].as_str().unwrap_or("").to_string();
    tokio::task::spawn_blocking(move || {
        if let Some(tx) = cursors().lock().unwrap().remove(&cursor_id) {
            let _ = tx.send(CursorCmd::Close);
        }
        Ok(json!({"ok":true}))
    }).await.map_err(|e| e.to_string())?
}

// Cancels a running query by requestId: looks up the MySQL connection id it was registered
// under, opens a FRESH connection with the same credentials, and runs KILL QUERY <id> on it.
// If the query already finished (nothing registered under that id), this is a harmless no-op.
#[tauri::command]
async fn cancel_query(req: Value) -> R {
    let rid = req["requestId"].as_str().unwrap_or("").to_string();
    let entry = running_queries().lock().unwrap().get(&rid).cloned();
    if let Some((cid, connj)) = entry {
        // Marked before the KILL lands so the query thread, which may fail immediately after,
        // can already see that its failure was a cancel rather than a real error.
        mark_query_cancelled(&rid);
        tokio::task::spawn_blocking(move || {
            if let Ok(mut kc) = build_conn(&connj) {
                let _ = kc.query_drop(format!("KILL QUERY {}", cid));
            }
        }).await.ok();
    }
    Ok(json!({"ok":true}))
}

#[tauri::command]
async fn exec(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let sql = req["sql"].as_str().unwrap_or("").to_string();
        if ro_mode(&req) && !sql_is_readonly(&sql) {
            return Ok(json!({"ok":false,"error":"Read-only mode: statement blocked."}));
        }
        let mut c = build_conn(&req["conn"])?;
        match c.query_drop(&sql) { Ok(_) => Ok(json!({"ok":true})), Err(e) => Ok(json!({"ok":false,"error":e.to_string()})) }
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
// NOTE: not currently called by the frontend (row edits are built and sent as plain SQL via
// applyChanges()/`lit()` -> the query/script command instead), but the command is still
// registered, so each value is written for its column's type, as Compare does (see sql_val_for).
async fn rowop(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        if ro_flag(&req) {
            return Ok(json!({"ok":false,"error":"Read-only mode: statement blocked."}));
        }
        let db = req["db"].as_str().unwrap_or(""); let table = req["table"].as_str().unwrap_or("");
        let obj = format!("{}.{}", sql_id(db), sql_id(table));
        let op = req["op"].as_str().unwrap_or("");
        let pairs = |o: &Value| -> Vec<(String, Value)> {
            o.as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()
        };
        let mut c = build_conn(&req["conn"])?; backslash_escapes_on(&mut c);
        let bin = binary_column_set(&mut c, db, table)?;
        let litc = |k: &str, v: &Value| -> String { json_val_for(v, bin.contains(&k.to_lowercase())) };
        let sql = match op {
            "update" => {
                let sets: Vec<String> = pairs(&req["set"]).iter().map(|(k, v)| format!("{}={}", sql_id(k), litc(k, v))).collect();
                let wh: Vec<String> = pairs(&req["where"]).iter().map(|(k, v)| format!("{}={}", sql_id(k), litc(k, v))).collect();
                if wh.is_empty() { return Ok(json!({"ok":false,"error":"no key columns"})); }
                format!("UPDATE {} SET {} WHERE {} LIMIT 1", obj, sets.join(","), wh.join(" AND "))
            }
            "delete" => {
                let wh: Vec<String> = pairs(&req["where"]).iter().map(|(k, v)| format!("{}={}", sql_id(k), litc(k, v))).collect();
                if wh.is_empty() { return Ok(json!({"ok":false,"error":"no key columns"})); }
                format!("DELETE FROM {} WHERE {} LIMIT 1", obj, wh.join(" AND "))
            }
            "insert" => {
                let vals = pairs(&req["values"]);
                if vals.is_empty() { return Ok(json!({"ok":false,"error":"no values"})); }
                let cols: Vec<String> = vals.iter().map(|(k, _)| sql_id(k)).collect();
                let vs: Vec<String> = vals.iter().map(|(k, v)| litc(k, v)).collect();
                format!("INSERT INTO {} ({}) VALUES ({})", obj, cols.join(","), vs.join(","))
            }
            _ => return Ok(json!({"ok":false,"error":"bad op"})),
        };
        match c.query_drop(&sql) { Ok(_) => Ok(json!({"ok":true})), Err(e) => Ok(json!({"ok":false,"error":e.to_string()})) }
    }).await.map_err(|e| e.to_string())?
}

// script / import / export shell out to the mysql/mysqldump CLI (DELIMITER-safe)
// A raw newline in a value would otherwise start a brand new line in the .cnf file, letting a
// saved connection's host/user/password inject an arbitrary extra option-file directive (e.g.
// "pager=<command>", which the mysql CLI executes) rather than staying part of THIS value.
// Backslash-doubling (below) only protects against \n being misread as an escape sequence -
// it does nothing for an actual embedded newline BYTE, which this strips outright since none of
// these fields have any legitimate use for one.
fn cnf_safe(s: &str) -> String { s.replace(['\r', '\n'], "") }

// A value for an option file, in double quotes. Unquoted, the client ends the value at a "#" (the
// rest is a comment), drops spaces at either end and takes off a pair of quotes around it, so a
// password such as "ab#cd" reached the server as "ab" - Export, Import and scripts failed to log in
// while the grid, which does not use the file, worked. Inside the quotes a backslash and a double
// quote are escaped; both clients were measured to read back exactly what was meant.
fn cnf_quote(s: &str) -> String { format!("\"{}\"", cnf_safe(s).replace('\\', "\\\\").replace('"', "\\\"")) }

// Which SSL option dialect a client binary speaks. The MariaDB and MySQL clients name these
// MUTUALLY EXCLUSIVELY, so the wrong set is not a weaker connection, it is no connection:
// MariaDB's client rejects "ssl-mode=REQUIRED" as an unknown variable, MySQL's rejects "--ssl" as
// an unknown option. It is a property of the binary, not of the server - the client parses the
// options file before it opens a socket. Cached per path; a Settings change points at a new one.
fn client_is_mariadb(tool: &str) -> bool {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Ok(m) = cache.lock() { if let Some(v) = m.get(tool) { return *v; } }
    // The tools this app downloads are MariaDB's, so that is the safe assumption if asking fails.
    let maria = Command::new(tool).arg("--version").output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("MariaDB"))
        .unwrap_or(true);
    if let Ok(mut m) = cache.lock() { m.insert(tool.to_string(), maria); }
    maria
}

// "verify-ca" on the MariaDB client is deliberately the same as "verify". That client has no
// chain-only mode: measured against MySQL 8 with its own CA, once a CA is supplied it checks the
// host name as well (on anything but loopback), and --skip-ssl-verify-server-cert does not relax
// that - nor, importantly, does it weaken the chain check. So the nearest honest mapping is the
// STRICTER one. A setting that asks for verification must never quietly get less of it; getting
// more only means a connection that fails where a looser client would have succeeded.
// MariaDB's client checks the server's certificate by default since 11.4, so "required" (encrypt,
// check nothing) and "default" failed against a remote server with a self-signed certificate -
// measured, 12.3 against MySQL 8 over the LAN: CERT_E_UNTRUSTEDROOT - while the app itself, which
// does not use the client, connected. skip-ssl-verify-server-cert says what those modes mean, and
// is the negated form of an option every MariaDB client knows.
fn ssl_cnf_lines(mode: &str, maria: bool) -> Vec<&'static str> {
    match (mode, maria) {
        ("disabled",  true)  => vec!["skip-ssl"],
        ("required",  true)  => vec!["ssl", "skip-ssl-verify-server-cert"],
        ("default",   true)  => vec!["skip-ssl-verify-server-cert"],
        ("verify",    true)  => vec!["ssl", "ssl-verify-server-cert"],
        ("verify-ca", true)  => vec!["ssl", "ssl-verify-server-cert"],
        // Without TLS, MySQL's client can only send a caching_sha2_password password encrypted with
        // the server's RSA key, and does not ask for that key unless told to: the first login after
        // a server restart failed with 2061 "Authentication requires secure connection". loose-,
        // so a client too old to know the option ignores it.
        ("disabled",  false) => vec!["ssl-mode=DISABLED", "loose-get-server-public-key"],
        ("required",  false) => vec!["ssl-mode=REQUIRED"],
        ("verify",    false) => vec!["ssl-mode=VERIFY_IDENTITY"],
        ("verify-ca", false) => vec!["ssl-mode=VERIFY_CA"],
        _ => vec![],   // "default", and anything unrecognised: leave it to the client
    }
}

// Whether a mode verifies the server at all, and so has any use for a CA.
fn ssl_mode_verifies(mode: &str) -> bool { mode == "verify" || mode == "verify-ca" }

// Where the client should look for authentication plugins.
//
// MySQL 8 authenticates every account with caching_sha2_password by default, root included. That
// is a CLIENT-side plugin - a separate DLL loaded at connect time, resolved relative to the
// client's own location unless it is told otherwise. download_tools unpacks the .exe files into a
// flat directory with no lib/plugin beside them, so export and import against a stock MySQL 8
// server died before they could authenticate:
//
//   ERROR 1045 (28000): Plugin caching_sha2_password could not be loaded:
//   The specified module could not be found. Library path is 'caching_sha2_password.dll'
//
// Only for OUR copy: a client from a real installation has its own lib/plugin next door and finds
// the right ones itself, and pointing it at another product's plugins would break what works.
fn tools_plugin_dir(tool: &str) -> Option<std::path::PathBuf> {
    let parent = std::path::Path::new(tool).parent()?;
    if parent != tools_dir().as_path() && parent != tools_dir_old().as_path() { return None; }
    let p = parent.join("plugin");
    if p.exists() { Some(p) } else { None }
}

// MariaDB's client has no setting that insists on TLS without also checking the certificate. With
// "ssl" and "skip-ssl-verify-server-cert" - which "required" needs, since the certificate a server
// generates for itself is self-signed - it carried on in plaintext when the server offered no TLS
// (MariaDB 12.3's client against 10.2 without TLS: connected, Ssl_cipher empty). Run on
// connecting, this statement fails such a session with a message that names the reason, and
// leaves an encrypted one as it was. It checks the server's side of the connection: what it
// cannot catch is someone in between who speaks TLS to the server - only the verify modes can.
fn tls_guard_sql(server_maria: bool) -> String {
    let status = if server_maria { "information_schema.SESSION_STATUS" } else { "performance_schema.session_status" };
    format!("SET SESSION sql_mode = IF((SELECT COUNT(*) FROM {status} WHERE VARIABLE_NAME = 'Ssl_cipher' AND VARIABLE_VALUE <> '') = 0, \
'NOT_ENCRYPTED_BUT_SSL_MODE_IS_REQUIRED', @@SESSION.sql_mode)")
}
// The guard a client tool needs on this connection: MariaDB's client on an "required" one. Finding
// out which table to ask goes through the native driver, whose "required" does insist on TLS - so a
// server without it is refused here, before the tool is started, and mariadb-dump (which has no
// init-command) is covered by that alone.
fn tls_guard_for(connj: &Value, tool: &str) -> Result<Option<String>, String> {
    if connj["ssl"].as_str() != Some("required") || !client_is_mariadb(tool) { return Ok(None); }
    let mut c = build_conn(connj)?;
    let v: String = c.query_first("SELECT VERSION()").map_err(|e| e.to_string())?.unwrap_or_default();
    Ok(Some(tls_guard_sql(v.to_lowercase().contains("mariadb"))))
}

#[cfg(test)]
fn cnf_file(connj: &Value, tool: &str) -> Result<(tempfile::NamedTempFile, String), String> { cnf_file_with(connj, tool, None) }

// The SHA-256 fingerprint of the certificate a server presents, read the way a client starts TLS on
// the MySQL protocol: the server's greeting, an SSL request, then the TLS handshake. Nothing is
// checked and nothing is sent after it - no user, no password. mariadb-dump has no init-command,
// so the TLS guard cannot reach its session; pinned to this fingerprint (--ssl-fp) it refuses a
// server without TLS, and any other certificate, by itself. A server that offers no TLS is an error
// here already.
fn server_cert_fingerprint(connj: &Value) -> Result<String, String> {
    use sha2::Digest;
    use std::io::{Read, Write};
    let (host, port) = endpoint(connj)?;
    let fail = |e: &dyn std::fmt::Display| format!("Could not read the server's certificate from {host}:{port}: {e}");
    let mut s = std::net::TcpStream::connect((host.as_str(), port)).map_err(|e| fail(&e))?;
    let t = Some(std::time::Duration::from_secs(10));
    let _ = s.set_read_timeout(t); let _ = s.set_write_timeout(t);
    let mut head = [0u8; 4];
    s.read_exact(&mut head).map_err(|e| fail(&e))?;
    let len = head[0] as usize | (head[1] as usize) << 8 | (head[2] as usize) << 16;
    let mut greeting = vec![0u8; len];
    s.read_exact(&mut greeting).map_err(|e| fail(&e))?;
    if greeting.first() == Some(&0xFF) { return Err(fail(&String::from_utf8_lossy(greeting.get(3..).unwrap_or(&[])))); }
    // protocol version, server version up to its NUL, thread id (4), scramble (8), filler (1), then
    // the lower two bytes of the capabilities - where CLIENT_SSL (0x0800) is.
    let nul = greeting.iter().skip(1).position(|&b| b == 0).ok_or_else(|| fail(&"a greeting without a server version"))? + 1;
    let at = nul + 1 + 4 + 8 + 1;
    let caps = greeting.get(at..at + 2).map(|c| u16::from_le_bytes([c[0], c[1]])).ok_or_else(|| fail(&"a greeting cut short"))?;
    if caps & 0x0800 == 0 { return Err(format!("SSL mode \"required\": the server at {host}:{port} offers no TLS.")); }
    // SSLRequest: capabilities (SSL, PROTOCOL_41, SECURE_CONNECTION, LONG_PASSWORD), max packet,
    // character set (utf8mb4_general_ci), 23 bytes of filler.
    let mut req = vec![32u8, 0, 0, 1];
    req.extend_from_slice(&(0x0800u32 | 0x0200 | 0x8000 | 0x0001).to_le_bytes());
    req.extend_from_slice(&(16u32 << 20).to_le_bytes());
    req.push(45);
    req.extend_from_slice(&[0u8; 23]);
    s.write_all(&req).map_err(|e| fail(&e))?;
    let tls = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true).danger_accept_invalid_hostnames(true)
        .build().map_err(|e| fail(&e))?
        .connect(&host, s).map_err(|e| fail(&e))?;
    let der = tls.peer_certificate().map_err(|e| fail(&e))?.ok_or_else(|| fail(&"no certificate"))?.to_der().map_err(|e| fail(&e))?;
    Ok(sha2::Sha256::digest(&der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":"))
}
// Temp files that hold a password are named so, and one a crash left behind is removed at the next
// start (sweep_secret_temp_files).
fn cnf_file_with(connj: &Value, tool: &str, guard: Option<&str>) -> Result<(tempfile::NamedTempFile, String), String> {
    let resolved = resolve_saved(connj);
    let connj = &resolved;
    let mut f = tempfile::Builder::new().prefix("nobs-cnf-").suffix(".cnf").tempfile().map_err(|e| e.to_string())?;
    let mut s = String::from("[client]\n");
    // LOAD DATA LOCAL INFILE lets the SERVER ask the client for any file it names. The app never
    // uses it, so the client tools are told to refuse (loose-: mysqldump has no such option).
    s += "loose-local-infile=0\n";
    // Through the SSH tunnel when the connection has one, as build_conn does.
    let (host, port) = endpoint(connj)?;
    s += &format!("host={}\nport={}\nuser={}\n", cnf_safe(&host), port, cnf_quote(connj["user"].as_str().unwrap_or("root")));
    // MySQL option files treat backslash as an escape character in values (\t, \n, \\, ...), so a
    // password containing a literal backslash has to be doubled here or the .cnf parser would
    // silently consume it as (the start of) an escape sequence instead of a literal character -
    // corrupting the password and breaking auth for anyone whose password happens to contain one.
    if let Some(p) = connj["password"].as_str() { if !p.is_empty() { s += &format!("password={}\n", cnf_quote(p)); } }
    // A .sql file without its own SET NAMES is read in the client's default character set, and
    // MySQL's mysql.exe takes the console code page for that (cp850 here), so an import through it
    // converted UTF-8 text as if it were cp850. MariaDB's client happens to default to utf8mb4.
    // Export's --default-character-set on the command line still takes precedence.
    s += "default-character-set=utf8mb4\n";
    // MySQL's own client from 8.0 on asks for utf8mb4 as utf8mb4_0900_ai_ci, a collation MySQL 5.7
    // (and MariaDB) does not have, and such a server quietly falls back to its own default - latin1
    // on a stock 5.7. A data-only dump then came out in latin1 under a "SET NAMES utf8mb4" heading
    // and turned every character latin1 cannot hold into "?". SET NAMES on connecting takes the
    // server's own utf8mb4 collation, on any server. "loose-" because 5.7's mysqldump has no
    // init-command (and asks for utf8mb4 in a way 5.7 understands).
    if !client_is_mariadb(tool) { s += "loose-init-command=SET NAMES utf8mb4\n"; }
    // "loose-", as mariadb-dump has no init-command (see tls_guard_for).
    else if let Some(g) = guard { s += &format!("loose-init-command={}\n", g); }
    // A PAM or LDAP account wants its password as typed. MySQL's client sends it only when told to,
    // and it is told only on a connection that is encrypted, as build_conn does. (MariaDB's sends it
    // when asked, and answers the dialog plugin as well.)
    let ssl = connj["ssl"].as_str().unwrap_or("default");
    // As build_conn: where the certificate is checked, or where the connection says so.
    if !client_is_mariadb(tool) && (ssl_mode_verifies(ssl) || (ssl == "required" && connj["clearPw"].as_bool().unwrap_or(false))) { s += "loose-enable-cleartext-plugin\n"; }
    // The ssl setting used to stop at the native driver: export and import went out over whatever
    // the CLI happened to negotiate, so a connection saved as "required" - or as "disabled" - was
    // quietly something else as soon as it was dumped or loaded.
    for line in ssl_cnf_lines(ssl, client_is_mariadb(tool)) { s += line; s += "\n"; }
    // Same CA the native driver uses, so a dump goes out under the same verification the rest of
    // the app does. MySQL's client REFUSES ssl-mode=VERIFY_* without one - "CA certificate is
    // required if ssl-mode is VERIFY_CA or VERIFY_IDENTITY" - so for that client this is not a
    // refinement of "verify", it is what makes it work at all.
    if ssl_mode_verifies(ssl) {
        if let Some(ca) = connj["sslCa"].as_str().filter(|s| !s.is_empty()) {
            s += &format!("ssl-ca={}\n", cnf_safe(ca).replace('\\', "\\\\"));
        }
    }
    if let Some(p) = tools_plugin_dir(tool) {
        s += &format!("plugin-dir={}\n", cnf_safe(&p.to_string_lossy()).replace('\\', "\\\\"));
    }
    f.write_all(s.as_bytes()).map_err(|e| e.to_string())?;
    let path = f.path().to_string_lossy().to_string();
    Ok((f, path))
}

// Splits a SQL script into individual statements, honoring quoted strings/identifiers,
// comments, and DELIMITER directives - so DDL bodies (procedures, triggers, functions) with
// semicolons inside BEGIN...END blocks split correctly without needing to shell out to the
// mysql CLI, which was previously the only reason this path needed it. Everything else in this
// app already talks to the server directly through the native driver.
//
// Operates on Vec<char> rather than raw byte indices: SQL string literals/comments can contain
// multi-byte UTF-8 text (accented characters, CJK, emoji), and Rust panics if you slice a &str
// at a non-character-boundary byte offset - char-level indexing sidesteps that entirely.
//
// Validated with 45 unit tests (quoting/escaping rules for ', ", `; --, #, /* */ comments;
// DELIMITER changes including nested procedures, CRLF, case-insensitivity; UTF-8 safety at
// split boundaries) plus a 2000-trial fuzz test asserting no panics on adversarial random
// input - see the project notes for the full suite this was checked against before integration.
fn split_sql_statements(sql: &str) -> Vec<String> {
    let chars: Vec<char> = sql.chars().collect();
    let n = chars.len();
    let mut statements: Vec<String> = Vec::new();
    let mut delimiter: Vec<char> = vec![';'];
    let mut buf: Vec<char> = Vec::new();
    let mut i: usize = 0;
    // true when buf (everything accumulated since the last statement boundary) is empty or
    // whitespace-only - used to recognize DELIMITER only when it's the start of a new statement
    let mut line_start = true;

    fn matches_at(chars: &[char], pos: usize, needle: &[char]) -> bool {
        let n = chars.len();
        if pos + needle.len() > n { return false; }
        chars[pos..pos + needle.len()] == *needle
    }
    fn matches_at_ci(chars: &[char], pos: usize, needle: &[char]) -> bool {
        let n = chars.len();
        if pos + needle.len() > n { return false; }
        chars[pos..pos + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    }
    fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
        let mut k = from;
        while k < chars.len() {
            if chars[k] == target { return Some(k); }
            k += 1;
        }
        None
    }
    fn find_two_char(chars: &[char], from: usize, a: char, b: char) -> Option<usize> {
        if chars.is_empty() { return None; }
        let mut k = from;
        while k + 1 < chars.len() {
            if chars[k] == a && chars[k + 1] == b { return Some(k); }
            k += 1;
        }
        None
    }

    let delimiter_kw: Vec<char> = "DELIMITER".chars().collect();

    while i < n {
        let c = chars[i];

        // --- DELIMITER directive: only recognized at the start of a new statement ---
        if line_start
            && matches_at_ci(&chars, i, &delimiter_kw)
            && (i + 9 >= n || chars[i + 9] == ' ' || chars[i + 9] == '\t')
        {
            let mut j = i + 9;
            while j < n && (chars[j] == ' ' || chars[j] == '\t') {
                j += 1;
            }
            let mut k = j;
            while k < n && chars[k] != '\r' && chars[k] != '\n' {
                k += 1;
            }
            // The first word of the line, as the mysql client reads it: "DELIMITER $ -- x" made "$ -- x" the
            // delimiter here, and no statement after it ever ended.
            let new_delim: String = chars[j..k].iter().collect::<String>().split_whitespace().next().unwrap_or("").to_string();
            if !new_delim.is_empty() {
                delimiter = new_delim.chars().collect();
            }
            i = k;
            buf.clear();
            line_start = true;
            continue;
        }

        // --- block comment /* ... */ ---
        if matches_at(&chars, i, &['/', '*']) {
            match find_two_char(&chars, i + 2, '*', '/') {
                Some(end) => { buf.extend_from_slice(&chars[i..end + 2]); i = end + 2; }
                None => { buf.extend_from_slice(&chars[i..]); i = n; }
            }
            continue;
        }

        // --- '--' line comment (MySQL requires whitespace/EOL/EOF right after '--') ---
        if matches_at(&chars, i, &['-', '-'])
            && (i + 2 >= n || chars[i + 2] == ' ' || chars[i + 2] == '\t' || chars[i + 2] == '\r' || chars[i + 2] == '\n')
        {
            match find_char(&chars, i, '\n') {
                Some(end) => { buf.extend_from_slice(&chars[i..end + 1]); i = end + 1; }
                None => { buf.extend_from_slice(&chars[i..]); i = n; }
            }
            continue;
        }

        // --- '#' line comment ---
        if c == '#' {
            match find_char(&chars, i, '\n') {
                Some(end) => { buf.extend_from_slice(&chars[i..end + 1]); i = end + 1; }
                None => { buf.extend_from_slice(&chars[i..]); i = n; }
            }
            continue;
        }

        // --- quoted strings / identifiers: ' " ` ---
        if c == '\'' || c == '"' || c == '`' {
            let quote = c;
            let mut j = i + 1;
            while j < n {
                if chars[j] == '\\' && quote != '`' && j + 1 < n {
                    // backslash escapes the next char (MySQL default sql_mode) - not inside backticks
                    j += 2;
                    continue;
                }
                if chars[j] == quote {
                    if j + 1 < n && chars[j + 1] == quote {
                        // doubled-quote escape ('' or "" or ``)
                        j += 2;
                        continue;
                    }
                    j += 1;
                    break;
                }
                j += 1;
            }
            buf.extend_from_slice(&chars[i..j]);
            i = j;
            line_start = false;
            continue;
        }

        // --- delimiter match ---
        if i + delimiter.len() <= n && chars[i..i + delimiter.len()] == delimiter[..] {
            let stmt: String = buf.iter().collect::<String>().trim().to_string();
            if !stmt.is_empty() {
                statements.push(stmt);
            }
            buf.clear();
            i += delimiter.len();
            line_start = true;
            continue;
        }

        buf.push(c);
        if !c.is_whitespace() {
            line_start = false;
        }
        i += 1;
    }

    let stmt: String = buf.iter().collect::<String>().trim().to_string();
    if !stmt.is_empty() {
        statements.push(stmt);
    }

    statements
}

// Replaces the previous mysql.exe shell-out for DDL/Apply operations (table designer, stored
// procedure/function/trigger create-or-replace, compare-databases apply, pending grid-edit
// apply) with the native driver, using split_sql_statements above for the one thing that
// actually required the CLI: DELIMITER handling. Runs statements sequentially and stops at the
// first failure (matching the CLI's default, non---force behavior), reporting which statement
// number failed and a preview of it - clearer than the CLI's own error output, which had no
// per-statement context since the whole script was piped to it as one stdin stream.
#[tauri::command]
async fn script(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let raw = req["sql"].as_str().unwrap_or("").to_string();
        if ro_mode(&req) && !sql_is_readonly(&raw) {
            return Ok(json!({"ok":false,"error":"Read-only mode: statement blocked."}));
        }
        let mut c = db_for(&req)?;
        let db = req["db"].as_str().unwrap_or("");
        if !db.is_empty() {
            c.query_drop(format!("USE {}", sql_id(db))).map_err(db_err)?;
        }
        let statements = split_sql_statements(&raw);
        let total = statements.len();
        let continue_on_error = req["continueOnError"].as_bool().unwrap_or(false);

        // Applying staged grid edits sends several statements that only make sense together -
        // if the third fails, the first two must not stay. Without this the batch ran on
        // autocommit and a failure left the table half-updated, which is the one outcome a
        // "pending changes" model exists to prevent.
        let transactional = req["transaction"].as_bool().unwrap_or(false) && !continue_on_error;

        if !continue_on_error {
            // Default: stop at the first failure. Callers that pass transaction:true also get
            // everything rolled back; the others keep the original autocommit behaviour.
            // In a tab's own transaction the batch is a savepoint in it instead: a COMMIT here would
            // commit everything the tab had not, and a ROLLBACK would throw it away.
            let in_session = c.in_session();
            if transactional { c.query_drop(if in_session { "SAVEPOINT nobs_batch" } else { "START TRANSACTION" }).map_err(db_err)?; }
            // Rows the statements changed, for a transaction's log to say what each run did.
            let mut affected: u64 = 0;
            for (idx, stmt) in statements.iter().enumerate() {
                let ran = c.query_drop(stmt);
                if ran.is_ok() { affected += c.affected_rows(); }
                if let Err(e) = ran {
                    let preview: String = stmt.chars().take(120).collect();
                    let suffix = if stmt.chars().count() > 120 { "..." } else { "" };
                    if transactional { let _ = c.query_drop(if in_session { "ROLLBACK TO SAVEPOINT nobs_batch" } else { "ROLLBACK" }); }
                    let e = db_err(e);
                    let note = if !transactional { "" } else if in_session { "\n\nNone of these changes were applied. The tab's transaction is still open." } else { "\n\nNo changes were applied - the batch was rolled back." };
                    return Ok(json!({"ok":false,"affected":if transactional { 0 } else { affected },"error":format!("Statement {} of {} failed: {}\n\n{}{}{}", idx+1, total, e, preview, suffix, note)}));
                }
            }
            if transactional && in_session {
                let _ = c.query_drop("RELEASE SAVEPOINT nobs_batch");
            } else if transactional {
                if let Err(e) = c.query_drop("COMMIT") {
                    let _ = c.query_drop("ROLLBACK");
                    return Ok(json!({"ok":false,"error":commit_failure_message(&db_err(e))}));
                }
            }
            return Ok(json!({"ok":true,"affected":affected}));
        }

        // Continue-on-error mode: run every statement regardless of earlier failures, and
        // report a full breakdown - the whole point of turning this on is seeing everything
        // that needs fixing in one pass, not stopping at (and hiding everything past) the first.
        let mut succeeded = 0usize;
        let mut failures: Vec<Value> = Vec::new();
        for (idx, stmt) in statements.iter().enumerate() {
            match c.query_drop(stmt) {
                Ok(_) => { succeeded += 1; }
                Err(e) => {
                    let preview: String = stmt.chars().take(120).collect();
                    let suffix = if stmt.chars().count() > 120 { "..." } else { "" };
                    failures.push(json!({"index":idx+1,"preview":format!("{}{}", preview, suffix),"error":e.to_string()}));
                }
            }
        }
        Ok(json!({"ok":failures.is_empty(),"total":total,"succeeded":succeeded,"failures":failures}))
    }).await.map_err(|e| e.to_string())?
}

// Runs a script on one connection and returns every result set it produces - a procedure's
// SELECTs, or several SELECTs in a row - which the plain script run discards. At most maxRows rows
// (default 1000) are kept per result; rowCount counts them all. Stops at the first error and
// returns it with the results produced before it.
#[tauri::command]
async fn script_results(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let raw = req["sql"].as_str().unwrap_or("").to_string();
        if ro_mode(&req) && !sql_is_readonly(&raw) {
            return Ok(json!({"ok":false,"error":"Read-only mode: statement blocked."}));
        }
        let max_rows = req["maxRows"].as_u64().unwrap_or(1000).max(1) as usize;
        let mut c = db_for(&req)?;
        if let Some(db) = req["db"].as_str().filter(|d| !d.is_empty()) {
            c.query_drop(format!("USE {}", sql_id(db))).map_err(db_err)?;
        }
        let request_id = req["requestId"].as_str().filter(|s| !s.is_empty()).map(String::from);
        if let Some(rid) = &request_id {
            if let Ok((_c, rows)) = run_select(&mut c, "SELECT CONNECTION_ID()") {
                if let Some(cid) = rows.first().and_then(|r| r.first()).cloned().flatten().and_then(|s| s.parse::<u64>().ok()) {
                    running_queries().lock().unwrap().insert(rid.clone(), (cid, req["conn"].clone()));
                }
            }
        }
        let statements = split_sql_statements(&raw);
        let total = statements.len();
        let mut results: Vec<Value> = Vec::new();
        let mut error: Option<String> = None;
        'statements: for (idx, stmt) in statements.iter().enumerate() {
            let preview: String = stmt.chars().take(120).collect();
            let mut qr = match c.query_iter(stmt) {
                Ok(r) => r,
                Err(e) => { error = Some(format!("Statement {} of {} failed: {}

{}", idx + 1, total, db_err(e), preview)); break },
            };
            while let Some(set) = qr.iter() {
                let (cols, bin, bit): (Vec<String>, Vec<bool>, Vec<bool>) = {
                    let cs = set.columns();
                    let sl: &[Column] = cs.as_ref();
                    (sl.iter().map(|c| c.name_str().to_string()).collect(), sl.iter().map(is_binaryish).collect(), sl.iter().map(is_bit_col).collect())
                };
                if cols.is_empty() { continue; }   // the OK packet of a statement without rows
                let mut rows: Vec<Vec<Option<String>>> = Vec::new();
                let mut count = 0usize;
                for r in set {
                    match r {
                        Ok(row) => { count += 1; if rows.len() < max_rows { rows.push(decode_row(&row, &bin)); } }
                        Err(e) => { error = Some(format!("Statement {} of {} failed: {}

{}", idx + 1, total, db_err(e), preview)); break 'statements; }
                    }
                }
                results.push(json!({"statement": idx + 1, "sql": preview, "columns": cols, "binaryCols": bin, "bitCols": bit,
                                    "rows": rows, "rowCount": count, "truncated": count > max_rows}));
            }
        }
        let cancelled = request_id.as_deref().map(|rid| { running_queries().lock().unwrap().remove(rid); take_query_cancelled(rid) }).unwrap_or(false);
        if cancelled { return Ok(json!({"ok":false,"cancelled":true,"error":"Query cancelled.","results":results,"total":total})); }
        Ok(json!({"ok": error.is_none(), "error": error, "results": results, "total": total}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn import(app: tauri::AppHandle, req: Value) -> R {
    if ro_flag(&req) {
        return Ok(json!({"ok":false,"error":"Read-only mode: statement blocked."}));
    }
    let mbin = match resolve_tool_for(&app, "mysql", &["mysql", "mariadb"], "MYSQL_BIN", &req["conn"]) { Ok(b) => b, Err(e) => return Ok(json!({"ok":false,"error":e})) };
    import_run(req, mbin).await
}

// ---------- restoring a dump into a database of your choosing ----------
// A dump made per database - the export dialog's default - opens with its own
//
//   /*!40000 DROP DATABASE IF EXISTS `shop`*/;
//   CREATE DATABASE /*!32312 IF NOT EXISTS*/ `shop` ...;
//   USE `shop`;
//
// and the import dialog's "Target database" was only handed to the client as its default
// database, which the file's own USE overrides on line three. So "import shop.sql into shop_copy"
// dropped and rebuilt `shop` itself and left `shop_copy` empty - and when the file then failed
// partway (MySQL rejecting a generated column's value, measured on 8.0.46), `shop` was left with
// the tables up to that point and nothing after them.
//
// When a target is chosen, those statements now name the target instead. Only whole statements
// of those three kinds at the start of a line are touched, and only their database identifier;
// data rows never start a line with them, because mysqldump escapes newlines inside values.
// A file that names more than one database is refused rather than squashed into one.

// The span and unescaped name of the database identifier in a USE / CREATE DATABASE /
// DROP DATABASE line, or None for any other line.
fn dump_db_ident(line: &[u8]) -> Option<(usize, usize, String)> {
    let n = line.len();
    let mut i = 0;
    let ws = |i: &mut usize| { while *i < n && (line[*i] == b' ' || line[*i] == b'\t') { *i += 1; } };
    let kw = |i: &mut usize, word: &str| -> bool {
        let w = word.as_bytes();
        if *i + w.len() <= n && line[*i..*i + w.len()].eq_ignore_ascii_case(w)
            && (*i + w.len() == n || !(line[*i + w.len()].is_ascii_alphanumeric() || line[*i + w.len()] == b'_')) {
            *i += w.len(); true
        } else { false }
    };
    let skip_comment = |i: &mut usize| {
        if *i + 3 <= n && &line[*i..*i + 3] == b"/*!" {
            if let Some(end) = line[*i..].windows(2).position(|w| w == b"*/") { *i += end + 2; }
        }
    };
    ws(&mut i);
    // mysqldump wraps DROP DATABASE in a version comment: /*!40000 DROP DATABASE ... */
    if i + 3 <= n && &line[i..i + 3] == b"/*!" {
        let mut j = i + 3;
        while j < n && line[j].is_ascii_digit() { j += 1; }
        let mut k = j; ws(&mut k);
        if kw(&mut k.clone(), "DROP") { i = k; } else { return None; }
    }
    if kw(&mut i, "USE") {
        ws(&mut i);
    } else if kw(&mut i, "CREATE") {
        ws(&mut i);
        if !(kw(&mut i, "DATABASE") || kw(&mut i, "SCHEMA")) { return None; }
        ws(&mut i); skip_comment(&mut i); ws(&mut i);
        if kw(&mut i, "IF") { ws(&mut i); if !kw(&mut i, "NOT") { return None; } ws(&mut i); if !kw(&mut i, "EXISTS") { return None; } ws(&mut i); }
    } else if kw(&mut i, "DROP") {
        ws(&mut i);
        if !(kw(&mut i, "DATABASE") || kw(&mut i, "SCHEMA")) { return None; }
        ws(&mut i);
        if kw(&mut i, "IF") { ws(&mut i); if !kw(&mut i, "EXISTS") { return None; } ws(&mut i); }
    } else {
        return None;
    }
    if i >= n { return None; }
    let start = i;
    if line[i] == b'`' {
        let mut name = Vec::new();
        i += 1;
        loop {
            if i >= n { return None; }
            if line[i] == b'`' {
                if i + 1 < n && line[i + 1] == b'`' { name.push(b'`'); i += 2; continue; }
                i += 1; break;
            }
            name.push(line[i]); i += 1;
        }
        Some((start, i, String::from_utf8_lossy(&name).to_string()))
    } else {
        while i < n && (line[i].is_ascii_alphanumeric() || line[i] == b'_' || line[i] == b'$') { i += 1; }
        if i == start { return None; }
        Some((start, i, String::from_utf8_lossy(&line[start..i]).to_string()))
    }
}

// Every database a dump file refers to, in first-seen order.
fn dump_db_names(path: &str) -> std::io::Result<Vec<String>> {
    use std::io::BufRead;
    let mut r = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    let mut seen: Vec<String> = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if r.read_until(b'\n', &mut line)? == 0 { break; }
        if let Some((_, _, name)) = dump_db_ident(&line) {
            if !seen.contains(&name) { seen.push(name); }
        }
    }
    Ok(seen)
}

// One line, with the database identifier renamed if it is `from` - and, on a line that is not row
// data, every name qualified with it (`from`.`t`) too: mysqldump writes a view's tables that way,
// and a routine or trigger that names its own database keeps it. Renaming only the CREATE DATABASE
// and USE lines left those pointing at the old database. Rows are never touched.
fn dump_rewrite_line(line: &[u8], from: &str, to: &str) -> Vec<u8> {
    let first = match dump_db_ident(line) {
        Some((s, e, name)) if name == from => {
            let mut out = Vec::with_capacity(line.len() + to.len());
            out.extend_from_slice(&line[..s]);
            out.extend_from_slice(format!("`{}`", to.replace('`', "``")).as_bytes());
            out.extend_from_slice(&line[e..]);
            out
        }
        _ => line.to_vec(),
    };
    let t = first.iter().position(|b| !b.is_ascii_whitespace()).map(|i| &first[i..]).unwrap_or(&[]);
    if t.starts_with(b"INSERT ") || t.starts_with(b"REPLACE ") { return first; }
    let needle = format!("`{}`.", from.replace('`', "``")).into_bytes();
    if !first.windows(needle.len()).any(|w| w == needle.as_slice()) { return first; }
    let repl = format!("`{}`.", to.replace('`', "``")).into_bytes();
    let mut out = Vec::with_capacity(first.len() + 16);
    let mut i = 0;
    while i < first.len() {
        if first[i..].starts_with(&needle) { out.extend_from_slice(&repl); i += needle.len(); } else { out.push(first[i]); i += 1; }
    }
    out
}

#[cfg(test)]
mod dump_rename_tests {
    use super::dump_rewrite_line;
    #[test]
    fn a_renamed_restore_takes_views_and_routines_along_and_leaves_rows_alone() {
        let view = dump_rewrite_line(b"/*!50001 VIEW `v` AS select `old`.`t`.`id` AS `id` from `old`.`t` */;\n", "old", "new");
        let view = String::from_utf8(view).unwrap();
        assert!(view.contains("`new`.`t`.`id`") && view.contains("from `new`.`t`") && !view.contains("`old`"), "{view}");
        let row = b"INSERT INTO `t` VALUES (1,'`old`.`t` in a value');\n";
        assert_eq!(dump_rewrite_line(row, "old", "new"), row.to_vec(), "a row's value was changed");
        let usel = String::from_utf8(dump_rewrite_line(b"USE `old`;\n", "old", "new")).unwrap();
        assert_eq!(usel, "USE `new`;\n");
    }
}

// What an import should do with a file, given the chosen target.
enum DumpPlan { AsIs, Rename(String), Refuse(String) }
fn dump_plan(names: &[String], target: &str) -> DumpPlan {
    if target.is_empty() || names.is_empty() || (names.len() == 1 && names[0] == target) { return DumpPlan::AsIs; }
    if names.len() == 1 { return DumpPlan::Rename(names[0].clone()); }
    DumpPlan::Refuse(format!(
        "this file contains {} databases ({}), so it cannot be restored into the single target '{}'. \
Clear \"Target database\" to restore each under its own name.",
        names.len(), names.join(", "), target))
}

// The body, split out so a test can drive it without a tauri::AppHandle - resolving the mysql
// path is the only thing the handle provided.
async fn import_run(req: Value, mbin: String) -> R {
    tokio::task::spawn_blocking(move || {
        let jid = req["jobId"].as_str().unwrap_or("").to_string();
        let job = job_start(&jid);
        let _guard = JobGuard(jid);
        let guard = tls_guard_for(&req["conn"], &mbin)?;
        let (_f, cnf) = cnf_file_with(&req["conn"], &mbin, guard.as_deref())?;
        let files: Vec<String> = req["files"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        let target = req["targetDb"].as_str().unwrap_or("").to_string();
        let mut log = Vec::new();
        let mut cancelled = false;
        let mut errors_skipped = 0usize;
        if !target.is_empty() && req["createDb"].as_bool().unwrap_or(false) {
            let _ = Command::new(&mbin).arg(format!("--defaults-extra-file={}", cnf))
                .arg("-e").arg(format!("CREATE DATABASE IF NOT EXISTS {}", sql_id(&target))).output();
            log.push(format!("Ensured database {}", target));
        }
        // A per-table export folder holds each view in a file of its own, and files go in by name:
        // "active_customers" ran before "customers" and failed with 1146, leaving the view out of the
        // restore. Views go after everything else; the order within each is kept.
        let mut files = files;
        files.sort_by_key(|f| dump_file_is_view(f));
        for f in files {
            if job_is_cancelled(&job) { log.push("CANCELLED (remaining files skipped)".into()); cancelled = true; break; }
            if !std::path::Path::new(&f).exists() { log.push(format!("SKIP (missing): {}", f)); continue; }
            // Warnings are asked for: a dump sets a non-strict sql_mode, so a value too long for its
            // column, or out of range, is cut and the import carried on - logged as a plain OK.
            // --binary-mode, always: a dump file is data, and without it mysql.exe carries out the
            // client commands in it - MySQL's runs "system <anything>" and "tee <file>" (measured with
            // 8.0 and 8.4), which a file someone sends you can hold. In binary mode both clients refuse
            // every client command but DELIMITER and \C in piped input; a raw NUL goes through too.
            let mut args = vec![format!("--defaults-extra-file={}", cnf), "--binary-mode".into(), "--show-warnings".into()];
            if req["force"].as_bool().unwrap_or(false) { args.push("--force".into()); }
            // This replaces the options file's init-command, so it repeats its SET NAMES (cnf_file).
            if req["fkOff"].as_bool().unwrap_or(false) {
                let first = if client_is_mariadb(&mbin) { guard.as_ref().map(|g| format!("{g}; ")).unwrap_or_default() } else { "SET NAMES utf8mb4; ".to_string() };
                args.push(format!("--init-command={}SET FOREIGN_KEY_CHECKS=0; SET UNIQUE_CHECKS=0", first));
            }
            // Export lets you raise mysqldump's --max-allowed-packet (needed for extended-insert
            // with large rows/BLOBs), but the mysql client re-importing that exact file has its
            // own, separate default (16M) - without a matching bump here, re-importing a dump
            // exported with a larger packet size fails with "MySQL server has gone away".
            if let Some(mp) = req["maxpacket"].as_str() { if !mp.is_empty() { args.push(format!("--max-allowed-packet={}", mp)); } }
            // As --database=, so a name cannot be read as another option.
            if !target.is_empty() { args.push(format!("--database={}", target)); }
            let short = || std::path::Path::new(&f).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| f.clone());
            let names = match dump_db_names(&f) { Ok(n) => n, Err(e) => { log.push(format!("FAILED {} : cannot read the file: {}", short(), e)); continue; } };
            let mut cmd = Command::new(&mbin);
            cmd.args(&args).stdout(Stdio::piped());
            let out = match dump_plan(&names, &target) {
                DumpPlan::Refuse(why) => { log.push(format!("SKIPPED {} : {}", short(), why)); continue; }
                // MariaDB's mariadb-dump (11.x and later) opens with a line neither MySQL's client
                // nor an older MariaDB one (10.2) knows - "/*M!999999\- enable the sandbox mode */" -
                // and restoring such a dump failed at line 1 with "Unknown command '\-'". Every
                // client is given the file without it: the line only asks the client to refuse
                // shell commands, and --binary-mode (above) refuses every client command already.
                DumpPlan::AsIs if dump_starts_with_sandbox_line(&f) => {
                    let path = f.clone();
                    let feed: StdinFeed = Box::new(move |mut stdin| {
                        use std::io::{BufRead, Write};
                        let Ok(file) = std::fs::File::open(&path) else { return };
                        let mut r = std::io::BufReader::with_capacity(1 << 20, file);
                        let mut first = Vec::new();
                        if r.read_until(b'\n', &mut first).is_err() { return; }
                        let mut w = std::io::BufWriter::with_capacity(1 << 20, &mut stdin);
                        let _ = std::io::copy(&mut r, &mut w);
                        let _ = w.flush();
                    });
                    run_job_child_fed(job.as_ref(), &mut cmd, Some(feed))
                }
                DumpPlan::AsIs => {
                    let file = std::fs::File::open(&f).map_err(|e| e.to_string())?;
                    cmd.stdin(Stdio::from(file));
                    run_job_child(job.as_ref(), &mut cmd)
                }
                DumpPlan::Rename(from) => {
                    log.push(format!("{} holds database '{}' - restoring it into '{}' instead", short(), from, target));
                    let (path, to) = (f.clone(), target.clone());
                    let mut skip_first = dump_starts_with_sandbox_line(&f);
                    let feed: StdinFeed = Box::new(move |mut stdin| {
                        use std::io::{BufRead, Write};
                        let Ok(file) = std::fs::File::open(&path) else { return };
                        let mut r = std::io::BufReader::with_capacity(1 << 20, file);
                        let mut w = std::io::BufWriter::with_capacity(1 << 20, &mut stdin);
                        let mut line = Vec::new();
                        loop {
                            line.clear();
                            match r.read_until(b'\n', &mut line) { Ok(0) | Err(_) => break, Ok(_) => {} }
                            if skip_first { skip_first = false; continue; }
                            if w.write_all(&dump_rewrite_line(&line, &from, &to)).is_err() { return; }
                        }
                        let _ = w.flush();
                    });
                    run_job_child_fed(job.as_ref(), &mut cmd, Some(feed))
                }
            };
            match out {
                Ok(o) if o.status.success() => {
                    // "Continue on error" passes --force, and mysql then exits 0 even when every
                    // statement failed, reporting what went wrong on stderr instead. Taking the
                    // exit code at face value turned a completely failed import into a clean list
                    // of OK lines - the worst possible outcome for a restore, since it looks like
                    // it worked. Report what the tool actually said.
                    let err = String::from_utf8_lossy(&o.stderr);
                    let errs: Vec<&str> = err.lines().map(|l| l.trim()).filter(|l| l.contains("ERROR")).collect();
                    let warns = import_warnings(&String::from_utf8_lossy(&o.stdout));
                    if errs.is_empty() && warns.is_empty() {
                        log.push(format!("OK  {}", short()));
                    } else if errs.is_empty() {
                        log.push(format!("OK with {} warning(s)  {} : {}{}", warns.len(), short(), warns[0],
                            if warns.len() > 1 { format!(" (+{} more)", warns.len() - 1) } else { String::new() }));
                    } else {
                        log.push(format!("OK with {} error(s) SKIPPED  {} : {}{}",
                            errs.len(), short(), errs[0],
                            if errs.len() > 1 { format!(" (+{} more)", errs.len() - 1) } else { String::new() }));
                        errors_skipped += errs.len();
                    }
                }
                // A killed child reports failure, but "FAILED file : ..." would read as a broken
                // import rather than the cancel the user just asked for.
                Ok(_) if job_is_cancelled(&job) => { log.push(format!("CANCELLED {}", short())); cancelled = true; break; }
                Ok(o) => {
                    let e = friendly_dump_err(&first_err(&String::from_utf8_lossy(&o.stderr)));
                    // A per-table dump carries no CREATE DATABASE or USE, so it has nowhere to go
                    // unless a target is chosen. "No database selected" is accurate and useless.
                    let hint = if e.contains("1046") || e.contains("No database selected") {
                        "\n  This file has no CREATE DATABASE/USE of its own - set \"Target database\" in the Import dialog."
                    } else { "" };
                    log.push(format!("FAILED {} : {}{}", short(), e, hint));
                }
                Err(e) => log.push(format!("FAILED {} : {}", f, e)),
            }
        }
        Ok(json!({"ok":true,"cancelled":cancelled,"errorsSkipped":errors_skipped,"log":log}))
    }).await.map_err(|e| e.to_string())?
}

// Tables in the given databases that have a generated column - but only when the server is
// MySQL; MariaDB's own tool understands MariaDB's generated columns. None means the check could
// not be made (no connection), in which case the export goes ahead as before.
fn mysql_generated_tables(connj: &Value, dbs: &[String], excl: &std::collections::HashSet<String>) -> Option<Vec<String>> {
    let mut c = build_conn(connj).ok()?;
    let ver: String = c.query_first("SELECT VERSION()").ok().flatten().unwrap_or_default();
    if ver.to_lowercase().contains("mariadb") || dbs.is_empty() { return Some(Vec::new()); }
    let list = dbs.iter().map(|d| sql_str_lit(d)).collect::<Vec<_>>().join(",");
    let (_c, rows) = run_select(&mut c, &format!(
        "SELECT DISTINCT TABLE_SCHEMA, TABLE_NAME FROM information_schema.COLUMNS          WHERE TABLE_SCHEMA IN ({}) AND GENERATION_EXPRESSION IS NOT NULL AND GENERATION_EXPRESSION <> ''          ORDER BY TABLE_SCHEMA, TABLE_NAME", list)).ok()?;
    Some(rows.iter().filter_map(|r| {
        let key = format!("{}.{}", r.first().cloned().flatten()?, r.get(1).cloned().flatten()?);
        if excl.contains(&key) { None } else { Some(key) }
    }).collect())
}

// Mirrors the PowerShell version's Api-Export exactly: three modes (table = one file per
// table, the default; db = one file per database; single = one combined file), a set of
// excluded "db.table" entries turned into --ignore-table flags (or skipped entirely in table
// mode), an optional timestamp suffix, and the same mysqldump flag set (including tz-utc and
// max-allowed-packet). Routines/events are database-level, so table mode writes them to one
// extra "<db>.routines_events.sql" file per database, same as the PowerShell backend.
#[tauri::command]
async fn export(app: tauri::AppHandle, req: Value) -> R {
    let dbin = match resolve_tool_for(&app, "mysqldump", &["mysqldump", "mariadb-dump"], "MYSQLDUMP_BIN", &req["conn"]) { Ok(b) => b, Err(e) => return Ok(json!({"ok":false,"error":e})) };
    export_run(req, dbin).await
}

// mysqldump's --databases mode can only exclude tables via a repeated --ignore-table flag per
// table - no ignore-list file, no wildcard. A schema with hundreds of tables where the user
// only wants (or only excludes) a handful can blow straight through Windows' ~32K-character
// CreateProcess command-line limit, failing with the unhelpful "os error 206: The filename or
// extension is too long" despite a perfectly ordinary filename. Picks whichever side of the
// include/exclude split is smaller for database `d`: a short exclude list stays --ignore-table
// (as before); a short include list switches to naming those tables positionally instead
// (mysqldump db table1 table2 ... dumps only the named tables - no --databases needed, but this
// only works for a single database at a time). Returns (ignore_table_args, positional_tables) -
// exactly one of the two is non-empty whenever `d` has any exclusions at all.
// The database and table names go after "--", so a name that starts with "-" is a name and not
// another option (a database called "--result-file=C:/x" would otherwise say where to write).
// Everything else has to come before, --result-file included.
fn push_names(a: &mut Vec<String>, result_file: &str, names: impl IntoIterator<Item = String>) {
    a.push(format!("--result-file={}", result_file));
    a.push("--".into());
    a.extend(names);
}

fn table_filter_args(d: &str, excl: &std::collections::HashSet<String>, conn_req: &Value) -> Result<(Vec<String>, Vec<String>), String> {
    let prefix = format!("{}.", d);
    let this_excl: Vec<&str> = excl.iter().filter(|k| k.starts_with(&prefix)).map(|k| k.as_str()).collect();
    if this_excl.is_empty() { return Ok((vec![], vec![])); }
    // A normal handful of exclusions is cheap as --ignore-table either way - only worth the
    // extra information_schema round-trip once the exclude list itself is already sizeable.
    if this_excl.len() < 40 {
        return Ok((this_excl.iter().map(|k| format!("--ignore-table={}", k)).collect(), vec![]));
    }
    let mut conn = build_conn(conn_req)?;
    let sql = format!("SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA={} ORDER BY TABLE_NAME", sql_lit(d));
    let (_cols, rows) = run_select(&mut conn, &sql)?;
    let excl_names: std::collections::HashSet<&str> = this_excl.iter().map(|k| k.split_once('.').map(|x| x.1).unwrap_or("")).collect();
    let included: Vec<String> = rows.iter().filter_map(|r| r.first().cloned().flatten())
        .filter(|t| !excl_names.contains(t.as_str())).collect();
    if this_excl.len() <= included.len() {
        Ok((this_excl.iter().map(|k| format!("--ignore-table={}", k)).collect(), vec![]))
    } else {
        Ok((vec![], included))
    }
}

// The name in a mysqldump section heading - "-- Table structure for table `t`", "-- Dumping data
// for table `t`" (the only heading a data-only dump has; in a full dump it follows its table's
// structure, and goes to the same file) and the view headings of both dump tools - or None for
// any other line. Data never looks like this: every
// data line is a statement, and a line break inside a value is written as \n.
fn dump_section_name(line: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(trim_eol(line)).ok()?;
    let rest = ["-- Table structure for table ", "-- Dumping data for table ", "-- Temporary view structure for view ",
                "-- Temporary table structure for view ", "-- Final view structure for view "]
        .iter().find_map(|p| s.strip_prefix(p))?;
    Some(rest.strip_prefix('`')?.strip_suffix('`')?.replace("``", "`"))
}
fn trim_eol(line: &[u8]) -> &[u8] {
    let mut l = line;
    while let [rest @ .., b'\n' | b'\r'] = l { l = rest; }
    l
}

// Splits a whole-database dump into one file per table or view, each with the dump's own opening
// and closing lines, so each restores on its own - the files the per-table export writes.
//
// That export used to run mysqldump once per table. --single-transaction makes one run
// consistent, not several, so with writes going on the files came from different moments - an
// order in one, its lines missing from the next. One dump of the whole database is one snapshot;
// splitting it keeps that. file_for gives each name its file (called once per name). Returns
// (name, file) in dump order. The dump is streamed, never held in memory.
fn split_dump_by_table(src: &std::path::Path, file_for: &mut dyn FnMut(&str) -> String) -> Result<Vec<(String, String)>, String> {
    use std::io::{BufRead, Read, Seek, Write};
    let open = || std::fs::File::open(src).map_err(|e| format!("{}: {}", src.display(), e));
    // Pass 1: where the sections start and where the closing lines begin.
    let mut r = std::io::BufReader::new(open()?);
    let (mut off, mut prev_off, mut prev_dashes) = (0u64, 0u64, false);
    let (mut first, mut last_section, mut tz, mut mode): (Option<u64>, u64, Option<u64>, Option<u64>) = (None, 0, None, None);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = r.read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
        if n == 0 { break; }
        if dump_section_name(&line).is_some() {
            first.get_or_insert(if prev_dashes { prev_off } else { off });
            last_section = off;
        }
        let t = trim_eol(&line);
        if t == b"/*!40103 SET TIME_ZONE=@OLD_TIME_ZONE */;" { tz = Some(off); }
        if t == b"/*!40101 SET SQL_MODE=@OLD_SQL_MODE */;" { mode = Some(off); }
        prev_dashes = t == b"--";
        prev_off = off;
        off += n as u64;
    }
    let total = off;
    let Some(first) = first else { return Ok(Vec::new()) };
    // The closing lines restore what the opening ones set; they start with the time zone (when
    // --tz-utc set one) or the SQL mode. A view's own closing lines look alike but come earlier.
    let closing = match (tz, mode) {
        (Some(z), Some(m)) if z > last_section && z < m => z,
        (_, Some(m)) if m > last_section => m,
        _ => total,
    };
    let mut f = open()?;
    let mut header = vec![0u8; first as usize];
    f.read_exact(&mut header).map_err(|e| e.to_string())?;
    let mut footer = Vec::new();
    f.seek(std::io::SeekFrom::Start(closing)).map_err(|e| e.to_string())?;
    f.read_to_end(&mut footer).map_err(|e| e.to_string())?;

    // Pass 2: each section to its name's file. A "--" line belongs to the section it heads.
    f.seek(std::io::SeekFrom::Start(first)).map_err(|e| e.to_string())?;
    let mut r = std::io::BufReader::new(f);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut cur: Option<std::io::BufWriter<std::fs::File>> = None;
    let mut pending: Option<Vec<u8>> = None;
    let mut off = first;
    let write = |w: &mut Option<std::io::BufWriter<std::fs::File>>, b: &[u8]| -> Result<(), String> {
        match w { Some(w) => w.write_all(b).map_err(|e| e.to_string()), None => Ok(()) }
    };
    while off < closing {
        line.clear();
        let n = r.read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
        if n == 0 { break; }
        off += n as u64;
        if let Some(name) = dump_section_name(&line) {
            if let Some(mut w) = cur.take() { w.flush().map_err(|e| e.to_string())?; }
            let path = match out.iter().find(|(n2, _)| *n2 == name) {
                Some((_, p)) => p.clone(),
                None => {
                    let p = file_for(&name);
                    std::fs::write(&p, &header).map_err(|e| format!("{}: {}", p, e))?;
                    out.push((name.clone(), p.clone()));
                    p
                }
            };
            let fh = std::fs::OpenOptions::new().append(true).open(&path).map_err(|e| format!("{}: {}", path, e))?;
            cur = Some(std::io::BufWriter::new(fh));
            if let Some(p) = pending.take() { write(&mut cur, &p)?; }
            write(&mut cur, &line)?;
            continue;
        }
        if let Some(p) = pending.take() { write(&mut cur, &p)?; }
        if trim_eol(&line) == b"--" { pending = Some(line.clone()); } else { write(&mut cur, &line)?; }
    }
    if let Some(p) = pending.take() { write(&mut cur, &p)?; }
    if let Some(mut w) = cur.take() { w.flush().map_err(|e| e.to_string())?; }
    for (_, p) in &out {
        let mut fh = std::fs::OpenOptions::new().append(true).open(p).map_err(|e| format!("{}: {}", p, e))?;
        fh.write_all(&footer).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

// The body, split out so it can be driven from a test without a tauri::AppHandle - resolving
// the mysqldump path is the only thing the handle was needed for.
async fn export_run(req: Value, dbin: String) -> R {
    EXPORT_CANCEL.store(false, Ordering::SeqCst);
    tokio::task::spawn_blocking(move || {
        let jid = req["jobId"].as_str().unwrap_or("").to_string();
        let job = job_start(&jid);
        let _guard = JobGuard(jid);
        let guard = tls_guard_for(&req["conn"], &dbin)?;
        let (_f, cnf) = cnf_file_with(&req["conn"], &dbin, guard.as_deref())?;
        // The guard is there for MariaDB's client on "required" - which mariadb-dump cannot run
        // (no init-command). It is pinned to the server's certificate instead, so the dump's own
        // session is encrypted or does not happen (see server_cert_fingerprint).
        let pin = if guard.is_some() {
            match server_cert_fingerprint(&req["conn"]) { Ok(fp) => Some(fp), Err(e) => return Ok(json!({"ok":false,"error":e})) }
        } else { None };
        let dbs: Vec<String> = req["dbs"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        if dbs.is_empty() { return Ok(json!({"ok":false,"error":"No databases selected."})); }
        let folder = req["folder"].as_str().unwrap_or(".").to_string();
        std::fs::create_dir_all(&folder).ok();
        let o = &req["options"];
        let flag = |k: &str| o[k].as_bool().unwrap_or(false);
        let stamp = if req["stamp"].as_bool().unwrap_or(false) {
            format!("_{}", chrono::Local::now().format("%Y%m%d_%H%M%S")) } else { String::new() };
        // mode: "table" (default), "db", or "single"; the older {single:true} flag still works.
        let mode = req["mode"].as_str().map(String::from).unwrap_or_else(|| {
            if req["single"].as_bool().unwrap_or(false) { "single".into() } else { "table".into() }
        });
        let excl: std::collections::HashSet<String> = req["excludes"].as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        let safe_name = |n: &str| -> String { n.chars().map(|c| if c.is_alphanumeric() || c=='_' || c=='.' || c=='-' { c } else { '_' }).collect() };
        let mkfile = |base: &str| format!("{}/{}{}.sql", folder.trim_end_matches(['/', '\\']), safe_name(base), stamp);
        // Only meaningful in "single" mode - db/table mode each produce one file per object, so
        // a single manual name has nowhere to go. A trailing .sql the user typed themselves is
        // stripped so it doesn't end up doubled ("backup.sql" + mkfile's own ".sql" suffix).
        // MariaDB's dump tool does not recognise a MySQL generated column as generated, so it
        // writes a value for it into every INSERT - and MySQL refuses exactly that on restore
        // ("The value specified for generated column ... is not allowed"). MySQL's own mysqldump
        // leaves those columns out. Measured on MySQL 8.0.46: the export reported OK and the file
        // could not be restored. A backup that looks fine and is not is worse than no backup, so
        // refuse up front and say what to change.
        // Structure only writes no INSERTs, so it has nothing to get wrong there.
        if client_is_mariadb(&dbin) && req["options"]["what"].as_str() != Some("structure") {
            if let Some(tables) = mysql_generated_tables(&req["conn"], &dbs, &excl) {
                if !tables.is_empty() {
                    return Ok(json!({"ok":false,"error":format!(
                        "Not exported: {} table(s) on this MySQL server have generated columns ({}). The MariaDB dump tool writes values into those columns, which MySQL refuses when the file is restored - the dump would not restore. In Settings, point mysqldump at MySQL's own mysqldump.exe (for example C:\\Program Files\\MySQL\\MySQL Server 8.0\\bin\\mysqldump.exe), or exclude those tables.",
                        tables.len(), tables.join(", "))}));
                }
            }
        }
        let custom_name = req["filename"].as_str().unwrap_or("").trim().to_string();
        let single_base = if custom_name.is_empty() { "all_selected".to_string() } else {
            custom_name.strip_suffix(".sql").or_else(|| custom_name.strip_suffix(".SQL")).unwrap_or(&custom_name).to_string()
        };

        // Flags shared by every mysqldump call in this run (no database-level flags here -
        // those differ between "table" mode, which dumps table-by-table, and db/single modes).
        let charset = o["charset"].as_str().unwrap_or("utf8mb4");
        let mut common = vec![format!("--defaults-extra-file={}", cnf), format!("--default-character-set={}", charset)];
        if let Some(fp) = &pin { common.push(format!("--ssl-fp={fp}")); }
        // The chosen character set replaces the options file's SET NAMES utf8mb4 (see cnf_file).
        if !client_is_mariadb(&dbin) && !charset.is_empty() && charset.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            common.push(format!("--loose-init-command=SET NAMES {}", charset));
        }
        if flag("singletx") { common.push("--single-transaction".into()); }
        if flag("quick") { common.push("--quick".into()); }
        if flag("hexblob") { common.push("--hex-blob".into()); }
        if flag("triggers") { common.push("--triggers".into()); } else { common.push("--skip-triggers".into()); }
        if flag("diskeys") { common.push("--disable-keys".into()); }
        if flag("notablespaces") { common.push("--no-tablespaces".into()); }
        if flag("colstats") { common.push("--column-statistics=0".into()); }
        if flag("compress") { common.push("--compress".into()); }
        if flag("gtid") { common.push("--set-gtid-purged=OFF".into()); }
        if flag("complete") { common.push("--complete-insert".into()); }
        if flag("extinsert") { common.push("--extended-insert".into()); } else { common.push("--skip-extended-insert".into()); }
        if flag("tzutc") { common.push("--tz-utc".into()); } else { common.push("--skip-tz-utc".into()); }
        // What goes in: both (the default), the CREATE statements alone, or the rows alone. For data
        // only the page also turns off routines, events, triggers and the DROP and CREATE lines.
        match o["what"].as_str() {
            Some("structure") => common.push("--no-data".into()),
            Some("data") => common.push("--no-create-info".into()),
            _ => {}
        }
        if let Some(mp) = o["maxpacket"].as_str() { if !mp.is_empty() { common.push(format!("--max-allowed-packet={}", mp)); } }

        // mysqldump has no flag for this (unlike HeidiSQL's exporter) - DEFINER=`user`@`host`
        // hardcodes whichever MySQL account happened to create each view/trigger/procedure/event
        // into the dump. Restoring on a server where that exact account doesn't exist (a
        // different host, a managed DB service, a teammate's machine, CI) then fails or warns on
        // every one of those objects. Stripping it leaves `SQL SECURITY DEFINER/INVOKER` intact
        // and just falls back to CURRENT_USER at creation time - safe on the same server too.
        let definer_re = if flag("nodefiner") { Some(definer_regex()) } else { None };

        let run = |bin: &str, args: &[String], file: &str| -> Result<(bool, String), String> {
            let mut cmd = Command::new(bin);
            cmd.args(args);
            let out = run_job_child(job.as_ref(), &mut cmd);
            match out {
                Ok(o2) if o2.status.success() => {
                    let mut note = String::new();
                    if let Some(re) = &definer_re {
                        if let Err(e) = strip_definers(file, re) { note = format!(" - DEFINER could not be left out: {}", e); }
                    }
                    let sz = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
                    Ok((true, format!("OK  {} ({:.2} MB){}", file, sz as f64 / 1048576.0, note)))
                }
                Ok(o2) => {
                    // A child killed by Cancel has no stderr to report - run_job_child skips
                    // reading it, because anything holding the pipe open would stall exactly
                    // the cancel it was asked to perform. That produced "FAILED <table> : "
                    // with nothing after the colon. Name the real reason instead, and never
                    // report an empty one.
                    let e = first_err(&String::from_utf8_lossy(&o2.stderr));
                    let kept = set_aside_partial(file);
                    if !e.trim().is_empty() { Ok((false, format!("{}{}", friendly_dump_err(&e), kept))) }
                    else if EXPORT_CANCEL.load(Ordering::SeqCst) || job_is_cancelled(&job) { Ok((false, RUN_CANCELLED.into())) }
                    else { Ok((false, format!("mysqldump exited with {} and no error output{}", o2.status, kept))) }
                }
                Err(e) => { let kept = set_aside_partial(file); Ok((false, format!("{}{}", e, kept))) }
            }
        };

        let mut log: Vec<String> = Vec::new();
        let mut cancelled = false;

        if mode == "single" {
            let file = mkfile(&single_base);
            // Positional per-table filtering only works against a single database at a time, so
            // the command-line-length fix (see table_filter_args) only kicks in when exactly one
            // db is selected - a multi-database single-file export needs --databases to combine
            // them anyway, and heavy exclusions on TOP of that is a rarer combination left as-is.
            let mut positional: Option<Vec<String>> = None;
            let mut listing_failed = false;
            if dbs.len() == 1 {
                match table_filter_args(&dbs[0], &excl, &req["conn"]) {
                    Ok((_, included)) if !included.is_empty() => positional = Some(included),
                    Ok(_) => {}
                    Err(e) => { log.push(format!("FAILED (list tables) {} : {}", dbs[0], e)); listing_failed = true; }
                }
            }
            if !listing_failed {
                let mut a = common.clone();
                if let Some(included) = positional {
                    if flag("routines") { a.push("--routines".into()); }
                    if flag("events") { a.push("--events".into()); }
                    if flag("adddroptb") { a.push("--add-drop-table".into()); } else { a.push("--skip-add-drop-table".into()); }
                    push_names(&mut a, &file, std::iter::once(dbs[0].clone()).chain(included));
                } else {
                    a.push("--databases".into());
                    if flag("routines") { a.push("--routines".into()); }
                    if flag("events") { a.push("--events".into()); }
                    if flag("adddropdb") { a.push("--add-drop-database".into()); }
                    if flag("adddroptb") { a.push("--add-drop-table".into()); } else { a.push("--skip-add-drop-table".into()); }
                    if !flag("createdb") { a.push("--no-create-db".into()); }
                    for k in &excl { a.push(format!("--ignore-table={}", k)); }
                    push_names(&mut a, &file, dbs.iter().cloned());
                }
                match run(&dbin, &a, &file) {
                    Ok((true, msg)) => log.push(msg),
                    Ok((false, err)) => { if err == RUN_CANCELLED { log.push(format!("CANCELLED {}", single_base)); cancelled = true; } else { log.push(format!("FAILED {} : {}", single_base, err)); } }
                    Err(e) => log.push(format!("FAILED {} : {}", single_base, e)),
                }
            }
        } else if mode == "db" {
            // One file per database, and never the same file for two: names that differ only in case,
            // or only in characters a file name cannot hold ("a b" and "a_b"), used to write the same
            // file, the second over the first. Windows' file names ignore case, so the check does too.
            let mut used_db: std::collections::HashSet<String> = std::collections::HashSet::new();
            for d in &dbs {
                if EXPORT_CANCEL.load(Ordering::SeqCst) || job_is_cancelled(&job) { log.push("CANCELLED (remaining databases skipped)".into()); cancelled = true; break; }
                let mut file = mkfile(d);
                let mut n = 2;
                while !used_db.insert(file.to_lowercase()) { file = mkfile(&format!("{}_{}", d, n)); n += 1; }
                let positional = match table_filter_args(d, &excl, &req["conn"]) {
                    Ok((_, included)) if !included.is_empty() => Some(included),
                    Ok(_) => None,
                    Err(e) => { log.push(format!("FAILED (list tables) {} : {}", d, e)); continue; }
                };
                let mut a = common.clone();
                if let Some(included) = positional {
                    if flag("routines") { a.push("--routines".into()); }
                    if flag("events") { a.push("--events".into()); }
                    if flag("adddroptb") { a.push("--add-drop-table".into()); } else { a.push("--skip-add-drop-table".into()); }
                    push_names(&mut a, &file, std::iter::once(d.clone()).chain(included));
                } else {
                    a.push("--databases".into());
                    if flag("routines") { a.push("--routines".into()); }
                    if flag("events") { a.push("--events".into()); }
                    if flag("adddropdb") { a.push("--add-drop-database".into()); }
                    if flag("adddroptb") { a.push("--add-drop-table".into()); } else { a.push("--skip-add-drop-table".into()); }
                    if !flag("createdb") { a.push("--no-create-db".into()); }
                    for k in &excl { if k.starts_with(&format!("{}.", d)) { a.push(format!("--ignore-table={}", k)); } }
                    push_names(&mut a, &file, std::iter::once(d.clone()));
                }
                match run(&dbin, &a, &file) {
                    Ok((true, msg)) => log.push(msg),
                    Ok((false, err)) => { if err == RUN_CANCELLED { log.push(format!("CANCELLED {}", d)); cancelled = true; } else { log.push(format!("FAILED {} : {}", d, err)); } }
                    Err(e) => log.push(format!("FAILED {} : {}", d, e)),
                }
            }
        } else {
            // PER TABLE (default): every table to its own file, like Workbench's Dump Project Folder -
            // cut from one dump of the database, so all of them come from the same moment (see
            // split_dump_by_table).
            'dbloop: for d in &dbs {
                if EXPORT_CANCEL.load(Ordering::SeqCst) || job_is_cancelled(&job) { log.push("CANCELLED (remaining databases skipped)".into()); cancelled = true; break; }
                let mut conn = match build_conn(&req["conn"]) { Ok(c) => c, Err(e) => { log.push(format!("FAILED (connect) {} : {}", d, e)); continue; } };
                // A view has no rows, so data only has nothing of it to split out; its tables only.
                let only_tables = if o["what"].as_str() == Some("data") { " AND TABLE_TYPE IN ('BASE TABLE','SYSTEM VERSIONED')" } else { "" };
                let sql = format!("SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA={}{} ORDER BY TABLE_NAME", sql_lit(d), only_tables);
                let tabs: Vec<String> = match run_select(&mut conn, &sql) {
                    Ok((_cols, rows)) => rows.iter().filter_map(|r| r.first().cloned().flatten()).collect(),
                    Err(e) => { log.push(format!("FAILED (list tables) {} : {}", d, e)); continue; }
                };
                if tabs.is_empty() { log.push(format!("(no tables) {}", d)); }
                let wanted: Vec<&String> = tabs.iter().filter(|t| {
                    let key = format!("{}.{}", d, t);
                    if excl.contains(&key) { log.push(format!("(excluded) {}", key)); false } else { true }
                }).collect();
                if !wanted.is_empty() {
                    if EXPORT_CANCEL.load(Ordering::SeqCst) || job_is_cancelled(&job) { log.push("CANCELLED (remaining tables skipped)".into()); cancelled = true; break 'dbloop; }
                    let whole = format!("{}/.{}{}.whole.sql.tmp", folder.trim_end_matches(['/', '\\']), safe_name(d), stamp);
                    let mut a = common.clone();
                    if flag("adddroptb") { a.push("--add-drop-table".into()); } else { a.push("--skip-add-drop-table".into()); }
                    // The excluded tables are left out; when they are most of the database, the
                    // wanted ones are named instead (see table_filter_args).
                    match table_filter_args(d, &excl, &req["conn"]) {
                        Ok((ignore, included)) => { a.extend(ignore); push_names(&mut a, &whole, std::iter::once(d.clone()).chain(included)); }
                        Err(e) => { log.push(format!("FAILED (list tables) {} : {}", d, e)); continue; }
                    }
                    let dumped = run(&dbin, &a, &whole);
                    match dumped {
                        Ok((true, _)) => {
                            let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
                            // The routines file is written below under "<db>.routines_events"; a
                            // table of that name would otherwise share the file with it, and one of
                            // the two would be lost.
                            if flag("routines") || flag("events") { used.insert(mkfile(&format!("{}.routines_events", d)).to_lowercase()); }
                            let mut file_for = |name: &str| {
                                // Names that differ only in characters a file name cannot hold
                                // ("a b", "a_b") used to write the same file, the later one
                                // replacing the earlier; each now gets its own.
                                let base = format!("{}.{}", d, name);
                                let mut p = mkfile(&base);
                                let mut i = 2;
                                while !used.insert(p.to_lowercase()) { p = mkfile(&format!("{}_{}", base, i)); i += 1; }
                                p
                            };
                            match split_dump_by_table(std::path::Path::new(&whole), &mut file_for) {
                                Ok(files) => {
                                    for t in &wanted {
                                        match files.iter().find(|(n, _)| n == *t) {
                                            Some((_, p)) => {
                                                let sz = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                                                log.push(format!("OK  {} ({:.2} MB)", p, sz as f64 / 1048576.0));
                                            }
                                            None => log.push(format!("FAILED {}.{} : not in the dump", d, t)),
                                        }
                                    }
                                }
                                Err(e) => log.push(format!("FAILED {} : could not split the dump into tables: {}", d, e)),
                            }
                        }
                        Ok((false, err)) => { if err == RUN_CANCELLED { log.push(format!("CANCELLED {}", d)); cancelled = true; } else { log.push(format!("FAILED {} : {}", d, err)); } }
                        Err(e) => log.push(format!("FAILED {} : {}", d, e)),
                    }
                    let _ = std::fs::remove_file(&whole);
                }
                if cancelled { break; }
                if EXPORT_CANCEL.load(Ordering::SeqCst) || job_is_cancelled(&job) { log.push(format!("CANCELLED {} routines/events", d)); cancelled = true; break; }
                if flag("routines") || flag("events") {
                    let file = mkfile(&format!("{}.routines_events", d));
                    let mut a = common.clone();
                    a.push("--no-create-info".into()); a.push("--no-data".into()); a.push("--no-create-db".into()); a.push("--skip-triggers".into());
                    if flag("routines") { a.push("--routines".into()); }
                    if flag("events") { a.push("--events".into()); }
                    push_names(&mut a, &file, std::iter::once(d.clone()));
                    match run(&dbin, &a, &file) {
                        Ok((true, msg)) => log.push(format!("{} (routines/events)", msg)),
                        Ok((false, err)) => { if err == RUN_CANCELLED { log.push(format!("CANCELLED {} routines/events", d)); cancelled = true; } else { log.push(format!("FAILED {} routines/events : {}", d, err)); } }
                        Err(e) => log.push(format!("FAILED {} routines/events : {}", d, e)),
                    }
                }
            }
        }
        // A cancel that arrived after the last step had finished still answers the click.
        if !cancelled && (EXPORT_CANCEL.load(Ordering::SeqCst) || job_is_cancelled(&job)) {
            log.push("CANCELLED (after the last step had finished - the files listed above are complete)".into());
            cancelled = true;
        }
        Ok(json!({"ok":true,"cancelled":cancelled,"log":log}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn importcsv(req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        if ro_flag(&req) {
            return Ok(json!({"ok":false,"error":"Read-only mode: statement blocked."}));
        }
        let file = req["file"].as_str().unwrap_or("").to_string();
        if !std::path::Path::new(&file).exists() { return Ok(json!({"ok":false,"error":"CSV file not found."})); }
        let db = req["db"].as_str().unwrap_or("").to_string();
        let table = req["table"].as_str().unwrap_or("").to_string();
        let mut c = build_conn(&req["conn"])?; backslash_escapes_on(&mut c);
        let colsql = format!("SELECT COLUMN_NAME,DATA_TYPE,EXTRA FROM information_schema.COLUMNS WHERE TABLE_SCHEMA={} AND TABLE_NAME={} ORDER BY ORDINAL_POSITION", sql_str_lit(&db), sql_str_lit(&table));
        let (_c, crows) = run_select(&mut c, &colsql)?;
        // sql_lit()'s "0xDEADBEEF passes through unquoted as a hex literal" rule exists so a
        // genuinely binary/BIT column can be filled from its own hex display - it was never meant
        // to apply to an ordinary text column that merely happens to contain a value that LOOKS
        // like hex ("0xFF", a hash, an ID). Used indiscriminately here, that silently reinterpreted
        // such a CSV cell as raw bytes instead of the literal text, with no error. Only the columns
        // information_schema actually reports as binary/BIT get that treatment; everything else
        // goes through sql_str_lit(), which always quotes.
        const BIN_TYPES: &[&str] = &["binary", "varbinary", "blob", "tinyblob", "mediumblob", "longblob", "bit", "vector"];
        let bin_cols: std::collections::HashSet<String> = crows.iter()
            .filter(|r| {
                let ty = r.get(1).and_then(|v| v.as_deref()).unwrap_or("").to_lowercase();
                BIN_TYPES.contains(&ty.as_str())
            })
            .filter_map(|r| r.first().cloned().flatten()).collect();
        let generated: Vec<String> = crows.iter()
            .filter(|r| is_generated_extra(r.get(2).and_then(|v| v.as_deref()).unwrap_or("")))
            .filter_map(|r| r.first().cloned().flatten()).collect();
        let table_cols: Vec<String> = crows.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect();
        if table_cols.is_empty() { return Ok(json!({"ok":false,"error":"Table not found or has no columns."})); }

        let has_header = req["hasHeader"].as_bool().unwrap_or(true);
        let mut rdr = csv::ReaderBuilder::new().has_headers(has_header).flexible(true)
            .from_path(&file).map_err(|e| format!("CSV open error: {}", e))?;
        let csv_cols: Vec<String> = if has_header {
            rdr.headers().map_err(|e| e.to_string())?.iter().map(String::from).collect()
        } else { table_cols.clone() };
        let null_marker = req["nullValue"].as_str().unwrap_or("\\N").to_string();
        // A header is matched to the table's columns ignoring case, as MySQL does. A column the
        // table does not have used to be skipped without a word - a typo in the header row left
        // that column's data out of every row imported.
        let matched: Vec<String> = match csv_import_columns(&csv_cols, &table_cols) {
            Ok(c) => c,
            Err(e) => return Ok(json!({"ok":false,"error":e})),
        };
        // A generated column cannot be given a value - the server computes it - so the CSV's copy
        // of it (this app's own CSV export includes them) is left out.
        let use_idx: Vec<usize> = (0..matched.len()).filter(|&i| !generated.contains(&matched[i])).collect();
        let skipped: Vec<String> = matched.iter().filter(|c| generated.contains(c)).cloned().collect();
        let use_cols: Vec<String> = use_idx.iter().map(|&i| matched[i].clone()).collect();
        if use_cols.is_empty() { return Ok(json!({"ok":false,"error":"The CSV has no column that can be written."})); }
        let col_list = use_cols.iter().map(|c| sql_id(c)).collect::<Vec<_>>().join(",");
        let obj = format!("{}.{}", sql_id(&db), sql_id(&table));

        // "Truncate table" + a mid-file failure (a bad value, an FK violation, disk full) used to
        // leave the table permanently truncated and only partially reloaded - every batch ran on
        // autocommit with nothing to undo the ones that had already landed. Wrapped in the same
        // START TRANSACTION/COMMIT/ROLLBACK pattern the grid's own edit-apply path uses for
        // exactly this failure mode (see the comment above it, near req["transaction"]).
        //
        // TRUNCATE TABLE itself is DDL - MySQL/MariaDB implicitly commit it the moment it runs,
        // transaction or not, so wrapping THAT in START TRANSACTION would do nothing to protect
        // it. DELETE FROM (no WHERE) does the same job here and, unlike TRUNCATE, is ordinary
        // transactional DML that a ROLLBACK genuinely undoes - the one real cost is that it
        // doesn't reset an AUTO_INCREMENT counter the way TRUNCATE does.
        // All of that holds only for an engine that can roll back. MyISAM and Aria cannot: Replace
        // deleted every row first, and a failure part way left the table holding only the rows
        // written before it, while the message said nothing had been imported. Replace is refused
        // on such a table, and a failed Append says what is already in.
        let (_e, erows) = run_select(&mut c, &format!("SELECT t.ENGINE, e.TRANSACTIONS FROM information_schema.TABLES t LEFT JOIN information_schema.ENGINES e ON e.ENGINE=t.ENGINE WHERE t.TABLE_SCHEMA={} AND t.TABLE_NAME={}", sql_str_lit(&db), sql_str_lit(&table)))?;
        let engine = erows.first().and_then(|r| r.first().cloned().flatten()).unwrap_or_default();
        let transactional = erows.first().and_then(|r| r.get(1).cloned().flatten()).map(|t| t.eq_ignore_ascii_case("YES")).unwrap_or(true);
        if !transactional && req["truncate"].as_bool().unwrap_or(false) {
            return Ok(json!({"ok":false,"error":format!("{}.{} uses the {} engine, which cannot undo a failed import - Replace would delete its rows before knowing whether the new ones go in. Nothing was changed. Empty the table yourself and import with Append, or convert it to InnoDB first.", db, table, engine)}));
        }
        let mut written = 0usize;
        c.query_drop("START TRANSACTION").map_err(|e| e.to_string())?;
        let import_result: Result<usize, String> = (|| {
            if req["truncate"].as_bool().unwrap_or(false) {
                c.query_drop(format!("DELETE FROM {}", obj)).map_err(|e| e.to_string())?;
            }
            // Foreign key and unique checks stay on. They were switched off here, which let a CSV
            // row point at a parent that does not exist - stored without an error.
            let mut n = 0usize; let mut batch: Vec<String> = Vec::new();
            for rec in rdr.records() {
                let rec = rec.map_err(|e| e.to_string())?;
                // A row with more or fewer fields than the header is a broken file (a stray
                // separator, an unquoted line break), not a row with empty values. Missing fields
                // used to become NULL and extra ones were dropped.
                if rec.len() != csv_cols.len() {
                    let line = rec.position().map(|p| p.line()).unwrap_or(0);
                    return Err(format!("Line {} has {} field(s), but the {} has {}. Nothing was imported.",
                        line, rec.len(), if has_header { "header" } else { "table" }, csv_cols.len()));
                }
                let vals: Vec<String> = use_idx.iter().enumerate().map(|(ci, &i)| {
                    // The marker decides what an empty cell means. With one set (the default, \N)
                    // the file states NULL explicitly, so an empty cell is an empty string and a
                    // round trip keeps both. Clearing the marker restores the older reading, where a
                    // blank means NULL - which is what a spreadsheet usually intends.
                    match rec.get(i) {
                        None => "NULL".to_string(),
                        Some(s) if !null_marker.is_empty() && s == null_marker => "NULL".to_string(),
                        Some("") if null_marker.is_empty() => "NULL".to_string(),
                        // sql_lit()'s hex-literal passthrough only applies to a column information_schema
                        // actually reports as binary/BIT - anything else always gets a real quoted string,
                        // even if the cell's text happens to look like hex (see bin_cols above).
                        // An empty binary value is exported as the bare "0x" - its hex display form -
                        // and sql_lit() only reads 0x as hex when a digit follows, so it used to store
                        // the two characters "0x" instead of zero bytes. Measured: X'' came back as 0x3078.
                        Some("0x") | Some("0X") if bin_cols.contains(&use_cols[ci]) => "X''".to_string(),
                        Some(s) if bin_cols.contains(&use_cols[ci]) => sql_lit(s),
                        Some(s) => sql_str_lit(s),
                    }
                }).collect();
                batch.push(format!("({})", vals.join(","))); n += 1;
                if batch.len() >= 500 {
                    c.query_drop(format!("INSERT INTO {} ({}) VALUES {}", obj, col_list, batch.join(","))).map_err(db_err)?;
                    written += batch.len();
                    batch.clear();
                }
            }
            if !batch.is_empty() {
                c.query_drop(format!("INSERT INTO {} ({}) VALUES {}", obj, col_list, batch.join(","))).map_err(db_err)?;
                written += batch.len();
            }
            Ok(n)
        })();
        let n = match import_result {
            Ok(n) => n,
            Err(e) => {
                let _ = c.query_drop("ROLLBACK");
                let after = if transactional || written == 0 { "No rows were imported - the batch was rolled back.".to_string() }
                    else { format!("{} row(s) were already written and are still in the table: the {} engine cannot undo them.", written, engine) };
                return Ok(json!({"ok":false,"error":format!("{}\n\n{}", e, after)}));
            }
        };
        if let Err(e) = c.query_drop("COMMIT") {
            let _ = c.query_drop("ROLLBACK");
            return Ok(json!({"ok":false,"error":format!("Could not commit: {}\n\nNo rows were imported.", e.to_string())}));
        }
        let note = if skipped.is_empty() { String::new() } else { format!("; generated, so computed by the server: {}", skipped.join(", ")) };
        Ok(json!({"ok":true,"message":format!("Imported {} row(s) into {}.{} (columns: {}{})", n, db, table, use_cols.join(", "), note)}))
    }).await.map_err(|e| e.to_string())?
}

// The table column each CSV column goes into, in CSV order, matched ignoring case. Any CSV column
// the table does not have is an error naming it, as is a column given twice.
fn csv_import_columns(csv_cols: &[String], table_cols: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for h in csv_cols {
        match table_cols.iter().find(|t| t.eq_ignore_ascii_case(h.trim())) {
            Some(t) if out.iter().any(|o| o.eq_ignore_ascii_case(t)) => return Err(format!("The CSV has the column {} twice. Nothing was imported.", t)),
            Some(t) => out.push(t.clone()),
            None => unknown.push(if h.is_empty() { "(empty)".to_string() } else { h.clone() }),
        }
    }
    if !unknown.is_empty() {
        return Err(format!("The table has no column named {}. Nothing was imported - rename the CSV column(s) or remove them.", unknown.join(", ")));
    }
    Ok(out)
}

// ---------- connection profiles (config file + OS keychain) ----------
fn conn_path() -> std::path::PathBuf {
    let mut p = dirs::config_dir().unwrap_or(std::env::temp_dir());
    p.push("NOBSSQL-Desktop"); std::fs::create_dir_all(&p).ok(); p.push("connections.json"); p
}
fn load_profiles() -> Vec<Value> {
    std::fs::read_to_string(conn_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
fn save_profiles(v: &Vec<Value>) { let _ = std::fs::write(conn_path(), serde_json::to_string_pretty(v).unwrap_or_default()); }

// A saved connection's non-secret fields. The password itself never lives in this JSON file -
// it goes through the OS keychain via `keyring`, same idea as DPAPI in the PowerShell version.
#[tauri::command]
async fn conn_list(_req: Value) -> R {
    // hasPassword: an actual keyring lookup per connection (not a field-presence check like the
    // PowerShell version, since here the password itself isn't stored alongside the connection's
    // other metadata at all - it lives in the OS keychain, keyed by connection name, matching
    // exactly how conn_get already retrieves it). Only checks whether an entry exists; never
    // logs or uses the returned secret itself for anything beyond that.
    let items: Vec<Value> = load_profiles().into_iter().map(|c| {
        let name = c["name"].as_str().unwrap_or("");
        let has_password = keyring::Entry::new("NOBSSQL-Desktop", name).ok().and_then(|e| e.get_password().ok()).is_some();
        json!({
            "name": c["name"], "host": c["host"], "port": c["port"], "user": c["user"], "ssl": c["ssl"],
            "sslCa": c["sslCa"], "clearPw": c["clearPw"].as_bool().unwrap_or(false), "sshHost": c["sshHost"], "sshPort": c["sshPort"], "sshUser": c["sshUser"], "sshKey": c["sshKey"],
            "accent": c["accent"], "env": c["env"], "readonly": c["readonly"].as_bool().unwrap_or(false),
            "primary": c["primary"].as_bool().unwrap_or(false), "hasPassword": has_password
        })
    }).collect();
    Ok(json!({"ok":true,"items":items}))
}
#[tauri::command]
async fn conn_get(req: Value) -> R {
    let name = req["name"].as_str().unwrap_or("").to_string();
    let c = load_profiles().into_iter().find(|c| c["name"].as_str() == Some(&name));
    match c {
        Some(c) => {
            // Whether there are saved passwords, not the passwords (see resolve_saved).
            Ok(json!({"ok":true,"conn":{"host":c["host"],"port":c["port"],"user":c["user"],"ssl":c["ssl"],"sslCa":c["sslCa"],"clearPw":c["clearPw"].as_bool().unwrap_or(false),
                "hasPassword":!db_pw_get(&name).is_empty(),
                "sshHost":c["sshHost"],"sshPort":c["sshPort"],"sshUser":c["sshUser"],"sshKey":c["sshKey"],"hasSshPassword":!ssh_pw_get(&name).is_empty()},
                "accent":c["accent"],"env":c["env"],"readonly":c["readonly"].as_bool().unwrap_or(false)}))
        }
        None => Ok(json!({"ok":false})),
    }
}
#[tauri::command]
async fn conn_save(req: Value) -> R {
    let name = req["name"].as_str().unwrap_or("").to_string();
    if name.is_empty() { return Ok(json!({"ok":false,"error":"name required"})); }
    let conn = &req["conn"];
    // savepw defaults to true (matches the PS backend's "quick save preserves the existing password" behavior);
    // savepw:false explicitly removes any saved password for this connection.
    let save_pw = req.get("savepw").and_then(|v| v.as_bool()).unwrap_or(true);
    // A password left empty keeps the one saved - under this name, or under keepFrom for a copy or
    // a rename - but only while the address is the one it was saved for: a saved connection pointed
    // at another host, port or user has its password typed again, never carried over to it.
    let keep_from = req["keepFrom"].as_str().filter(|s| !s.is_empty()).unwrap_or(&name).to_string();
    let same_place = profile_named(&keep_from).map(|p| endpoint_key(&p) == endpoint_key(conn)).unwrap_or(false);
    let pick = |typed: &str, saved: String| -> String {
        if !save_pw { String::new() } else if !typed.is_empty() { typed.to_string() } else if same_place { saved } else { String::new() }
    };
    let db = pick(conn["password"].as_str().unwrap_or(""), db_pw_get(&keep_from));
    let ssh = pick(conn["sshPassword"].as_str().unwrap_or(""), ssh_pw_get(&keep_from));
    db_pw_set(&name, &db);
    ssh_pw_set(&name, &ssh);
    let before = load_profiles();
    let was_primary = before.iter().find(|c| c["name"].as_str() == Some(name.as_str()))
        .map(|c| c["primary"].as_bool().unwrap_or(false)).unwrap_or(false);
    let mut list: Vec<Value> = before.into_iter().filter(|c| c["name"].as_str() != Some(name.as_str())).collect();
    list.push(json!({
        "name": name, "host": conn["host"], "port": conn["port"], "user": conn["user"], "ssl": conn["ssl"],
        "sslCa": conn["sslCa"], "clearPw": conn["clearPw"].as_bool().unwrap_or(false),
        "sshHost": conn["sshHost"], "sshPort": conn["sshPort"], "sshUser": conn["sshUser"], "sshKey": conn["sshKey"],
        "accent": req.get("accent").cloned().unwrap_or(Value::Null),
        "env": req.get("env").cloned().unwrap_or(Value::Null),
        "readonly": req.get("readonly").and_then(|v| v.as_bool()).unwrap_or(false),
        "primary": was_primary
    }));
    save_profiles(&list);
    Ok(json!({"ok":true}))
}
#[tauri::command]
async fn conn_delete(req: Value) -> R {
    let name = req["name"].as_str().unwrap_or("").to_string();
    if let Ok(e) = keyring::Entry::new("NOBSSQL-Desktop", &name) { let _ = e.delete_credential(); }
    ssh_pw_set(&name, "");
    let list: Vec<Value> = load_profiles().into_iter().filter(|c| c["name"].as_str() != Some(&name)).collect();
    save_profiles(&list);
    Ok(json!({"ok":true}))
}
// Mirrors the PowerShell version's Api-ConnSetPrimary: "primary" is a plain field on each saved
// connection object (not a separate config key) - setting one clears it from all the others.
#[tauri::command]
async fn conn_primary(req: Value) -> R {
    let name = req["name"].as_str().unwrap_or("").to_string();
    let list: Vec<Value> = load_profiles().into_iter().map(|mut c| {
        let is_this_one = !name.is_empty() && c["name"].as_str() == Some(name.as_str());
        if let Some(obj) = c.as_object_mut() {
            obj.insert("primary".into(), json!(is_this_one));
        }
        c
    }).collect();
    save_profiles(&list);
    Ok(json!({"ok":true}))
}
// Wipes ALL saved connections (used by "Clear all app data"), including each one's keychain
// password entry so nothing orphaned is left behind in the OS credential store.
#[tauri::command]
async fn conn_clear(_req: Value) -> R {
    for c in load_profiles() {
        if let Some(name) = c["name"].as_str() {
            if let Ok(e) = keyring::Entry::new("NOBSSQL-Desktop", name) { let _ = e.delete_credential(); }
            ssh_pw_set(name, "");
        }
    }
    save_profiles(&Vec::new());
    Ok(json!({"ok":true}))
}

// ---- Compare Databases: structure diff + one-way sync (source -> target) ----
// Mirrors the PowerShell version's Resolve-SavedConn/Get-SchemaColumns/Compare-TableSets exactly,
// so both apps generate the same diffs and the same SQL for the same two databases.

#[derive(Clone)]
struct ColumnDef { name: String, ctype: String, nullable: String, default: Option<String>, extra: String,
                   charset: Option<String>, collation: Option<String>, comment: String, generation: String }

// Looks up a saved connection by name and returns (connection JSON usable with build_conn, readonly).
fn resolve_saved_conn(name: &str) -> Result<(Value, bool), String> {
    let c = load_profiles().into_iter().find(|c| c["name"].as_str() == Some(name))
        .ok_or_else(|| "Connection not found.".to_string())?;
    let pass = keyring::Entry::new("NOBSSQL-Desktop", name).ok().and_then(|e| e.get_password().ok()).unwrap_or_default();
    // Saved connections are what Compare uses, and Compare reads TIMESTAMP values as text on one
    // server and writes that text on the other. Each server reads it in its own session time zone,
    // so between servers in different zones every copied TIMESTAMP moved by the difference (Zurich
    // to UTC: 12:00 UTC arrived as 14:00 UTC), and equal values showed as different. UTC on both
    // sides makes the text mean the same instant everywhere.
    let connj = json!({"host":c["host"],"port":c["port"],"user":c["user"],"ssl":c["ssl"],"sslCa":c["sslCa"],"clearPw":c["clearPw"].as_bool().unwrap_or(false),"password":pass,"utc":true,
        "sshHost":c["sshHost"],"sshPort":c["sshPort"],"sshUser":c["sshUser"],"sshKey":c["sshKey"],"sshPassword":ssh_pw_get(name)});
    Ok((connj, c["readonly"].as_bool().unwrap_or(false)))
}

fn get_schema_columns(conn: &mut Conn, db: &str) -> Result<std::collections::BTreeMap<String, Vec<ColumnDef>>, String> {
    let sql = format!(
        "SELECT TABLE_NAME,COLUMN_NAME,COLUMN_TYPE,IS_NULLABLE,COLUMN_DEFAULT,EXTRA,CHARACTER_SET_NAME,COLLATION_NAME,COLUMN_COMMENT,GENERATION_EXPRESSION FROM information_schema.COLUMNS WHERE TABLE_SCHEMA={} ORDER BY TABLE_NAME,ORDINAL_POSITION",
        sql_lit(db)
    );
    let (_cols, rows) = run_select(conn, &sql)?;
    let mut map: std::collections::BTreeMap<String, Vec<ColumnDef>> = std::collections::BTreeMap::new();
    for r in rows {
        let t = r.first().cloned().flatten().unwrap_or_default();
        let cd = ColumnDef {
            name: r.get(1).cloned().flatten().unwrap_or_default(),
            ctype: r.get(2).cloned().flatten().unwrap_or_default(),
            nullable: r.get(3).cloned().flatten().unwrap_or_default(),
            default: r.get(4).cloned().flatten(),
            extra: r.get(5).cloned().flatten().unwrap_or_default(),
            charset: r.get(6).cloned().flatten(),
            collation: r.get(7).cloned().flatten(),
            comment: r.get(8).cloned().flatten().unwrap_or_default(),
            generation: r.get(9).cloned().flatten().unwrap_or_default(),
        };
        map.entry(t).or_default().push(cd);
    }
    Ok(map)
}

// A column's EXTRA as far as it describes the column: AUTO_INCREMENT, ON UPDATE CURRENT_TIMESTAMP,
// INVISIBLE, VIRTUAL/STORED GENERATED. It was not compared at all, so a column that had lost its
// ON UPDATE or AUTO_INCREMENT on the target showed as the same. The two servers spell it
// differently - MySQL adds DEFAULT_GENERATED and writes CURRENT_TIMESTAMP, MariaDB
// current_timestamp() - and that difference alone is not one.
fn extra_norm(extra: &str) -> String {
    let e = extra.to_lowercase().replace("default_generated", "").replace("current_timestamp()", "current_timestamp");
    e.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn get_create_table_sql(conn: &mut Conn, db: &str, table: &str) -> Option<String> {
    let sql = format!("SHOW CREATE TABLE {}.{}", sql_id(db), sql_id(table));
    run_select(conn, &sql).ok().and_then(|(_c, rows)| rows.into_iter().next()).and_then(|r| r.get(1).cloned().flatten())
}

// Best-effort DEFAULT clause: numeric and keyword defaults (CURRENT_TIMESTAMP, NULL) are emitted
// bare; everything else is quoted as a string literal. Always double-check via Preview SQL.
fn col_default_clause(default: &Option<String>) -> String {
    match default {
        None => String::new(),
        Some(d) => {
            let is_numeric = regex::Regex::new(r"^-?[0-9]+(\.[0-9]+)?$").unwrap().is_match(d);
            let is_keyword = regex::Regex::new(r"^(CURRENT_TIMESTAMP(\(\d*\))?|NULL)$").unwrap().is_match(d);
            if is_numeric || is_keyword { format!(" DEFAULT {}", d) } else { format!(" DEFAULT {}", sql_lit(d)) }
        }
    }
}
// Each column's definition as the server writes it in SHOW CREATE TABLE, keyed by lower-cased name.
// Schema sync used to rebuild a column from its type, NULL, default and EXTRA alone, so MODIFY
// COLUMN turned a latin1_bin column into the table's default character set and collation (case-
// insensitive utf8mb4), dropped its comment, and could not write a generated column at all.
fn column_definitions(create: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for line in create.lines() {
        let l = line.trim();
        if !l.starts_with('`') { continue; }
        let mut name = String::new();
        let mut chars = l[1..].chars().peekable();
        let mut closed = false;
        while let Some(ch) = chars.next() {
            if ch == '`' {
                if chars.peek() == Some(&'`') { chars.next(); name.push('`'); continue; }
                closed = true; break;
            }
            name.push(ch);
        }
        if closed { out.insert(name.to_lowercase(), l.trim_end_matches(',').to_string()); }
    }
    out
}
// The definition to write for col: the server's own line, with the character set and collation
// spelled out when the line leaves them to the table default - which on the target may differ.
fn col_definition(col: &ColumnDef, defs: &std::collections::HashMap<String, String>) -> String {
    let Some(def) = defs.get(&col.name.to_lowercase()) else { return col_def_line(col) };
    let upper = def.to_uppercase();
    if let (Some(cs), Some(co)) = (&col.charset, &col.collation) {
        if !upper.contains(" CHARACTER SET ") && !upper.contains(" COLLATE ") {
            let head = format!("{} {}", sql_id(&col.name), col.ctype);
            if def.len() >= head.len() && def[..head.len()].eq_ignore_ascii_case(&head) {
                return format!("{} CHARACTER SET {} COLLATE {}{}", head, cs, co, &def[head.len()..]);
            }
        }
    }
    def.clone()
}
fn col_def_line(col: &ColumnDef) -> String {
    let null_part = if col.nullable == "YES" { "NULL" } else { "NOT NULL" };
    let extra_part = if col.extra.is_empty() { String::new() } else { format!(" {}", col.extra) };
    format!("{} {} {}{}{}", sql_id(&col.name), col.ctype, null_part, col_default_clause(&col.default), extra_part)
}

struct SqlStmt { stmt: String, checked: bool, kind: &'static str }
struct TableDiff { name: String, status: &'static str, sql: Vec<SqlStmt> }

// The table's keys and constraints as the server writes them in SHOW CREATE TABLE, by what they
// are and their name: ("pk", "PRIMARY"), ("index", name), ("fk", name), ("check", name), each with
// its line.
fn key_lines(create: &str) -> Vec<(&'static str, String, String)> {
    let mut out = Vec::new();
    for raw in create.lines() {
        let l = raw.trim().trim_end_matches(',').to_string();
        let name_after = |prefix_len: usize| -> Option<String> {
            let rest = &l[prefix_len..];
            let rest = rest.trim_start();
            if !rest.starts_with('`') { return None; }
            let mut name = String::new(); let mut it = rest[1..].chars().peekable();
            while let Some(c) = it.next() {
                if c == '`' { if it.peek() == Some(&'`') { it.next(); name.push('`'); continue; } return Some(name); }
                name.push(c);
            }
            None
        };
        let up = l.to_uppercase();
        if up.starts_with("PRIMARY KEY") { out.push(("pk", "PRIMARY".to_string(), l.clone())); continue; }
        for pre in ["UNIQUE KEY", "FULLTEXT KEY", "SPATIAL KEY", "KEY"] {
            if up.starts_with(pre) { if let Some(n) = name_after(pre.len()) { out.push(("index", n, l.clone())); } break; }
        }
        if up.starts_with("CONSTRAINT") {
            if let Some(n) = name_after("CONSTRAINT".len()) {
                if up.contains(" FOREIGN KEY ") { out.push(("fk", n, l.clone())); } else if up.contains(" CHECK ") || up.contains(" CHECK(") { out.push(("check", n, l.clone())); }
            }
        }
    }
    out
}

// The statements that bring the target's keys and constraints to the source's: what is missing is
// added, what differs is dropped and added again, and what only the target has is offered as a
// drop, not ticked - as with columns. Foreign keys go last among the additions, after the indexes
// they may need, and are dropped first.
fn key_diffs(t: &str, src_create: &str, tgt_create: &str) -> Vec<SqlStmt> {
    let src = key_lines(src_create); let tgt = key_lines(tgt_create);
    let find = |v: &Vec<(&'static str, String, String)>, k: &str, n: &str| v.iter().find(|(kk, nn, _)| *kk == k && nn == n).map(|(_, _, l)| l.clone());
    let drop_clause = |k: &str, n: &str| match k { "pk" => "DROP PRIMARY KEY".to_string(), "index" => format!("DROP INDEX {}", sql_id(n)), "fk" => format!("DROP FOREIGN KEY {}", sql_id(n)), _ => format!("DROP CONSTRAINT {}", sql_id(n)) };
    let (mut first, mut adds, mut fks, mut drops) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (k, n, l) in &src {
        match find(&tgt, k, n) {
            None => { let s = SqlStmt { stmt: format!("ALTER TABLE {} ADD {};", sql_id(t), l), checked: true, kind: "add_key" }; if *k == "fk" { fks.push(s) } else { adds.push(s) } }
            Some(tl) if &tl != l => {
                if *k == "fk" || *k == "check" {
                    first.push(SqlStmt { stmt: format!("ALTER TABLE {} {};", sql_id(t), drop_clause(k, n)), checked: true, kind: "modify_key" });
                    let s = SqlStmt { stmt: format!("ALTER TABLE {} ADD {};", sql_id(t), l), checked: true, kind: "modify_key" };
                    if *k == "fk" { fks.push(s) } else { adds.push(s) }
                } else {
                    adds.push(SqlStmt { stmt: format!("ALTER TABLE {} {}, ADD {};", sql_id(t), drop_clause(k, n), l), checked: true, kind: "modify_key" });
                }
            }
            _ => {}
        }
    }
    let mut tgt_only: Vec<&(&'static str, String, String)> = tgt.iter().filter(|(k, n, _)| find(&src, k, n).is_none()).collect();
    tgt_only.sort_by_key(|(k, _, _)| if *k == "fk" { 0 } else { 1 });
    for (k, n, _) in tgt_only { drops.push(SqlStmt { stmt: format!("ALTER TABLE {} {};", sql_id(t), drop_clause(k, n)), checked: false, kind: "drop_key" }); }
    first.into_iter().chain(adds).chain(fks).chain(drops).collect()
}

fn compare_table_sets(
    src_conn: &mut Conn, src_db: &str,
    tgt_conn: &mut Conn, tgt_db: &str,
    src_cols: &std::collections::BTreeMap<String, Vec<ColumnDef>>,
    tgt_cols: &std::collections::BTreeMap<String, Vec<ColumnDef>>,
    request_id: Option<&str>,
) -> (Vec<TableDiff>, bool) {
    // Column names ignore case on every server, and table names do wherever the target's
    // lower_case_table_names is 1 or 2 (Windows and macOS by default). Matched exactly, a source
    // `Users` against a target `users` came out as a table missing from the target - whose CREATE
    // then failed as already there - and a column `Name` against `name` as a column to add, which
    // was refused as a duplicate. What is sent to the target names its tables as the target does.
    let fold = tgt_conn.query_first::<u32, _>("SELECT @@lower_case_table_names").ok().flatten().unwrap_or(0) != 0;
    let key = |n: &str| if fold { n.to_lowercase() } else { n.to_string() };
    let src_by: std::collections::BTreeMap<String, &String> = src_cols.keys().map(|n| (key(n), n)).collect();
    let tgt_by: std::collections::BTreeMap<String, &String> = tgt_cols.keys().map(|n| (key(n), n)).collect();
    let mut names: Vec<&String> = src_by.keys().chain(tgt_by.keys()).collect();
    names.sort(); names.dedup();
    let mut out = Vec::new();
    let mut cancelled = false;
    for k in names {
        if let Some(rid) = request_id { if is_compare_cancelled(rid) { cancelled = true; break; } }
        let (t, tt) = match (src_by.get(k).copied(), tgt_by.get(k).copied()) {
            (Some(s), None) => {
                let ddl = get_create_table_sql(&mut *src_conn, src_db, s).unwrap_or_default();
                out.push(TableDiff { name: s.clone(), status: "missing_target",
                    sql: vec![SqlStmt { stmt: ddl, checked: true, kind: "create_table" }] });
                continue;
            }
            (None, Some(tn)) => {
                out.push(TableDiff { name: tn.clone(), status: "missing_source",
                    sql: vec![SqlStmt { stmt: format!("DROP TABLE {};", sql_id(tn)), checked: false, kind: "drop_table" }] });
                continue;
            }
            (Some(s), Some(tn)) => (s, tn),
            (None, None) => continue,
        };
        let s_cols = &src_cols[t]; let t_cols = &tgt_cols[tt];
        let mut defs: Option<std::collections::HashMap<String, String>> = None;
        let mut def_of = |c: &ColumnDef, conn: &mut Conn| -> String {
            let d = defs.get_or_insert_with(|| get_create_table_sql(conn, src_db, t).map(|s| column_definitions(&s)).unwrap_or_default());
            col_definition(c, d)
        };
        let t_by_name: std::collections::HashMap<String, &ColumnDef> = t_cols.iter().map(|c| (c.name.to_lowercase(), c)).collect();
        let s_by_name: std::collections::HashMap<String, &ColumnDef> = s_cols.iter().map(|c| (c.name.to_lowercase(), c)).collect();
        let mut diffs = Vec::new();
        for (ci, c) in s_cols.iter().enumerate() {
            match t_by_name.get(&c.name.to_lowercase()) {
                // In the place it has in the source, after the same column: an added column used to go
                // at the end, so the two tables' column order - and every SELECT * and INSERT without
                // column names - differed from then on.
                None => {
                    let place = if ci == 0 { " FIRST".to_string() } else { format!(" AFTER {}", sql_id(&s_cols[ci - 1].name)) };
                    diffs.push(SqlStmt { stmt: format!("ALTER TABLE {} ADD COLUMN {}{};", sql_id(tt), def_of(c, &mut *src_conn), place), checked: true, kind: "add_column" })
                }
                Some(tc) => {
                    if c.ctype != tc.ctype || c.nullable != tc.nullable || c.default != tc.default
                        || c.collation != tc.collation || c.comment != tc.comment || c.generation != tc.generation
                        || extra_norm(&c.extra) != extra_norm(&tc.extra) {
                        diffs.push(SqlStmt { stmt: format!("ALTER TABLE {} MODIFY COLUMN {};", sql_id(tt), def_of(c, &mut *src_conn)), checked: true, kind: "modify_column" });
                    }
                }
            }
        }
        for c in t_cols {
            if !s_by_name.contains_key(&c.name.to_lowercase()) {
                diffs.push(SqlStmt { stmt: format!("ALTER TABLE {} DROP COLUMN {};", sql_id(tt), sql_id(&c.name)), checked: false, kind: "drop_column" });
            }
        }
        // Keys, foreign keys and CHECK constraints were not compared at all: a table whose only
        // difference was a missing index or foreign key showed as the same.
        if let (Some(sc), Some(tc)) = (get_create_table_sql(&mut *src_conn, src_db, t), get_create_table_sql(&mut *tgt_conn, tgt_db, tt)) {
            diffs.extend(key_diffs(tt, &sc, &tc));
        }
        if diffs.is_empty() { out.push(TableDiff { name: t.clone(), status: "same", sql: Vec::new() }); }
        else { out.push(TableDiff { name: t.clone(), status: "diff", sql: diffs }); }
    }
    (out, cancelled)
}

// Reusable primary-key column lookup (plain Vec, not a JSON response) - used by the row-level
// compare below. Returns an empty Vec if the table has no primary key.
fn get_table_pk_cols(conn: &mut Conn, db: &str, table: &str) -> Result<Vec<String>, String> {
    let sql = format!(
        "SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA={} AND TABLE_NAME={} AND CONSTRAINT_NAME='PRIMARY' ORDER BY ORDINAL_POSITION",
        sql_lit(db), sql_lit(table)
    );
    let (_cols, rows) = run_select(conn, &sql)?;
    Ok(rows.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect())
}
fn get_table_fk_cols(conn: &mut Conn, db: &str, table: &str) -> Result<Vec<String>, String> {
    let sql = format!(
        "SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA={} AND TABLE_NAME={} AND REFERENCED_TABLE_NAME IS NOT NULL ORDER BY ORDINAL_POSITION",
        sql_lit(db), sql_lit(table)
    );
    let (_cols, rows) = run_select(conn, &sql)?;
    Ok(rows.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect())
}
// The columns of a table whose values this app shows as 0x.. hex (see is_binaryish): binary
// strings, BIT and the spatial types. Lowercased, since MySQL column names ignore case.
fn binary_column_set(conn: &mut Conn, db: &str, table: &str) -> Result<std::collections::HashSet<String>, String> {
    column_set_of(conn, db, table, &["binary", "varbinary", "tinyblob", "blob", "mediumblob", "longblob", "bit",
        "geometry", "point", "linestring", "polygon", "multipoint", "multilinestring", "multipolygon",
        "geometrycollection", "geomcollection", "vector"])
}
// A FLOAT is read as rounded text (1.1 is stored as 1.10000002384), and that text compared to the
// column matches nothing - so rows keyed by one could not be fetched or updated by their key. Such
// key columns are compared as text instead (see key_col).
fn float_column_set(conn: &mut Conn, db: &str, table: &str) -> Result<std::collections::HashSet<String>, String> {
    column_set_of(conn, db, table, &["float"])
}
fn key_col(col: &str, float: &std::collections::HashSet<String>) -> String {
    if float.contains(&col.to_lowercase()) { format!("CAST({} AS CHAR)", sql_id(col)) } else { sql_id(col) }
}
// The columns of db.table in order, each with whether it is generated. Copies name their columns
// from this rather than using SELECT *: SELECT * leaves out INVISIBLE columns (MySQL 8.0.23+,
// MariaDB 10.3+), so a copy made from it stored NULL in them, and a generated column cannot be
// given a value, so a copy that included one was refused.
fn table_columns(conn: &mut Conn, db: &str, table: &str) -> Result<Vec<(String, bool)>, String> {
    let (_c, rows) = run_select(conn, &format!(
        "SELECT COLUMN_NAME, EXTRA FROM information_schema.COLUMNS WHERE TABLE_SCHEMA={} AND TABLE_NAME={} ORDER BY ORDINAL_POSITION",
        sql_str_lit(db), sql_str_lit(table)))?;
    if rows.is_empty() { return Err(format!("Could not read the columns of {}.{}.", db, table)); }
    Ok(rows.into_iter().map(|r| {
        let name = r.first().cloned().flatten().unwrap_or_default();
        let extra = r.get(1).cloned().flatten().unwrap_or_default().to_uppercase();
        (name, is_generated_extra(&extra))
    }).collect())
}
// EXTRA for a generated column: VIRTUAL/STORED GENERATED on both servers, PERSISTENT GENERATED on
// older MariaDB. MySQL's DEFAULT_GENERATED only marks an expression default.
fn is_generated_extra(extra: &str) -> bool {
    let e = extra.to_uppercase();
    e.contains("VIRTUAL GENERATED") || e.contains("STORED GENERATED") || e.contains("PERSISTENT GENERATED")
}
// The columns a copy of db.table reads: all but the generated ones, invisible ones included - plus
// any generated column in keep (a key column), without which rows could not be told apart.
fn copy_columns(conn: &mut Conn, db: &str, table: &str, keep: &[String]) -> Result<Vec<String>, String> {
    Ok(table_columns(conn, db, table)?.into_iter()
        .filter(|(n, g)| !*g || keep.iter().any(|k| k.eq_ignore_ascii_case(n)))
        .map(|(n, _)| n).collect())
}
fn select_list(cols: &[String]) -> String { cols.iter().map(|c| sql_id(c)).collect::<Vec<_>>().join(",") }
// An INSERT that skips a row whose key already exists, as INSERT IGNORE did - without IGNORE's
// other effect: it turns errors into warnings, so a value too long for its column was cut short and
// an impossible date stored as 0000-00-00, silently, when the file was run.
fn insert_skip_existing(tbl: &str, cols: &[String], values: &str) -> String {
    let first = cols.first().map(|c| sql_id(c)).unwrap_or_default();
    format!("INSERT INTO {} ({}) VALUES {} ON DUPLICATE KEY UPDATE {}={};
", tbl, select_list(cols), values, first, first)
}

// Lower-cased names of the columns of db.table whose DATA_TYPE is one of types.
fn column_set_of(conn: &mut Conn, db: &str, table: &str, types: &[&str]) -> Result<std::collections::HashSet<String>, String> {
    let (_c, rows) = run_select(conn, &format!(
        "SELECT COLUMN_NAME, DATA_TYPE FROM information_schema.COLUMNS WHERE TABLE_SCHEMA={} AND TABLE_NAME={}",
        sql_str_lit(db), sql_str_lit(table)))?;
    if rows.is_empty() { return Err(format!("Could not read the column types of {}.{}.", db, table)); }
    Ok(rows.iter().filter(|r| {
        let ty = r.get(1).cloned().flatten().unwrap_or_default().to_lowercase();
        types.contains(&ty.as_str())
    }).filter_map(|r| r.first().cloned().flatten().map(|n| n.to_lowercase())).collect())
}
// sql_val_lit guesses from the value's shape, which is wrong both ways for data being copied: a
// text column holding '0x41' was written as the byte A, and an empty binary value - shown as the
// bare 0x - was written as the two characters 0x. Where the table is known, ask it instead.
fn sql_val_for(v: Option<&str>, binary: bool) -> String {
    match v {
        None => "NULL".to_string(),
        Some("0x") if binary => "X''".to_string(),
        Some(s) if binary && s.len() > 2 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit()) => s.to_string(),
        Some(s) => sql_str_lit(s),
    }
}
fn json_val_for(v: &Value, binary: bool) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::String(s) => sql_val_for(Some(s), binary),
        other => sql_str_lit(other.to_string().trim_matches('"')),
    }
}
// A WHERE clause matching a chunk of primary-key tuples, each value written for its column's type.
fn pk_where(pk_cols: &[String], chunk: &[&Vec<Option<String>>], bin: &std::collections::HashSet<String>, float: &std::collections::HashSet<String>) -> String {
    let is_bin: Vec<bool> = pk_cols.iter().map(|c| bin.contains(&c.to_lowercase())).collect();
    let is_float: Vec<bool> = pk_cols.iter().map(|c| float.contains(&c.to_lowercase())).collect();
    let tuple = |r: &Vec<Option<String>>| r.iter().enumerate()
        .map(|(i, v)| if is_float.get(i).copied().unwrap_or(false) { v.as_deref().map(sql_str_lit).unwrap_or_else(|| "NULL".into()) }
                      else { sql_val_for(v.as_deref(), is_bin.get(i).copied().unwrap_or(false)) }).collect::<Vec<_>>();
    if pk_cols.len() == 1 {
        let vals = chunk.iter().map(|r| tuple(r).into_iter().next().unwrap_or_else(|| "NULL".into())).collect::<Vec<_>>().join(",");
        format!("{} IN ({})", key_col(&pk_cols[0], float), vals)
    } else {
        let pk_list = pk_cols.iter().map(|c| key_col(c, float)).collect::<Vec<_>>().join(",");
        let tuples = chunk.iter().map(|r| format!("({})", tuple(r).join(","))).collect::<Vec<_>>().join(",");
        format!("({}) IN ({})", pk_list, tuples)
    }
}
// Joins a row's cell values with a control character (0x01) that can't appear in normal data,
// to build a single comparable key for both single-column and composite primary keys.
fn row_key(row: &[Option<String>]) -> String {
    row.iter().map(|v| v.clone().unwrap_or_default()).collect::<Vec<_>>().join("\u{1}")
}

// Fetches full row data for a SPECIFIC list of primary-key value combinations, chunked (same
// reasoning as elsewhere: MySQL's max_allowed_packet and general sanity for very large IN-lists).
// Shared by compare_rows (its first page) and compare_rows_fetch_by_pk (loading a later page the
// client already knows about, without re-scanning the whole table again).
fn get_rows_by_pk(conn: &mut Conn, db: &str, table: &str, pk_cols: &[String], pk_values: &[Vec<Option<String>>]) -> Result<Table, String> {
    if pk_values.is_empty() { return Ok((Vec::new(), Vec::new())); }
    let bin = binary_column_set(conn, db, table)?;
    let float = float_column_set(conn, db, table)?;
    let copy = select_list(&copy_columns(conn, db, table, pk_cols)?);
    let fetch_chunk = 200;
    let mut full_cols: Vec<String> = Vec::new();
    let mut full_rows: Vec<Vec<Option<String>>> = Vec::new();
    for chunk in pk_values.chunks(fetch_chunk) {
        let refs: Vec<&Vec<Option<String>>> = chunk.iter().collect();
        let where_clause = pk_where(pk_cols, &refs, &bin, &float);
        let (cols, rows) = run_select(conn, &format!("SELECT {} FROM {}.{} WHERE {}", copy, sql_id(db), sql_id(table), where_clause))?;
        if full_cols.is_empty() { full_cols = cols; }
        full_rows.extend(rows);
    }
    Ok((full_cols, full_rows))
}

// Standalone command for loading a LATER page of missing rows the client already knows about
// (from compare_rows' allMissingPks) - a lightweight, bounded fetch that never re-scans the
// whole table, unlike re-running the full comparison.
#[tauri::command]
async fn compare_rows_fetch_by_pk(req: Value) -> R {
    let src_name = req["sourceConnName"].as_str().unwrap_or("").to_string();
    let src_db = req["sourceDb"].as_str().unwrap_or("").to_string();
    let table = req["table"].as_str().unwrap_or("").to_string();
    let pk_cols: Vec<String> = req["pkCols"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let pks: Vec<Vec<Option<String>>> = req["pks"].as_array().map(|a| a.iter().map(|row| {
        row.as_array().map(|r| r.iter().map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
    }).collect()).unwrap_or_default();
    tokio::task::spawn_blocking(move || {
        if pk_cols.is_empty() { return Ok(json!({"ok":false,"error":"Missing primary key columns."})); }
        let (src_connj, _) = resolve_saved_conn(&src_name)?;
        let mut src_conn = build_conn(&src_connj)?;
        let (cols, rows) = get_rows_by_pk(&mut src_conn, &src_db, &table, &pk_cols, &pks)?;
        Ok(json!({"ok":true,"columns":cols,"rows":rows}))
    }).await.map_err(|e| e.to_string())?
}

// Generates a user + grants transfer script for the CURRENT connection, using SHOW CREATE USER
// and SHOW GRANTS FOR rather than hand-building CREATE USER/GRANT text from the grant tables
// directly. This matters: SHOW CREATE USER encodes whatever auth plugin and password hash the
// account actually uses (native password, ed25519, unix_socket, etc.) instead of assuming
// mysql_native_password, and SHOW GRANTS FOR already includes column/routine grants, WITH GRANT
// OPTION, and (on MariaDB) role grants - all of which a plain SELECT against the grant tables
// would silently miss. Works unchanged on MySQL 5.7.6+ and MariaDB 10.2+.
// CREATE USER statements are emitted before any GRANT statements (genuinely grouped that way,
// not just alphabetically) so replaying the result on a target server never grants to a user
// that doesn't exist yet.
// True for MySQL 8.0.17 and later - the first version with print_identified_with_as_hex.
fn mysql_supports_hex_identified(conn: &mut Conn) -> bool {
    let v: String = conn.query_first("SELECT VERSION()").ok().flatten().unwrap_or_default();
    mysql_version_has_hex_identified(&v)
}
fn mysql_version_has_hex_identified(v: &str) -> bool {
    if v.to_lowercase().contains("mariadb") { return false; }
    let n: Vec<u64> = v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).take(3)
        .map(|s| s.parse().unwrap_or(0)).collect();
    n.len() >= 3 && (n[0], n[1], n[2]) >= (8, 0, 17)
}

// SHOW CREATE USER's statement, made to leave an account that is already there alone.
fn create_if_not_exists(stmt: &str) -> String {
    let t = stmt.trim_start();
    if t.len() >= 12 && t[..12].eq_ignore_ascii_case("CREATE USER ") && !t[12..].trim_start().to_ascii_uppercase().starts_with("IF NOT EXISTS") {
        format!("CREATE USER IF NOT EXISTS {}", &t[12..])
    } else { t.to_string() }
}
// The first statement of a transfer script: it stops the script on the other kind of server, before
// anything is changed. Password hashes and role statements are written differently on MySQL and
// MariaDB, and every statement failed there; a scalar subquery of two rows is an error on both, and
// this line - which says why - is what a client shows with it.
fn transfer_guard(maria: bool) -> String {
    let (want, made) = if maria { ("LIKE", "MariaDB") } else { ("NOT LIKE", "MySQL") };
    format!("-- This script only runs on {made}: the next line stops it anywhere else.\nSELECT IF(VERSION() {want} '%MariaDB%', 'ok', (SELECT 'This script was made on {made} and cannot run on this server' UNION SELECT 'stopped')) AS target_check;\n")
}

// Accounts MySQL and MariaDB create for their own use. Listed by name rather than matched as
// "mysql.%": a user is free to create an account called mysql.backup, and that one is theirs.
const SYSTEM_ACCOUNTS: [&str; 4] = ["mysql.sys", "mysql.session", "mysql.infoschema", "mariadb.sys"];

#[tauri::command]
async fn gen_user_transfer(req: Value) -> R {
    let exclude_raw = req["exclude"].as_str().unwrap_or("").to_string();
    let conn_json = req["conn"].clone();
    tokio::task::spawn_blocking(move || {
        let mut excl: Vec<String> = exclude_raw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if excl.is_empty() {
            excl = ["mysql.sys","root","debian-sys-maint","mariadb.sys","healthcheck","mariabackup","galera","replica","PUBLIC"].iter().map(|s| s.to_string()).collect();
        }
        // The server's own internal accounts are never moved, whatever the list says. They exist
        // on every install of that server and are managed by it, so a script carrying them cannot
        // run: on MySQL 8 it opened with CREATE USER `mysql.infoschema` and `mysql.session` -
        // which already exist on the target - and handed SUPER and SYSTEM_USER grants to them.
        // Only mysql.sys was on the default list; the other two came through.
        for sys in SYSTEM_ACCOUNTS {
            if !excl.iter().any(|e| e == sys) { excl.push(sys.to_string()); }
        }
        let in_list = excl.iter().map(|s| sql_str_lit(s)).collect::<Vec<_>>().join(",");
        let mut conn = build_conn(&conn_json)?;
        let version: String = conn.query_first("SELECT VERSION()").ok().flatten().unwrap_or_default();
        let maria = version.to_lowercase().contains("mariadb");
        // A MySQL 8 caching_sha2_password hash carries a salt of arbitrary 7-bit bytes, control
        // characters included, and SHOW CREATE USER prints it raw inside the quoted literal. That
        // is valid SQL, but not text: pasted, saved or shown in a text box, a control character can
        // be dropped or changed, and the account then arrives with a password nobody knows. With
        // print_identified_with_as_hex (8.0.17+) the hash comes out as a plain 0x... literal.
        if mysql_supports_hex_identified(&mut conn) {
            let _ = conn.query_drop("SET SESSION print_identified_with_as_hex = ON");
        }
        // MariaDB keeps its roles in mysql.user as rows with no host and is_role='Y'. They are not
        // accounts: SHOW CREATE USER cannot read them, so they went to the "could not be read" list,
        // and every GRANT of a role and SET DEFAULT ROLE after them failed on the target. They are
        // made with CREATE ROLE here instead, ahead of everything that names them.
        let has_is_role = maria && run_select(&mut conn, "SELECT 1 FROM information_schema.COLUMNS WHERE TABLE_SCHEMA='mysql' AND TABLE_NAME='user' AND COLUMN_NAME='is_role'")
            .map(|(_, r)| !r.is_empty()).unwrap_or(false);
        let not_role = if has_is_role { " AND is_role <> 'Y'" } else { "" };
        let (_cols, mut user_rows) = run_select(&mut conn, &format!("SELECT user, host FROM mysql.user WHERE user NOT IN ({}) AND user <> ''{}", in_list, not_role))?;
        let maria_roles: Vec<String> = if has_is_role {
            run_select(&mut conn, &format!("SELECT user FROM mysql.user WHERE is_role = 'Y' AND user NOT IN ({})", in_list))
                .map(|(_, r)| r.into_iter().filter_map(|x| x.into_iter().next().flatten()).collect()).unwrap_or_default()
        } else { Vec::new() };
        // MySQL 8's roles are accounts, but one can only be granted, or named as a default role,
        // once it exists: a user whose CREATE USER carried DEFAULT ROLE came before its role, failed,
        // and took every grant of that user down with it. Roles are made first.
        if !maria {
            let roles: std::collections::HashSet<(String, String)> = run_select(&mut conn,
                "SELECT FROM_USER, FROM_HOST FROM mysql.role_edges UNION SELECT DEFAULT_ROLE_USER, DEFAULT_ROLE_HOST FROM mysql.default_roles")
                .map(|(_, r)| r.into_iter().map(|x| (x.first().cloned().flatten().unwrap_or_default(), x.get(1).cloned().flatten().unwrap_or_default())).collect())
                .unwrap_or_default();
            user_rows.sort_by_key(|r| !roles.contains(&(r.first().cloned().flatten().unwrap_or_default(), r.get(1).cloned().flatten().unwrap_or_default())));
        }
        if user_rows.is_empty() && maria_roles.is_empty() {
            return Ok(json!({"ok":true,"sql":"-- No accounts matched (everything was excluded, or mysql.user is empty).","userCount":0,"errorCount":0}));
        }
        let mut create_lines: Vec<String> = Vec::new();
        let mut grant_lines: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for role in &maria_roles {
            create_lines.push(format!("CREATE ROLE IF NOT EXISTS {};", sql_id(role)));
            match run_select(&mut conn, &format!("SHOW GRANTS FOR {}", sql_id(role))) {
                Ok((_c, rows)) => for r in rows { if let Some(v) = r.first().cloned().flatten() { grant_lines.push(format!("{};", v)); } },
                Err(e) => errors.push(format!("SHOW GRANTS for role '{}': {}", role, e)),
            }
        }
        for row in &user_rows {
            let u = row.first().cloned().flatten().unwrap_or_default();
            let h = row.get(1).cloned().flatten().unwrap_or_default();
            let uq = sql_str_lit(&u);
            let hq = sql_str_lit(&h);
            match run_select(&mut conn, &format!("SHOW CREATE USER {}@{}", uq, hq)) {
                Ok((_c, rows)) => {
                    if let Some(first) = rows.first().and_then(|r| r.first()).cloned().flatten() {
                        // IF NOT EXISTS, so the script can run again on a server that has some of them.
                        create_lines.push(format!("{};", create_if_not_exists(&first)));
                    } else {
                        errors.push(format!("SHOW CREATE USER for '{}'@'{}': no result returned", u, h));
                    }
                }
                Err(e) => errors.push(format!("SHOW CREATE USER for '{}'@'{}': {}", u, h, e)),
            }
            match run_select(&mut conn, &format!("SHOW GRANTS FOR {}@{}", uq, hq)) {
                Ok((_c, rows)) => {
                    for r in rows {
                        if let Some(v) = r.first().cloned().flatten() { grant_lines.push(format!("{};", v)); }
                    }
                }
                Err(e) => errors.push(format!("SHOW GRANTS for '{}'@'{}': {}", u, h, e)),
            }
        }
        let mut out = String::new();
        out.push_str(&format!("-- Generated user transfer script - {} account(s){} matched (after exclusions)\n", user_rows.len(),
            if maria_roles.is_empty() { String::new() } else { format!(" and {} role(s)", maria_roles.len()) }));
        out.push_str(&format!("-- Made on {}.\n", version));
        out.push_str("-- Run this on the TARGET server, after its databases are in place: a grant on a table or a\n");
        out.push_str("-- routine needs that table or routine to exist. Roles and accounts come first, so the grants\n");
        out.push_str("-- can name them; each is made only if it is not there yet, so the script can run again.\n\n");
        out.push_str(&transfer_guard(maria));
        out.push_str("\n-- ===== CREATE ROLE / CREATE USER =====\n");
        for l in &create_lines { out.push_str(l); out.push('\n'); }
        out.push_str("\n-- ===== GRANTS =====\n");
        for l in &grant_lines { out.push_str(l); out.push('\n'); }
        if !errors.is_empty() {
            out.push_str(&format!("\n-- ===== {} account(s) could not be read (the script above is complete for everyone else) =====\n", errors.len()));
            for e in &errors { out.push_str("-- "); out.push_str(&e.replace(['\n', '\r'], " ")); out.push('\n'); }
        }
        Ok(json!({"ok":true,"sql":out,"userCount":user_rows.len(),"errorCount":errors.len()}))
    }).await.map_err(|e| e.to_string())?
}

// Finds rows present in the source table but missing (by primary key) on the target - INSERT
// only, never UPDATE/DELETE. Row data is fetched from the source and re-inserted with the exact
// same primary key value(s), so ids stay identical between the two databases. Capped at 2000
// rows per comparison to stay interactive; a larger gap should go through Export/Import instead.
#[tauri::command]
async fn compare_rows(req: Value) -> R {
    let src_name = req["sourceConnName"].as_str().unwrap_or("").to_string();
    let tgt_name = req["targetConnName"].as_str().unwrap_or("").to_string();
    let src_db = req["sourceDb"].as_str().unwrap_or("").to_string();
    let tgt_db = req["targetDb"].as_str().unwrap_or("").to_string();
    let table = req["table"].as_str().unwrap_or("").to_string();
    let rid = req["requestId"].as_str().map(String::from);
    tokio::task::spawn_blocking(move || {
        // A single SELECT can't be interrupted mid-flight once the driver call has started (no
        // chunking here, unlike compare_rows_diff below), so this can only bail out BETWEEN the
        // blocking steps - still meaningful for cmpScanRowDiffs' bulk scan, which calls this once
        // per table: it skips the (often equally expensive) target-side query and the display-row
        // fetch entirely once Stop has been clicked, rather than doing that work for nothing.
        let cancelled_now = |rid: &Option<String>| rid.as_deref().map(is_compare_cancelled).unwrap_or(false);
        if cancelled_now(&rid) { return Ok(json!({"ok":true,"pkCols":Vec::<String>::new(),"columns":Vec::<String>::new(),"rows":Vec::<Value>::new(),"missingTotal":0,"truncated":false,"targetReadonly":false,"allMissingPks":Vec::<Value>::new(),"extraTotal":0,"extraPks":Vec::<Value>::new(),"cancelled":true})); }
        let (src_connj, _) = resolve_saved_conn(&src_name)?;
        let (tgt_connj, tgt_ro) = resolve_saved_conn(&tgt_name)?;
        let mut src_conn = build_conn(&src_connj)?;
        let mut tgt_conn = build_conn(&tgt_connj)?;
        // From here on, a Cancel click can KILL QUERY whichever of these two connections is
        // actually blocked on a SELECT right now, instead of only being noticed once that query
        // finishes on its own. _guard drops (and unregisters) on every return path below.
        register_compare_conn(&rid, &mut src_conn, &src_connj);
        register_compare_conn(&rid, &mut tgt_conn, &tgt_connj);
        let _guard = CompareConnGuard(rid.clone());
        let pk = get_table_pk_cols(&mut src_conn, &src_db, &table)?;
        if pk.is_empty() { return Ok(json!({"ok":false,"error":"Table has no primary key - cannot compare rows."})); }
        let _fk = get_table_fk_cols(&mut src_conn, &src_db, &table).unwrap_or_default();
        let pk_list = pk.iter().map(|c| sql_id(c)).collect::<Vec<_>>().join(",");
        let (_c1, src_pk_rows) = match run_select(&mut src_conn, &format!("SELECT {} FROM {}.{}", pk_list, sql_id(&src_db), sql_id(&table))) {
            Ok(v) => v,
            // A KILL QUERY from Cancel surfaces here as an ordinary MySQL error, so check whether
            // this request was actually cancelled before reporting it as a real failure.
            Err(e) => {
                if cancelled_now(&rid) {
                    if let Some(r) = &rid { clear_compare_cancel(r); }
                    return Ok(json!({"ok":true,"pkCols":pk,"columns":Vec::<String>::new(),"rows":Vec::<Value>::new(),"missingTotal":0,"truncated":false,"targetReadonly":tgt_ro,"allMissingPks":Vec::<Value>::new(),"extraTotal":0,"extraPks":Vec::<Value>::new(),"cancelled":true}));
                }
                return Ok(json!({"ok":false,"error":e}));
            }
        };
        if cancelled_now(&rid) {
            if let Some(r) = &rid { clear_compare_cancel(r); }
            return Ok(json!({"ok":true,"pkCols":pk,"columns":Vec::<String>::new(),"rows":Vec::<Value>::new(),"missingTotal":0,"truncated":false,"targetReadonly":tgt_ro,"allMissingPks":Vec::<Value>::new(),"extraTotal":0,"extraPks":Vec::<Value>::new(),"cancelled":true}));
        }
        let tgt_pk_rows: Vec<Vec<Option<String>>> = match run_select(&mut tgt_conn, &format!("SELECT {} FROM {}.{}", pk_list, sql_id(&tgt_db), sql_id(&table))) {
            Ok((_c, rows)) => rows,
            // Target table hasn't been created yet - treat it as new/empty rather than failing,
            // so every source row correctly comes back as "missing".
            Err(e) => { if e.contains("1146") || e.to_lowercase().contains("doesn't exist") { Vec::new() } else { return Ok(json!({"ok":false,"error":e})); } }
        };
        const CAP: usize = 2000;
        let tgt_set: std::collections::HashSet<String> = tgt_pk_rows.iter().map(|r| row_key(r)).collect();
        // Rows present only in the TARGET. Nothing here acts on them - this comparison inserts
        // into the target and never deletes from it - but not REPORTING them let a target holding
        // extra rows read as "no row differences", which is the wrong conclusion to hand someone
        // comparing a production database against a copy. Both primary-key sets are already in
        // memory at this point, so the answer costs one more pass and no extra query; only the key
        // values are returned, not full rows, to keep the per-table cost of a bulk scan unchanged.
        let src_set: std::collections::HashSet<String> = src_pk_rows.iter().map(|r| row_key(r)).collect();
        let extra: Vec<Vec<Option<String>>> = tgt_pk_rows.iter().filter(|r| !src_set.contains(&row_key(r))).cloned().collect();
        let extra_total = extra.len();
        let extra_pks: Vec<Vec<Option<String>>> = extra.into_iter().take(CAP).collect();
        let missing: Vec<Vec<Option<String>>> = src_pk_rows.into_iter().filter(|r| !tgt_set.contains(&row_key(r))).collect();
        let missing_total = missing.len();
        let truncated = missing_total > CAP;
        let use_rows: Vec<Vec<Option<String>>> = missing.iter().take(CAP).cloned().collect();
        if cancelled_now(&rid) {
            return Ok(json!({"ok":true,"pkCols":pk,"columns":Vec::<String>::new(),"rows":Vec::<Value>::new(),"missingTotal":missing_total,"truncated":truncated,"targetReadonly":tgt_ro,"allMissingPks":missing,"extraTotal":extra_total,"extraPks":extra_pks,"cancelled":true}));
        }
        if let Some(r) = &rid { clear_compare_cancel(r); }
        if use_rows.is_empty() {
            return Ok(json!({"ok":true,"pkCols":pk,"columns":Vec::<String>::new(),"rows":Vec::<Value>::new(),"missingTotal":0,"truncated":false,"targetReadonly":tgt_ro,"allMissingPks":Vec::<Value>::new(),"extraTotal":extra_total,"extraPks":extra_pks,"cancelled":false}));
        }
        let (full_cols, full_rows) = match get_rows_by_pk(&mut src_conn, &src_db, &table, &pk, &use_rows) {
            Ok(v) => v,
            Err(e) => {
                if cancelled_now(&rid) {
                    return Ok(json!({"ok":true,"pkCols":pk,"columns":Vec::<String>::new(),"rows":Vec::<Value>::new(),"missingTotal":missing_total,"truncated":truncated,"targetReadonly":tgt_ro,"allMissingPks":missing,"extraTotal":extra_total,"extraPks":extra_pks,"cancelled":true}));
                }
                return Ok(json!({"ok":false,"error":e}));
            }
        };
        // allMissingPks: the FULL (uncapped) list of missing primary-key values, sent to the
        // client alongside the first page - just id values, not full row data, so it's cheap
        // compared to what a full table re-scan would cost. The client uses it to load later
        // pages, or to remove just-inserted rows and pull the next batch, WITHOUT ever
        // re-scanning the table again.
        Ok(json!({"ok":true,"pkCols":pk,"columns":full_cols,"rows":full_rows,"missingTotal":missing_total,"truncated":truncated,"targetReadonly":tgt_ro,"allMissingPks":missing,"extraTotal":extra_total,"extraPks":extra_pks,"cancelled":false}))
    }).await.map_err(|e| e.to_string())?
}

// Inserts the (client-selected) missing rows into the target, batched, using the exact column
// list and values fetched from the source - so ids/keys match the source exactly. Always
// INSERT-only; never touches an existing target row.
// Finds rows present on BOTH sides (same primary key) whose CONTENT differs - detection only,
// never writes anything. Capped tighter (500) than the missing-rows check since this fetches
// full row data from BOTH source and target for every candidate, which is heavier. Mirrors the
// PowerShell version's Api-CompareRowsDiff exactly.
#[tauri::command]
async fn compare_rows_diff(req: Value) -> R {
    let src_name = req["sourceConnName"].as_str().unwrap_or("").to_string();
    let tgt_name = req["targetConnName"].as_str().unwrap_or("").to_string();
    let src_db = req["sourceDb"].as_str().unwrap_or("").to_string();
    let tgt_db = req["targetDb"].as_str().unwrap_or("").to_string();
    let table = req["table"].as_str().unwrap_or("").to_string();
    let rid = req["requestId"].as_str().map(String::from);
    tokio::task::spawn_blocking(move || {
        let cancelled_now = |rid: &Option<String>| rid.as_deref().map(is_compare_cancelled).unwrap_or(false);
        let (src_connj, _) = resolve_saved_conn(&src_name)?;
        let (tgt_connj, tgt_ro) = resolve_saved_conn(&tgt_name)?;
        let mut src_conn = build_conn(&src_connj)?;
        let mut tgt_conn = build_conn(&tgt_connj)?;
        // See compare_rows above: lets Cancel actually KILL QUERY whichever of these is blocked,
        // rather than only being noticed once the current SELECT finishes on its own.
        register_compare_conn(&rid, &mut src_conn, &src_connj);
        register_compare_conn(&rid, &mut tgt_conn, &tgt_connj);
        let _guard = CompareConnGuard(rid.clone());
        let pk = get_table_pk_cols(&mut src_conn, &src_db, &table)?;
        if pk.is_empty() { return Ok(json!({"ok":false,"error":"Table has no primary key - cannot compare rows."})); }
        let fk = get_table_fk_cols(&mut src_conn, &src_db, &table).unwrap_or_default();
        let pk_list = pk.iter().map(|c| sql_id(c)).collect::<Vec<_>>().join(",");
        let (_c1, src_pk_rows) = match run_select(&mut src_conn, &format!("SELECT {} FROM {}.{}", pk_list, sql_id(&src_db), sql_id(&table))) {
            Ok(v) => v,
            Err(e) => {
                if cancelled_now(&rid) { return Ok(json!({"ok":true,"pkCols":pk,"fkCols":fk,"diffs":Vec::<Value>::new(),"commonTotal":0,"comparedCount":0,"truncated":false,"targetReadonly":tgt_ro,"cancelled":true})); }
                return Ok(json!({"ok":false,"error":e}));
            }
        };
        let tgt_pk_rows: Vec<Vec<Option<String>>> = match run_select(&mut tgt_conn, &format!("SELECT {} FROM {}.{}", pk_list, sql_id(&tgt_db), sql_id(&table))) {
            Ok((_c, rows)) => rows,
            Err(e) => {
                if e.contains("1146") || e.to_lowercase().contains("doesn't exist") { Vec::new() }
                else if cancelled_now(&rid) { return Ok(json!({"ok":true,"pkCols":pk,"fkCols":fk,"diffs":Vec::<Value>::new(),"commonTotal":0,"comparedCount":0,"truncated":false,"targetReadonly":tgt_ro,"cancelled":true})); }
                else { return Ok(json!({"ok":false,"error":e})); }
            }
        };
        let tgt_pk_set: std::collections::HashSet<String> = tgt_pk_rows.iter().map(|r| row_key(r)).collect();
        let common: Vec<&Vec<Option<String>>> = src_pk_rows.iter().filter(|r| tgt_pk_set.contains(&row_key(r))).collect();
        let common_total = common.len();
        const CAP: usize = 500;
        let truncated = common_total > CAP;
        let use_common: Vec<&Vec<Option<String>>> = common.into_iter().take(CAP).collect();
        if use_common.is_empty() {
            return Ok(json!({"ok":true,"pkCols":pk,"fkCols":fk,"diffs":Vec::<Value>::new(),"commonTotal":common_total,"comparedCount":0,"truncated":false,"targetReadonly":tgt_ro}));
        }
        let fetch_chunk = 200;
        let bin = binary_column_set(&mut src_conn, &src_db, &table)?;
        let float = float_column_set(&mut src_conn, &src_db, &table)?;
        // The same columns, in the same order, on both sides - SELECT * compared them by position.
        let copy = select_list(&copy_columns(&mut src_conn, &src_db, &table, &pk)?);
        let mut full_cols: Option<Vec<String>> = None;
        let mut src_full: std::collections::HashMap<String, Vec<Option<String>>> = std::collections::HashMap::new();
        let mut tgt_full: std::collections::HashMap<String, Vec<Option<String>>> = std::collections::HashMap::new();
        let mut cancelled = false;
        for chunk in use_common.chunks(fetch_chunk) {
            if let Some(r) = &rid { if is_compare_cancelled(r) { cancelled = true; break; } }
            let where_clause = pk_where(&pk, chunk, &bin, &float);
            let (sc, sr) = run_select(&mut src_conn, &format!("SELECT {} FROM {}.{} WHERE {}", copy, sql_id(&src_db), sql_id(&table), where_clause))?;
            if full_cols.is_none() { full_cols = Some(sc); }
            let cols_ref = full_cols.as_ref().unwrap();
            let pk_idx: Vec<usize> = pk.iter().map(|c| cols_ref.iter().position(|x| x == c).unwrap_or(0)).collect();
            for row in sr { let k = pk_idx.iter().map(|&i| row[i].clone().unwrap_or_default()).collect::<Vec<_>>().join("\u{1}"); src_full.insert(k, row); }
            let (_tc, tr) = run_select(&mut tgt_conn, &format!("SELECT {} FROM {}.{} WHERE {}", copy, sql_id(&tgt_db), sql_id(&table), where_clause))?;
            for row in tr { let k = pk_idx.iter().map(|&i| row[i].clone().unwrap_or_default()).collect::<Vec<_>>().join("\u{1}"); tgt_full.insert(k, row); }
        }
        let cols_final = full_cols.unwrap_or_default();
        let pk_idx_final: Vec<usize> = pk.iter().map(|c| cols_final.iter().position(|x| x == c).unwrap_or(0)).collect();
        let mut diffs: Vec<Value> = Vec::new();
        for (k, s_row) in src_full.iter() {
            let t_row = match tgt_full.get(k) { Some(r) => r, None => continue };
            let mut col_diffs: Vec<Value> = Vec::new();
            for ci in 0..cols_final.len() {
                if s_row[ci] != t_row[ci] {
                    col_diffs.push(json!({"col": cols_final[ci], "src": s_row[ci], "tgt": t_row[ci]}));
                }
            }
            if !col_diffs.is_empty() {
                let pk_vals: Vec<Option<String>> = pk_idx_final.iter().map(|&i| s_row[i].clone()).collect();
                diffs.push(json!({"pk": pk_vals, "colDiffs": col_diffs}));
            }
        }
        if let Some(r) = &rid { clear_compare_cancel(r); }
        Ok(json!({"ok":true,"pkCols":pk,"fkCols":fk,"diffs":diffs,"commonTotal":common_total,"comparedCount":use_common.len(),"truncated":truncated,"targetReadonly":tgt_ro,"cancelled":cancelled}))
    }).await.map_err(|e| e.to_string())?
}

// Applies the (client-selected) content updates: one UPDATE per row, using the SOURCE value for
// each column flagged as different, matched by primary key. This OVERWRITES existing target
// data for those rows - the only write path in Compare that does so - and is always
// client-confirmed with an explicit warning before this is ever called.
#[tauri::command]
async fn compare_rows_apply_diff(req: Value) -> R {
    let tgt_name = req["targetConnName"].as_str().unwrap_or("").to_string();
    let tgt_db = req["targetDb"].as_str().unwrap_or("").to_string();
    let table = req["table"].as_str().unwrap_or("").to_string();
    let pk_cols: Vec<String> = req["pkCols"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let updates: Vec<Value> = req["updates"].as_array().cloned().unwrap_or_default();
    tokio::task::spawn_blocking(move || {
        let (connj, readonly) = resolve_saved_conn(&tgt_name)?;
        if readonly { return Ok(json!({"ok":false,"error":"Target connection is read-only / safe mode - blocked."})); }
        if pk_cols.is_empty() || updates.is_empty() { return Ok(json!({"ok":false,"error":"No rows to update."})); }
        let mut c = build_conn(&connj)?; backslash_escapes_on(&mut c);
        let obj = format!("{}.{}", sql_id(&tgt_db), sql_id(&table));
        let bin = binary_column_set(&mut c, &tgt_db, &table)?;
        let float = float_column_set(&mut c, &tgt_db, &table)?;
        // Unlike compare_rows_apply/compare_rows_insert_all (INSERT-only, each batch already
        // atomic as a single multi-row statement, and a chunk failing partway through a large
        // bulk insert shouldn't block the rest), this updates EXISTING target rows one at a time -
        // the exact "apply this reviewed set of corrections" shape the grid's own staged-edits
        // apply already wraps in a transaction for. A batch failing partway through here left
        // some rows changed and others not, with no way back - same failure mode, same fix.
        c.query_drop("START TRANSACTION").map_err(|e| e.to_string())?;
        let mut log = Vec::new();
        let mut failed = false;
        for u in &updates {
            let col_diffs = u["colDiffs"].as_array().cloned().unwrap_or_default();
            let pk_vals = u["pk"].as_array().cloned().unwrap_or_default();
            if col_diffs.is_empty() || pk_vals.len() != pk_cols.len() {
                log.push("SKIPPED (no columns/key)".to_string());
                continue;
            }
            let sets = col_diffs.iter().map(|cd| {
                let col = cd["col"].as_str().unwrap_or("");
                format!("{}={}", sql_id(col), json_val_for(&cd["src"], bin.contains(&col.to_lowercase())))
            }).collect::<Vec<_>>().join(",");
            let wheres = pk_cols.iter().zip(pk_vals.iter()).map(|(col, v)| {
                if float.contains(&col.to_lowercase()) { format!("{}={}", key_col(col, &float), json_val_for(v, false)) }
                else { format!("{}={}", sql_id(col), json_val_for(v, bin.contains(&col.to_lowercase()))) }
            }).collect::<Vec<_>>().join(" AND ");
            let pk_desc = pk_vals.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",");
            // The update used to count as done whatever it matched: a row deleted on the target
            // since the comparison, or a key that could not be matched, was reported as updated.
            let matched = run_select(&mut c, &format!("SELECT COUNT(*) FROM {} WHERE {}", obj, wheres))
                .ok().and_then(|(_, r)| r.first().and_then(|x| x.first()).cloned().flatten());
            if matched.as_deref() != Some("1") {
                log.push(format!("FAILED id={} : the target has {} row(s) with this key, not 1", pk_desc, matched.unwrap_or_else(|| "?".into())));
                failed = true; break;
            }
            let sql = format!("UPDATE {} SET {} WHERE {} LIMIT 1", obj, sets, wheres);
            match c.query_drop(&sql) {
                Ok(_) => log.push(format!("OK  updated id={}", pk_desc)),
                Err(e) => { log.push(format!("FAILED id={} : {}", pk_desc, e)); failed = true; break; }
            }
        }
        if failed {
            let _ = c.query_drop("ROLLBACK");
            log.push("No rows were updated - the batch was rolled back.".to_string());
            return Ok(json!({"ok":false,"log":log}));
        }
        if let Err(e) = c.query_drop("COMMIT") {
            let _ = c.query_drop("ROLLBACK");
            log.push(format!("Could not commit: {} - no rows were updated.", e));
            return Ok(json!({"ok":false,"log":log}));
        }
        Ok(json!({"ok":true,"log":log}))
    }).await.map_err(|e| e.to_string())?
}

// Inserts EVERY missing row (source rows absent from target), not just the first 2000 that fit
// in the interactive review list. Unlike compare_rows, this never sends the row data back to
// the frontend at all - it fetches a chunk of missing rows from source and inserts that SAME
// chunk into target immediately, chunk by chunk, so the amount of data moved isn't limited by
// what's practical to render as a checkbox list. Still insert-only.
//
// Deliberately not wrapped in one big transaction across every chunk: each chunk's INSERT is
// already one SQL statement, which MySQL/InnoDB itself only ever applies all-or-nothing - a
// chunk failing partway through can't leave that chunk half-inserted. What continue-on-error
// buys here is that ONE bad chunk (say, a duplicate key from a row someone else inserted since
// the scan started) doesn't roll back or abort inserting the rest of what could be tens of
// thousands of otherwise-good rows - unlike compare_rows_apply_diff below, this never touches an
// existing row, so a chunk that fails simply leaves those rows still missing, not corrupted.
#[tauri::command]
async fn compare_rows_insert_all(req: Value) -> R {
    let src_name = req["sourceConnName"].as_str().unwrap_or("").to_string();
    let tgt_name = req["targetConnName"].as_str().unwrap_or("").to_string();
    let src_db = req["sourceDb"].as_str().unwrap_or("").to_string();
    let tgt_db = req["targetDb"].as_str().unwrap_or("").to_string();
    let table = req["table"].as_str().unwrap_or("").to_string();
    let rid = req["requestId"].as_str().map(String::from);
    tokio::task::spawn_blocking(move || {
        let (src_connj, _) = resolve_saved_conn(&src_name)?;
        let (tgt_connj, tgt_ro) = resolve_saved_conn(&tgt_name)?;
        if tgt_ro { return Ok(json!({"ok":false,"error":"Target connection is read-only / safe mode - blocked."})); }
        let mut src_conn = build_conn(&src_connj)?;
        let mut tgt_conn = build_conn(&tgt_connj)?;
        let pk = get_table_pk_cols(&mut src_conn, &src_db, &table)?;
        if pk.is_empty() { return Ok(json!({"ok":false,"error":"Table has no primary key - cannot compare rows."})); }
        let pk_list = pk.iter().map(|c| sql_id(c)).collect::<Vec<_>>().join(",");
        let (_c1, src_pk_rows) = run_select(&mut src_conn, &format!("SELECT {} FROM {}.{}", pk_list, sql_id(&src_db), sql_id(&table)))?;
        let tgt_pk_rows: Vec<Vec<Option<String>>> = match run_select(&mut tgt_conn, &format!("SELECT {} FROM {}.{}", pk_list, sql_id(&tgt_db), sql_id(&table))) {
            Ok((_c, rows)) => rows,
            Err(e) => { if e.contains("1146") || e.to_lowercase().contains("doesn't exist") { Vec::new() } else { return Ok(json!({"ok":false,"error":e})); } }
        };
        let tgt_set: std::collections::HashSet<String> = tgt_pk_rows.iter().map(|r| row_key(r)).collect();
        let missing: Vec<Vec<Option<String>>> = src_pk_rows.into_iter().filter(|r| !tgt_set.contains(&row_key(r))).collect();
        let missing_total = missing.len();
        if missing_total == 0 {
            return Ok(json!({"ok":true,"missingTotal":0,"inserted":0,"cancelled":false,"log":Vec::<String>::new()}));
        }
        let chunk_size = 200;
        let src_bin = binary_column_set(&mut src_conn, &src_db, &table)?;
        let tgt_bin = binary_column_set(&mut tgt_conn, &tgt_db, &table)?;
        let src_float = float_column_set(&mut src_conn, &src_db, &table)?;
        let copy = select_list(&copy_columns(&mut src_conn, &src_db, &table, &pk)?);
        let mut log: Vec<String> = Vec::new();
        let mut inserted: usize = 0;
        let mut cancelled = false;
        for (ci, chunk) in missing.chunks(chunk_size).enumerate() {
            if let Some(r) = &rid { if is_compare_cancelled(r) { cancelled = true; break; } }
            let refs: Vec<&Vec<Option<String>>> = chunk.iter().collect();
            let where_clause = pk_where(&pk, &refs, &src_bin, &src_float);
            let (full_cols, full_rows) = match run_select(&mut src_conn, &format!("SELECT {} FROM {}.{} WHERE {}", copy, sql_id(&src_db), sql_id(&table), where_clause)) {
                Ok(v) => v,
                Err(e) => { log.push(format!("FAILED (fetch) chunk {} : {}", ci + 1, e)); continue; }
            };
            // A row that cannot be read back by its key is not copied; say so rather than
            // reporting the chunk as done.
            if full_rows.len() != chunk.len() {
                log.push(format!("FAILED (fetch) chunk {} : {} of {} row(s) could not be read back by their key and were not copied", ci + 1, chunk.len() - full_rows.len(), chunk.len()));
            }
            if full_rows.is_empty() { continue; }
            let col_list = full_cols.iter().map(|c| sql_id(c)).collect::<Vec<_>>().join(",");
            let obj = format!("{}.{}", sql_id(&tgt_db), sql_id(&table));
            let col_bin: Vec<bool> = full_cols.iter().map(|c| tgt_bin.contains(&c.to_lowercase())).collect();
            let values_sql = full_rows.iter().map(|row| {
                let vs = row.iter().enumerate().map(|(i, v)| sql_val_for(v.as_deref(), col_bin[i])).collect::<Vec<_>>().join(",");
                format!("({})", vs)
            }).collect::<Vec<_>>().join(",");
            let sql = format!("INSERT INTO {} ({}) VALUES {}", obj, col_list, values_sql);
            match tgt_conn.query_drop(&sql) {
                Ok(_) => { inserted += full_rows.len(); log.push(format!("OK  inserted {} row(s) ({} of {} so far)", full_rows.len(), inserted, missing_total)); }
                Err(e) => log.push(format!("FAILED (insert) chunk {} : {}", ci + 1, e)),
            }
        }
        if let Some(r) = &rid { clear_compare_cancel(r); }
        Ok(json!({"ok":true,"missingTotal":missing_total,"inserted":inserted,"cancelled":cancelled,"log":log}))
    }).await.map_err(|e| e.to_string())?
}

// Same reasoning as compare_rows_insert_all above: insert-only, each batch already atomic as one
// SQL statement, continue-on-error across batches so one bad batch doesn't block the rest of a
// large reviewed set from landing.
#[tauri::command]
async fn compare_rows_apply(req: Value) -> R {
    let tgt_name = req["targetConnName"].as_str().unwrap_or("").to_string();
    let tgt_db = req["targetDb"].as_str().unwrap_or("").to_string();
    let table = req["table"].as_str().unwrap_or("").to_string();
    let columns: Vec<String> = req["columns"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let rows: Vec<Vec<Value>> = req["rows"].as_array().map(|a| a.iter().map(|r| r.as_array().cloned().unwrap_or_default()).collect()).unwrap_or_default();
    tokio::task::spawn_blocking(move || {
        let (connj, readonly) = resolve_saved_conn(&tgt_name)?;
        if readonly { return Ok(json!({"ok":false,"error":"Target connection is read-only / safe mode - blocked."})); }
        if columns.is_empty() || rows.is_empty() { return Ok(json!({"ok":false,"error":"No rows to insert."})); }
        let mut c = build_conn(&connj)?; backslash_escapes_on(&mut c);
        let col_list = columns.iter().map(|c| sql_id(c)).collect::<Vec<_>>().join(",");
        let obj = format!("{}.{}", sql_id(&tgt_db), sql_id(&table));
        let bin = binary_column_set(&mut c, &tgt_db, &table)?;
        let col_bin: Vec<bool> = columns.iter().map(|c| bin.contains(&c.to_lowercase())).collect();
        let mut log = Vec::new();
        const BATCH: usize = 500;
        for (bi, chunk) in rows.chunks(BATCH).enumerate() {
            let values_sql = chunk.iter().map(|row| {
                let vs = row.iter().enumerate()
                    .map(|(i, v)| json_val_for(v, col_bin.get(i).copied().unwrap_or(false))).collect::<Vec<_>>().join(",");
                format!("({})", vs)
            }).collect::<Vec<_>>().join(",");
            let sql = format!("INSERT INTO {} ({}) VALUES {}", obj, col_list, values_sql);
            match c.query_drop(&sql) {
                Ok(_) => log.push(format!("OK  inserted {} row(s) (batch {})", chunk.len(), bi + 1)),
                Err(e) => log.push(format!("FAILED batch {} : {}", bi + 1, e)),
            }
        }
        Ok(json!({"ok":true,"log":log}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn compare_dbs(req: Value) -> R {
    let name = req["connName"].as_str().unwrap_or("").to_string();
    tokio::task::spawn_blocking(move || {
        let (connj, readonly) = resolve_saved_conn(&name)?;
        let mut c = build_conn(&connj)?;
        let (_cols, rows) = run_select(&mut c, "SHOW DATABASES")?;
        let dbs: Vec<String> = rows.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect();
        Ok(json!({"ok":true,"databases":dbs,"readonly":readonly}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn compare_tables(req: Value) -> R {
    let src_name = req["sourceConnName"].as_str().unwrap_or("").to_string();
    let tgt_name = req["targetConnName"].as_str().unwrap_or("").to_string();
    let src_db = req["sourceDb"].as_str().unwrap_or("").to_string();
    let tgt_db = req["targetDb"].as_str().unwrap_or("").to_string();
    tokio::task::spawn_blocking(move || {
        let (src_connj, _) = resolve_saved_conn(&src_name)?;
        let (tgt_connj, _) = resolve_saved_conn(&tgt_name)?;
        let mut src_conn = build_conn(&src_connj)?;
        let mut tgt_conn = build_conn(&tgt_connj)?;
        let sql1 = format!("SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA={} ORDER BY TABLE_NAME", sql_lit(&src_db));
        let sql2 = format!("SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA={} ORDER BY TABLE_NAME", sql_lit(&tgt_db));
        let (_c1, r1) = run_select(&mut src_conn, &sql1)?;
        let (_c2, r2) = run_select(&mut tgt_conn, &sql2)?;
        let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for r in r1 { if let Some(Some(n)) = r.into_iter().next() { set.insert(n); } }
        for r in r2 { if let Some(Some(n)) = r.into_iter().next() { set.insert(n); } }
        let names: Vec<String> = set.into_iter().collect();
        Ok(json!({"ok":true,"tables":names}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn compare_schemas(req: Value) -> R {
    let src_name = req["sourceConnName"].as_str().unwrap_or("").to_string();
    let tgt_name = req["targetConnName"].as_str().unwrap_or("").to_string();
    let src_db = req["sourceDb"].as_str().unwrap_or("").to_string();
    let tgt_db = req["targetDb"].as_str().unwrap_or("").to_string();
    if src_db.is_empty() || tgt_db.is_empty() { return Ok(json!({"ok":false,"error":"Pick a database on both sides."})); }
    tokio::task::spawn_blocking(move || {
        let (src_connj, _sro) = resolve_saved_conn(&src_name)?;
        let (tgt_connj, tgt_ro) = resolve_saved_conn(&tgt_name)?;
        let mut src_conn = build_conn(&src_connj)?;
        let mut tgt_conn = build_conn(&tgt_connj)?;
        let mut src_cols = get_schema_columns(&mut src_conn, &src_db)?;
        let mut tgt_cols = get_schema_columns(&mut tgt_conn, &tgt_db)?;
        if let Some(arr) = req.get("tables").and_then(|v| v.as_array()) {
            let keep: std::collections::HashSet<String> = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            src_cols.retain(|k, _| keep.contains(k));
            tgt_cols.retain(|k, _| keep.contains(k));
        }
        let rid = req["requestId"].as_str().map(String::from);
        let (diffs, cancelled) = compare_table_sets(&mut src_conn, &src_db, &mut tgt_conn, &tgt_db, &src_cols, &tgt_cols, rid.as_deref());
        if let Some(r) = &rid { clear_compare_cancel(r); }
        let tables: Vec<Value> = diffs.into_iter().map(|t| json!({
            "name": t.name, "status": t.status,
            "sql": t.sql.into_iter().map(|s| json!({"stmt": s.stmt, "checked": s.checked, "kind": s.kind})).collect::<Vec<_>>()
        })).collect();
        Ok(json!({"ok":true,"tables":tables,"targetReadonly":tgt_ro,"cancelled":cancelled}))
    }).await.map_err(|e| e.to_string())?
}

// Deliberately NOT wrapped in a transaction: these are schema-diff statements (ALTER/CREATE/DROP
// TABLE), and every one of them is an implicit-commit statement in MySQL/MariaDB - a
// START TRANSACTION here would be silently ignored the moment the first DDL statement ran, giving
// false confidence that a failure partway through could be rolled back when it can't be. Running
// each independently and reporting OK/FAILED per line (as already done below) is the honest
// behavior given that constraint - a half-migrated schema is visible in the log, not hidden by a
// rollback that was never actually possible.
#[tauri::command]
async fn compare_apply(req: Value) -> R {
    let tgt_name = req["targetConnName"].as_str().unwrap_or("").to_string();
    let tgt_db = req["targetDb"].as_str().unwrap_or("").to_string();
    let statements: Vec<String> = req["statements"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    tokio::task::spawn_blocking(move || {
        let (connj, readonly) = resolve_saved_conn(&tgt_name)?;
        if readonly { return Ok(json!({"ok":false,"error":"Target connection is read-only / safe mode - blocked."})); }
        let mut c = build_conn(&connj)?;
        if !tgt_db.is_empty() { let _ = c.query_drop(format!("USE {}", sql_id(&tgt_db))); }
        // Missing tables are created in name order, so a child table came before its parent and
        // failed on its foreign key. A CREATE TABLE that failed is tried again after the others, for
        // as long as each round gets at least one more through; anything else stands as it failed.
        let mut result: Vec<String> = vec![String::new(); statements.len()];
        let mut pending: Vec<usize> = (0..statements.len()).collect();
        loop {
            let (mut again, mut progress) = (Vec::new(), false);
            for i in pending {
                match c.query_drop(&statements[i]) {
                    Ok(_) => { result[i] = format!("OK  {}", statements[i]); progress = true; }
                    Err(e) => {
                        result[i] = format!("FAILED  {}  :  {}", statements[i], e);
                        if statements[i].trim_start().to_uppercase().starts_with("CREATE TABLE") { again.push(i); }
                    }
                }
            }
            if again.is_empty() || !progress { break; }
            pending = again;
        }
        Ok(json!({"ok":true,"log":result}))
    }).await.map_err(|e| e.to_string())?
}

// Server-side folder/file browser backing the in-app mBrowse modal (matches the PowerShell
// version's Api-Browse: "path":"ROOT" lists available drives on Windows / "/" on Unix;
// otherwise it lists the subdirectories ("dirs") and, unless dirsOnly, matching files ("files")
// of the given directory, plus "parent" so the modal's Up button can navigate back out.
// Saved-query library (favorites), mirrors the PowerShell version's Load-Lib/Save-Lib -
// a flat JSON array of {name, sql, schema, ts} stored alongside connections.json.
fn lib_path() -> std::path::PathBuf {
    let mut p = dirs::config_dir().unwrap_or(std::env::temp_dir());
    p.push("NOBSSQL-Desktop"); std::fs::create_dir_all(&p).ok(); p.push("library.json"); p
}
fn load_lib() -> Vec<Value> {
    std::fs::read_to_string(lib_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
fn save_lib(v: &Vec<Value>) { let _ = std::fs::write(lib_path(), serde_json::to_string_pretty(v).unwrap_or_default()); }
fn now_ms() -> i64 { chrono::Utc::now().timestamp_millis() }

#[tauri::command]
async fn lib_list(_req: Value) -> R {
    Ok(json!({"ok":true,"items":load_lib()}))
}
#[tauri::command]
async fn lib_save(req: Value) -> R {
    let name = req["name"].as_str().unwrap_or("").to_string();
    if name.is_empty() { return Ok(json!({"ok":false,"error":"name required"})); }
    let ts = req["ts"].as_i64().unwrap_or_else(now_ms);
    let mut list: Vec<Value> = load_lib().into_iter().filter(|x| x["name"].as_str() != Some(name.as_str())).collect();
    list.insert(0, json!({"name":name,"sql":req["sql"].as_str().unwrap_or(""),"schema":req["schema"].as_str().unwrap_or(""),"ts":ts}));
    save_lib(&list);
    Ok(json!({"ok":true}))
}
#[tauri::command]
async fn lib_delete(req: Value) -> R {
    let name = req["name"].as_str().unwrap_or("").to_string();
    let list: Vec<Value> = load_lib().into_iter().filter(|x| x["name"].as_str() != Some(name.as_str())).collect();
    save_lib(&list);
    Ok(json!({"ok":true}))
}
#[tauri::command]
async fn lib_clear(_req: Value) -> R {
    save_lib(&Vec::new());
    Ok(json!({"ok":true}))
}
#[tauri::command]
async fn lib_replace(req: Value) -> R {
    let mut list = Vec::new();
    if let Some(items) = req["items"].as_array() {
        for x in items {
            if x["name"].as_str().map(|s| !s.is_empty()).unwrap_or(false) {
                let ts = x["ts"].as_i64().unwrap_or_else(now_ms);
                list.push(json!({"name":x["name"],"sql":x["sql"].as_str().unwrap_or(""),"schema":x["schema"].as_str().unwrap_or(""),"ts":ts}));
            }
        }
    }
    save_lib(&list);
    Ok(json!({"ok":true}))
}

#[tauri::command]
async fn search_all_schemas(req: Value) -> R {
    let term = req["term"].as_str().unwrap_or("").trim().to_string();
    if term.is_empty() { return Ok(json!({"ok":false,"error":"Empty search term."})); }
    tokio::task::spawn_blocking(move || {
        let mut c = build_conn(&req["conn"])?;
        let like = sql_lit(&format!("%{}%", term));
        let sql = format!(
            "SELECT TABLE_SCHEMA,'table',TABLE_NAME FROM information_schema.TABLES WHERE TABLE_TYPE IN ('BASE TABLE','SYSTEM VERSIONED') AND TABLE_NAME LIKE {t} \
             UNION ALL SELECT TABLE_SCHEMA,'view',TABLE_NAME FROM information_schema.TABLES WHERE TABLE_TYPE IN ('VIEW','SYSTEM VIEW') AND TABLE_NAME LIKE {t} \
             UNION ALL SELECT ROUTINE_SCHEMA,IF(ROUTINE_TYPE='PROCEDURE','procedure','function'),ROUTINE_NAME FROM information_schema.ROUTINES WHERE ROUTINE_NAME LIKE {t} \
             UNION ALL SELECT TRIGGER_SCHEMA,'trigger',TRIGGER_NAME FROM information_schema.TRIGGERS WHERE TRIGGER_NAME LIKE {t} \
             UNION ALL SELECT EVENT_SCHEMA,'event',EVENT_NAME FROM information_schema.EVENTS WHERE EVENT_NAME LIKE {t} \
             ORDER BY 1,2,3", t = like);
        match run_select(&mut c, &sql) {
            Ok((_cols, rows)) => {
                let items: Vec<Value> = rows.iter().map(|r| json!({
                    "schema": r.first().cloned().flatten(),
                    "type": r.get(1).cloned().flatten(),
                    "name": r.get(2).cloned().flatten()
                })).collect();
                Ok(json!({"ok":true,"items":items}))
            }
            Err(e) => Ok(json!({"ok":false,"error":e})),
        }
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn browse(req: Value) -> R {
    let raw_path = req["path"].as_str().unwrap_or("ROOT").to_string();
    let filter = req["filter"].as_str().unwrap_or("*").to_string(); // e.g. "*.sql"
    let dirs_only = req["dirsOnly"].as_bool().unwrap_or(false);
    let ext = filter.trim_start_matches("*.").to_lowercase();

    if raw_path.is_empty() || raw_path == "ROOT" {
        // List drives on Windows (C:\, D:\, ...); a single root on Unix.
        let mut dirs = Vec::new();
        #[cfg(target_os = "windows")]
        {
            for letter in b'A'..=b'Z' {
                let drive = format!("{}:\\", letter as char);
                if std::path::Path::new(&drive).exists() {
                    dirs.push(json!({"name": drive.clone(), "path": drive}));
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            dirs.push(json!({"name": "/", "path": "/"}));
        }
        return Ok(json!({"ok":true,"path":"","parent":Value::Null,"dirs":dirs,"files":Vec::<Value>::new()}));
    }

    let dir = std::path::Path::new(&raw_path);
    match std::fs::read_dir(dir) {
        Ok(rd) => {
            let mut dirs = Vec::new();
            let mut files = Vec::new();
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if p.is_dir() {
                    dirs.push(json!({"name": name, "path": p.to_string_lossy()}));
                } else if !dirs_only && (filter == "*" || filter.is_empty() || name.to_lowercase().ends_with(&format!(".{}", ext))) {
                    files.push(json!({"path": p.to_string_lossy(), "name": name, "dir": false, "size": e.metadata().map(|m| m.len()).ok()}));
                }
            }
            dirs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            // "parent": the containing directory, or "ROOT" if we're already at a drive/filesystem root.
            let parent = match dir.parent() {
                Some(pp) if !pp.as_os_str().is_empty() && pp != dir => json!(pp.to_string_lossy()),
                _ => json!("ROOT"),
            };
            Ok(json!({"ok":true,"path":dir.to_string_lossy(),"parent":parent,"dirs":dirs,"files":files}))
        }
        Err(e) => Ok(json!({"ok":false,"error":format!("Cannot read folder: {}", e)})),
    }
}

#[tauri::command]
fn cancel_export() { EXPORT_CANCEL.store(true, Ordering::SeqCst); }

// Cancels one Export or Import run by the jobId the UI generated for it. Sets the flag the run's
// loops check before starting the next child, and kills the child running right now so a large
// single dump stops immediately instead of finishing minutes later. Messages match the
// PowerShell backend's Api-CancelJob, including the case where the job already completed.
#[tauri::command]
fn cancel_job(req: Value) -> R {
    let id = req["jobId"].as_str().unwrap_or("").to_string();
    if id.is_empty() { return Ok(json!({"ok":false,"error":"no jobId"})); }
    let job = jobs().lock().ok().and_then(|m| m.get(&id).cloned());
    match job {
        Some(j) => {
            j.cancelled.store(true, Ordering::SeqCst);
            if let Ok(mut slot) = j.child.lock() {
                if let Some(c) = slot.as_mut() { kill_child(c); }
            }
            Ok(json!({"ok":true,"message":"Cancel requested."}))
        }
        None => Ok(json!({"ok":false,"error":"Job not found - it may have already finished."})),
    }
}

// The page does not name a file to write: it asks for the Save dialog here, and what the user picks
// there may be written once. A page that could pass any path to save_text, save_binary or
// export_table could write any file the user can - a script into the Startup folder.
fn save_grants() -> &'static Mutex<std::collections::HashSet<String>> {
    static G: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    G.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}
fn take_save_grant(path: &str) -> bool { save_grants().lock().unwrap().remove(path) }

#[tauri::command]
async fn pick_save_path(app: tauri::AppHandle, req: Value) -> R {
    use tauri_plugin_dialog::DialogExt;
    let mut d = app.dialog().file();
    if let Some(n) = req["defaultPath"].as_str().filter(|s| !s.is_empty()) { d = d.set_file_name(n); }
    for f in req["filters"].as_array().cloned().unwrap_or_default() {
        let exts: Vec<String> = f["extensions"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
        d = d.add_filter(f["name"].as_str().unwrap_or(""), &refs);
    }
    let picked = tokio::task::spawn_blocking(move || d.blocking_save_file()).await.map_err(|e| e.to_string())?;
    let Some(fp) = picked else { return Ok(Value::Null) };
    let p = fp.into_path().map_err(|e| e.to_string())?.to_string_lossy().to_string();
    save_grants().lock().unwrap().insert(p.clone());
    Ok(json!(p))
}

// The GUI tests cannot click through a native Save dialog, so a debug build (which is what they run)
// lets them name the file; a release build does not.
#[tauri::command]
fn grant_save_path_for_test(req: Value) -> R {
    #[cfg(debug_assertions)]
    {
        save_grants().lock().unwrap().insert(req["path"].as_str().unwrap_or("").to_string());
        Ok(json!({"ok":true}))
    }
    #[cfg(not(debug_assertions))]
    { let _ = req; Ok(json!({"ok":false,"error":"not in this build"})) }
}

#[tauri::command]
async fn export_table(app: tauri::AppHandle, req: Value) -> R {
    if !take_save_grant(req["file"].as_str().unwrap_or("")) { return Ok(json!({"ok":false,"error":"not a path chosen in the Save dialog"})); }
    export_table_run(Some(app), req).await
}

// The body, split out so a test can drive it without a tauri::AppHandle. The handle is only used
// to emit progress events, which simply do not fire when there is nobody to receive them.
// An INSERT export writes a backslash in a value as \\ (sql_str_lit). Loaded into a session whose
// sql_mode has NO_BACKSLASH_ESCAPES, every one of them was stored doubled. The file says which way it
// is written and puts the session back afterwards, as mysqldump's own header does for its settings.
const INSERTS_HEAD: &str = "-- Values in this file escape a backslash as \\\\, so the session reads them that way.\nSET @nobs_old_sql_mode = @@SESSION.sql_mode;\nSET SESSION sql_mode = TRIM(BOTH ',' FROM REPLACE(CONCAT(',', @@SESSION.sql_mode, ','), ',NO_BACKSLASH_ESCAPES,', ','));\n";
const INSERTS_TAIL: &str = "SET SESSION sql_mode = @nobs_old_sql_mode;\n";

async fn export_table_run(app: Option<tauri::AppHandle>, req: Value) -> R {
    tokio::task::spawn_blocking(move || -> R {
        use std::io::Write as _;
        let db = req["db"].as_str().unwrap_or("").to_string();
        let table = req["table"].as_str().unwrap_or("").to_string();
        let file = req["file"].as_str().unwrap_or("").to_string();
        let fmt = req["format"].as_str().unwrap_or("csv").to_string();
        if db.is_empty() || table.is_empty() { return Ok(json!({"ok":false,"error":"no table"})); }
        if file.is_empty() { return Ok(json!({"ok":false,"error":"no output file"})); }
        EXPORT_CANCEL.store(false, Ordering::SeqCst);
        let mut c = build_conn(&req["conn"])?;
        let f = std::fs::File::create(&file).map_err(|e| e.to_string())?;
        let mut w = std::io::BufWriter::new(f);
        let tbl = format!("{}.{}", sql_id(&db), sql_id(&table));
        // Every column for CSV, invisible ones included; INSERTs leave out generated columns, which
        // cannot be given a value (the CSV import skips them).
        let all = table_columns(&mut c, &db, &table)?;
        let chosen: Vec<String> = all.iter().filter(|(_, g)| fmt != "inserts" || !*g).map(|(n, _)| n.clone()).collect();
        let mut result = c.query_iter(format!("SELECT {} FROM {}", select_list(&chosen), tbl)).map_err(db_err)?;
        let (cols, bin): (Vec<String>, Vec<bool>) = {
            let cs = result.columns();
            let sl: &[Column] = cs.as_ref();
            (sl.iter().map(|c| c.name_str().to_string()).collect(),
             sl.iter().map(is_binaryish).collect())
        };
        // A NULL and an empty string both used to come out as an empty field, so the two were
        // indistinguishable in the file - and the CSV importer turns an empty cell into NULL, so
        // an empty string did not survive a round trip. Write NULL as an explicit marker
        // instead, defaulting to \N, which is what LOAD DATA reads and what HeidiSQL defaults
        // to. The marker is never quoted: the server only recognises \N unenclosed.
        let null_marker = req["nullValue"].as_str().unwrap_or("\\N").to_string();
        // A bare \r (no following \n) has to be quoted too, not just \n - both this app's own
        // CSV importer and a spreadsheet's CSV rules treat a lone \r as ending the row, so an
        // unquoted one silently splits one logical row into two and shifts every column after it.
        let csv_field = |o: &Option<String>| -> String {
            match o { None => null_marker.clone(), Some(s) => {
                if s.contains('"') || s.contains(',') || s.contains('\n') || s.contains('\r') { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.clone() }
            }}
        };
        if fmt == "inserts" { w.write_all(INSERTS_HEAD.as_bytes()).map_err(|e| e.to_string())?; }
        if fmt != "inserts" {
            let hdr = cols.iter().map(|c| if c.contains(',')||c.contains('"')||c.contains('\n')||c.contains('\r'){format!("\"{}\"",c.replace('"',"\"\""))}else{c.clone()}).collect::<Vec<_>>().join(",");
            w.write_all(hdr.as_bytes()).map_err(|e| e.to_string())?; w.write_all(b"\n").map_err(|e| e.to_string())?;
        }
        let mut n: usize = 0;
        let mut clash: usize = 0;
        let mut batch: Vec<String> = Vec::new();
        let mut cancelled = false;
        for rr in result.by_ref() {
            if EXPORT_CANCEL.load(Ordering::SeqCst) { cancelled = true; break; }
            let row = rr.map_err(|e| e.to_string())?;
            let cells: Vec<Option<String>> = (0..cols.len()).map(|i| val_to_opt(row.as_ref(i).unwrap_or(&MyValue::NULL), bin[i])).collect();
            if fmt == "inserts" {
                // The column type decides, not the value's shape - see sql_val_for.
                let vals = cells.iter().enumerate().map(|(i, o)| sql_val_for(o.as_deref(), bin[i])).collect::<Vec<_>>().join(",");
                batch.push(format!("({})", vals));
                if batch.len() >= 1000 {
                    w.write_all(insert_skip_existing(&tbl, &cols, &batch.join(",")).as_bytes()).map_err(|e| e.to_string())?;
                    batch.clear();
                }
            } else {
                // A value that is the NULL marker's own text is written as NULL is, and comes back
                // from the import as NULL; nothing in a CSV tells them apart, so it is counted and said.
                if !null_marker.is_empty() { clash += cells.iter().filter(|c| c.as_deref() == Some(null_marker.as_str())).count(); }
                let line = cells.iter().map(csv_field).collect::<Vec<_>>().join(",");
                w.write_all(line.as_bytes()).map_err(|e| e.to_string())?; w.write_all(b"\n").map_err(|e| e.to_string())?;
            }
            n += 1;
            if n.is_multiple_of(2000) { if let Some(a) = &app { let _ = a.emit("export_progress", json!({"rows": n})); } }
        }
        if fmt == "inserts" && !batch.is_empty() && !cancelled {
            w.write_all(insert_skip_existing(&tbl, &cols, &batch.join(",")).as_bytes()).map_err(|e| e.to_string())?;
        }
        if fmt == "inserts" { w.write_all(INSERTS_TAIL.as_bytes()).map_err(|e| e.to_string())?; }
        w.flush().map_err(|e| e.to_string())?;
        drop(w);
        if cancelled {
            let _ = std::fs::remove_file(&file);
            log_line(&format!("EXPORT cancelled: {} ({} rows written before cancel)", tbl, n));
            return Ok(json!({"ok":false,"error":"Export cancelled.","cancelled":true}));
        }
        if let Some(a) = &app { let _ = a.emit("export_progress", json!({"rows": n, "done": true})); }
        log_line(&format!("EXPORT ok: {} rows from {} to {}", n, tbl, file));
        let note = if clash > 0 { format!(" - {} value(s) are the text {}, which this file also uses for NULL: importing it will read them as NULL. Choose another NULL value if they must stay text.", clash, null_marker) } else { String::new() };
        Ok(json!({"ok":true,"message":format!("Exported {} row(s) to {}{}", n, file, note)}))
    }).await.map_err(|e| e.to_string())?
}

// tools_status's checks are expensive enough to notice (a directory tree walk under Program
// Files, and - see below - a real process spawn for mysqldump's --version) that recomputing them
// on every single Settings-dialog open was the actual source of the multi-second delay, not any
// one check in isolation. The paths/flavor can't change out from under the app on their own, so
// this caches the whole result for the life of the process and only recomputes when something
// that could actually invalidate it happens: save_config (the user picked a new path) or a
// successful download_tools (new binaries appeared on disk) both clear it.
fn tools_status_cache() -> &'static Mutex<Option<Value>> {
    static CACHE: OnceLock<Mutex<Option<Value>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

// "MariaDB 12.3.3" or "MySQL 8.4.9" out of a client tool's --version text, so Settings can show what
// a download (or an installation) actually is - downloaded tools never update themselves.
fn tool_version_label(version_text: &str) -> Option<String> {
    let maria = regex::Regex::new(r"(\d+\.\d+\.\d+)-MariaDB").unwrap();
    if let Some(c) = maria.captures(version_text) { return Some(format!("MariaDB {}", &c[1])); }
    let mysql = regex::Regex::new(r"Ver (\d+\.\d+\.\d+)\b.*MySQL").unwrap();
    mysql.captures(version_text).map(|c| format!("MySQL {}", &c[1]))
}
// Windows' loader refuses to start a program whose DLLs are missing, and reports it as this exit
// status rather than anything on stdout. Naming the library turns "no version" into the one thing
// worth knowing: the tools are there but incomplete, and downloading them again is the fix.
const STATUS_DLL_NOT_FOUND: i32 = 0xC000_0135u32 as i32;
// Reading a tool's --version means starting a process, and Settings asks for four of them every
// time it opens - noticeable on a machine whose antivirus inspects each start. The answer cannot
// change unless the file does, so it is remembered per path, with the file's length and mtime as
// the receipt. Kept beside the config so it survives a restart, which is when the wait was worst.
fn tool_stamp(path: &str) -> Option<String> {
    let md = std::fs::metadata(path).ok()?;
    let t = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(format!("{}:{}", md.len(), t))
}
fn tool_version_cache_file() -> std::path::PathBuf { config_file().with_file_name("tool-versions.json") }
fn tool_version_cached(path: &str, stamp: &str) -> Option<String> {
    let raw = std::fs::read_to_string(tool_version_cache_file()).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let e = v.get(path)?;
    if e.get("stamp")?.as_str()? != stamp { return None; }
    e.get("version")?.as_str().map(String::from)
}
fn tool_version_remember(path: &str, stamp: &str, version: &str) {
    let file = tool_version_cache_file();
    let mut v: Value = std::fs::read_to_string(&file).ok()
        .and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_else(|| json!({}));
    v[path] = json!({"stamp": stamp, "version": version});
    let _ = std::fs::write(&file, serde_json::to_string_pretty(&v).unwrap_or_default());
}
fn tool_version(path: &str) -> Option<String> {
    if path.is_empty() || path == "(not found)" { return None; }
    let stamp = tool_stamp(path);
    if let Some(st) = stamp.as_deref() {
        if let Some(v) = tool_version_cached(path, st) { return if v.is_empty() { None } else { Some(v) }; }
    }
    let out = Command::new(path).arg("--version").output().ok()?;
    let label = if out.status.code() == Some(STATUS_DLL_NOT_FOUND) {
        Some("cannot start - a library it needs is missing (download the tools again)".to_string())
    } else {
        tool_version_label(&String::from_utf8_lossy(&out.stdout))
    };
    if let Some(st) = stamp.as_deref() { tool_version_remember(path, st, label.as_deref().unwrap_or("")); }
    label
}

#[tauri::command]
fn tools_status(app: tauri::AppHandle) -> R {
    if let Some(cached) = tools_status_cache().lock().unwrap().clone() { return Ok(cached); }
    fn describe(app: &tauri::AppHandle, base: &str, names: &[&str], env_key: &str) -> (String, String) {
        let cfg = load_cfg();
        if let Some(p) = cfg.get(format!("{}_bin", base)).and_then(|v| v.as_str()) {
            if !p.is_empty() && std::path::Path::new(p).exists() { return (p.to_string(), "configured / downloaded".into()); }
        }
        if let Ok(p) = std::env::var(env_key) { if !p.is_empty() && std::path::Path::new(&p).exists() { return (p, format!("env {}", env_key)); } }
        match resolve_tool(app, base, names, env_key) {
            Ok(p) => {
                let src = if p.contains('/') || p.contains('\\') { "found on system".to_string() } else { "found on PATH".to_string() };
                (p, src)
            }
            Err(_) => ("(not found)".into(), "missing".into()),
        }
    }
    let (m, ms) = describe(&app, "mysql", &["mysql", "mariadb"], "MYSQL_BIN");
    let (d, ds) = describe(&app, "mysqldump", &["mysqldump", "mariadb-dump"], "MYSQLDUMP_BIN");
    // A handful of export options only exist on one dump-tool flavor: --set-gtid-purged is
    // MySQL 5.6+ only, --column-statistics is MySQL 8+ only - MariaDB's mysqldump has neither,
    // and checking either against it aborts the whole export with "unknown variable". Knowing which
    // flavor the dump tool is lets the export dialog grey those options out up front instead of
    // letting the user discover it mid-export. The version string is the only way to tell them
    // apart - but tool_version has already read it (and remembers it per binary), so this reads the
    // answer rather than starting the program a second time.
    let dump_version = tool_version(&d);
    let dump_is_mariadb = if d != "(not found)" {
        dump_version.as_deref().map(|v| v.to_lowercase().contains("mariadb"))
    } else { None };
    let (my_m, my_d) = (mysql_flavor_tool("mysql"), mysql_flavor_tool("mysqldump"));
    let result = json!({
        "ok": true,
        "mysql": m, "mysql_source": ms,
        "mysqldump": d, "mysqldump_source": ds,
        "mysqldump_is_mariadb": dump_is_mariadb,
        "mysql_version": tool_version(&m), "mysqldump_version": dump_version,
        "mysql_for_mysql_version": my_m.as_ref().and_then(|x| tool_version(&x.0)),
        "mysqldump_for_mysql_version": my_d.as_ref().and_then(|x| tool_version(&x.0)),
        "mysql_for_mysql": my_m.as_ref().map(|x| x.0.clone()), "mysql_for_mysql_source": my_m.as_ref().map(|x| x.1.clone()),
        "mysqldump_for_mysql": my_d.as_ref().map(|x| x.0.clone()), "mysqldump_for_mysql_source": my_d.as_ref().map(|x| x.1.clone()),
        "download_dir": tools_dir().to_string_lossy(),
        "config_file": config_file().to_string_lossy()
    });
    *tools_status_cache().lock().unwrap() = Some(result.clone());
    Ok(result)
}

// The tools export and import will use for the CONNECTED server, and whether that mysqldump is
// MariaDB's - the export dialog greys out the options only MySQL's understands.
#[tauri::command]
async fn tools_for_conn(app: tauri::AppHandle, req: Value) -> R {
    tokio::task::spawn_blocking(move || {
        let maria = server_is_mariadb(&req["conn"]);
        let m = resolve_tool_for(&app, "mysql", &["mysql", "mariadb"], "MYSQL_BIN", &req["conn"]).ok();
        let d = resolve_tool_for(&app, "mysqldump", &["mysqldump", "mariadb-dump"], "MYSQLDUMP_BIN", &req["conn"]).ok();
        let dump_maria = d.as_deref().map(client_is_mariadb);
        Ok(json!({"ok":true, "serverIsMariadb": maria, "mysql": m, "mysqldump": d, "mysqldumpIsMariadb": dump_maria}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_config(_app: tauri::AppHandle) -> R {
    Ok(json!({"ok":true, "config": load_cfg(), "mariadbDownloadUrlDefault": DEFAULT_MARIADB_DOWNLOAD_TEMPLATE}))
}

// Whether a path Settings is about to save is the client tool its box asks for, and starts: a
// mistyped path used to be saved without a word and found out at the next export.
// Opens one of the app's own folders in Explorer - only these two, never a path from the page.
#[tauri::command]
fn open_folder(req: Value) -> R {
    let dir = match req["which"].as_str().unwrap_or("") {
        "config" => config_file().parent().map(|p| p.to_path_buf()).unwrap_or_else(std::env::temp_dir),
        "tools" => tools_dir(),
        _ => return Ok(json!({"ok":false,"error":"unknown folder"})),
    };
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(windows)]
    let opener = "explorer";
    #[cfg(not(windows))]
    let opener = "xdg-open";
    match Command::new(opener).arg(&dir).spawn() {
        Ok(_) => Ok(json!({"ok":true})),
        Err(e) => Ok(json!({"ok":false,"error":e.to_string()})),
    }
}

#[tauri::command]
fn check_tool(req: Value) -> R {
    let path = req["path"].as_str().unwrap_or("").trim().to_string();
    let kind = req["kind"].as_str().unwrap_or("");
    if path.is_empty() { return Ok(json!({"ok":true})); }
    if let Some(e) = tool_path_problem(&path, kind) { return Ok(json!({"ok":true,"error":e})); }
    if !std::path::Path::new(&path).is_file() { return Ok(json!({"ok":true,"error":"there is no file at this path"})); }
    match tool_version(&path) {
        Some(v) if v.starts_with("cannot start") => Ok(json!({"ok":true,"error":v})),
        Some(v) => Ok(json!({"ok":true,"version":v})),
        None => Ok(json!({"ok":true,"error":"it does not answer as a MySQL or MariaDB client"})),
    }
}

// Whether a path may be run as a client tool: a local .exe (not \\server\share, not \\?\ or a
// device), called mysql/mariadb or mysqldump/mariadb-dump as its box asks. None when it may.
fn tool_path_problem(path: &str, kind: &str) -> Option<&'static str> {
    if kind != "mysql" && kind != "mysqldump" { return Some("unknown kind of tool"); }
    if path.starts_with("\\\\") || path.starts_with("//") { return Some("a tool has to be on this computer, not on a network share"); }
    let p = std::path::Path::new(path);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let is_dump = stem.contains("dump");
    if kind == "mysqldump" && !is_dump { return Some("this is not mysqldump.exe or mariadb-dump.exe"); }
    if kind == "mysql" && (is_dump || !(stem == "mysql" || stem == "mariadb")) { return Some("this is not mysql.exe or mariadb.exe"); }
    if !p.extension().map(|e| e.eq_ignore_ascii_case("exe")).unwrap_or(false) { return Some("a tool is an .exe file"); }
    None
}

#[tauri::command]
fn save_config(req: Value) -> R {
    let mut cfg = load_cfg();
    // Only what Settings saves, and checked as Settings checks it: a tool path is a MySQL or MariaDB
    // client on this computer, the download address is https. Anything else in config.json is
    // edited by hand or not at all - a page that could write any key could point the app at any
    // program to run, or at another download page to take a checksum from.
    const TOOLS: &[(&str, &str)] = &[("mysql_bin", "mysql"), ("mysqldump_bin", "mysqldump"), ("mysql_bin_mysql", "mysql"), ("mysqldump_bin_mysql", "mysqldump")];
    if let Some(m) = req["config"].as_object() {
        for (k, v) in m {
            let s = v.as_str().unwrap_or("").trim().to_string();
            if let Some((_, kind)) = TOOLS.iter().find(|(n, _)| n == k) {
                if !s.is_empty() { if let Some(e) = tool_path_problem(&s, kind) { return Ok(json!({"ok":false,"error":format!("{k} : {e}")})); } }
            } else if k == "mariadb_download_url_template" {
                if !s.is_empty() && !s.starts_with("https://") { return Ok(json!({"ok":false,"error":"the download address has to start with https://"})); }
            } else {
                return Ok(json!({"ok":false,"error":format!("{k} cannot be set here")}));
            }
            cfg[k] = json!(s);
        }
    }
    match std::fs::write(config_file(), serde_json::to_string_pretty(&cfg).unwrap_or_default()) {
        Ok(_) => {
            // The saved config may have changed mysql_bin/mysqldump_bin - drop the cached
            // tools_status result so the next check reflects the new path instead of a stale one.
            *tools_status_cache().lock().unwrap() = None;
            Ok(json!({"ok":true}))
        }
        Err(e) => Ok(json!({"ok":false,"error":e.to_string()})),
    }
}

// Default MariaDB client-tools download URL template - user-editable in Settings (stored under
// "mariadb_download_url_template" in the same config file as the mysql_bin/mysqldump_bin
// paths). {version} and {file_name} are substituted from the latest LTS release the MariaDB
// API itself reports. A direct mirror is used by default rather than the API's own
// file_download_url field, which has been observed returning an error page (403) instead of
// the actual archive.
const DEFAULT_MARIADB_DOWNLOAD_TEMPLATE: &str = "https://mirror.mariadb.org/mariadb-{version}/winx64-packages/{file_name}";

// The winx64 archive in one patch release's file list, as the release API describes it: its name,
// the API's own download URL, and the SHA-256 it must hash to (None when the API lists no usable
// checksum). Kept out of download_tools() for the same reason as parse_mysql_download_page below -
// so the part that reads someone else's response format can be tested without the network.
fn mariadb_winx64_zip(files: &[Value]) -> Option<(String, String, Option<String>)> {
    let entry = files.iter().find(|f| {
        let n = f["file_name"].as_str().unwrap_or("");
        n.contains("winx64") && n.ends_with(".zip") && !n.contains("debug")
    })?;
    let file_name = entry["file_name"].as_str()?.to_string();
    let api_url = entry["file_download_url"].as_str()?.to_string();
    // Anything that is not a full hex SHA-256 is treated as no checksum at all rather than
    // compared and failed - a truncated or renamed field must read as "cannot be checked".
    let sha256 = entry["checksum"]["sha256sum"].as_str().map(|s| s.trim().to_lowercase())
        .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()));
    Some((file_name, api_url, sha256))
}

// A downloaded archive is the one the release API describes, or it is not installed - the rule the
// MySQL side already applies with its MD5. The checksum always comes from the API, never from the
// mirror that served the bytes: a mirror that returned the wrong archive cannot also vouch for it,
// and the mirror URL here is a user-editable template while the API address is not.
fn verify_sha256(bytes: &[u8], want: &str) -> Result<(), String> {
    use sha2::Digest as _;
    let got = hex::encode(sha2::Sha256::digest(bytes));
    if got == want { Ok(()) } else { Err(format!("checksum mismatch (got {got}, the release API says {want})")) }
}

#[tauri::command]
async fn download_tools() -> R {
    tokio::task::spawn_blocking(download_mariadb_tools).await.map_err(|e| e.to_string())?
}

// The download itself, as a plain blocking function rather than a closure inside the command, so
// that a test can run the real thing end to end - see the_mariadb_client_tools_download_verifies
// _and_extracts. The command above only moves it off the async runtime, and takes no arguments,
// like download_mysql_tools.
fn download_mariadb_tools() -> R {
    let client = reqwest::blocking::Client::builder()
        .user_agent("NOBSSQL-Desktop")
        .timeout(std::time::Duration::from_secs(600))
        .build().map_err(|e| e.to_string())?;
    // 1) latest LTS stable branch
    let root: Value = client.get("https://downloads.mariadb.org/rest-api/mariadb/")
        .send().map_err(|e| e.to_string())?.json().map_err(|e| e.to_string())?;
    let mut branches: Vec<String> = root["major_releases"].as_array().cloned().unwrap_or_default().iter()
        .filter(|r| r["release_status"].as_str() == Some("Stable")
                 && r["release_support_type"].as_str() == Some("Long Term Support"))
        .filter_map(|r| r["release_id"].as_str().map(String::from)).collect();
    branches.sort_by_key(|a| std::cmp::Reverse(ver_key(a)));
    let branch = branches.first().cloned().ok_or("No stable LTS branch found.")?;
    // 2) latest patch version in that branch
    let binfo: Value = client.get(format!("https://downloads.mariadb.org/rest-api/mariadb/{}/", branch))
        .send().map_err(|e| e.to_string())?.json().map_err(|e| e.to_string())?;
    let rel = binfo["releases"].as_object().ok_or("No releases in branch.")?;
    let mut patches: Vec<String> = rel.keys().cloned().collect();
    patches.sort_by_key(|a| std::cmp::Reverse(ver_key(a)));
    let patch = patches.first().cloned().ok_or("No patch version found.")?;
    // 3) winx64 zip (non-debug)
    let files = binfo["releases"][&patch]["files"].as_array().cloned().unwrap_or_default();
    let (file_name, api_url, sha256) = mariadb_winx64_zip(&files)
        .ok_or("No winx64 zip found in the MariaDB release.")?;
    // A download that cannot be checked is not installed - the binaries and authentication
    // plugins unpacked below are executed by this app afterwards.
    let want_sha = sha256.ok_or_else(|| format!(
        "The MariaDB release API listed no SHA-256 checksum for {file_name}, so the download was not attempted."))?;
    // 4) download - the API's own file_download_url has been observed returning an error
    // page (403) instead of the actual archive, which is exactly what produces "Could not
    // find EOCD": the downloaded bytes simply aren't a valid zip at all. A direct mirror URL
    // is more reliable, so it's tried first here, falling back to the API's own URL only if
    // that fails too. The mirror URL is a user-editable template (Settings), not hardcoded,
    // so this can be corrected without a code update if mariadb.org's layout changes again.
    let cfg = load_cfg();
    let template = cfg.get("mariadb_download_url_template").and_then(|v| v.as_str())
        .filter(|s| !s.is_empty()).unwrap_or(DEFAULT_MARIADB_DOWNLOAD_TEMPLATE).to_string();
    let primary_url = template.replace("{version}", &patch).replace("{file_name}", &file_name);
    let mut zipf = None;
    let mut errs: Vec<String> = Vec::new();
    for (label, u) in [("configured download URL", primary_url.as_str()), ("MariaDB API URL", api_url.as_str())] {
        let attempt = client.get(u).send().map_err(|e| e.to_string())
            .and_then(|r| r.bytes().map_err(|e| e.to_string()))
            .and_then(|b| verify_sha256(&b, &want_sha).map(|()| b))
            .and_then(|b| zip::ZipArchive::new(std::io::Cursor::new(b)).map_err(|e| e.to_string()));
        match attempt {
            Ok(z) => { zipf = Some(z); break; }
            Err(e) => errs.push(format!("{}: {}", label, e)),
        }
    }
    let mut zipf = zipf.ok_or_else(|| format!("Could not download a valid archive from either source.\n{}", errs.join("\n")))?;
    // 5) extract wanted client binaries
    let dest = tools_dir();
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let want = ["mysqldump.exe", "mysql.exe", "mysqlimport.exe", "mysqlcheck.exe", "mariadb.exe", "mariadb-dump.exe"];
    // The client AUTHENTICATION plugins, which were not being unpacked at all - without
    // caching_sha2_password the client cannot log in to a stock MySQL 8 server, so export and
    // import failed against one however the connection itself was configured. The archive also
    // carries storage engines and audit plugins; those belong to a server, not here.
    let want_plugins = ["caching_sha2_password.dll", "sha256_password.dll",
                        "client_ed25519.dll", "parsec.dll",
                        "dialog.dll", "mysql_clear_password.dll",
                        "auth_gssapi_client.dll", "authentication_windows_client.dll",
                        "auth_named_pipe.dll"];
    let plugin_dest = dest.join("plugin");
    let mut got: Vec<String> = Vec::new();
    let mut got_plugins = 0usize;
    for i in 0..zipf.len() {
        let mut f = zipf.by_index(i).map_err(|e| e.to_string())?;
        let full = f.name().to_string();
        let base = full.rsplit(['/', '\\']).next().unwrap_or("").to_string();
        if want.contains(&base.as_str()) {
            let out = dest.join(&base);
            let mut o = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut f, &mut o).map_err(|e| e.to_string())?;
            got.push(base);
        } else if want_plugins.contains(&base.as_str())
               && full.replace('\\', "/").contains("/lib/plugin/") {
            // Matched on the archive path too, so these come from lib/plugin and not from
            // something else that happens to share a file name.
            std::fs::create_dir_all(&plugin_dest).map_err(|e| e.to_string())?;
            let mut o = std::fs::File::create(plugin_dest.join(&base)).map_err(|e| e.to_string())?;
            std::io::copy(&mut f, &mut o).map_err(|e| e.to_string())?;
            got_plugins += 1;
        }
    }
    log_line(&format!("download_tools: {} binaries, {} auth plugins", got.len(), got_plugins));
    if got.is_empty() { return Ok(json!({"ok":false,"error":"Downloaded the archive but found no client binaries inside."})); }
    // 6) record paths in config
    let pick = |a: &str, b: &str| -> Option<String> {
        for n in [a, b] { let p = dest.join(n); if p.exists() { return Some(p.to_string_lossy().to_string()); } }
        None
    };
    let mut cfg = load_cfg();
    if let Some(p) = pick("mysql.exe", "mariadb.exe") { cfg["mysql_bin"] = json!(p); }
    if let Some(p) = pick("mysqldump.exe", "mariadb-dump.exe") { cfg["mysqldump_bin"] = json!(p); }
    std::fs::write(config_file(), serde_json::to_string_pretty(&cfg).unwrap_or_default()).map_err(|e| e.to_string())?;
    // New binaries just landed on disk and the config above may point at them now - the
    // cached tools_status result (if any) is stale.
    *tools_status_cache().lock().unwrap() = None;
    Ok(json!({"ok":true, "message": format!("Downloaded MariaDB {} client tools to {}", patch, dest.to_string_lossy()), "config": cfg}))
}

// ---------- MySQL's own client tools ----------
// For machines without a MySQL installation: a MySQL server otherwise gets MariaDB's tools (see
// resolve_tool_for). MySQL has no release API like MariaDB's, but its download page names the
// current Windows ZIP and prints its MD5 beside it. Both addresses can be overridden in
// config.json (mysql_download_page, mysql_download_url_template) if MySQL moves them.
const DEFAULT_MYSQL_DOWNLOAD_PAGE: &str = "https://dev.mysql.com/downloads/mysql/8.4.html";
const DEFAULT_MYSQL_DOWNLOAD_TEMPLATE: &str = "https://cdn.mysql.com/Downloads/MySQL-{series}/{file_name}";
// Where a release goes once a newer one replaces it on the CDN.
const MYSQL_ARCHIVE_TEMPLATE: &str = "https://downloads.mysql.com/archives/get/p/23/file/{file_name}";

// The ZIP archive named on MySQL's download page, its version, and the MD5 printed after it.
fn parse_mysql_download_page(html: &str) -> Option<(String, String, Option<String>)> {
    let c = regex::Regex::new(r"\((mysql-(\d+\.\d+\.\d+)-winx64\.zip)\)").unwrap().captures(html)?;
    let rest = &html[c.get(0)?.end()..];
    let window = &rest[..rest.char_indices().nth(2000).map(|x| x.0).unwrap_or(rest.len())];
    let md5 = regex::Regex::new(r#"class="md5">\s*([0-9a-fA-F]{32})\s*<"#).unwrap()
        .captures(window).map(|m| m[1].to_lowercase());
    Some((c[1].to_string(), c[2].to_string(), md5))
}
// The two binaries the app runs, and the OpenSSL libraries they load. They are NOT self-contained,
// whatever an earlier reading of this said: mysql.exe and mysqldump.exe from the winx64 zip import
// libcrypto-3-x64.dll and libssl-3-x64.dll. Taking only the .exe files left them working on a
// machine that happens to have MySQL installed - its bin is on PATH, and that is where Windows
// found the libraries - and failing on one that does not, with "libcrypto-3-x64.dll was not
// found" from the loader. MariaDB's client tools do not import them, so this is only here.
fn mysql_zip_member(name: &str) -> Option<String> {
    let n = name.replace('\\', "/");
    let mut parts = n.split('/');
    let (_root, bin, file) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || bin != "bin" { return None; }
    let keep = file == "mysql.exe" || file == "mysqldump.exe"
        || ((file.starts_with("libcrypto") || file.starts_with("libssl")) && file.ends_with(".dll"));
    if keep { Some(file.to_string()) } else { None }
}
fn mysql_tools_dir() -> std::path::PathBuf { tools_dir().join("mysql") }

#[tauri::command]
async fn download_mysql_tools() -> R {
    tokio::task::spawn_blocking(download_mysql_tools_blocking).await.map_err(|e| e.to_string())?
}

// Split out of the command for the same reason as download_mariadb_tools: so the download can be
// run by a test (the_mysql_client_tools_download_verifies_and_extracts).
fn download_mysql_tools_blocking() -> R {
    use md5::Digest;
    use std::io::{Read, Seek, Write};
    let client = reqwest::blocking::Client::builder()
        // dev.mysql.com answers a browser-like User-Agent with 403 (it expects the JavaScript a
        // browser would run first) and serves the page to one that says it is curl. Measured.
        .user_agent("curl/8.0 NOBSSQL-Desktop")
        .timeout(std::time::Duration::from_secs(1800))
        .build().map_err(|e| e.to_string())?;
    let cfg = load_cfg();
    let setting = |k: &str, d: &str| cfg.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or(d).to_string();
    // An override is taken only as https: the page is where the checksum comes from.
    let https = |k: &str, d: &str| { let v = setting(k, d); if v.starts_with("https://") { v } else { d.to_string() } };
    let page = https("mysql_download_page", DEFAULT_MYSQL_DOWNLOAD_PAGE);
    let html = client.get(&page).send().and_then(|r| r.error_for_status()).and_then(|r| r.text())
        .map_err(|e| format!("Could not read MySQL's download page {page}: {e}"))?;
    let (file_name, version, md5) = parse_mysql_download_page(&html)
        .ok_or_else(|| format!("MySQL's download page {page} did not name a Windows ZIP archive."))?;
    // A download that cannot be checked is not installed.
    let md5 = md5.ok_or_else(|| format!("MySQL's download page did not show a checksum for {file_name}, so the download was not attempted."))?;
    let series = version.split('.').take(2).collect::<Vec<_>>().join(".");
    let fill = |t: &str| t.replace("{series}", &series).replace("{version}", &version).replace("{file_name}", &file_name);
    let sources = [("download URL", fill(&https("mysql_download_url_template", DEFAULT_MYSQL_DOWNLOAD_TEMPLATE))),
                   ("MySQL archive", fill(MYSQL_ARCHIVE_TEMPLATE))];
    let mut tmp = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut ok = false;
    let mut errs: Vec<String> = Vec::new();
    for (label, url) in &sources {
        let attempt = (|| -> Result<(), String> {
            tmp.set_len(0).map_err(|e| e.to_string())?;
            tmp.rewind().map_err(|e| e.to_string())?;
            let mut resp = client.get(url).send().and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
            let mut hasher = md5::Md5::new();
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let n = resp.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 { break; }
                hasher.update(&buf[..n]);
                tmp.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            }
            let got = hex::encode(hasher.finalize());
            if got != md5 { return Err(format!("checksum mismatch (got {got}, the page says {md5})")); }
            Ok(())
        })();
        match attempt {
            Ok(()) => { ok = true; break; }
            Err(e) => errs.push(format!("{label} {url}: {e}")),
        }
    }
    if !ok { return Ok(json!({"ok":false,"error":format!("Could not download {file_name}.\n{}", errs.join("\n"))})); }
    tmp.rewind().map_err(|e| e.to_string())?;
    let mut zipf = zip::ZipArchive::new(tmp).map_err(|e| e.to_string())?;
    let dest = mysql_tools_dir();
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let mut got: Vec<String> = Vec::new();
    for i in 0..zipf.len() {
        let mut f = zipf.by_index(i).map_err(|e| e.to_string())?;
        let Some(base) = mysql_zip_member(f.name()) else { continue };
        let mut o = std::fs::File::create(dest.join(&base)).map_err(|e| e.to_string())?;
        std::io::copy(&mut f, &mut o).map_err(|e| e.to_string())?;
        got.push(base);
    }
    if !got.iter().any(|g| g == "mysql.exe") || !got.iter().any(|g| g == "mysqldump.exe") {
        return Ok(json!({"ok":false,"error":format!("{file_name} was downloaded and checked, but mysql.exe and mysqldump.exe were not both inside.")}));
    }
    // Without these the binaries above do not start at all on a machine with no MySQL of its own.
    if !got.iter().any(|g| g.starts_with("libcrypto")) {
        return Ok(json!({"ok":false,"error":format!("{file_name} held the client binaries but not the OpenSSL libraries they load (libcrypto-3-x64.dll), so they would not run on a machine without MySQL installed.")}));
    }
    let mut cfg = load_cfg();
    cfg["mysql_bin_mysql"] = json!(dest.join("mysql.exe").to_string_lossy());
    cfg["mysqldump_bin_mysql"] = json!(dest.join("mysqldump.exe").to_string_lossy());
    std::fs::write(config_file(), serde_json::to_string_pretty(&cfg).unwrap_or_default()).map_err(|e| e.to_string())?;
    *tools_status_cache().lock().unwrap() = None;
    log_line(&format!("download_mysql_tools: {file_name}"));
    Ok(json!({"ok":true, "message": format!("Downloaded MySQL {version} client tools to {} (checksum verified)", dest.to_string_lossy()), "config": cfg}))
}

// ---------- update notice ----------
// The app says when a newer release exists and links to it. It never downloads or installs
// anything itself. The UI asks once per start unless that is switched off in Settings.
const RELEASES_REPO: &str = "monsama/nobs-sql-editor";
fn release_is_newer(latest: &str, current: &str) -> bool {
    let key = |v: &str| -> Vec<u64> {
        v.trim().trim_start_matches(['v', 'V']).split('.')
            .map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0)).collect()
    };
    let (mut a, mut b) = (key(latest), key(current));
    let n = a.len().max(b.len());
    a.resize(n, 0); b.resize(n, 0);
    !latest.trim().is_empty() && a > b
}
// Opened through the shell, so only this app's own release pages - never whatever URL arrives.
// Case-insensitive because a GitHub owner and repository name are: the API answers with whatever
// case the repository currently carries, and a rename underneath an already-released build would
// otherwise have it refuse to open the very page its own update check just found. Renaming this
// repository is what turned that from a hypothetical into a measured one.
fn release_page_ok(url: &str) -> bool {
    regex::Regex::new(&format!(r"(?i)^https://github\.com/{}/releases/tag/v\d+(\.\d+){{1,3}}$", regex::escape(RELEASES_REPO)))
        .unwrap().is_match(url)
}

#[tauri::command]
async fn update_check(app: tauri::AppHandle) -> R {
    let current = app.package_info().version.to_string();
    tokio::task::spawn_blocking(move || {
        let client = reqwest::blocking::Client::builder()
            .user_agent("NOBSSQL-Desktop")
            .timeout(std::time::Duration::from_secs(10))
            .build().map_err(|e| e.to_string())?;
        let got = client.get(format!("https://api.github.com/repos/{}/releases/latest", RELEASES_REPO))
            .header("Accept", "application/vnd.github+json")
            .send().and_then(|r| r.error_for_status()).and_then(|r| r.json::<Value>());
        Ok(match got {
            Ok(j) => {
                let tag = j["tag_name"].as_str().unwrap_or("").to_string();
                let url = j["html_url"].as_str().unwrap_or("").to_string();
                json!({"ok": true, "current": current, "latest": tag.trim_start_matches(['v', 'V']),
                       "url": url, "newer": release_is_newer(&tag, &current)})
            }
            Err(e) => json!({"ok": false, "current": current, "error": e.to_string()}),
        })
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn open_release_page(req: Value) -> R {
    let url = req["url"].as_str().unwrap_or("");
    if !release_page_ok(url) { return Ok(json!({"ok": false, "error": "Not a release page of this app."})); }
    #[cfg(target_os = "windows")]
    { std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn().map_err(|e| e.to_string())?; }
    #[cfg(target_os = "macos")]
    { std::process::Command::new("open").arg(url).spawn().map_err(|e| e.to_string())?; }
    #[cfg(all(unix, not(target_os = "macos")))]
    { std::process::Command::new("xdg-open").arg(url).spawn().map_err(|e| e.to_string())?; }
    Ok(json!({"ok": true}))
}

#[tauri::command]
fn app_info(app: tauri::AppHandle) -> R {
    let pi = app.package_info();
    Ok(json!({"ok":true, "name": pi.name, "version": pi.version.to_string()}))
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) { app.exit(0); }
#[tauri::command]
fn quit(app: tauri::AppHandle) { app.exit(0); }

// Opens the Buy Me a Coffee link in the OS's default browser, not this app's own webview -
// deliberately takes no argument and hardcodes the exact URL rather than accepting one from the
// frontend, so this can never become a way to launch an arbitrary command/URL. No existing
// shell/opener plugin is set up in this project, and adding one is a bigger dependency change
// than one static link needs - a plain OS-specific spawn is simpler and has no new attack surface
// beyond "open this one fixed https URL", the same thing every platform's own browser already does.
#[tauri::command]
fn open_support_link(_req: Value) -> Result<(), String> {
    const URL: &str = "https://buymeacoffee.com/monsama";
    #[cfg(target_os = "windows")]
    { std::process::Command::new("cmd").args(["/C", "start", "", URL]).spawn().map_err(|e| e.to_string())?; }
    #[cfg(target_os = "macos")]
    { std::process::Command::new("open").arg(URL).spawn().map_err(|e| e.to_string())?; }
    #[cfg(all(unix, not(target_os = "macos")))]
    { std::process::Command::new("xdg-open").arg(URL).spawn().map_err(|e| e.to_string())?; }
    Ok(())
}

#[tauri::command]
// Companion to save_text for binary content (currently just PNG diagram exports). Sent as a
// plain JSON array of byte values rather than base64 - decoding base64 correctly would need a
// new crate dependency that can't be verified compiles without a working cargo toolchain, while
// a numeric array only needs the array/number extraction serde_json already provides elsewhere
// in this file. The size cost of skipping base64 is irrelevant for a one-off, at-most-a-few-MB
// diagram export.
fn save_binary(req: Value) -> R {
    let path = req["path"].as_str().unwrap_or("");
    if path.is_empty() { return Ok(json!({"ok":false,"error":"no path"})); }
    if !take_save_grant(path) { return Ok(json!({"ok":false,"error":"not a path chosen in the Save dialog"})); }
    let bytes: Vec<u8> = req["bytes"].as_array().map(|a| a.iter().filter_map(|v| v.as_u64().map(|n| n as u8)).collect()).unwrap_or_default();
    match std::fs::write(path, bytes) {
        Ok(_) => Ok(json!({"ok":true})),
        Err(e) => Ok(json!({"ok":false,"error":e.to_string()})),
    }
}

#[tauri::command]
fn save_text(req: Value) -> R {
    let path = req["path"].as_str().unwrap_or("");
    let content = req["content"].as_str().unwrap_or("");
    if path.is_empty() { return Ok(json!({"ok":false,"error":"no path"})); }
    if !take_save_grant(path) { return Ok(json!({"ok":false,"error":"not a path chosen in the Save dialog"})); }
    match std::fs::write(path, content) {
        Ok(_) => Ok(json!({"ok":true})),
        Err(e) => Ok(json!({"ok":false,"error":e.to_string()})),
    }
}

// Password files (options files, the SSH password) a crash or a kill left in %TEMP%: older than a
// day, so nothing another running copy of the app still uses.
fn sweep_secret_temp_files() {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else { return };
    let day = std::time::Duration::from_secs(86_400);
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if !(n.starts_with("nobs-cnf-") || n.starts_with("nobs-ssh-")) { continue; }
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|a| a > day).unwrap_or(false);
        if old { let _ = std::fs::remove_file(e.path()); }
    }
}

fn main() {
    // ssh asks for an SSH password through the program named in SSH_ASKPASS, and that program is
    // this one: started again by ssh with the name of the file that holds the password (see
    // open_tunnel_on), it answers and is gone before any window would open.
    if std::env::var_os("NOBS_SSH_ASKPASS").is_some() {
        use std::io::Write;
        let pw = std::env::var_os("NOBS_SSH_PWFILE").and_then(|f| std::fs::read(f).ok()).unwrap_or_default();
        let _ = std::io::stdout().write_all(&pw);
        return;
    }
    std::thread::spawn(sweep_secret_temp_files);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // The window is made here rather than by the config (create: false) so that the GUI
            // tests can open WebView2's debugging port and give it a data folder of its own.
            // WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS does the first only where WebView2 honours it
            // (not in the elevated session CI runs in), and a second copy of the app otherwise
            // joins the first one's browser, whose port is not the one asked for.
            let cfg = app.config().app.windows.first().cloned().ok_or("no window in tauri.conf.json")?;
            // Debug builds only (the tests run those): an open debugging port lets any program on
            // the machine drive the page and read what it holds, passwords included.
            #[cfg_attr(not(debug_assertions), allow(unused_mut))]
            let mut builder = tauri::WebviewWindowBuilder::from_config(app.handle(), &cfg)?;
            #[cfg(debug_assertions)]
            if let Some(port) = std::env::var("NOBS_WEBVIEW_DEBUG_PORT").ok().and_then(|p| p.parse::<u16>().ok()) {
                // wry's own defaults, which this replaces, plus the port.
                builder = builder.additional_browser_args(&format!(
                    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --remote-debugging-port={port}"));
            }
            #[cfg(debug_assertions)]
            if let Ok(dir) = std::env::var("NOBS_WEBVIEW_DATA_DIR") {
                if !dir.is_empty() { builder = builder.data_directory(std::path::PathBuf::from(dir)); }
            }
            let w = builder.build()?;
            // WebView2 fills fields in and offers to save passwords on its own, the same as the
            // browser it is built from. The page asks it not to (autocomplete="off" on every
            // field), but Chromium treats that as advice rather than instruction for its own
            // autofill, and the "save password?" bubble answers to neither. These two settings are
            // where it is actually decided, so it is decided here: this window shows one local page
            // whose fields hold database credentials and database values, and there is nothing in
            // it worth remembering between sessions.
            #[cfg(windows)]
            {
                use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings4;
                use windows::core::Interface;
                let _ = w.with_webview(|webview| unsafe {
                    if let Ok(core) = webview.controller().CoreWebView2() {
                        if let Ok(settings) = core.Settings() {
                            if let Ok(s4) = settings.cast::<ICoreWebView2Settings4>() {
                                let _ = s4.SetIsGeneralAutofillEnabled(false);
                                let _ = s4.SetIsPasswordAutosaveEnabled(false);
                            }
                        }
                    }
                });
            }
            let _ = w.maximize();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            session_end,
            connect, schemas, objects, ddl, pk, query, exec, rowop, script, script_results, fetch_cursor_batch, close_cursor,
            import, export, importcsv, browse, quit_app, save_text, save_binary, pick_save_path, grant_save_path_for_test, export_table, cancel_export, cancel_job, app_info, get_config, save_config, check_tool, open_folder, download_tools, download_mysql_tools, tools_status, tools_for_conn, update_check, open_release_page, conn_list, conn_get, conn_save, conn_delete, conn_primary, conn_clear, quit, lib_list, lib_save, lib_delete, lib_clear, lib_replace, search_all_schemas, cancel_query, compare_dbs, compare_schemas, compare_apply, compare_tables, compare_rows, compare_rows_apply, compare_rows_diff, compare_rows_apply_diff, compare_cancel, fk, compare_rows_insert_all, compare_rows_fetch_by_pk, gen_user_transfer, process_list, kill_process, schema_erd, open_support_link
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        // ssh.exe is a process of its own and outlives the app unless it is ended here.
        .run(|_app, ev| { if let tauri::RunEvent::Exit = ev { close_tunnels(); } });
}

// ---------------------------------------------------------------------------
// Unit tests for the pure helpers that sit on destructive paths: the read-only
// gate, statement splitting, identifier and literal quoting, and the USE-prefix
// handling that decides WHICH database a script runs against. These are where a
// bug silently loses or corrupts someone's data, and they are all pure
// functions, so there is no reason not to pin their behaviour.
// ---------------------------------------------------------------------------
// What the test server supports, for the live tests that use a feature older servers lack. The
// compatibility suite (.github/workflows/compat.yml) runs every live test against MySQL 5.7 and
// MariaDB 10.2 as well; a test uses what the server has and leaves out only what it cannot have.
#[cfg(test)]
pub(crate) mod caps {
    use super::*;
    pub struct Caps { pub maria: bool, pub ver: (u32, u32, u32), pub tls: bool }
    impl Caps {
        fn since(&self, mysql: (u32, u32, u32), maria: (u32, u32, u32)) -> bool { self.ver >= if self.maria { maria } else { mysql } }
        // Enforced CHECK constraints; MySQL 5.7 parses and ignores them.
        pub fn check(&self) -> bool { self.since((8, 0, 16), (10, 2, 1)) }
        pub fn invisible(&self) -> bool { self.since((8, 0, 23), (10, 3, 3)) }
        pub fn roles(&self) -> bool { self.since((8, 0, 0), (10, 0, 5)) }
        pub fn account_lock(&self) -> bool { self.since((5, 7, 6), (10, 4, 2)) }
        // UUID arrived in MariaDB 10.7, INET4 in 10.10.
        pub fn uuid_inet_types(&self) -> bool { self.maria && self.ver >= (10, 10, 0) }
        // MySQL 9.0's VECTOR (MariaDB's 11.7 VECTOR is written differently and not covered here).
        pub fn mysql_vector(&self) -> bool { !self.maria && self.ver >= (9, 0, 0) }
        // " INVISIBLE" where the server has it, else nothing - the column is then an ordinary one.
        pub fn invisible_kw(&self) -> &'static str { if self.invisible() { " INVISIBLE" } else { "" } }
    }
    pub fn of(conn: &Value) -> Caps {
        let mut c = build_conn(conn).expect("connect to read the server's version");
        let v: String = c.query_first("SELECT VERSION()").unwrap().unwrap_or_default();
        let mut n = v.split(|ch: char| !ch.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse().unwrap_or(0));
        let ver = (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0));
        // Only a server that says outright it has no TLS counts as without it, so a TLS setup that
        // is merely broken still fails the TLS tests.
        let ssl: Option<(String, String)> = c.query_first("SHOW VARIABLES LIKE 'have_ssl'").unwrap_or(None);
        Caps { maria: v.to_lowercase().contains("mariadb"), ver, tls: ssl.map(|s| s.1 != "DISABLED").unwrap_or(true) }
    }
    pub fn from_env() -> Option<Caps> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let d: Vec<&str> = dsn.splitn(4, ':').collect();
        if d.len() != 4 { return None; }
        Some(of(&json!({"host":d[0],"port":d[1],"user":d[2],"password":d[3],"ssl":"default"})))
    }
    // For the TLS tests: false, with a note, on a server built or started without TLS.
    pub fn tls_or_skip() -> bool {
        match from_env() {
            Some(c) if !c.tls => { eprintln!("the server has no TLS (have_ssl=DISABLED) - skipping"); false }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Without an SSH host a connection goes where it says; with one, "verify" is refused with the
    // way out, and a tunnel that cannot open gives ssh's reason (or says ssh is missing).
    #[test]
    fn endpoint_without_ssh_is_the_connection_itself() {
        assert_eq!(endpoint(&json!({"host":"db.example","port":"3307"})).unwrap(), ("db.example".to_string(), 3307));
        assert_eq!(endpoint(&json!({"host":"db.example","port":3306,"sshHost":"  "})).unwrap(), ("db.example".to_string(), 3306));
    }
    #[test]
    fn verify_through_a_tunnel_is_refused_naming_verify_ca() {
        let e = endpoint(&json!({"host":"db","port":"3306","ssl":"verify","sshHost":"bastion.invalid"})).unwrap_err();
        assert!(e.contains("verify-ca"), "{e}");
    }
    // A transfer script makes an account only when it is not there, and stops on the other kind of server.
    #[test]
    fn transfer_script_creates_only_what_is_missing() {
        assert_eq!(create_if_not_exists("CREATE USER `a`@`%` IDENTIFIED BY PASSWORD '*AB'"), "CREATE USER IF NOT EXISTS `a`@`%` IDENTIFIED BY PASSWORD '*AB'");
        assert_eq!(create_if_not_exists("CREATE USER IF NOT EXISTS `a`@`%`"), "CREATE USER IF NOT EXISTS `a`@`%`");
        assert!(transfer_guard(true).contains("VERSION() LIKE '%MariaDB%'"));
        assert!(transfer_guard(false).contains("VERSION() NOT LIKE '%MariaDB%'"));
    }
    // A transfer script, replayed on the server it was made on after its accounts were dropped, gives
    // back the same accounts, grants and roles - on MariaDB and on MySQL. NOBS_TEST_DSN names the
    // server (the fixture's nobs_test database must exist); run with --ignored.
    #[tokio::test]
    #[ignore]
    async fn a_transfer_script_gives_back_what_it_was_made_from() {
        let Ok(dsn) = std::env::var("NOBS_TEST_DSN") else { return };
        let d: Vec<&str> = dsn.splitn(4, ':').collect();
        let conn = json!({"host":d[0],"port":d[1],"user":d[2],"password":d[3],"ssl":"default"});
        let mut c = build_conn(&conn).unwrap();
        let v: String = c.query_first("SELECT VERSION()").unwrap().unwrap();
        let maria = v.to_lowercase().contains("mariadb");
        let cap = caps::of(&conn);
        let role = if maria { "nobs_xfer_role".to_string() } else { "'nobs_xfer_role'@'%'".to_string() };
        let accounts = ["'nobs_xfer_plain'@'%'", "'nobs_xfer_cols'@'localhost'"];
        let clean = |c: &mut Conn| {
            for a in accounts { let _ = c.query_drop(format!("DROP USER IF EXISTS {}", a)); }
            let _ = c.query_drop(format!("DROP ROLE IF EXISTS {}", role));
        };
        let snap = |c: &mut Conn| -> Vec<String> {
            let mut out = Vec::new();
            for a in accounts {
                out.push(c.query_first::<String, _>(format!("SHOW CREATE USER {}", a)).ok().flatten().unwrap_or_else(|| "(missing)".into()));
                let mut g: Vec<String> = c.query(format!("SHOW GRANTS FOR {}", a)).unwrap_or_default(); g.sort(); out.extend(g);
            }
            let mut g: Vec<String> = c.query(format!("SHOW GRANTS FOR {}", role)).unwrap_or_default(); g.sort(); out.extend(g);
            out
        };
        clean(&mut c);
        let lock = if cap.account_lock() { " ACCOUNT LOCK" } else { "" };
        let setup = [format!("CREATE ROLE {}", role), format!("GRANT SELECT ON nobs_test.* TO {}", role),
                  "CREATE USER 'nobs_xfer_plain'@'%' IDENTIFIED BY 'Plain-pw-1'".into(), "GRANT INSERT ON nobs_test.* TO 'nobs_xfer_plain'@'%'".into(),
                  format!("GRANT {} TO 'nobs_xfer_plain'@'%'", role),
                  if maria { format!("SET DEFAULT ROLE {} FOR 'nobs_xfer_plain'@'%'", role) } else { format!("SET DEFAULT ROLE {} TO 'nobs_xfer_plain'@'%'", role) },
                  format!("CREATE USER 'nobs_xfer_cols'@'localhost' IDENTIFIED BY 'Cols-pw-2' WITH MAX_QUERIES_PER_HOUR 100{lock}"),
                  "GRANT SELECT (id) ON nobs_test.ro_canary TO 'nobs_xfer_cols'@'localhost' WITH GRANT OPTION".into()];
        // A server without roles (MySQL 5.7) still transfers its accounts.
        for s in setup.iter().filter(|s| cap.roles() || !s.contains("ROLE") && !s.contains(role.as_str())) {
            c.query_drop(s).unwrap_or_else(|e| panic!("setup {}: {}", s, e));
        }
        let before = snap(&mut c);
        let others: Vec<String> = c.query("SELECT DISTINCT user FROM mysql.user WHERE user NOT LIKE 'nobs\\_xfer\\_%'").unwrap();
        let r = gen_user_transfer(json!({"conn":conn,"exclude":others.join(",")})).await.unwrap();
        let sql = r["sql"].as_str().unwrap().to_string();
        assert_eq!(r["errorCount"], json!(0), "{}", sql);
        clean(&mut c);
        for stmt in split_sql_statements(&sql) {
            c.query_drop(&stmt).unwrap_or_else(|e| panic!("replaying {}: {}\n\n{}", stmt, e, sql));
        }
        for stmt in split_sql_statements(&sql) { c.query_drop(&stmt).unwrap_or_else(|e| panic!("running it again, {}: {}", stmt, e)); }
        let after = snap(&mut c);
        clean(&mut c);
        assert_eq!(before, after);
    }
    // A real tunnel: NOBS_TEST_SSH = host|port|user|key file, and NOBS_TEST_DSN the database the SSH
    // server reaches. Run with --ignored; there is no SSH server in CI.
    #[test]
    #[ignore]
    fn a_query_goes_through_a_real_tunnel() {
        let (Ok(ssh), Ok(dsn)) = (std::env::var("NOBS_TEST_SSH"), std::env::var("NOBS_TEST_DSN")) else { return };
        let s: Vec<&str> = ssh.split('|').collect();
        let d: Vec<&str> = dsn.splitn(4, ':').collect();
        let conn = json!({"host":d[0],"port":d[1],"user":d[2],"password":d[3],"ssl":"default",
            "sshHost":s[0],"sshPort":s[1],"sshUser":s[2],"sshKey":s.get(3).copied().unwrap_or("")});
        for _ in 0..2 {
            let mut c = build_conn(&conn).expect("connect through the tunnel");
            let v: Option<u64> = c.query_first("SELECT 1+1").unwrap();
            assert_eq!(v, Some(2));
        }
        assert_eq!(tunnels().lock().unwrap().len(), 1, "the second connection reused the tunnel");
        close_tunnels();
    }
    #[test]
    fn a_tunnel_that_cannot_open_says_why() {
        let e = endpoint(&json!({"host":"db","port":"3306","sshHost":"127.0.0.1","sshPort":"1","sshUser":"nobody"})).unwrap_err();
        assert!(e.contains("Could not establish SSH tunnel to 127.0.0.1") || e.contains("Could not start ssh"), "{e}");
    }

    #[test]
    fn sql_id_wraps_and_escapes_backticks() {
        assert_eq!(sql_id("users"), "`users`");
        assert_eq!(sql_id("my table"), "`my table`");
        assert_eq!(sql_id("we`ird"), "`we``ird`");
        assert_eq!(sql_id("t` ; DROP TABLE x; --"), "`t`` ; DROP TABLE x; --`");
    }

    #[test]
    fn sql_lit_escapes_quotes_and_backslashes() {
        assert_eq!(sql_lit("plain"), "'plain'");
        assert_eq!(sql_lit("O'Brien"), "'O''Brien'");
        assert_eq!(sql_lit("back\\slash"), "'back\\\\slash'");
        assert_eq!(sql_lit("'; DROP TABLE x; --"), "'''; DROP TABLE x; --'");
    }

    // A trailing backslash right before the string's closing quote is the classic bypass for a
    // quote-only escaper (```'{}'``, s.replace("'","''")``` alone): the backslash would combine
    // with the literal `'` MySQL emits right after it to produce an escaped quote, closing the
    // string one character early and leaving whatever follows to execute as SQL. sql_str_lit
    // (and sql_lit, which delegates to it) escape the backslash FIRST so this can't happen -
    // objects()/pk()/fk()/schema_erd() and friends now go through this instead of an ad-hoc
    // single-quote-only replace() that didn't have this protection.
    #[test]
    fn sql_str_lit_neutralizes_trailing_backslash_quote_bypass() {
        assert_eq!(sql_str_lit("x\\"), "'x\\\\'");
        assert_eq!(sql_lit("x\\"), "'x\\\\'");
    }

    #[test]
    fn cnf_safe_strips_embedded_newlines() {
        assert_eq!(cnf_safe("normal-host"), "normal-host");
        assert_eq!(cnf_safe("evil\npager=touch /tmp/pwned"), "evilpager=touch /tmp/pwned");
        assert_eq!(cnf_safe("evil\r\nmore"), "evilmore");
    }

    // Row data is written for its column's type. Going by the value's shape, a text value '0x41'
    // was stored as the byte A, and an empty binary value - shown as the bare 0x - as the two
    // characters 0x.
    #[test]
    fn values_are_written_for_their_column_type() {
        assert_eq!(sql_val_for(Some("0xDEADBEEF"), true), "0xDEADBEEF");
        assert_eq!(sql_val_for(Some("0x00"), true), "0x00");
        assert_eq!(sql_val_for(Some("0x"), true), "X''");
        assert_eq!(sql_val_for(Some("0xZZ"), true), "'0xZZ'");
        assert_eq!(sql_val_for(None, true), "NULL");
        assert_eq!(sql_val_for(Some("0x41"), false), "'0x41'");
        assert_eq!(sql_val_for(Some("0x"), false), "'0x'");
        assert_eq!(sql_val_for(Some("NULL"), false), "'NULL'");
        assert_eq!(sql_val_for(None, false), "NULL");
        assert_eq!(json_val_for(&json!(null), false), "NULL");
        assert_eq!(json_val_for(&json!("0x"), true), "X''");
        assert_eq!(json_val_for(&json!(5), false), "'5'");
    }

    // mysql.exe reading a script turns every CR LF into LF, so a raw CR before a line feed was
    // dropped from any value that went through it. Measured with both clients.
    #[test]
    fn a_carriage_return_is_escaped_in_literals() {
        assert_eq!(sql_str_lit("a\r\nb"), "'a\\r\nb'");
        assert_eq!(sql_str_lit("a\0b"), "'a\\0b'");
        assert_eq!(sql_val_for(Some("x\r"), false), "'x\\r'");
    }

    #[test]
    fn read_only_allows_reads() {
        for sql in ["SELECT 1", "select * from t", "SHOW TABLES", "EXPLAIN SELECT 1",
                    "DESCRIBE t", "WITH x AS (SELECT 1) SELECT * FROM x", "", "   "] {
            assert!(sql_is_readonly(sql), "should be allowed: {:?}", sql);
        }
    }

    #[test]
    fn read_only_blocks_writes() {
        for sql in ["DELETE FROM t", "delete from t", "UPDATE t SET a=1", "INSERT INTO t VALUES (1)",
                    "DROP TABLE t", "TRUNCATE t", "ALTER TABLE t ADD c INT", "CREATE TABLE t (a INT)",
                    "GRANT ALL ON *.* TO x", "SELECT 1; DELETE FROM t"] {
            assert!(!sql_is_readonly(sql), "should be blocked: {:?}", sql);
        }
    }

    #[test]
    fn read_only_reads_comments_as_the_server_does() {
        // "--" without a space is two minus signs, and nothing in quotes is a comment: each of
        // these hid a DELETE from the check while the server ran it.
        for sql in ["SELECT 1--1; DELETE FROM t", "SELECT '#'; DELETE FROM t", "SELECT \"--\"; DELETE FROM t",
                    "SELECT '/*'; DELETE FROM t; SELECT '*/'", "SELECT `#x`; DELETE FROM t",
                    // a backslash that is an escape on one server and an ordinary character on another
                    "SELECT 'a\\'; DELETE FROM t; SELECT '", "SELECT 'a\\''; DELETE FROM t; -- '",
                    "/*M!100100 DELETE FROM t */", "SELECT 1 /*!50000 ; DELETE FROM t */"] {
            assert!(!sql_is_readonly(sql), "should be blocked: {:?}", sql);
        }
        for sql in ["SELECT 1 -- DELETE FROM t", "SELECT 1 # DELETE FROM t", "SELECT 1 /* DELETE FROM t */",
                    "SELECT 1--1", "SELECT '--', '#', '/*' FROM t", "SELECT 1 --\tDELETE"] {
            assert!(sql_is_readonly(sql), "should be allowed: {:?}", sql);
        }
    }

    #[test]
    fn read_only_sees_through_comments() {
        assert!(!sql_is_readonly("/* harmless */ DELETE FROM t"));
        assert!(!sql_is_readonly("-- comment\nDELETE FROM t"));
        assert!(!sql_is_readonly("# comment\nDELETE FROM t"));
        assert!(sql_is_readonly("SELECT 1 -- DELETE FROM t"));
    }

    #[test]
    fn read_only_blocks_mysql_executable_comments() {
        // MySQL EXECUTES the body of /*! ... */ - it is a version-gated directive, not a
        // comment - so stripping it before the keyword check lets a write through.
        assert!(!sql_is_readonly("/*!50000 DELETE FROM t */"));
        assert!(!sql_is_readonly("SELECT 1; /*!DROP TABLE t */"));
    }

    #[test]
    fn read_only_blocks_cte_prefixed_writes() {
        assert!(sql_is_readonly("WITH x AS (SELECT 1) SELECT * FROM x"));
        assert!(sql_is_readonly("WITH x AS (SELECT 1), y AS (SELECT 2) SELECT * FROM x, y"));
        assert!(!sql_is_readonly("WITH x AS (SELECT 1) DELETE FROM t WHERE id IN (SELECT id FROM x)"));
        assert!(!sql_is_readonly("WITH x AS (SELECT 1) UPDATE t SET a=1"));
        assert!(!sql_is_readonly("WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x"));
    }

    // A ')' - or a keyword - inside a quoted string isn't a real paren/token: without tracking
    // quote state, this exact statement's embedded ")SELECT(" leaked out as an exposed depth-0
    // "SELECT", which "first verb wins" then picked over the real (and dangerous) trailing DELETE.
    #[test]
    fn read_only_blocks_cte_with_paren_in_string_literal() {
        assert!(!sql_is_readonly("WITH x AS (SELECT 1 FROM t WHERE a=')SELECT(') DELETE FROM t"));
        assert!(!sql_is_readonly("WITH x AS (SELECT 1 FROM t WHERE a=\")SELECT(\") DELETE FROM t"));
        assert!(sql_is_readonly("WITH x AS (SELECT 1 FROM t WHERE a=')DELETE(') SELECT * FROM x"));
    }

    #[test]
    fn read_only_blocks_analyze_wrapped_writes() {
        // ANALYZE TABLE is a genuinely read-only maintenance statement.
        assert!(sql_is_readonly("ANALYZE TABLE t"));
        assert!(sql_is_readonly("analyze table t, t2"));
        // MariaDB's ANALYZE [FORMAT=JSON] <statement> form actually EXECUTES the statement it
        // wraps - a SELECT is fine, anything else must be blocked exactly like it would be
        // unwrapped.
        assert!(sql_is_readonly("ANALYZE SELECT 1"));
        assert!(sql_is_readonly("ANALYZE FORMAT=JSON SELECT * FROM t"));
        assert!(!sql_is_readonly("ANALYZE DELETE FROM t"));
        assert!(!sql_is_readonly("ANALYZE INSERT INTO t VALUES (1)"));
        assert!(!sql_is_readonly("ANALYZE UPDATE t SET a=1"));
        assert!(!sql_is_readonly("ANALYZE FORMAT=JSON DELETE FROM t"));
    }

// SET is allow-listed because a session variable is harmless, and that allow-listing was
    // letting three SET forms that are not variable assignments at all straight through on a
    // connection the user had marked read-only.
    // SELECT is allow-listed, and INTO OUTFILE / INTO DUMPFILE hang off a SELECT. They write no
    // table data - they write a FILE, on the database server, as the mysqld user. Verified against
    // a live MariaDB with an empty secure_file_priv: read-only mode reported the statement as
    // allowed and the file appeared on disk with the expected contents.
    #[test]
    fn read_only_blocks_select_into_outfile() {
        assert!(!sql_is_readonly("SELECT * FROM t INTO OUTFILE '/tmp/x.csv'"));
        assert!(!sql_is_readonly("SELECT * FROM t INTO DUMPFILE '/tmp/x.bin'"));
        assert!(!sql_is_readonly("select 1 into outfile '/tmp/x'"));
        // MySQL also accepts the clause before FROM.
        assert!(!sql_is_readonly("SELECT * INTO OUTFILE '/tmp/x' FROM t"));
        // Reachable behind the other allow-listed prefixes too.
        assert!(!sql_is_readonly("WITH x AS (SELECT 1) SELECT * FROM x INTO OUTFILE '/tmp/x'"));
        assert!(!sql_is_readonly("SELECT 1; SELECT * FROM t INTO OUTFILE '/tmp/x'"));

        // SELECT ... INTO @var is an ordinary variable assignment, not a file write.
        assert!(sql_is_readonly("SELECT COUNT(*) INTO @n FROM t"));
        assert!(sql_is_readonly("SELECT a, b INTO @x, @y FROM t"));
        // An INTO OUTFILE that is only ever text inside a string is not a clause.
        assert!(sql_is_readonly("SELECT 'INTO OUTFILE' AS s"));
        assert!(sql_is_readonly("SELECT * FROM t WHERE note = 'dump INTO OUTFILE now'"));
        // A column that merely happens to be called outfile is not the clause either.
        assert!(sql_is_readonly("SELECT outfile FROM t"));
        // Ordinary reads keep working.
        assert!(sql_is_readonly("SELECT * FROM t"));
    }

    #[test]
    fn read_only_blocks_set_forms_that_are_not_session_variables() {
        // Changes an account's credentials - any account, root included.
        assert!(!sql_is_readonly("SET PASSWORD FOR 'u'@'%' = PASSWORD('x')"));
        assert!(!sql_is_readonly("SET PASSWORD = PASSWORD('x')"));
        assert!(!sql_is_readonly("set password for 'u'@'%' = 'x'"));
        // Grants a role to an account.
        assert!(!sql_is_readonly("SET DEFAULT ROLE admin FOR 'u'@'%'"));
        // MariaDB's SET STATEMENT ... FOR <statement> EXECUTES the statement it wraps, the same
        // way ANALYZE <statement> does. Verified against a live server: "... FOR DELETE FROM t"
        // emptied the table while read-only mode reported the statement as allowed.
        assert!(!sql_is_readonly("SET STATEMENT max_statement_time=1 FOR DELETE FROM t"));
        assert!(!sql_is_readonly("SET STATEMENT max_statement_time=1 FOR DROP TABLE t"));
        assert!(!sql_is_readonly("SET STATEMENT a=1, b=2 FOR UPDATE t SET x=1"));
        // ...but the wrapped statement is judged on its own merits, so a read it wraps is fine.
        assert!(sql_is_readonly("SET STATEMENT max_statement_time=1 FOR SELECT 1"));
        assert!(sql_is_readonly("SET STATEMENT max_statement_time=1 FOR SHOW TABLES"));
        // An unrecognised SET STATEMENT form is refused rather than guessed at.
        assert!(!sql_is_readonly("SET STATEMENT max_statement_time=1"));
        // Ordinary session assignments must keep working - this guard is worthless if it makes
        // read-only mode unusable for actual work.
        assert!(sql_is_readonly("SET autocommit=0"));
        assert!(sql_is_readonly("SET SESSION sql_mode='STRICT_TRANS_TABLES'"));
        assert!(sql_is_readonly("SET NAMES utf8mb4"));
        assert!(sql_is_readonly("SET @x = 1"));
    }

    // A FOR inside a string literal is not the separator, so it must not be where the statement
    // gets split - otherwise the "wrapped statement" checked is a fragment, not what will run.
    #[test]
    fn split_off_keyword_ignores_quoted_and_partial_matches() {
        assert_eq!(split_off_keyword("SET STATEMENT x=1 FOR SELECT 1", "FOR", true), Some("SELECT 1".to_string()));
        assert_eq!(split_off_keyword("SET STATEMENT x='FOR' FOR DELETE FROM t", "FOR", true), Some("DELETE FROM t".to_string()));
        // FORMAT starts with FOR but is not it.
        assert_eq!(split_off_keyword("ANALYZE FORMAT=JSON SELECT 1", "FOR", true), None);
        assert_eq!(split_off_keyword("SELECT for_id FROM t", "FOR", true), None);
        assert_eq!(split_off_keyword("SELECT 1", "FOR", true), None);
    }

    // A saved connection's passwords are filled in by name, and only for the address they were saved
    // for, with that connection's own SSL settings.
    #[test]
    fn saved_passwords_go_only_where_they_were_saved_for() {
        let prof = json!({"name":"prod","host":"db.example","port":"3306","user":"app","ssl":"verify-ca","sslCa":"C:/ca.pem","clearPw":false,"sshHost":"","sshPort":"","sshUser":"","sshKey":""});
        let pw = || "secret".to_string(); let none = || String::new();
        let asked = json!({"savedName":"prod","host":"DB.example ","port":3306,"user":"app","password":"","ssl":"disabled"});
        let r = resolve_with(&asked, Some(&prof), pw, none);
        assert_eq!(r["password"], "secret", "the same address gets the saved password");
        assert_eq!(r["ssl"], "verify-ca", "and the saved SSL setting, not the one the page sent");
        for (k, v) in [("host", json!("evil.example")), ("port", json!("3307")), ("user", json!("root")), ("sshHost", json!("jump"))] {
            let mut other = asked.clone(); other[k] = v;
            let r = resolve_with(&other, Some(&prof), pw, none);
            assert_eq!(r["password"], "", "another {k} gets no password");
        }
        let typed = json!({"savedName":"prod","host":"db.example","port":"3306","user":"app","password":"typed"});
        assert_eq!(resolve_with(&typed, Some(&prof), pw, none)["password"], "typed", "a typed password is used as typed");
        assert_eq!(resolve_with(&asked, None, pw, none)["password"], "", "an unknown name gets nothing");
    }

    // The gaps a security review found in the read-only check.
    #[test]
    fn read_only_security_review_gaps() {
        // A backslash is not an escape inside backticks: `a\` is the identifier a\ and the FOR
        // after it is real. In '...' it escapes only without NO_BACKSLASH_ESCAPES.
        assert_eq!(split_off_keyword("SET STATEMENT `a\\`=1 FOR DELETE FROM t", "FOR", true), Some("DELETE FROM t".to_string()));
        assert_eq!(split_off_keyword("SET STATEMENT x='\\' FOR DELETE FROM t", "FOR", false), Some("DELETE FROM t".to_string()));
        assert_eq!(split_off_keyword("SET STATEMENT x='\\' FOR DELETE FROM t", "FOR", true), None);
        assert!(strip_parens("WITH x AS (SELECT `a\\`) DELETE FROM t", true).contains("DELETE"));
        assert!(strip_parens("WITH x AS (SELECT '\\') DELETE FROM t", false).contains("DELETE"));
        // GLOBAL / PERSIST in any assignment, not only the first.
        assert!(!sql_is_readonly("SET @a = 1, GLOBAL max_connections = 1"));
        assert!(!sql_is_readonly("SET SESSION wait_timeout = 10, PERSIST max_connections = 1"));
        assert!(!sql_is_readonly("SET @a = 1, PERSIST_ONLY max_connections = 1"));
        assert!(!sql_is_readonly("SET RESOURCE GROUP rg FOR 12"));
        assert!(sql_is_readonly("SET @a = 'GLOBAL', @b = 2"));
        // EXPLAIN ANALYZE runs what it profiles.
        assert!(!sql_is_readonly("EXPLAIN ANALYZE DELETE t1 FROM t1 JOIN t2 ON t1.id = t2.id"));
        assert!(!sql_is_readonly("EXPLAIN ANALYZE FORMAT=TREE UPDATE t1, t2 SET t1.a = 1"));
        assert!(!sql_is_readonly("DESCRIBE ANALYZE DELETE t1 FROM t1, t2"));
        assert!(!sql_is_readonly("EXPLAIN ANALYZE SELECT 1 INTO OUTFILE '/tmp/x'"));
        assert!(sql_is_readonly("EXPLAIN ANALYZE SELECT * FROM t"));
        assert!(sql_is_readonly("EXPLAIN ANALYZE FORMAT=TREE SELECT * FROM t"));
        assert!(sql_is_readonly("EXPLAIN DELETE FROM t"));
        // Every INTO, not only the first.
        assert!(!sql_is_readonly("SELECT a INTO @x FROM t UNION SELECT b FROM t INTO OUTFILE '/tmp/x'"));
        assert!(!sql_is_readonly("SELECT 'into' AS a INTO DUMPFILE '/tmp/x'"));
        assert!(sql_is_readonly("SELECT a INTO @x FROM t"));
        assert!(sql_is_readonly("SELECT 'INTO OUTFILE' AS a"));
    }


    #[test]
    fn read_only_blocks_server_state_changes() {
        // SET on a session variable is harmless; GLOBAL and PERSIST change the server for
        // everyone, which "read-only / safe mode" should not permit.
        assert!(sql_is_readonly("SET autocommit=0"));
        assert!(!sql_is_readonly("SET GLOBAL max_connections=1"));
        assert!(!sql_is_readonly("SET PERSIST max_connections=1"));
    }

    #[test]
    fn split_handles_plain_statements() {
        assert_eq!(split_sql_statements("SELECT 1; SELECT 2"), vec!["SELECT 1", "SELECT 2"]);
        assert_eq!(split_sql_statements("SELECT 1;"), vec!["SELECT 1"]);
        assert_eq!(split_sql_statements("   "), Vec::<String>::new());
    }

    #[test]
    fn split_does_not_break_on_semicolons_inside_strings() {
        assert_eq!(split_sql_statements("SELECT 'a;b'"), vec!["SELECT 'a;b'"]);
        assert_eq!(split_sql_statements("SELECT \"a;b\""), vec!["SELECT \"a;b\""]);
        assert_eq!(split_sql_statements("SELECT 'it''s;fine'"), vec!["SELECT 'it''s;fine'"]);
    }

    #[test]
    fn split_honours_delimiter_directive() {
        let sql = "DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END$$\nDELIMITER ;";
        let out = split_sql_statements(sql);
        assert_eq!(out.len(), 1, "procedure body must stay one statement, got {:?}", out);
        assert!(out[0].contains("SELECT 1; SELECT 2"), "body was split: {:?}", out);
    }

    #[test]
    fn strip_use_reports_last_database_and_remainder() {
        let (db, rest) = strip_leading_use_statements("USE shop; SELECT 1");
        assert_eq!(db.as_deref(), Some("shop"));
        assert_eq!(rest.trim(), "SELECT 1");

        let (db, _) = strip_leading_use_statements("USE `a b`; USE second; SELECT 1");
        assert_eq!(db.as_deref(), Some("second"), "the LAST USE wins");

        let (db, rest) = strip_leading_use_statements("SELECT 1");
        assert_eq!(db, None);
        assert_eq!(rest.trim(), "SELECT 1");
    }

    #[test]
    fn row_key_distinguishes_rows_that_differ_only_by_field_boundary() {
        let a = vec![Some("a".to_string()), Some("b".to_string())];
        let b = vec![Some("ab".to_string()), None];
        assert_ne!(row_key(&a), row_key(&b));
        assert_eq!(row_key(&[None]), row_key(&[Some(String::new())]));
    }

    #[test]
    fn ver_key_orders_numerically_not_lexically() {
        assert!(ver_key("11.4.2") > ver_key("9.9.9"), "11.x must beat 9.x");
        assert!(ver_key("10.11.0") > ver_key("10.9.0"), "10.11 must beat 10.9");
    }

    // The whole point of table_filter_args is to never build a command line that scales with a
    // huge table count - below its 40-exclusion threshold it must stay on the cheap --ignore-table
    // path without ever touching the database (a bogus connection here would panic/error out if
    // it did), and it must ignore exclusions that belong to a different database entirely.
    #[test]
    fn table_filter_args_stays_on_ignore_table_path_under_threshold() {
        let bogus_conn = json!({"host":"unreachable.invalid","port":3306,"user":"x","password":"x"});
        let mut excl = std::collections::HashSet::new();
        excl.insert("mydb.orders".to_string());
        excl.insert("mydb.customers".to_string());
        excl.insert("otherdb.orders".to_string()); // different database - must be filtered out
        let (ignore_args, positional) = table_filter_args("mydb", &excl, &bogus_conn).unwrap();
        assert!(positional.is_empty());
        assert_eq!(ignore_args.len(), 2);
        assert!(ignore_args.contains(&"--ignore-table=mydb.orders".to_string()));
        assert!(ignore_args.contains(&"--ignore-table=mydb.customers".to_string()));
        assert!(!ignore_args.iter().any(|a| a.contains("otherdb")));
    }

    #[test]
    fn table_filter_args_is_a_noop_with_no_exclusions() {
        let bogus_conn = json!({"host":"unreachable.invalid","port":3306,"user":"x","password":"x"});
        let excl = std::collections::HashSet::new();
        let (ignore_args, positional) = table_filter_args("mydb", &excl, &bogus_conn).unwrap();
        assert!(ignore_args.is_empty() && positional.is_empty());
    }

    #[test]
    fn tool_search_dirs_covers_every_real_install_layout() {
        let root = std::env::temp_dir().join(format!("nobs-tooltest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layouts = ["Program Files/MariaDB 11.4/bin",
                       "Program Files/MySQL/MySQL Server 8.0/bin",
                       "wamp64/bin/mariadb/mariadb11.4/bin"];
        for l in layouts { std::fs::create_dir_all(root.join(l)).unwrap(); }
        std::fs::create_dir_all(root.join("Program Files/Unrelated/bin")).unwrap();

        let bases = vec![root.join("Program Files"), root.join("wamp64/bin")];
        let direct = vec![root.join("xampp/mysql/bin")];
        let dirs = tool_search_dirs(&bases, &direct);

        for l in layouts { assert!(dirs.contains(&root.join(l)), "layout not searched: {}", l); }
        assert!(dirs.contains(&root.join("xampp/mysql/bin")), "direct dir not searched");
        assert!(!dirs.iter().any(|d| d.to_string_lossy().contains("Unrelated")),
                "unrelated Program Files entry should be skipped");
        let _ = std::fs::remove_dir_all(&root);
    }

    // A MySQL server gets MySQL's own tools when an installation has them, newest version first.
    // Version folders are compared as numbers, so 8.10 comes before 8.4.
    // Verbatim from https://dev.mysql.com/downloads/mysql/8.4.html (September 2026): the MSI row,
    // then the ZIP row. The MD5 has to be the one printed for the ZIP, not the MSI's before it.
    const MYSQL_PAGE: &str = r#"<td class="sub-text">(mysql-8.4.11-winx64.msi)</td>
            <td class="sub-text" style="text-align:right;" colspan="4">
                MD5: <code class="md5">b5c515a0f410cd6903cd41057ed5d662</code> |
        </tr>
                            <td class="col1"><b>Windows (x86, 64-bit), ZIP Archive</b></td>
                        <td class="col3">8.4.11</td>
            <td class="col4">268.2M</td>
                                <div class="button03"><a href="/downloads/file/?id=556213">Download</a></div>
            <td class="sub-text">(mysql-8.4.11-winx64.zip)</td>
            <td class="sub-text" style="text-align:right;" colspan="4">
                MD5: <code class="md5">2e833921898a9a030ea6bfe81bd811bc</code> |
            <td class="sub-text">(mysql-8.4.11-winx64-debug-test.zip)</td>
                MD5: <code class="md5">00000000000000000000000000000000</code> |"#;

    #[test]
    fn the_mysql_download_page_gives_the_zip_and_its_checksum() {
        let (file, ver, md5) = parse_mysql_download_page(MYSQL_PAGE).unwrap();
        assert_eq!(file, "mysql-8.4.11-winx64.zip");
        assert_eq!(ver, "8.4.11");
        assert_eq!(md5.as_deref(), Some("2e833921898a9a030ea6bfe81bd811bc"));
        assert!(parse_mysql_download_page("<html>nothing here</html>").is_none());
        let (_, _, none) = parse_mysql_download_page("(mysql-9.1.0-winx64.zip) no checksum").unwrap();
        assert!(none.is_none(), "no checksum on the page must not produce one");
    }

    // One patch release's file list, shaped like the real https://downloads.mariadb.org/rest-api/
    // response: the debug archive first (so picking the first winx64 zip would be wrong), the one
    // that is wanted second, and a source tarball that has no business being chosen.
    fn mariadb_files() -> Vec<Value> {
        vec![
            json!({"file_name":"mariadb-11.8.9-winx64-debug.zip","file_download_url":"https://dlm.mariadb.com/debug",
                   "checksum":{"sha256sum":"1111111111111111111111111111111111111111111111111111111111111111"}}),
            json!({"file_name":"mariadb-11.8.9-winx64.zip","file_download_url":"https://dlm.mariadb.com/winx64",
                   "checksum":{"md5sum":"d41d8cd98f00b204e9800998ecf8427e",
                               "sha256sum":"830C46727D9278EAE212AE3ECA44EEB9E71B2A68704E95F344A64FBA7B1963F5"}}),
            json!({"file_name":"mariadb-11.8.9.tar.gz","file_download_url":"https://dlm.mariadb.com/src",
                   "checksum":{"sha256sum":"2222222222222222222222222222222222222222222222222222222222222222"}}),
        ]
    }

    #[test]
    fn the_mariadb_api_gives_the_winx64_zip_and_its_checksum() {
        let (file, url, sha) = mariadb_winx64_zip(&mariadb_files()).unwrap();
        assert_eq!(file, "mariadb-11.8.9-winx64.zip");
        assert_eq!(url, "https://dlm.mariadb.com/winx64");
        // Lower-cased, so it compares equal to what hex::encode produces.
        assert_eq!(sha.as_deref(), Some("830c46727d9278eae212ae3eca44eeb9e71b2a68704e95f344a64fba7b1963f5"));
        assert!(mariadb_winx64_zip(&[]).is_none());
        assert!(mariadb_winx64_zip(&[json!({"file_name":"mariadb-11.8.9-winx64-debug.zip","file_download_url":"u"})]).is_none(),
            "only a debug archive must not be offered as the client tools");
    }

    #[test]
    fn a_checksum_that_is_not_a_sha256_reads_as_no_checksum() {
        // Each of these must leave the caller refusing the download rather than comparing against
        // something that cannot match: missing, truncated, and not hex.
        for entry in [json!({"file_name":"mariadb-1-winx64.zip","file_download_url":"u"}),
                      json!({"file_name":"mariadb-1-winx64.zip","file_download_url":"u","checksum":{"md5sum":"d41d8cd98f00b204e9800998ecf8427e"}}),
                      json!({"file_name":"mariadb-1-winx64.zip","file_download_url":"u","checksum":{"sha256sum":"830c4672"}}),
                      json!({"file_name":"mariadb-1-winx64.zip","file_download_url":"u","checksum":{"sha256sum":"not hex, sixty-four characters long, but still not hexadecimal!!"}})] {
            let (_, _, sha) = mariadb_winx64_zip(std::slice::from_ref(&entry)).unwrap();
            assert!(sha.is_none(), "{entry} must not yield a checksum");
        }
    }

    #[test]
    fn an_archive_is_accepted_only_when_it_hashes_to_what_the_api_said() {
        // The published SHA-256 of "abc", so this asserts the real digest and not just that two
        // calls agree with each other.
        const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify_sha256(b"abc", ABC).is_ok());
        let wrong = verify_sha256(b"abd", ABC).unwrap_err();
        assert!(wrong.contains("checksum mismatch"), "{wrong}");
        assert!(wrong.contains(ABC), "the message must name what was expected: {wrong}");
        // A truncated download is the realistic failure, not a crafted one.
        assert!(verify_sha256(b"ab", ABC).is_err());
        assert!(verify_sha256(b"", ABC).is_err());
    }

    // The real download, against mariadb.org: the one path that exercises the release API's
    // current response shape, the checksum, and the zip crate against a genuine 90 MB archive.
    // Nothing else covers it - the GUI scenario only checks that Settings draws the buttons - so a
    // zip or API change would otherwise surface as a user's failed download. CI runs it (see
    // .github/workflows/test.yml, the tools-download job); it is #[ignore]d and env-gated on top
    // of that because it downloads 90 MB and installs into the config directory, which is not
    // something `cargo test -- --include-ignored` should do to a developer's own tools.
    #[test]
    #[ignore]
    fn the_mariadb_client_tools_download_verifies_and_extracts() {
        if std::env::var("NOBS_TEST_TOOLS_DOWNLOAD").is_err() {
            eprintln!("NOBS_TEST_TOOLS_DOWNLOAD not set - skipping"); return;
        }
        let r = download_mariadb_tools().expect("the download returned an error");
        assert_eq!(r["ok"], json!(true), "{r}");
        // The two the app actually runs, and one authentication plugin - the plugins live deeper in
        // the archive and were once missed entirely, so they are worth asserting separately.
        for exe in ["mysql.exe", "mysqldump.exe"] {
            let p = tools_dir().join(exe);
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            assert!(size > 1_000_000, "{} is missing or truncated ({size} bytes)", p.display());
        }
        assert!(tools_dir().join("plugin").join("caching_sha2_password.dll").exists(),
            "the client authentication plugins were not extracted");
        // And the config now points at what was just unpacked, which is what makes export work.
        let cfg = load_cfg();
        assert!(cfg["mysql_bin"].as_str().unwrap_or("").ends_with(".exe"), "{cfg}");
    }

    // The MySQL download, same idea as the MariaDB one above and for a reason of its own: MySQL
    // publishes no release API, so the version, the file name and the MD5 are all read out of the
    // HTML of dev.mysql.com's download page (parse_mysql_download_page). A restyle of that page
    // breaks the regex, and because the code then correctly refuses to install what it cannot
    // check, the feature stops working silently - nothing fails until a user clicks the button.
    // This is the check that notices. ~270 MB, so CI runs it on the weekly schedule rather than on
    // every push (see .github/workflows/test.yml).
    #[test]
    #[ignore]
    fn the_mysql_client_tools_download_verifies_and_extracts() {
        if std::env::var("NOBS_TEST_TOOLS_DOWNLOAD").is_err() {
            eprintln!("NOBS_TEST_TOOLS_DOWNLOAD not set - skipping"); return;
        }
        let r = download_mysql_tools_blocking().expect("the download returned an error");
        assert_eq!(r["ok"], json!(true), "{r}");
        // Only these two are kept out of the server archive, and they are what Export runs.
        for exe in ["mysql.exe", "mysqldump.exe"] {
            let p = mysql_tools_dir().join(exe);
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            assert!(size > 1_000_000, "{} is missing or truncated ({size} bytes)", p.display());
        }
        // The MySQL paths are separate from the MariaDB ones on purpose - MySQL servers use these.
        let cfg = load_cfg();
        assert!(cfg["mysql_bin_mysql"].as_str().unwrap_or("").ends_with(".exe"), "{cfg}");
    }

    #[test]
    fn the_client_binaries_and_their_openssl_are_taken_from_the_archive() {
        let want = |n: &str| mysql_zip_member(n).as_deref().map(String::from);
        assert_eq!(want("mysql-8.4.11-winx64/bin/mysql.exe"), Some("mysql.exe".into()));
        assert_eq!(want(r"mysql-8.4.11-winx64\bin\mysqldump.exe"), Some("mysqldump.exe".into()));
        // The libraries the two import - without them the loader refuses to start either.
        assert_eq!(want("mysql-8.4.11-winx64/bin/libcrypto-3-x64.dll"), Some("libcrypto-3-x64.dll".into()));
        assert_eq!(want("mysql-8.4.11-winx64/bin/libssl-3-x64.dll"), Some("libssl-3-x64.dll".into()));
        for n in ["mysql-8.4.11-winx64/bin/mysqld.exe", "mysql-8.4.11-winx64/lib/plugin/mysql.exe",
                  "mysql-8.4.11-winx64/mysql.exe", "mysql-8.4.11-winx64/bin/sub/mysql.exe", "mysql.exe",
                  "mysql-8.4.11-winx64/lib/libcrypto-3-x64.dll", "mysql-8.4.11-winx64/bin/abseil_dll.dll"] {
            assert_eq!(mysql_zip_member(n), None, "{n}");
        }
    }

    #[test]
    fn only_a_mysql_server_switches_to_mysql_tools() {
        let def = || Ok::<String, String>("default".into());
        assert_eq!(choose_tool(Some(false), || Some("mysql".into()), def).unwrap(), "mysql");
        assert_eq!(choose_tool(Some(false), || None, def).unwrap(), "default", "no MySQL tools: the default pair");
        assert_eq!(choose_tool(Some(true), || Some("mysql".into()), def).unwrap(), "default", "MariaDB keeps the default pair");
        assert_eq!(choose_tool(None, || Some("mysql".into()), def).unwrap(), "default", "unknown server keeps the default pair");
    }

    #[test]
    fn mysql_server_installs_are_found_newest_first() {
        let root = std::env::temp_dir().join(format!("nobs-mysqldirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for l in ["Program Files/MySQL/MySQL Server 8.0/bin", "Program Files/MySQL/MySQL Server 8.10/bin",
                  "Program Files/MySQL/MySQL Server 8.4/bin", "Program Files/MySQL/MySQL Workbench 8.0 CE",
                  "Program Files/MariaDB 11.4/bin", "Program Files (x86)/MySQL/MySQL Server 5.7/bin"] {
            std::fs::create_dir_all(root.join(l)).unwrap();
        }
        let dirs = mysql_server_bin_dirs(&[root.join("Program Files"), root.join("Program Files (x86)"), root.join("missing")]);
        let names: Vec<String> = dirs.iter()
            .map(|d| d.parent().unwrap().file_name().unwrap().to_string_lossy().to_string()).collect();
        assert_eq!(names, vec!["MySQL Server 8.10", "MySQL Server 8.4", "MySQL Server 8.0", "MySQL Server 5.7"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    // Verbatim --version output of the tools this app downloads or finds.
    #[test]
    fn the_tool_version_is_read_from_its_version_text() {
        let cases = [
            (r"C:\Users\x\NOBSSQL-Desktop\bin\mysql.exe from 12.3.3-MariaDB, client 15.2 for Win64 (AMD64), source revision 83e909fc", Some("MariaDB 12.3.3")),
            (r"C:\x\mysqldump.exe from 12.3.3-MariaDB, client 10.20 for Win64 (AMD64)", Some("MariaDB 12.3.3")),
            ("mysqldump  Ver 8.4.9 for Win64 on x86_64 (MySQL Community Server - GPL)", Some("MySQL 8.4.9")),
            (r"C:\x\mysql.exe  Ver 8.0.46 for Win64 on x86_64 (MySQL Community Server - GPL)", Some("MySQL 8.0.46")),
            ("something else", None),
        ];
        for (text, want) in cases { assert_eq!(tool_version_label(text).as_deref(), want, "{text}"); }
    }

    #[test]
    fn a_release_is_newer_only_when_its_version_is_higher() {
        assert!(release_is_newer("v1.3.0", "1.2.0"));
        assert!(release_is_newer("v1.10.0", "1.9.3"), "compared as numbers, not text");
        assert!(release_is_newer("2.0.0", "1.99.99"));
        assert!(!release_is_newer("v1.2.0", "1.2.0"));
        assert!(!release_is_newer("v1.1.0", "1.2.0"), "an older release is not an update");
        assert!(!release_is_newer("v1.2", "1.2.0"), "1.2 and 1.2.0 are the same version");
        assert!(!release_is_newer("", "1.2.0"));
    }

    // The page is opened through the shell, so nothing but this app's own release pages.
    #[test]
    fn only_this_apps_release_pages_are_opened() {
        assert!(release_page_ok("https://github.com/monsama/nobs-sql-editor/releases/tag/v1.3.0"));
        // The same page, spelled the way the repository was named before it was renamed: GitHub
        // resolves either, so the app has to accept either rather than refuse the page its own
        // update check was just handed.
        assert!(release_page_ok("https://github.com/Monsama/nobs-sql-editor/releases/tag/v1.3.0"));
        for bad in ["https://github.com/monsama/nobs-sql-editor/releases/tag/v1.3.0&calc",
                    "https://github.com/monsama/nobs-sql-editor/releases/tag/v1.3.0\" & calc",
                    "https://evil.example/monsama/nobs-sql-editor/releases/tag/v1.3.0",
                    "https://github.com/someone/else/releases/tag/v1.3.0",
                    "file:///C:/Windows/System32/calc.exe", ""] {
            assert!(!release_page_ok(bad), "{bad}");
        }
    }

    #[test]
    fn first_err_prefers_the_error_line() {
        assert_eq!(first_err("some echoed statement\nERROR 1064 (42000): You have an error"),
                   "ERROR 1064 (42000): You have an error");
        assert_eq!(first_err("plain failure text"), "plain failure text");
    }
}

// ---------------------------------------------------------------------------
// Integration tests that need a live server. They are ignored by default so an
// ordinary `cargo test` stays offline; run them with a database available as:
//
//   NOBS_TEST_DSN='127.0.0.1:3306:root:secret' cargo test -- --ignored --nocapture
//
// A test whose environment is missing prints a line and then PASSES, so a green
// `cargo test` does not on its own mean it ran - check the timing, or watch for
// "... not set - skipping" under --nocapture.
//
//   NOBS_TEST_DSN   - host:port:user:password. Gates all the live tests.
//   MYSQL_BIN /     - full paths to the client tools. The 3 import/export tests
//   MYSQLDUMP_BIN     shell out to them and fall back to a bare "mysql" /
//                     "mysqldump", so without these they do not skip - they RUN
//                     and fail with "program not found" on any machine where the
//                     tools are not on PATH, which is the normal case. Note the
//                     app's config.json records where it EXPECTS them
//                     (%APPDATA%\NOBSSQL-Desktop\bin), which stays empty until
//                     the in-app download has run - so that path may not exist.
//
// Use --test-threads=1: the live tests share nobs_test and interfere in parallel.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod live_tests {
    use super::*;

    // "default" encrypts whenever the server offers TLS. It used to mean plaintext, always, in this
    // edition only - the client tools behind Export and Import negotiated TLS on the same setting.
    #[test]
    #[ignore]
    fn default_ssl_uses_tls_when_the_server_offers_it() {
        let Some(c) = conn_json() else { return; };
        let mut conn = build_conn(&c).expect("connect");
        let offered: Option<(String, String)> = conn.query_first("SHOW GLOBAL VARIABLES LIKE 'have_ssl'").unwrap();
        let cipher: Option<(String, String)> = conn.query_first("SHOW SESSION STATUS LIKE 'Ssl_cipher'").unwrap();
        if offered.map(|v| v.1 == "YES").unwrap_or(false) {
            assert!(cipher.map(|v| !v.1.is_empty()).unwrap_or(false), "the server offers TLS and the session is not encrypted");
        }
    }

    fn conn_json() -> Option<Value> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let p: Vec<&str> = dsn.split(':').collect();
        if p.len() != 4 { return None; }
        Some(json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"}))
    }

    // The bug this guards: `query` decided a failure was a cancellation by asking whether a
    // requestId had been supplied. The editor supplies one on every run, so every genuine
    // error - a missing table, a syntax error, no database selected - was reported to the user
    // as "Query cancelled." with the real cause discarded.
    #[tokio::test]
    #[ignore]
    async fn genuine_errors_are_not_reported_as_cancellations() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let cases = [
            ("missing table",       "SELECT * FROM nobs_test.definitely_not_a_table"),
            ("syntax error",        "SELEKT 1"),
            ("no database selected","SELECT (SELECT COUNT(*) FROM ro_canary) AS n"),
        ];
        for (label, sql) in cases {
            let req = json!({"sql": sql, "conn": conn, "requestId": format!("test-{}", label)});
            let r = query(req).await.expect("command returned Err");
            let err = r["error"].as_str().unwrap_or("");
            println!("  {:<22} -> {}", label, err);
            assert_eq!(r["ok"], false, "{label} should fail");
            assert_ne!(err, "Query cancelled.",
                       "{label}: the real error was masked as a cancellation");
            assert!(r["cancelled"].is_null(), "{label}: should not be flagged as cancelled");
        }
    }

    // ...while a query that really is cancelled still says so.
    //
    // Deliberately a slow JOIN and not SELECT SLEEP(10), which is what this used to be. MySQL
    // documents SLEEP() as RETURNING 1 when KILL QUERY interrupts it - the statement then
    // succeeds, with a row - so on MySQL 8 that made the test look like the cancel had been
    // ignored when nothing was wrong with it. MariaDB raises ER_QUERY_INTERRUPTED instead, which
    // is why it only ever passed there. A real query behaves the same on both, and this is one:
    // measured on MySQL 8.0.46 and MariaDB 12.2, cancelling it reports "Query cancelled." on each.
    //
    // Bounded by id so that a cancel which does NOT work fails as a slow test rather than hanging
    // - unrestricted, this join runs for over three minutes.
    #[tokio::test]
    #[ignore]
    async fn a_cancelled_query_still_reports_as_cancelled() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let rid = "test-real-cancel".to_string();
        let sql = "SELECT COUNT(*) FROM bulk_rows a JOIN bulk_rows b ON a.category = b.category \
                   WHERE a.id < 10000";
        let q = tokio::spawn(query(json!({"sql": sql, "conn": conn.clone(), "db": "nobs_test", "requestId": rid})));
        // let it register its CONNECTION_ID() before killing it
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let c = cancel_query(json!({"requestId":"test-real-cancel", "conn": conn})).await.expect("cancel failed");
        assert_eq!(c["ok"], true);
        let r = q.await.expect("join failed").expect("command returned Err");
        println!("  cancelled query        -> {}", r["error"].as_str().unwrap_or(""));
        assert_eq!(r["error"], "Query cancelled.");
        assert_eq!(r["cancelled"], true);
    }
}

#[cfg(test)]
mod apply_tests {
    use super::*;
    fn conn_json() -> Option<Value> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let p: Vec<&str> = dsn.split(':').collect();
        if p.len() != 4 { return None; }
        Some(json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"}))
    }
    async fn one(conn: &Value, sql: &str) -> Value {
        query(json!({"sql":sql,"conn":conn,"db":"nobs_test"})).await.unwrap()
    }

    // Staged grid edits are applied as one batch. If a later statement fails, the earlier ones
    // must not remain - a half-applied edit is the outcome the pending-changes model exists to
    // prevent, and the README promises a transaction.
    #[tokio::test]
    #[ignore]
    async fn a_failed_batch_applies_nothing() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        if !caps::of(&conn).check() { eprintln!("the server does not enforce CHECK - skipping"); return; }
        one(&conn, "UPDATE txn_child SET descr='original' WHERE id=1").await;
        let batch = "UPDATE nobs_test.txn_child SET descr='EDITED FIRST' WHERE id=1 LIMIT 1;\n\
                     UPDATE nobs_test.txn_child SET qty=-1 WHERE id=2 LIMIT 1;\n\
                     UPDATE nobs_test.txn_child SET descr='EDITED THIRD' WHERE id=3 LIMIT 1;";
        let r = script(json!({"sql":batch,"conn":conn,"db":"nobs_test","transaction":true})).await.unwrap();
        assert_eq!(r["ok"], false, "the batch should fail on the CHECK constraint");
        println!("  batch error: {}", r["error"].as_str().unwrap_or("").lines().next().unwrap_or(""));
        let after = one(&conn, "SELECT descr FROM txn_child WHERE id=1").await;
        let descr = after["rows"][0][0].as_str().unwrap_or("");
        println!("  descr of row 1 after the failed batch: {:?}", descr);
        assert_eq!(descr, "original", "an earlier statement survived a failed batch");
    }

    // Grid edits used to run with FOREIGN_KEY_CHECKS=0, so an edit could point a row at a
    // parent that does not exist.
    #[tokio::test]
    #[ignore]
    async fn foreign_keys_are_enforced_on_apply() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        one(&conn, "UPDATE txn_child SET parent_id=2 WHERE code='CCC'").await;
        let batch = "UPDATE nobs_test.txn_child SET parent_id=99 WHERE code='CCC' LIMIT 1;";
        let r = script(json!({"sql":batch,"conn":conn,"db":"nobs_test","transaction":true})).await.unwrap();
        println!("  FK-violating edit: ok={} err={}", r["ok"],
                 r["error"].as_str().unwrap_or("").lines().next().unwrap_or(""));
        assert_eq!(r["ok"], false, "an edit pointing at a missing parent must be rejected");
        let after = one(&conn, "SELECT parent_id FROM txn_child WHERE code='CCC'").await;
        assert_eq!(after["rows"][0][0].as_str().unwrap_or(""), "2", "the orphaning edit was applied anyway");
    }

    // ...while a valid batch still applies in full.
    #[tokio::test]
    #[ignore]
    async fn a_valid_batch_applies_completely() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        // Rows 4 and 5, which no other test in this module touches: cargo runs these in
        // parallel against one database, and sharing a row made them race.
        let batch = "UPDATE nobs_test.txn_child SET descr='batch-a' WHERE id=4 LIMIT 1;\n\
                     UPDATE nobs_test.txn_child SET descr='batch-b' WHERE id=5 LIMIT 1;";
        let r = script(json!({"sql":batch,"conn":conn,"db":"nobs_test","transaction":true})).await.unwrap();
        assert_eq!(r["ok"], true, "valid batch should apply");
        let a = one(&conn, "SELECT descr FROM txn_child WHERE id=4").await;
        let b = one(&conn, "SELECT descr FROM txn_child WHERE id=5").await;
        println!("  after a valid batch: {:?} / {:?}", a["rows"][0][0], b["rows"][0][0]);
        assert_eq!(a["rows"][0][0].as_str().unwrap_or(""), "batch-a");
        assert_eq!(b["rows"][0][0].as_str().unwrap_or(""), "batch-b");
    }
}

#[cfg(test)]
mod export_cancel_tests {
    use super::*;

    // Cancelling an export kills the running mysqldump, which then has no stderr to report -
    // run_job_child deliberately does not wait for it. The log line was built from that empty
    // string, so a cancelled dump appeared as "FAILED <db> routines/events : " with nothing
    // after the colon: a failure, with no reason, for something the user asked to stop.
    #[tokio::test]
    #[ignore]
    async fn cancelling_an_export_is_logged_as_cancelled_not_an_empty_failure() {
        let Some(dsn) = std::env::var("NOBS_TEST_DSN").ok() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let p: Vec<&str> = dsn.split(':').collect();
        let conn = json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"});
        let dir = std::env::temp_dir().join("nobs-export-cancel");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 300 procedures make the routines/events step take a moment, and it starts once the table
        // files are written - so a cancel sent when they appear lands while mysqldump is running.
        {
            let mut c = build_conn(&conn).unwrap();
            c.query_drop("DROP DATABASE IF EXISTS nobs_cancel_rt").unwrap();
            c.query_drop("CREATE DATABASE nobs_cancel_rt").unwrap();
            c.query_drop("CREATE TABLE nobs_cancel_rt.t (id INT PRIMARY KEY)").unwrap();
            for i in 0..300 { c.query_drop(format!("CREATE PROCEDURE nobs_cancel_rt.p{i}() SELECT {i}")).unwrap(); }
        }
        let jid = "export-cancel-test";
        let req = json!({
            "dbs": ["nobs_cancel_rt"], "folder": dir.to_string_lossy(), "mode": "table",
            "conn": conn, "jobId": jid, "excludes": [],
            "options": {"charset":"utf8mb4","routines":true,"events":true,"quick":true,"extinsert":true}
        });
        let handle = tokio::spawn(export_run(req, std::env::var("MYSQLDUMP_BIN").unwrap_or_else(|_| "mysqldump".into())));
        for _ in 0..600 {
            if dir.join("nobs_cancel_rt.t.sql").exists() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let c = cancel_job(json!({"jobId": jid})).unwrap();
        println!("  cancel_job -> {}", c);
        let r = handle.await.unwrap().unwrap();

        let empty: Vec<Value> = Vec::new();
        let lines: Vec<String> = r["log"].as_array().unwrap_or(&empty).iter()
            .map(|v| v.as_str().unwrap_or("").to_string()).collect();
        for l in &lines { println!("  | {}", l); }

        let empty_failure: Vec<&String> = lines.iter()
            .filter(|l| l.starts_with("FAILED") && l.trim_end().ends_with(':')).collect();
        assert!(empty_failure.is_empty(), "a FAILED line with no reason: {:?}", empty_failure);
        assert!(lines.iter().any(|l| l.contains("CANCELLED")), "nothing reported the cancellation: {:?}", lines);
        assert_eq!(r["cancelled"], true, "the run should report itself cancelled");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = build_conn(&conn).map(|mut c| c.query_drop("DROP DATABASE IF EXISTS nobs_cancel_rt"));
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;

    // mariadb-dump has no init-command, and on "required" it carried on in plaintext against a
    // server without TLS. It is pinned to the server's certificate now: where the server has TLS
    // the export goes through, where it has none nothing is exported.
    #[tokio::test]
    #[ignore]
    async fn a_required_export_through_mariadb_dump_is_encrypted_or_refused() {
        let (Ok(dsn), Ok(dbin)) = (std::env::var("NOBS_TEST_DSN"), std::env::var("MYSQLDUMP_BIN")) else { eprintln!("NOBS_TEST_DSN or MYSQLDUMP_BIN not set - skipping"); return };
        if !client_is_mariadb(&dbin) { eprintln!("MYSQLDUMP_BIN is not MariaDB's - skipping"); return; }
        let d: Vec<&str> = dsn.splitn(4, ':').collect();
        let conn = json!({"host":d[0],"port":d[1],"user":d[2],"password":d[3],"ssl":"required"});
        let cap = caps::of(&json!({"host":d[0],"port":d[1],"user":d[2],"password":d[3],"ssl":"default"}));
        let dir = tempfile::tempdir().unwrap();
        let ex = export_run(json!({"conn":conn,"dbs":["nobs_test"],"folder":dir.path().to_string_lossy(),"mode":"db",
            "options":{"charset":"utf8mb4","what":"structure"}}), dbin).await;
        let wrote = std::fs::read_dir(dir.path()).unwrap().flatten().any(|e| e.path().extension().map(|x| x == "sql").unwrap_or(false));
        if cap.tls {
            let fp = server_cert_fingerprint(&conn).unwrap();
            assert_eq!(fp.len(), 32 * 3 - 1, "a SHA-256 fingerprint: {fp}");
            let ex = ex.unwrap();
            assert!(wrote && !ex.to_string().contains("FAILED"), "a server with TLS is exported: {ex}");
        } else {
            assert!(server_cert_fingerprint(&conn).unwrap_err().contains("offers no TLS"));
            assert!(!wrote, "nothing is exported from a server without TLS on \"required\": {ex:?}");
            assert!(ex.map(|v| v["ok"] == false).unwrap_or(true), "and it says it did not export");
        }
    }

    // A dump sets a non-strict sql_mode, so a value too long for its column is cut with a warning
    // and the import carries on. That was logged as a plain OK - the data was changed and nothing
    // said so. SELECT output in the file is not collected, only the warnings.
    #[tokio::test]
    #[ignore]
    async fn import_reports_the_warnings_that_changed_data() {
        let Some(dsn) = std::env::var("NOBS_TEST_DSN").ok() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let p: Vec<&str> = dsn.split(':').collect();
        let conn = json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"});
        let mbin = std::env::var("MYSQL_BIN").unwrap_or_else(|_| "mysql".into());
        let f = std::env::temp_dir().join("nobs-import-warn.sql");
        std::fs::write(&f, "SET SESSION sql_mode='';\nDROP TABLE IF EXISTS imp_warn;\nCREATE TABLE imp_warn (v VARCHAR(2));\nSELECT 'x';\nINSERT INTO imp_warn VALUES ('abcd');\nSET character_set_client = utf8;\nDROP TABLE imp_warn;\n").unwrap();
        let req = json!({"files":[f.to_string_lossy()], "targetDb":"nobs_test", "conn":conn});
        let r = import_run(req, mbin).await.unwrap();
        let line = r["log"].as_array().unwrap().iter().filter_map(|l| l.as_str()).find(|l| l.contains("nobs-import-warn")).unwrap_or("").to_string();
        println!("  log line: {}", line);
        assert!(line.starts_with("OK with 1 warning(s)"), "the truncation was not reported: {line}");
        assert!(line.contains("1265"), "{line}");
        let _ = std::fs::remove_file(&f);
    }

    // mariadb-dump 11.x+ opens every dump with a line MySQL's client does not know. With MySQL's
    // tools the import failed at line 1 ("Unknown command '\-'"); the line is left out for them.
    #[tokio::test]
    #[ignore]
    async fn a_mariadb_dump_opens_on_either_client() {
        let Some(dsn) = std::env::var("NOBS_TEST_DSN").ok() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let p: Vec<&str> = dsn.split(':').collect();
        let conn = json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"});
        let mbin = std::env::var("MYSQL_BIN").unwrap_or_else(|_| "mysql".into());
        let f = std::env::temp_dir().join("nobs-import-sandbox.sql");
        std::fs::write(&f, "/*M!999999\\- enable the sandbox mode */ \n-- MariaDB dump\nSELECT 1;\n").unwrap();
        let r = import_run(json!({"files":[f.to_string_lossy()], "targetDb":"nobs_test", "conn":conn}), mbin).await.unwrap();
        let line = r["log"].as_array().unwrap().iter().filter_map(|l| l.as_str()).find(|l| l.contains("nobs-import-sandbox")).unwrap_or("").to_string();
        assert!(line.starts_with("OK  "), "{line}");
        let _ = std::fs::remove_file(&f);
    }

    // "Continue on error" passes --force to mysql, which then exits 0 even when every statement
    // failed. Trusting the exit code turned a wholly failed import into a list of OK lines - the
    // worst outcome for a restore, because it looks like it worked.
    #[tokio::test]
    #[ignore]
    async fn force_mode_reports_the_errors_it_skipped() {
        let Some(dsn) = std::env::var("NOBS_TEST_DSN").ok() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let p: Vec<&str> = dsn.split(':').collect();
        let conn = json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"});
        let mbin = std::env::var("MYSQL_BIN").unwrap_or_else(|_| "mysql".into());

        let f = std::env::temp_dir().join("nobs-import-bad.sql");
        std::fs::write(&f, "INSERT INTO nobs_test.no_such_table VALUES (1);\nSELECT 1;\n").unwrap();
        let req = json!({"files":[f.to_string_lossy()], "targetDb":"nobs_test", "conn":conn, "force":true});
        let r = import_run(req, mbin).await.unwrap();
        let line = r["log"][0].as_str().unwrap_or("").to_string();
        println!("  log line: {}", line);
        println!("  errorsSkipped: {}", r["errorsSkipped"]);
        assert!(!line.starts_with("OK  "), "a failed import was reported as a clean OK: {line}");
        assert!(line.contains("error(s) SKIPPED"), "the skipped error was not reported: {line}");
        assert_eq!(r["errorsSkipped"], 1);
        let _ = std::fs::remove_file(&f);
    }

    // A MySQL server gets MySQL's own tools when there are any, because MariaDB's mysqldump writes
    // values into a MySQL generated column and the dump does not restore. The tools are chosen as
    // export and import choose them (choose_tool), and the table has to come back whole.
    // The per-table export ran mysqldump once per table, so with writes going on its files came
    // from different moments. A writer adds an order and its line in one transaction the whole
    // time; restored, every order must still have its line and no line may lack its order.
    #[tokio::test]
    #[ignore]
    async fn a_per_table_export_is_one_snapshot() {
        let Some(dsn) = std::env::var("NOBS_TEST_DSN").ok() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let p: Vec<&str> = dsn.split(':').collect();
        let conn = json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"});
        let maria = server_is_mariadb(&conn);
        let env_or = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.into());
        let dbin = choose_tool(maria, || mysql_flavor_tool("mysqldump").map(|x| x.0), || Ok(env_or("MYSQLDUMP_BIN", "mysqldump"))).unwrap();
        let mbin = choose_tool(maria, || mysql_flavor_tool("mysql").map(|x| x.0), || Ok(env_or("MYSQL_BIN", "mysql"))).unwrap();
        let sql = |s: &str| { let mut c = build_conn(&conn).unwrap(); c.query_drop(s).unwrap(); };
        for s in ["DROP DATABASE IF EXISTS snap_src", "DROP DATABASE IF EXISTS snap_tgt", "CREATE DATABASE snap_src", "CREATE DATABASE snap_tgt",
                  "CREATE TABLE snap_src.a_orders (id INT PRIMARY KEY, pad TEXT)",
                  "CREATE TABLE snap_src.b_filler (id INT PRIMARY KEY, pad TEXT)",
                  "CREATE TABLE snap_src.c_lines (id INT PRIMARY KEY, order_id INT, pad TEXT)",
                  "CREATE TABLE snap_src.`x y` (id INT PRIMARY KEY)", "CREATE TABLE snap_src.x_y (id INT PRIMARY KEY)",
                  "INSERT INTO snap_src.`x y` VALUES (1)", "INSERT INTO snap_src.x_y VALUES (2)",
                  "CREATE VIEW snap_src.v_orders AS SELECT id FROM snap_src.a_orders"] { sql(s); }
        // A table between the two that takes a moment to dump.
        sql("INSERT INTO snap_src.b_filler SELECT id, REPEAT('x', 300) FROM nobs_test.bulk_rows WHERE id <= 60000");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let writer = {
            let (stop, conn) = (stop.clone(), conn.clone());
            std::thread::spawn(move || {
                let mut c = build_conn(&conn).unwrap();
                let mut i = 0;
                while !stop.load(Ordering::SeqCst) {
                    i += 1;
                    c.query_drop(format!("START TRANSACTION; INSERT INTO snap_src.a_orders VALUES ({i}, 'o'); INSERT INTO snap_src.c_lines VALUES ({i}, {i}, 'l'); COMMIT")).unwrap();
                }
                i
            })
        };
        // Some orders before the snapshot, so it has something to be consistent about.
        for _ in 0..200 {
            let mut c = build_conn(&conn).unwrap();
            let n: Option<i64> = c.query_first("SELECT COUNT(*) FROM snap_src.a_orders").unwrap();
            if n.unwrap_or(0) >= 20 { break; }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let dir = tempfile::tempdir().unwrap();
        let ex = export_run(json!({"conn":conn,"dbs":["snap_src"],"folder":dir.path().to_string_lossy(),"mode":"table",
            "options":{"charset":"utf8mb4","singletx":true,"quick":true,"triggers":true,"extinsert":true}}), dbin).await.unwrap();
        stop.store(true, Ordering::SeqCst);
        let written = writer.join().unwrap();
        let log = ex["log"].to_string();
        assert!(!log.contains("FAILED"), "{ex}");
        let mut files: Vec<String> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        files.sort();
        assert_eq!(files, ["snap_src.a_orders.sql", "snap_src.b_filler.sql", "snap_src.c_lines.sql", "snap_src.v_orders.sql", "snap_src.x_y.sql", "snap_src.x_y_2.sql"],
            "one file per table and view, and two names that make the same file name get two files");
        // Tables first, then the view, as a restore would.
        let order = ["snap_src.a_orders.sql", "snap_src.b_filler.sql", "snap_src.c_lines.sql", "snap_src.x_y.sql", "snap_src.x_y_2.sql", "snap_src.v_orders.sql"];
        let paths: Vec<String> = order.iter().map(|f| dir.path().join(f).to_string_lossy().into_owned()).collect();
        let im = import_run(json!({"conn":conn,"files":paths,"targetDb":"snap_tgt"}), mbin).await.unwrap();
        assert!(!im.to_string().contains("FAILED"), "import: {im}");
        let mut c = build_conn(&conn).unwrap();
        let counts: (Option<i64>, Option<i64>, Option<i64>) = c.query_first(
            "SELECT (SELECT COUNT(*) FROM snap_tgt.a_orders), (SELECT COUNT(*) FROM snap_tgt.c_lines), \
                    (SELECT COUNT(*) FROM snap_tgt.a_orders o LEFT JOIN snap_tgt.c_lines l ON l.order_id = o.id WHERE l.id IS NULL) \
                  + (SELECT COUNT(*) FROM snap_tgt.c_lines l LEFT JOIN snap_tgt.a_orders o ON o.id = l.order_id WHERE o.id IS NULL)").unwrap().unwrap();
        println!("  orders written during the export: {written}, restored orders/lines: {:?}", counts);
        assert!(written > 0 && counts.0.unwrap_or(0) > 0);
        assert_eq!(counts.0, counts.1, "orders and lines come from the same moment");
        assert_eq!(counts.2, Some(0), "no order without its line, no line without its order");
        let both: Option<String> = c.query_first("SELECT CONCAT((SELECT COUNT(*) FROM snap_tgt.`x y`), (SELECT COUNT(*) FROM snap_tgt.x_y), (SELECT COUNT(*) FROM snap_tgt.v_orders) > 0)").unwrap();
        assert_eq!(both.as_deref(), Some("111"), "both look-alike tables and the view are restored");
        sql("DROP DATABASE snap_src"); sql("DROP DATABASE snap_tgt");
    }

    #[tokio::test]
    #[ignore]
    async fn generated_columns_survive_export_and_import_with_the_chosen_tools() {
        let Some(dsn) = std::env::var("NOBS_TEST_DSN").ok() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let p: Vec<&str> = dsn.split(':').collect();
        let conn = json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"});
        let maria = server_is_mariadb(&conn);
        let env_or = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.into());
        let dbin = choose_tool(maria, || mysql_flavor_tool("mysqldump").map(|x| x.0), || Ok(env_or("MYSQLDUMP_BIN", "mysqldump"))).unwrap();
        let mbin = choose_tool(maria, || mysql_flavor_tool("mysql").map(|x| x.0), || Ok(env_or("MYSQL_BIN", "mysql"))).unwrap();
        println!("  server mariadb={maria:?}  mysqldump={dbin}  mysql={mbin}");
        if maria == Some(false) && mysql_flavor_tool("mysqldump").is_some() {
            assert!(!client_is_mariadb(&dbin), "a MySQL server with MySQL tools available must get MySQL's mysqldump, got {dbin}");
        }
        let sql = |s: &str| { let mut c = build_conn(&conn).unwrap(); c.query_drop(s).unwrap(); };
        for s in ["DROP DATABASE IF EXISTS gen_rt_src", "DROP DATABASE IF EXISTS gen_rt_tgt",
                  "CREATE DATABASE gen_rt_src", "CREATE DATABASE gen_rt_tgt",
                  "CREATE TABLE gen_rt_src.t (id INT PRIMARY KEY, a INT, dbl INT GENERATED ALWAYS AS (a * 2) STORED, v VARCHAR(8) GENERATED ALWAYS AS (CONCAT('x', a)) VIRTUAL)",
                  "INSERT INTO gen_rt_src.t (id, a) VALUES (1, 5), (2, 7)"] { sql(s); }
        let dir = tempfile::tempdir().unwrap();
        let ex = export_run(json!({"conn":conn,"dbs":["gen_rt_src"],"folder":dir.path().to_string_lossy(),"mode":"db",
            "options":{"charset":"utf8mb4","singletx":true,"triggers":true,"extinsert":true,"createdb":true}}), dbin.clone()).await.unwrap();
        if maria == Some(false) && client_is_mariadb(&dbin) {
            // No MySQL tools on this machine: the export must refuse rather than write a dump that
            // does not restore.
            assert_eq!(ex["ok"], false, "{ex}");
            assert!(ex["error"].as_str().unwrap_or("").contains("generated columns"), "{ex}");
        } else {
            assert!(!ex.to_string().contains("FAILED"), "export: {ex}");
            let file = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.path())
                .find(|p| p.extension().map(|x| x == "sql").unwrap_or(false)).expect("no dump written");
            let im = import_run(json!({"conn":conn,"files":[file.to_string_lossy()],"targetDb":"gen_rt_tgt"}), mbin).await.unwrap();
            assert!(im["log"].as_array().map(|l| l.iter().any(|x| x.as_str().unwrap_or("").starts_with("OK  "))).unwrap_or(false), "import: {im}");
            let mut c = build_conn(&conn).unwrap();
            let got: Option<String> = c.query_first("SELECT GROUP_CONCAT(CONCAT(id, ':', a, ':', dbl, ':', v) ORDER BY id) FROM gen_rt_tgt.t").unwrap();
            assert_eq!(got.as_deref(), Some("1:5:10:x5,2:7:14:x7"));
        }
        sql("DROP DATABASE IF EXISTS gen_rt_src");
        sql("DROP DATABASE IF EXISTS gen_rt_tgt");
    }

    // A per-table dump has no CREATE DATABASE or USE, so without a target it fails with a
    // message that does not say what to do about it.
    #[tokio::test]
    #[ignore]
    async fn a_missing_target_database_explains_itself() {
        let Some(dsn) = std::env::var("NOBS_TEST_DSN").ok() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let p: Vec<&str> = dsn.split(':').collect();
        let conn = json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"});
        let mbin = std::env::var("MYSQL_BIN").unwrap_or_else(|_| "mysql".into());

        let f = std::env::temp_dir().join("nobs-import-nodb.sql");
        std::fs::write(&f, "INSERT INTO ro_canary (label) VALUES ('x');\n").unwrap();
        let req = json!({"files":[f.to_string_lossy()], "targetDb":"", "conn":conn});
        let r = import_run(req, mbin).await.unwrap();
        let line = r["log"][0].as_str().unwrap_or("").to_string();
        println!("  log line: {}", line.replace('\n', " | "));
        assert!(line.contains("1046") || line.contains("No database selected"));
        assert!(line.contains("Target database"), "no hint about choosing a target: {line}");
        let _ = std::fs::remove_file(&f);
    }
}

#[cfg(test)]
mod binary_col_tests {
    use super::*;
    #[tokio::test]
    #[ignore]
    async fn query_reports_which_columns_are_binary() {
        let Some(dsn) = std::env::var("NOBS_TEST_DSN").ok() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let p: Vec<&str> = dsn.split(':').collect();
        let conn = json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"});
        let r = query(json!({"sql":"SELECT id, emoji, bin_col, blob_col, bit_col, bit8 FROM charset_binary",
                             "conn":conn,"db":"nobs_test"})).await.unwrap();
        let cols: Vec<String> = r["columns"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let bin: Vec<bool> = r["binaryCols"].as_array().expect("binaryCols missing").iter().map(|v| v.as_bool().unwrap()).collect();
        let bit: Vec<bool> = r["bitCols"].as_array().expect("bitCols missing").iter().map(|v| v.as_bool().unwrap()).collect();
        for ((c, b), t) in cols.iter().zip(bin.iter()).zip(bit.iter()) { println!("  {:<10} binary={} bit={}", c, b, t); }
        assert_eq!(bin, vec![false, false, true, true, true, true],
                   "id and emoji are text; bin_col, blob_col and both BIT columns are binary");
        // bitCols narrows binaryCols to just the BIT(n) columns: bin_col/blob_col are real
        // binary data (only a 0x.. hex literal is safe there) but not BIT, so a bare integer
        // literal sent to them would store the bytes of its digit character, not a number -
        // unlike bit_col/bit8, where MySQL accepts a bare integer as the correct bit pattern.
        assert_eq!(bit, vec![false, false, false, false, true, true],
                   "only bit_col and bit8 are BIT columns; bin_col/blob_col are binary but not BIT");
    }
}

#[cfg(test)]
mod browse_charset_tests {
    use super::*;

    #[test]
    fn only_a_charset_the_server_has_is_accepted() {
        let with = |v: Value| browse_charset(&json!({"charset": v}));
        assert_eq!(with(json!("latin1")), Some("latin1".into()));
        assert_eq!(with(json!("BINARY")), Some("binary".into()), "the name is not case-sensitive");
        assert_eq!(with(json!("  utf8mb4 ")), Some("utf8mb4".into()));
        // No charset asked for at all - every ordinary connection.
        assert_eq!(with(json!("")), None);
        assert_eq!(with(json!("default")), None);
        assert_eq!(browse_charset(&json!({})), None);
        // This value is interpolated into SET NAMES, which takes no placeholder. Nothing that is
        // not a charset gets through, so there is nothing to escape.
        for bad in ["latin1; DROP DATABASE nobs_test", "latin1'", "utf8mb4 --", "'binary'",
                    "latin1 /*", "sjis`", "big5\\", "../latin1", "utf8mb4;SET autocommit=0"] {
            assert_eq!(browse_charset(&json!({"charset": bad})), None, "{bad}");
        }
    }

    #[test]
    fn browsing_in_another_charset_is_read_only_whatever_the_request_says() {
        let plain = json!({"host":"127.0.0.1","user":"root"});
        let browsing = json!({"host":"127.0.0.1","user":"root","charset":"latin1"});
        assert!(!ro_mode(&json!({"conn": plain, "ro": false})));
        assert!(ro_mode(&json!({"conn": plain, "ro": true})), "a connection its owner marked read-only");
        // The UI sets ro for these too. This is what holds if it ever does not.
        assert!(ro_mode(&json!({"conn": browsing, "ro": false})));
        assert!(ro_mode(&json!({"conn": browsing})));
    }
}

#[cfg(test)]
mod browse_charset_live_tests {
    use super::*;

    fn conn(charset: Option<&str>) -> Option<Value> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let p: Vec<&str> = dsn.split(':').collect();
        if p.len() != 4 { return None; }
        let mut c = json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"});
        if let Some(cs) = charset { c["charset"] = json!(cs); }
        Some(c)
    }

    // The bug this answers: a value that renders as mojibake, where nothing in the app can say
    // whether the data is wrong or only the reading of it. Here the bytes of "café" in UTF-8 are
    // stored in a latin1 column - which is the common mistake - so the server transcodes them into
    // the session charset and the default connection shows "cafÃ©". Reading the same row in latin1
    // asks for no transcoding into utf8mb4, so the bytes arrive as stored and decode as "café":
    // the storage is what it always was, and the difference is the diagnostic.
    #[tokio::test]
    #[ignore]
    async fn the_same_row_reads_differently_in_another_charset() {
        let Some(plain) = conn(None) else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let latin1 = conn(Some("latin1")).unwrap();
        let binary = conn(Some("binary")).unwrap();

        script(json!({"conn":plain,"sql":
            "DROP TABLE IF EXISTS nobs_test.charset_browse;\n\
             CREATE TABLE nobs_test.charset_browse (id INT PRIMARY KEY, t VARCHAR(40) CHARACTER SET latin1);\n\
             INSERT INTO nobs_test.charset_browse VALUES (1, 0x636166C3A9);"})).await.unwrap();

        let read = |c: Value| async move {
            let r = query(json!({"sql":"SELECT t FROM nobs_test.charset_browse WHERE id=1","conn":c,"db":"nobs_test"})).await.unwrap();
            assert_eq!(r["ok"], json!(true), "{}", r["error"]);
            (r["rows"][0][0].as_str().unwrap_or("").to_string(),
             r["binaryCols"][0].as_bool().unwrap_or(false))
        };

        let (default_read, _) = read(plain.clone()).await;
        let (latin1_read, latin1_bin) = read(latin1.clone()).await;
        let (binary_read, binary_bin) = read(binary).await;
        println!("  default {default_read:?}  latin1 {latin1_read:?}  binary {binary_read:?}");

        assert_eq!(default_read, "cafÃ©", "the bytes transcoded from latin1 into utf8mb4");
        assert_eq!(latin1_read, "café", "the bytes as stored, which are UTF-8");
        assert!(!latin1_bin, "latin1 is a text charset - the column is not reported binary");
        // SET NAMES binary asks for no transcoding, and every text column then arrives marked
        // charset 63, which the app shows as the bytes themselves.
        assert!(binary_bin, "in binary, a text column is reported binary");
        assert_eq!(binary_read, "0x636166c3a9", "the bytes, as bytes - this app writes hex in lower case");

        // And nothing can be written from such a connection. Both halves are checked: the endpoint
        // refuses to send the statement, and the server refuses the session even if it were sent.
        let blocked = query(json!({"sql":"UPDATE nobs_test.charset_browse SET t='x' WHERE id=1","conn":latin1.clone(),"db":"nobs_test"})).await.unwrap();
        assert_eq!(blocked["ok"], json!(false));
        assert!(blocked["error"].as_str().unwrap().contains("Read-only"), "{}", blocked["error"]);

        let mut c = build_conn(&latin1).unwrap();
        let at_the_server = c.query_drop("UPDATE nobs_test.charset_browse SET t='x' WHERE id=1");
        assert!(at_the_server.is_err(), "the server accepted a write on a browsing connection");
        println!("  the server said: {}", at_the_server.unwrap_err());

        let (still, _) = read(plain.clone()).await;
        assert_eq!(still, "cafÃ©", "the row is as it was");
        script(json!({"conn":plain,"sql":"DROP TABLE IF EXISTS nobs_test.charset_browse;"})).await.unwrap();
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;
    fn conn_json() -> Option<Value> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let p: Vec<&str> = dsn.split(':').collect();
        if p.len() != 4 { return None; }
        Some(json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"}))
    }

    // A page also ends at PAGE_BYTES: a thousand large BLOBs in one answer hung the app. The first
    // page of 12 rows of 3 MB (6 MB each as hex) holds fewer than 12, and paging on returns every
    // row, whole, with none lost at the page boundaries.
    #[tokio::test]
    #[ignore]
    async fn a_page_of_large_blobs_stops_at_its_size_budget() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let q = |sql: &str| { let c = conn.clone(); let s = sql.to_string();
            async move { query(json!({"sql":s,"conn":c,"db":"nobs_test"})).await.unwrap() } };
        q("DROP TABLE IF EXISTS page_blobs").await;
        q("CREATE TABLE page_blobs (id INT PRIMARY KEY, b LONGBLOB)").await;
        for i in 1..=12 { q(&format!("INSERT INTO page_blobs VALUES ({i}, REPEAT(0x41, 3*1024*1024))")).await; }
        let first = query(json!({"sql":"SELECT id, b FROM page_blobs ORDER BY id","conn":conn,"db":"nobs_test","pageSize":1000})).await.unwrap();
        let n0 = first["rows"].as_array().map(|r| r.len()).unwrap_or(0);
        assert!((1..12).contains(&n0) && first["hasMore"] == true, "the first page was not cut by size: {} rows, hasMore {}", n0, first["hasMore"]);
        let cid = first["cursorId"].as_str().unwrap_or("").to_string();
        let mut ids: Vec<String> = first["rows"].as_array().unwrap().iter().map(|r| r[0].as_str().unwrap().to_string()).collect();
        let whole = |r: &Value| r[1].as_str().map(|s| s.len() == 2 + 2 * 3 * 1024 * 1024).unwrap_or(false);
        assert!(first["rows"].as_array().unwrap().iter().all(whole), "a value came back cut");
        let mut more = true;
        for _ in 0..20 {
            if !more { break; }
            let b = fetch_cursor_batch(json!({"cursorId": cid, "pageSize": 1000})).await.unwrap();
            assert!(b["rows"].as_array().unwrap().iter().all(whole), "a value came back cut");
            ids.extend(b["rows"].as_array().unwrap().iter().map(|r| r[0].as_str().unwrap().to_string()));
            more = b["hasMore"].as_bool().unwrap_or(false);
        }
        assert_eq!(ids, (1..=12).map(|i| i.to_string()).collect::<Vec<_>>());
        q("DROP TABLE IF EXISTS page_blobs").await;
    }

    // Opens a cursor over the first `total` rows of bulk_rows and pages it to exhaustion at
    // `page` rows a time, returning every id it was handed, in order.
    async fn page_all(conn: &Value, total: usize, page: usize) -> Vec<String> {
        let sql = format!("SELECT id FROM bulk_rows ORDER BY id LIMIT {total}");
        let first = query(json!({"sql": sql, "conn": conn, "db": "nobs_test", "pageSize": page})).await.unwrap();
        assert_eq!(first["ok"], true, "opening the cursor failed: {first}");
        let take = |v: &Value| -> Vec<String> {
            v["rows"].as_array().cloned().unwrap_or_default().iter()
                .map(|r| r[0].as_str().unwrap_or("?").to_string()).collect()
        };
        let mut got = take(&first);
        let mut has_more = first["hasMore"].as_bool().unwrap_or(false);
        if !has_more { return got; }
        let cursor_id = first["cursorId"].as_str().unwrap_or("").to_string();
        assert!(!cursor_id.is_empty(), "a result with more pages must return a cursorId: {first}");
        // Bounded so a has_more that never clears fails as a test rather than hanging.
        for _ in 0..(total / page + 4) {
            if !has_more { break; }
            let b = fetch_cursor_batch(json!({"cursorId": cursor_id, "pageSize": page})).await.unwrap();
            assert_eq!(b["ok"], true, "fetching the next page failed: {b}");
            got.extend(take(&b));
            has_more = b["hasMore"].as_bool().unwrap_or(false);
        }
        assert!(!has_more, "cursor still reported more rows after paging past the end");
        got
    }

    // The bug this guards: the fetch read one row beyond the page to answer "is there more?" and
    // then threw that row away. `result` is forward-only, so it could not be re-read - exactly one
    // row disappeared at every page boundary, and because has_more went false at the end nothing
    // reported it. bulk_rows' 100k rows came back as 99,901.
    //
    // 25 rows at 10/page covers a partial final page; 20 at 10 covers the case where the last page
    // is exactly full, where the look-ahead finds nothing and must not invent a further page.
    #[tokio::test]
    #[ignore]
    async fn paging_a_cursor_delivers_every_row_exactly_once() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        for (total, page) in [(25usize, 10usize), (20, 10), (10, 10), (7, 3), (1, 1)] {
            let got = page_all(&conn, total, page).await;
            let want: Vec<String> = (0..total).map(|i| i.to_string()).collect();
            assert_eq!(got.len(), total,
                       "{total} rows at {page}/page: got {} - a row was dropped at a page boundary: {got:?}",
                       got.len());
            assert_eq!(got, want, "{total} rows at {page}/page came back in the wrong order or with gaps");
        }
    }

    // The same failure, at the size it was actually noticed: the default page size over a table
    // big enough for ~100 pages. Guards against a fix that only works for small page counts.
    #[tokio::test]
    #[ignore]
    async fn paging_the_full_bulk_table_loses_nothing() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let got = page_all(&conn, 100_000, 1000).await;
        assert_eq!(got.len(), 100_000, "expected all 100000 rows across 100 pages, got {}", got.len());
        let mut uniq = got.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), 100_000, "the same row was delivered on more than one page");
    }
}

#[cfg(test)]
mod csv_null_tests {
    use super::*;
    fn conn_json() -> Option<Value> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let p: Vec<&str> = dsn.split(':').collect();
        if p.len() != 4 { return None; }
        Some(json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"}))
    }

    // MyISAM cannot roll back: Replace deleted the rows before a failure it could not undo, and a
    // failed Append claimed nothing had been imported while its first batches stayed.
    #[tokio::test]
    #[ignore]
    async fn csv_import_is_honest_about_an_engine_that_cannot_roll_back() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let q = |sql: &str| { let c = conn.clone(); let s = sql.to_string();
            async move { query(json!({"sql":s,"conn":c,"db":"nobs_test"})).await.unwrap() } };
        q("DROP TABLE IF EXISTS csv_myisam").await;
        q("CREATE TABLE csv_myisam (id INT PRIMARY KEY, v VARCHAR(8)) ENGINE=MyISAM").await;
        q("INSERT INTO csv_myisam VALUES (1,'keep'),(2,'keep')").await;
        // 600 good rows (a whole batch of 500 is written) and then a duplicate key
        let mut csv = String::from("id,v\n");
        for i in 10..610 { csv.push_str(&format!("{},x\n", i)); }
        csv.push_str("10,dup\n");
        let file = std::env::temp_dir().join("csv_myisam.csv");
        std::fs::write(&file, csv).unwrap();

        let r = importcsv(json!({"conn":conn,"db":"nobs_test","table":"csv_myisam","file":file.to_string_lossy(),"hasHeader":true,"truncate":true})).await.unwrap();
        assert_eq!(r["ok"], false, "Replace on MyISAM must be refused: {}", r);
        assert!(r["error"].as_str().unwrap_or("").contains("cannot undo"), "{}", r);
        let n = q("SELECT COUNT(*) FROM csv_myisam").await;
        assert_eq!(n["rows"][0][0].as_str(), Some("2"), "the refused Replace changed the table");

        let r = importcsv(json!({"conn":conn,"db":"nobs_test","table":"csv_myisam","file":file.to_string_lossy(),"hasHeader":true})).await.unwrap();
        assert_eq!(r["ok"], false, "{}", r);
        let e = r["error"].as_str().unwrap_or("");
        assert!(e.contains("already written") && !e.contains("rolled back"), "the failed Append must say what stayed: {}", e);
        q("DROP TABLE IF EXISTS csv_myisam").await;
        let _ = std::fs::remove_file(&file);
    }

    // MySQL 9's VECTOR is a column of 4-byte floats that arrives as its bytes. The grid shows it as
    // hex, and a value written back the way the grid writes a binary one is stored unchanged.
    #[tokio::test]
    #[ignore]
    async fn a_mysql_vector_column_reads_and_writes_as_its_bytes() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        if !caps::of(&conn).mysql_vector() { eprintln!("no MySQL VECTOR type on this server - skipping"); return; }
        let run = |sql: &str| { let c = conn.clone(); let s = sql.to_string();
            async move { let r = script(json!({"sql":s,"conn":c,"db":"nobs_test"})).await.unwrap(); assert_eq!(r["ok"], true, "{r}"); } };
        run("DROP TABLE IF EXISTS vec_t; CREATE TABLE vec_t (id INT PRIMARY KEY, v VECTOR(3)); INSERT INTO vec_t VALUES (1, STRING_TO_VECTOR('[1,2,3]'))").await;
        let r = query(json!({"sql":"SELECT id, v FROM vec_t","conn":conn,"db":"nobs_test"})).await.unwrap();
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["binaryCols"][1], true, "a VECTOR is bytes, shown as hex: {r}");
        assert_eq!(r["rows"][0][1].as_str().map(str::to_uppercase).as_deref(), Some("0X0000803F0000004000004040"), "{r}");
        // 1, 2 and 4 as little-endian floats - what an edited cell holding that hex writes.
        run("UPDATE vec_t SET v = 0x0000803F0000004000008040 WHERE id = 1 LIMIT 1").await;
        let back = query(json!({"sql":"SELECT VECTOR_TO_STRING(v) FROM vec_t","conn":conn,"db":"nobs_test"})).await.unwrap();
        assert_eq!(back["rows"][0][0].as_str(), Some("[1.00000e+00,2.00000e+00,4.00000e+00]"), "{back}");
        run("DROP TABLE vec_t").await;
    }

    // MariaDB's UUID, INET4 and INET6 hold text-like values. Flagged as binary, the grid would show
    // them as hex and key an edit by a hex literal the column does not compare equal to.
    #[tokio::test]
    #[ignore]
    async fn mariadb_uuid_and_inet_columns_are_not_binary() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let q = |sql: &str| { let c = conn.clone(); let s = sql.to_string();
            async move { query(json!({"sql":s,"conn":c,"db":"nobs_test"})).await.unwrap() } };
        let v = q("SELECT VERSION()").await;
        let ver = v["rows"][0][0].as_str().unwrap_or("").to_string();
        if !ver.contains("MariaDB") { eprintln!("not MariaDB ({ver}) - skipping"); return; }
        if !caps::of(&conn).uuid_inet_types() { eprintln!("MariaDB {ver} has no UUID/INET4 types - skipping"); return; }
        q("DROP TABLE IF EXISTS uuid_inet").await;
        q("CREATE TABLE uuid_inet (u UUID PRIMARY KEY, a INET6, b INET4)").await;
        q("INSERT INTO uuid_inet VALUES ('123e4567-e89b-12d3-a456-426614174000', '2001:db8::1', '10.0.0.1')").await;
        let r = q("SELECT u, a, b FROM uuid_inet").await;
        println!("  {}", r);
        let bin: Vec<bool> = r["binaryCols"].as_array().unwrap().iter().map(|v| v.as_bool().unwrap()).collect();
        assert_eq!(bin, vec![false, false, false], "{r}");
        assert_eq!(r["rows"][0][0].as_str(), Some("123e4567-e89b-12d3-a456-426614174000"), "{r}");
        q("DROP TABLE IF EXISTS uuid_inet").await;
    }

    // A NULL and an empty string both came out as an empty field, so a CSV could not tell them
    // apart - and the importer reads an empty cell as NULL, so an empty string did not survive a
    // round trip at all.
    #[tokio::test]
    #[ignore]
    async fn null_and_empty_string_survive_a_csv_round_trip() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let q = |sql: &str| { let c = conn.clone(); let s = sql.to_string();
            async move { query(json!({"sql":s,"conn":c,"db":"nobs_test"})).await.unwrap() } };

        q("DROP TABLE IF EXISTS csv_null_rt").await;
        q("CREATE TABLE csv_null_rt (id INT PRIMARY KEY, v VARCHAR(32) NULL)").await;
        q("INSERT INTO csv_null_rt VALUES (1, NULL), (2, ''), (3, 'text')").await;

        let file = std::env::temp_dir().join("csv_null_rt.csv");
        let r = export_table_run(None, json!({"conn":conn,"db":"nobs_test","table":"csv_null_rt",
                                    "file":file.to_string_lossy(),"format":"csv","nullValue":"\\N"})).await.unwrap();
        assert_eq!(r["ok"], true, "export failed: {}", r);
        let text = std::fs::read_to_string(&file).unwrap();
        println!("  exported csv:");
        for l in text.lines() { println!("    {}", l); }
        assert!(text.contains("1,\\N"), "NULL was not written as the marker");
        assert!(text.contains("2,\n") || text.ends_with("2,"), "empty string should be an empty field");

        // read it back into a fresh table
        q("DROP TABLE IF EXISTS csv_null_rt2").await;
        q("CREATE TABLE csv_null_rt2 (id INT PRIMARY KEY, v VARCHAR(32) NULL)").await;
        let ir = importcsv(json!({"conn":conn,"db":"nobs_test","table":"csv_null_rt2",
                                  "file":file.to_string_lossy(),"hasHeader":true,"nullValue":"\\N"})).await.unwrap();
        assert_eq!(ir["ok"], true, "import failed: {}", ir);

        let back = q("SELECT id, v IS NULL AS is_null, v = '' AS is_empty FROM csv_null_rt2 ORDER BY id").await;
        let rows = back["rows"].as_array().unwrap();
        for r in rows { println!("  back: id={} is_null={:?} is_empty={:?}", r[0], r[1], r[2]); }
        assert_eq!(rows[0][1].as_str(), Some("1"), "row 1 must come back as NULL");
        assert_eq!(rows[1][1].as_str(), Some("0"), "row 2 must NOT be NULL - it was an empty string");
        assert_eq!(rows[1][2].as_str(), Some("1"), "row 2 must come back as an empty string");

        q("DROP TABLE IF EXISTS csv_null_rt").await;
        q("DROP TABLE IF EXISTS csv_null_rt2").await;
        let _ = std::fs::remove_file(&file);
    }

    // A procedure's results, and every SELECT but the last in a script, were run and thrown away.
    #[tokio::test]
    #[ignore]
    async fn a_script_returns_every_result_set() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let run = |sql: &str, max: u64| { let c = conn.clone(); let s = sql.to_string();
            async move { script_results(json!({"sql":s,"conn":c,"db":"nobs_test","maxRows":max})).await.unwrap() } };
        let setup = script(json!({"conn":conn,"db":"nobs_test","sql":
            "DROP PROCEDURE IF EXISTS sr_two; DROP TABLE IF EXISTS sr_t; CREATE TABLE sr_t (id INT PRIMARY KEY, v VARCHAR(10), b VARBINARY(4)); \
             INSERT INTO sr_t VALUES (1,'one',0x00FF),(2,NULL,NULL),(3,'NULL',X'')"})).await.unwrap();
        assert_eq!(setup["ok"], true, "{setup}");
        let proc = script(json!({"conn":conn,"db":"nobs_test","sql":
            "DELIMITER $$\nCREATE PROCEDURE sr_two(IN n INT)\nBEGIN\n  SELECT id, v FROM sr_t WHERE id <= n ORDER BY id;\n  UPDATE sr_t SET v = 'touched' WHERE id = 3;\n  SELECT COUNT(*) AS c, MAX(b) AS mb FROM sr_t;\nEND$$\nDELIMITER ;"})).await.unwrap();
        assert_eq!(proc["ok"], true, "{proc}");

        let r = run("CALL sr_two(2);", 1000).await;
        assert_eq!(r["ok"], true, "{r}");
        let sets = r["results"].as_array().unwrap();
        assert_eq!(sets.len(), 2, "both of the procedure's results: {r}");
        assert_eq!(sets[0]["columns"], json!(["id", "v"]));
        assert_eq!(sets[0]["rows"], json!([["1", "one"], ["2", null]]), "NULL stays NULL");
        assert_eq!(sets[1]["rows"], json!([["3", "0x00ff"]]), "binary as hex");
        assert_eq!(sets[1]["binaryCols"], json!([false, true]));

        let r = run("SELECT 'NULL' AS a; SELECT id FROM sr_t WHERE 0; SELECT id FROM bulk_rows ORDER BY id", 10).await;
        let sets = r["results"].as_array().unwrap();
        assert_eq!(sets.len(), 3, "{r}");
        assert_eq!(sets[0]["rows"], json!([["NULL"]]), "the text NULL is text");
        assert_eq!(sets[1]["columns"], json!(["id"]), "an empty result still has its columns");
        assert_eq!((sets[2]["rows"].as_array().unwrap().len(), sets[2]["rowCount"].clone(), sets[2]["truncated"].clone()), (10, json!(100000), json!(true)));

        let r = run("SELECT 1 AS a; SELECT * FROM sr_no_such_table; SELECT 2 AS b", 10).await;
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap().starts_with("Statement 2 of 3 failed"), "{r}");
        assert_eq!(r["results"].as_array().unwrap().len(), 1, "the result before the error is kept: {r}");

        let ro = script_results(json!({"sql":"CALL sr_two(1)","conn":conn,"db":"nobs_test","ro":true})).await.unwrap();
        assert_eq!(ro["ok"], false, "read-only mode refuses a CALL: {ro}");

        let _ = script(json!({"conn":conn,"db":"nobs_test","sql":"DROP PROCEDURE sr_two; DROP TABLE sr_t"})).await;
    }

    // Exports read SELECT *, which leaves out INVISIBLE columns, and the INSERT export wrote a
    // generated column (refused on import) under INSERT IGNORE (which cuts a too-long value short
    // instead of failing). The CSV import skipped unknown columns, filled short rows with NULL and
    // switched foreign key checks off.
    #[tokio::test]
    #[ignore]
    async fn exports_and_csv_import_keep_every_value_and_refuse_broken_files() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let q = |sql: &str| { let c = conn.clone(); let s = sql.to_string();
            async move { let r = script(json!({"sql":s,"conn":c,"db":"nobs_test"})).await.unwrap(); assert_eq!(r["ok"], true, "{r}"); } };
        let one = |sql: &str| { let c = conn.clone(); let s = sql.to_string();
            async move { let r = query(json!({"sql":s,"conn":c,"db":"nobs_test"})).await.unwrap(); r["rows"][0][0].as_str().unwrap_or("").to_string() } };
        q("DROP TABLE IF EXISTS csv_x_child; DROP TABLE IF EXISTS csv_x_src; DROP TABLE IF EXISTS csv_x_dst; DROP TABLE IF EXISTS csv_x_parent").await;
        let inv = caps::of(&conn).invisible_kw();
        q(&format!("CREATE TABLE csv_x_src (id INT PRIMARY KEY, a INT, secret VARCHAR(10){inv}, g INT GENERATED ALWAYS AS (a * 2) VIRTUAL)")).await;
        q("INSERT INTO csv_x_src (id, a, secret) VALUES (1, 5, 'hidden')").await;
        q("CREATE TABLE csv_x_dst LIKE csv_x_src").await;
        let dir = std::env::temp_dir();

        let sqlf = dir.join("csv_x_src.sql");
        let r = export_table_run(None, json!({"conn":conn,"db":"nobs_test","table":"csv_x_src","file":sqlf.to_string_lossy(),"format":"inserts"})).await.unwrap();
        assert_eq!(r["ok"], true, "{r}");
        let text = std::fs::read_to_string(&sqlf).unwrap();
        assert!(text.contains("(`id`,`a`,`secret`)") && !text.contains("`g`") && text.contains("ON DUPLICATE KEY UPDATE `id`=`id`")
                && !text.contains("IGNORE"), "{text}");
        q(&text.replace("`nobs_test`.`csv_x_src`", "`nobs_test`.`csv_x_dst`")).await;
        assert_eq!(one("SELECT CONCAT_WS('|', a, secret, g) FROM csv_x_dst").await, "5|hidden|10", "the INSERT export restores every value");

        let csvf = dir.join("csv_x_src.csv");
        let r = export_table_run(None, json!({"conn":conn,"db":"nobs_test","table":"csv_x_src","file":csvf.to_string_lossy(),"format":"csv","nullValue":"\\N"})).await.unwrap();
        assert_eq!(r["ok"], true, "{r}");
        assert!(std::fs::read_to_string(&csvf).unwrap().starts_with("id,a,secret,g\n"), "the CSV has every column");
        q("DELETE FROM csv_x_dst").await;
        let ir = importcsv(json!({"conn":conn,"db":"nobs_test","table":"csv_x_dst","file":csvf.to_string_lossy(),"hasHeader":true,"nullValue":"\\N"})).await.unwrap();
        assert_eq!(ir["ok"], true, "{ir}");
        assert!(ir["message"].as_str().unwrap().contains("computed by the server: g"), "{ir}");
        assert_eq!(one("SELECT CONCAT_WS('|', a, secret, g) FROM csv_x_dst").await, "5|hidden|10", "and the CSV import skips the generated column");

        let bad = |name: &str, body: &str| { let p = dir.join(name); std::fs::write(&p, body).unwrap(); p };
        let imp = |p: std::path::PathBuf, table: &str| { let c = conn.clone(); let t = table.to_string();
            async move { importcsv(json!({"conn":c,"db":"nobs_test","table":t,"file":p.to_string_lossy(),"hasHeader":true,"nullValue":"\\N"})).await.unwrap() } };
        q("DELETE FROM csv_x_dst").await;
        let r = imp(bad("csv_x_unknown.csv", "ID,A,nmae\n2,3,x\n"), "csv_x_dst").await;
        assert!(r["error"].as_str().unwrap_or("").contains("no column named nmae"), "an unknown column is refused, ID matches id: {r}");
        let r = imp(bad("csv_x_short.csv", "id,a\n2,3\n4\n"), "csv_x_dst").await;
        assert!(r["error"].as_str().unwrap_or("").contains("Line 3 has 1 field(s), but the header has 2"), "a short row is refused: {r}");
        let r = imp(bad("csv_x_long.csv", "id,a\n2,3,9\n"), "csv_x_dst").await;
        assert!(r["error"].as_str().unwrap_or("").contains("has 3 field(s)"), "a long row is refused: {r}");
        assert_eq!(one("SELECT COUNT(*) FROM csv_x_dst").await, "0", "and nothing was imported");

        q("CREATE TABLE csv_x_parent (id INT PRIMARY KEY)").await;
        q("CREATE TABLE csv_x_child (id INT PRIMARY KEY, pid INT, FOREIGN KEY (pid) REFERENCES csv_x_parent (id))").await;
        let r = imp(bad("csv_x_fk.csv", "id,pid\n1,999\n"), "csv_x_child").await;
        assert_eq!(r["ok"], false, "a row pointing at a missing parent is refused: {r}");
        assert!(!r["error"].as_str().unwrap_or("").contains("MySqlError"), "the error is shown without the driver's wrapper: {r}");
        assert_eq!(one("SELECT COUNT(*) FROM csv_x_child").await, "0");

        q("DROP TABLE csv_x_child; DROP TABLE csv_x_parent; DROP TABLE csv_x_src; DROP TABLE csv_x_dst").await;
        for f in ["csv_x_src.sql", "csv_x_src.csv", "csv_x_unknown.csv", "csv_x_short.csv", "csv_x_long.csv", "csv_x_fk.csv"] { let _ = std::fs::remove_file(dir.join(f)); }
    }

    // A CSV import with "Truncate table" checked used to leave the table permanently truncated
    // and only partially reloaded if a later row failed (e.g. a duplicate key) - every batch ran
    // on autocommit with nothing to undo the ones that had already landed. It's now wrapped in a
    // transaction: a mid-file failure must leave the table exactly as it was before the import.
    #[tokio::test]
    #[ignore]
    async fn a_failed_csv_import_with_truncate_leaves_the_table_untouched() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let q = |sql: &str| { let c = conn.clone(); let s = sql.to_string();
            async move { query(json!({"sql":s,"conn":c,"db":"nobs_test"})).await.unwrap() } };

        q("DROP TABLE IF EXISTS csv_txn_rt").await;
        q("CREATE TABLE csv_txn_rt (id INT PRIMARY KEY, v VARCHAR(32))").await;
        q("INSERT INTO csv_txn_rt VALUES (100, 'original')").await;

        // 501 good rows (id 1..501, spanning the importer's 500-row batch boundary) followed by
        // a row that duplicates id 1 - the SECOND batch's INSERT fails. A single statement is
        // already atomic on its own in MySQL, so this specifically checks that the FIRST batch
        // (which had already landed inside the open transaction) gets rolled back too, not just
        // that the failing batch itself doesn't partially apply.
        let mut csv = String::from("id,v\n");
        for i in 1..=501 { csv.push_str(&format!("{},row{}\n", i, i)); }
        csv.push_str("1,duplicate\n");
        let file = std::env::temp_dir().join("csv_txn_rt.csv");
        std::fs::write(&file, csv).unwrap();

        let ir = importcsv(json!({"conn":conn,"db":"nobs_test","table":"csv_txn_rt",
                                   "file":file.to_string_lossy(),"hasHeader":true,"truncate":true})).await.unwrap();
        assert_eq!(ir["ok"], false, "the import should fail on the duplicate key");
        println!("  import error: {}", ir["error"].as_str().unwrap_or("").lines().next().unwrap_or(""));

        let back = q("SELECT id, v FROM csv_txn_rt").await;
        let rows = back["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "the truncate should have been rolled back along with the failed batch");
        assert_eq!(rows[0][0].as_str(), Some("100"));
        assert_eq!(rows[0][1].as_str(), Some("original"), "the original row must survive a rolled-back import");

        q("DROP TABLE IF EXISTS csv_txn_rt").await;
        let _ = std::fs::remove_file(&file);
    }

    // Clearing the marker restores the older reading, where a blank cell means NULL - which is
    // what a spreadsheet exported from Excel usually intends.
    #[tokio::test]
    #[ignore]
    async fn an_empty_marker_makes_blank_cells_null_again() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let q = |sql: &str| { let c = conn.clone(); let s = sql.to_string();
            async move { query(json!({"sql":s,"conn":c,"db":"nobs_test"})).await.unwrap() } };
        q("DROP TABLE IF EXISTS csv_blank_rt").await;
        q("CREATE TABLE csv_blank_rt (id INT PRIMARY KEY, v VARCHAR(32) NULL)").await;
        let file = std::env::temp_dir().join("csv_blank_rt.csv");
        std::fs::write(&file, "id,v\n1,\n2,text\n").unwrap();
        let ir = importcsv(json!({"conn":conn,"db":"nobs_test","table":"csv_blank_rt",
                                  "file":file.to_string_lossy(),"hasHeader":true,"nullValue":""})).await.unwrap();
        assert_eq!(ir["ok"], true, "import failed: {}", ir);
        let back = q("SELECT id, v IS NULL AS is_null FROM csv_blank_rt ORDER BY id").await;
        let rows = back["rows"].as_array().unwrap();
        for r in rows { println!("  blank-marker: id={} is_null={:?}", r[0], r[1]); }
        assert_eq!(rows[0][1].as_str(), Some("1"), "with no marker, a blank cell must import as NULL");
        q("DROP TABLE IF EXISTS csv_blank_rt").await;
        let _ = std::fs::remove_file(&file);
    }
}

#[cfg(test)]
mod interop_tests {
    use super::*;
    fn conn_json() -> Option<Value> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let p: Vec<&str> = dsn.split(':').collect();
        if p.len() != 4 { return None; }
        Some(json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"}))
    }

    // Can a CSV written by another client be read back with NULL and empty string intact?
    // HeidiSQL defaults to \N for NULL; MySQL Workbench writes the literal word NULL.
    #[tokio::test]
    #[ignore]
    async fn csv_from_heidisql_and_workbench_round_trips() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let q = |sql: &str| { let c = conn.clone(); let s = sql.to_string();
            async move { query(json!({"sql":s,"conn":c,"db":"nobs_test"})).await.unwrap() } };

        // (label, marker the user would set, file contents as that tool writes them)
        let cases = [
            ("HeidiSQL default (\\N)",   "\\N",   "id,v\n1,\\N\n2,\n3,text\n"),
            ("Workbench (literal NULL)", "NULL",  "id,v\n1,NULL\n2,\n3,text\n"),
            ("spreadsheet (blank=NULL)", "",      "id,v\n1,\n2,\n3,text\n"),
        ];
        for (label, marker, body) in cases {
            q("DROP TABLE IF EXISTS csv_interop").await;
            q("CREATE TABLE csv_interop (id INT PRIMARY KEY, v VARCHAR(32) NULL)").await;
            let f = std::env::temp_dir().join("csv_interop.csv");
            std::fs::write(&f, body).unwrap();
            let r = importcsv(json!({"conn":conn,"db":"nobs_test","table":"csv_interop",
                                     "file":f.to_string_lossy(),"hasHeader":true,"nullValue":marker})).await.unwrap();
            assert_eq!(r["ok"], true, "{label}: import failed: {r}");
            let back = q("SELECT id, CASE WHEN v IS NULL THEN 'NULL' WHEN v='' THEN 'empty' ELSE v END AS got FROM csv_interop ORDER BY id").await;
            let got: Vec<String> = back["rows"].as_array().unwrap().iter()
                .map(|r| r[1].as_str().unwrap_or("?").to_string()).collect();
            println!("  {:<26} NULL value={:<6} -> {:?}", label, format!("{:?}", marker), got);
            assert_eq!(got[0], "NULL", "{label}: row 1 should be NULL");
            assert_eq!(got[2], "text", "{label}: row 3 should be text");
            let _ = std::fs::remove_file(&f);
        }
        q("DROP TABLE IF EXISTS csv_interop").await;
    }

    // The app's own CSV export read back by its own import, for the binary values that have
    // tripped it: an empty value is written as the bare "0x", and the importer used to store that
    // as the two characters 0x (hex 3078) instead of zero bytes. Measured through the GUI.
    #[tokio::test]
    #[ignore]
    async fn binary_values_survive_the_apps_own_csv_round_trip() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let q = |sql: &str| { let c = conn.clone(); let s = sql.to_string();
            async move { query(json!({"sql":s,"conn":c,"db":"nobs_test"})).await.unwrap() } };
        q("DROP TABLE IF EXISTS csv_bin_src").await;
        q("DROP TABLE IF EXISTS csv_bin_dst").await;
        q("CREATE TABLE csv_bin_src (id INT PRIMARY KEY, b VARBINARY(16) NULL, t VARCHAR(16) NULL)").await;
        q("INSERT INTO csv_bin_src VALUES (1, X'', '0x'), (2, NULL, NULL), (3, 0x00, '0x00'), (4, 0x0A0D, 'a'), (5, 0x3078, 'x')").await;
        q("CREATE TABLE csv_bin_dst LIKE csv_bin_src").await;
        let f = std::env::temp_dir().join("csv_bin_roundtrip.csv");
        let ex = export_table_run(None, json!({"conn":conn,"db":"nobs_test","table":"csv_bin_src",
                                               "file":f.to_string_lossy(),"format":"csv","nullValue":"\\N"})).await.unwrap();
        assert_eq!(ex["ok"], true, "export failed: {ex}");
        let im = importcsv(json!({"conn":conn,"db":"nobs_test","table":"csv_bin_dst",
                                  "file":f.to_string_lossy(),"hasHeader":true,"nullValue":"\\N"})).await.unwrap();
        assert_eq!(im["ok"], true, "import failed: {im}");
        let shape = |t: &str| format!("SELECT GROUP_CONCAT(CONCAT_WS('|', id, IFNULL(HEX(b),'N'), IFNULL(t,'N')) ORDER BY id SEPARATOR ';') FROM {t}");
        let a = q(&shape("csv_bin_src")).await; let b = q(&shape("csv_bin_dst")).await;
        assert_eq!(a["rows"], b["rows"], "binary values changed on the way through the app's own CSV");
        // And specifically: an empty binary value is empty, and text that reads '0x' stays text.
        let one = q("SELECT HEX(b), t FROM csv_bin_dst WHERE id = 1").await;
        assert_eq!(one["rows"][0][0], "", "an empty binary value came back as {}", one["rows"][0][0]);
        assert_eq!(one["rows"][0][1], "0x", "a text column holding '0x' must stay text");
        q("DROP TABLE IF EXISTS csv_bin_src").await;
        q("DROP TABLE IF EXISTS csv_bin_dst").await;
        let _ = std::fs::remove_file(&f);
    }
}

#[cfg(test)]
mod compare_tests {
    use super::*;

    // Compare DB writes to a target database, so it carries the same risks as the grid's apply
    // path. These drive it end to end against a live server.
    //
    // compare_* resolves its servers through resolve_saved_conn(), i.e. by saved connection NAME,
    // so the only way to drive it is to put profiles in conn_path() - the same file the real app
    // keeps its connections in. That file is the user's, so ConnFileGuard below snapshots it and
    // puts it back on Drop (which runs on unwind too, so a failing assert still restores it).
    //
    // resolve_saved_conn() reads the password from the OS keyring rather than the profile JSON.
    // These tests therefore write their own keyring entries under the test-only profile names and
    // delete them in cleanup, which is what lets them use a normal password-protected account -
    // the earlier version dodged the keyring by hardcoding a passwordless `nobsnp` on port 3399,
    // an address that existed only on the machine they were written on, so they could not run
    // anywhere else. Everything now comes from NOBS_TEST_DSN like every other live test.
    const P_RW: &str = "nobs_cmp_test_rw";
    const P_RO: &str = "nobs_cmp_test_ro";

    // connections.json is a single global file, and every test here replaces it and then puts it
    // back. Run two of them at once - which is cargo's default - and the first to finish restores
    // the user's real file while the second is still using the test profiles, so that one fails
    // with "Connection not found." on a completely healthy setup. It looked like a compare bug and
    // was a test one. This serialises them; the lock is held for as long as the guard lives.
    //
    // Poisoning is deliberately ignored: a panicking test still restores the file through the Drop
    // below, so the next one may safely take the lock rather than being failed by its predecessor.
    fn conn_file_lock() -> &'static Mutex<()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        L.get_or_init(|| Mutex::new(()))
    }

    // Restores the real connections.json when the test ends, however it ends.
    struct ConnFileGuard(Option<Vec<u8>>, #[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for ConnFileGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(b) => { let _ = std::fs::write(conn_path(), b); }
                None    => { let _ = std::fs::remove_file(conn_path()); }
            }
            for n in [P_RW, P_RO] {
                if let Ok(e) = keyring::Entry::new("NOBSSQL-Desktop", n) { let _ = e.delete_credential(); }
            }
        }
    }

    fn dsn() -> Option<(String, String, String, String)> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let p: Vec<&str> = dsn.split(':').collect();
        if p.len() != 4 { return None; }
        Some((p[0].into(), p[1].into(), p[2].into(), p[3].into()))
    }

    // Writes the two test profiles (one writable, one read-only) pointing at NOBS_TEST_DSN, and
    // returns a guard that undoes it. None means the DSN is absent and the caller should skip.
    fn setup_conns() -> Option<ConnFileGuard> {
        let (host, port, user, pass) = dsn()?;
        // Taken BEFORE the file is read, so the snapshot is of the user's file and never of
        // another test's profiles.
        let lock = conn_file_lock().lock().unwrap_or_else(|e| e.into_inner());
        let guard = ConnFileGuard(std::fs::read(conn_path()).ok(), lock);
        let profiles = json!([
            {"name":P_RW, "host":host,"port":port,"user":user,"ssl":"default","readonly":false},
            {"name":P_RO, "host":host,"port":port,"user":user,"ssl":"default","readonly":true}
        ]);
        std::fs::write(conn_path(), serde_json::to_string_pretty(&profiles).unwrap()).unwrap();
        for n in [P_RW, P_RO] {
            keyring::Entry::new("NOBSSQL-Desktop", n).unwrap().set_password(&pass).unwrap();
        }
        Some(guard)
    }
    fn conn_j() -> Value {
        let (host, port, user, pass) = dsn().unwrap();
        json!({"host":host,"port":port,"user":user,"password":pass,"ssl":"default"})
    }
    fn raw(sql: &str) {
        let mut conn = build_conn(&conn_j()).unwrap();
        conn.query_drop(sql).unwrap();
    }
    fn scalar(sql: &str) -> String {
        let mut conn = build_conn(&conn_j()).unwrap();
        let (_c, r) = run_select(&mut conn, sql).unwrap();
        r.first().and_then(|x| x.first()).cloned().flatten().unwrap_or_default()
    }

    #[tokio::test]
    #[ignore]
    async fn compare_finds_schema_and_row_differences_and_can_apply_them() {
        let Some(_guard) = setup_conns() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };

        raw("DROP DATABASE IF EXISTS cmp_src"); raw("CREATE DATABASE cmp_src");
        raw("DROP DATABASE IF EXISTS cmp_tgt"); raw("CREATE DATABASE cmp_tgt");
        raw("CREATE TABLE cmp_src.t (id INT PRIMARY KEY, v VARCHAR(32) NULL, extra INT NULL)");
        raw("CREATE TABLE cmp_tgt.t (id INT PRIMARY KEY, v VARCHAR(32) NULL)");   // missing a column
        raw("CREATE TABLE cmp_src.only_here (id INT PRIMARY KEY)");                // missing a table
        raw("INSERT INTO cmp_src.t VALUES (1,'same',NULL),(2,'differs',NULL),(3,NULL,NULL),(4,'',NULL)");
        raw("INSERT INTO cmp_tgt.t VALUES (1,'same'),(2,'OTHER'),(3,''),(5,'extra row')");

        // --- structure
        let r = compare_schemas(json!({"sourceConnName":P_RW,"sourceDb":"cmp_src",
                                       "targetConnName":P_RW,"targetDb":"cmp_tgt"})).await.unwrap();
        assert_eq!(r["ok"], true, "compare_schemas failed: {r}");
        // the response groups statements per table, each with its own checked/kind flags
        let mut stmts: Vec<String> = Vec::new();
        for t in r["tables"].as_array().cloned().unwrap_or_default() {
            for sq in t["sql"].as_array().cloned().unwrap_or_default() {
                if let Some(st) = sq["stmt"].as_str() { stmts.push(st.to_string()); }
            }
        }
        println!("  schema diff produced {} statement(s):", stmts.len());
        for s in &stmts { println!("    {}", s.chars().take(100).collect::<String>()); }
        assert!(stmts.iter().any(|s| s.contains("only_here")), "missing table not detected");
        assert!(stmts.iter().any(|s| s.to_uppercase().contains("EXTRA")), "missing column not detected");

        // --- a read-only target must refuse to apply
        let ro = compare_apply(json!({"targetConnName":P_RO,"targetDb":"cmp_tgt",
                                      "statements":["CREATE TABLE cmp_tgt.should_not_exist (id INT)"]})).await.unwrap();
        println!("  read-only target -> ok={} error={:?}", ro["ok"], ro["error"].as_str().unwrap_or(""));
        assert_eq!(ro["ok"], false, "a read-only target must refuse to apply");
        assert_eq!(scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='cmp_tgt' AND table_name='should_not_exist'"), "0");

        // --- applying the structure diff for real
        let ap = compare_apply(json!({"targetConnName":P_RW,"targetDb":"cmp_tgt","statements":stmts})).await.unwrap();
        assert_eq!(ap["ok"], true, "apply failed: {ap}");
        assert_eq!(scalar("SELECT COUNT(*) FROM information_schema.columns WHERE table_schema='cmp_tgt' AND table_name='t' AND column_name='extra'"), "1",
                   "the missing column was not created");
        assert_eq!(scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='cmp_tgt' AND table_name='only_here'"), "1",
                   "the missing table was not created");
        println!("  structure applied: column and table now present in the target");

        // --- rows missing from the target, found by primary key
        let mr = compare_rows(json!({"sourceConnName":P_RW,"sourceDb":"cmp_src",
                                     "targetConnName":P_RW,"targetDb":"cmp_tgt","table":"t"})).await.unwrap();
        assert_eq!(mr["ok"], true, "compare_rows failed: {mr}");
        let missing: Vec<String> = mr["rows"].as_array().cloned().unwrap_or_default().iter()
            .map(|r| r[0].as_str().unwrap_or("?").to_string()).collect();
        println!("  rows missing in target: {:?}  (source has 4, target had 1,2,3,5)", missing);
        assert_eq!(missing, vec!["4"], "row 4 exists only in the source and should be reported");

        // --- rows present in both but differing, including NULL vs empty string
        let dr = compare_rows_diff(json!({"sourceConnName":P_RW,"sourceDb":"cmp_src",
                                          "targetConnName":P_RW,"targetDb":"cmp_tgt","table":"t"})).await.unwrap();
        assert_eq!(dr["ok"], true, "compare_rows_diff failed: {dr}");
        let mut found: Vec<(String,String,String,String)> = Vec::new();
        for d in dr["diffs"].as_array().cloned().unwrap_or_default() {
            let pk = d["pk"][0].as_str().unwrap_or("?").to_string();
            for cd in d["colDiffs"].as_array().cloned().unwrap_or_default() {
                found.push((pk.clone(), cd["col"].as_str().unwrap_or("?").to_string(),
                            format!("{}", cd["src"]), format!("{}", cd["tgt"])));
            }
        }
        for f in &found { println!("  differs: id={} col={} src={} tgt={}", f.0, f.1, f.2, f.3); }
        assert!(found.iter().any(|f| f.0=="2" && f.1=="v"), "a plain value difference was missed");
        assert!(found.iter().any(|f| f.0=="3" && f.1=="v" && f.2=="null" && f.3=="\"\""),
                "NULL in the source vs empty string in the target was NOT reported: {found:?}");
        assert!(!found.iter().any(|f| f.0=="1"), "row 1 is identical and must not be reported");
        println!("  NULL vs empty string is detected as a difference");

        // --- a row that exists only in the TARGET
        // --- a row that exists only in the TARGET
        // id=5 is in cmp_tgt.t and not in cmp_src.t. It is deliberately NOT in `missing` (that
        // list drives inserts INTO the target) and not in the per-column diffs (there is no source
        // row to diff it against), so for a long time nothing mentioned it at all and a target
        // holding extra rows looked identical to one holding none. It now comes back under
        // extraTotal/extraPks: reported, never acted on. Asserted rather than printed, because a
        // printed note is exactly what let this sit unnoticed.
        assert!(!missing.contains(&"5".to_string()), "a target-only row must not be offered for insert INTO the target");
        assert!(!found.iter().any(|f| f.0=="5"), "a target-only row has no source row to diff against");
        let extra_total = mr["extraTotal"].as_u64().expect("extraTotal missing from compare_rows");
        let extra_pks: Vec<String> = mr["extraPks"].as_array().cloned().unwrap_or_default().iter()
            .map(|r| r[0].as_str().unwrap_or("?").to_string()).collect();
        println!("  rows only in target: total={} pks={:?}", extra_total, extra_pks);
        assert_eq!(extra_total, 1, "the target's extra row (id=5) was not reported");
        assert_eq!(extra_pks, vec!["5"], "extraPks should name exactly the target-only row");

        // And the case that actually misleads: no missing rows at all, but the target still has
        // extras. Before this, that combination reported nothing whatsoever.
        raw("INSERT INTO cmp_tgt.t (id, v, extra) SELECT id, v, extra FROM cmp_src.t WHERE id NOT IN (SELECT id FROM cmp_tgt.t)");
        let mr2 = compare_rows(json!({"sourceConnName":P_RW,"sourceDb":"cmp_src",
                                      "targetConnName":P_RW,"targetDb":"cmp_tgt","table":"t"})).await.unwrap();
        assert_eq!(mr2["ok"], true, "{mr2}");
        assert_eq!(mr2["missingTotal"].as_u64().unwrap_or(9), 0, "every source row should now be present on the target");
        assert_eq!(mr2["extraTotal"].as_u64().unwrap_or(0), 1,
                   "with nothing missing, the target's extra row must still be reported - otherwise this reads as 'no differences'");

        // These used to be dropped only at the START of the next run, so every server this ran
        // against kept two stray databases in its schema list in between.
        raw("DROP DATABASE IF EXISTS cmp_src");
        raw("DROP DATABASE IF EXISTS cmp_tgt");
    }

    // All three write paths - insert-all (server to server), apply (rows that went through the UI
    // as JSON) and apply-diff - must copy every value exactly. Writing by the value's shape stored a
    // text '0x41' as the byte A and an empty binary value as the two characters 0x, and a binary
    // key written that way did not match its own row.
    // Export and import pick their tools by what the server says it is.
    #[test]
    #[ignore]
    fn the_server_flavor_is_read_from_the_server() {
        if dsn().is_none() { eprintln!("NOBS_TEST_DSN not set - skipping"); return; }
        let v = scalar("SELECT VERSION()");
        assert_eq!(server_is_mariadb(&conn_j()), Some(v.to_lowercase().contains("mariadb")), "version {v}");
        let nowhere = json!({"host":"127.0.0.1","port":"1","user":"x","password":"x","ssl":"default"});
        assert_eq!(server_is_mariadb(&nowhere), None, "an unreachable server is unknown, not MariaDB or MySQL");
    }

    #[tokio::test]
    #[ignore]
    async fn compare_copies_every_value_exactly() {
        let Some(_guard) = setup_conns() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let def = "(id VARBINARY(4) PRIMARY KEY, txt TEXT NULL, bin VARBINARY(8) NULL, big MEDIUMTEXT NULL, bits BIT(8) NULL, geo GEOMETRY NULL)";
        raw("DROP DATABASE IF EXISTS cmp_val_src"); raw("CREATE DATABASE cmp_val_src CHARACTER SET utf8mb4");
        // latin1 on the target on purpose: text is converted, never poured in as bytes.
        raw("DROP DATABASE IF EXISTS cmp_val_tgt"); raw("CREATE DATABASE cmp_val_tgt CHARACTER SET latin1");
        raw(&format!("CREATE TABLE cmp_val_src.t {def}"));
        raw(&format!("CREATE TABLE cmp_val_tgt.t {def}"));
        raw("INSERT INTO cmp_val_src.t VALUES \
             (0x0001, 'NULL', X'', REPEAT('xy', 40000), b'101', ST_GeomFromText('POINT(1 2)')), \
             (0x00FF, NULL, NULL, CONCAT('a', CHAR(13), 'b', CHAR(10), 'c', CHAR(13), CHAR(10), CHAR(9), 'd'), NULL, NULL), \
             (0x41, '0x41', 0x0041, 'null', b'0', NULL), \
             (0x0A0D, CONVERT(x'C3A9' USING utf8mb4), 0x00, '', b'11111111', NULL), \
             (X'', '<&>\"''\\\\', 0x0A0D, '0x', NULL, NULL), \
             (0x0B, CONVERT(x'610062' USING utf8mb4), NULL, CONVERT(CONCAT('x', CHAR(0), 'y') USING utf8mb4), NULL, NULL)");
        let same = "SELECT COUNT(*) FROM cmp_val_src.t s JOIN cmp_val_tgt.t d ON s.id = d.id WHERE \
            CAST(CONVERT(s.txt USING utf8mb4) AS BINARY) <=> CAST(CONVERT(d.txt USING utf8mb4) AS BINARY) AND \
            s.bin <=> d.bin AND CAST(s.big AS BINARY) <=> CAST(d.big AS BINARY) AND s.bits <=> d.bits AND \
            ST_AsBinary(s.geo) <=> ST_AsBinary(d.geo)";
        let names = json!({"sourceConnName":P_RW,"sourceDb":"cmp_val_src","targetConnName":P_RW,"targetDb":"cmp_val_tgt","table":"t"});

        let ia = compare_rows_insert_all(names.clone()).await.unwrap();
        assert_eq!(ia["inserted"], 6, "insert-all: {ia}");
        assert_eq!(scalar(same), "6", "insert-all did not copy every value exactly");

        raw("DELETE FROM cmp_val_tgt.t");
        let mr = compare_rows(names.clone()).await.unwrap();
        assert_eq!(mr["missingTotal"], 6, "all 6 rows, binary keys included, are missing: {mr}");
        let ap = compare_rows_apply(json!({"targetConnName":P_RW,"targetDb":"cmp_val_tgt","table":"t",
                                           "columns":mr["columns"],"rows":mr["rows"]})).await.unwrap();
        assert!(!ap["log"].to_string().contains("FAILED"), "apply: {ap}");
        assert_eq!(scalar(same), "6", "apply did not copy every value exactly");

        raw("UPDATE cmp_val_tgt.t SET txt = 'changed', bin = 0x99, big = 'x', bits = b'1', geo = NULL");
        let dr = compare_rows_diff(names.clone()).await.unwrap();
        assert_eq!(dr["diffs"].as_array().map(|a| a.len()), Some(6), "diff: {dr}");
        let ad = compare_rows_apply_diff(json!({"targetConnName":P_RW,"targetDb":"cmp_val_tgt","table":"t",
                                                "pkCols":dr["pkCols"],"updates":dr["diffs"]})).await.unwrap();
        assert_eq!(ad["ok"], true, "apply-diff: {ad}");
        assert_eq!(scalar(same), "6", "apply-diff did not restore every value exactly");

        raw("DROP DATABASE IF EXISTS cmp_val_src");
        raw("DROP DATABASE IF EXISTS cmp_val_tgt");
    }

    // compare_rows_apply_diff updates existing target rows one at a time, unlike the insert-only
    // apply paths - a batch failing partway through used to leave some rows corrected and others
    // not, with no way back. It's now wrapped in a transaction: a mid-batch failure must leave
    // every target row exactly as it was before the apply.
    #[tokio::test]
    #[ignore]
    async fn compare_rows_apply_diff_rolls_back_a_failed_batch() {
        let Some(_guard) = setup_conns() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };

        if !caps::of(&conn_j()).check() { eprintln!("the server does not enforce CHECK - skipping"); return; }
        raw("DROP DATABASE IF EXISTS cmp_diff_rt"); raw("CREATE DATABASE cmp_diff_rt");
        raw("CREATE TABLE cmp_diff_rt.t (id INT PRIMARY KEY, qty INT CHECK (qty >= 0))");
        raw("INSERT INTO cmp_diff_rt.t VALUES (1, 10), (2, 20)");

        // Row 1's update is valid; row 2's violates the CHECK constraint and fails.
        let updates = json!([
            {"pk":[1], "colDiffs":[{"col":"qty","src":99}]},
            {"pk":[2], "colDiffs":[{"col":"qty","src":-1}]},
        ]);
        let r = compare_rows_apply_diff(json!({"targetConnName":P_RW,"targetDb":"cmp_diff_rt",
                                                "table":"t","pkCols":["id"],"updates":updates})).await.unwrap();
        assert_eq!(r["ok"], false, "the batch should fail on the CHECK constraint: {r}");
        println!("  log: {:?}", r["log"]);

        let mut conn = build_conn(&conn_j()).unwrap();
        let (_c, rows) = run_select(&mut conn, "SELECT id, qty FROM cmp_diff_rt.t ORDER BY id").unwrap();
        let qty1 = rows[0][1].clone().unwrap_or_default();
        assert_eq!(qty1, "10", "row 1's update must have been rolled back along with row 2's failure, got qty={qty1}");

        raw("DROP DATABASE IF EXISTS cmp_diff_rt");
    }

    // Compare reads TIMESTAMP values as text on one server and writes them on another; between
    // servers in different time zones every copied value moved. Its sessions now run in UTC.
    #[tokio::test]
    #[ignore]
    async fn compare_sessions_run_in_utc() {
        let Some(_guard) = setup_conns() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let (connj, _) = resolve_saved_conn(P_RW).unwrap();
        let mut c = build_conn(&connj).unwrap();
        let (_c, r) = run_select(&mut c, "SELECT @@session.time_zone").unwrap();
        assert_eq!(r[0][0].as_deref(), Some("+00:00"));
        assert_eq!(scalar("SELECT @@session.time_zone = '+00:00'"), "0", "an ordinary connection keeps the server's zone");
    }

    // A FLOAT key is read as rounded text, which matches nothing when compared to the column, so
    // such rows could neither be fetched nor updated; and an update counted as done whatever it
    // matched. Invisible columns were left out of copies (SELECT *), and generated ones made them fail.
    #[tokio::test]
    #[ignore]
    async fn compare_copies_float_keys_invisible_and_generated_columns() {
        let Some(_guard) = setup_conns() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let inv = caps::of(&conn_j()).invisible_kw();
        for db in ["cmp_kx_src", "cmp_kx_tgt"] {
            raw(&format!("DROP DATABASE IF EXISTS {db}")); raw(&format!("CREATE DATABASE {db}"));
            raw(&format!("CREATE TABLE {db}.t (k FLOAT PRIMARY KEY, a INT, secret VARCHAR(10){inv}, g INT GENERATED ALWAYS AS (a * 2) STORED)"));
        }
        raw("INSERT INTO cmp_kx_src.t (k, a, secret) VALUES (1.1, 5, 's1'), (0.3, 6, 's3')");
        raw("INSERT INTO cmp_kx_tgt.t (k, a, secret) VALUES (0.3, 0, 'old')");
        let base = json!({"sourceConnName":P_RW,"sourceDb":"cmp_kx_src","targetConnName":P_RW,"targetDb":"cmp_kx_tgt","table":"t"});

        let ins = compare_rows_insert_all(base.clone()).await.unwrap();
        assert_eq!(ins["inserted"], 1, "{ins}");
        assert_eq!(scalar("SELECT CONCAT_WS('|', a, secret, g) FROM cmp_kx_tgt.t WHERE k > 1"), "5|s1|10",
            "the missing row arrives whole, invisible column included, generated column computed");

        let diff = compare_rows_diff(base.clone()).await.unwrap();
        assert_eq!(diff["diffs"].as_array().map(|a| a.len()), Some(1), "the FLOAT-keyed common row is compared: {diff}");
        let ap = compare_rows_apply_diff(json!({"targetConnName":P_RW,"targetDb":"cmp_kx_tgt","table":"t",
            "pkCols":diff["pkCols"],"updates":diff["diffs"]})).await.unwrap();
        assert_eq!(ap["ok"], true, "{ap}");
        assert_eq!(scalar("SELECT CONCAT_WS('|', a, secret, g) FROM cmp_kx_tgt.t WHERE k < 1"), "6|s3|12", "and updated by its key");

        // A row deleted on the target since the comparison fails the batch instead of counting as updated.
        raw("DELETE FROM cmp_kx_tgt.t WHERE k < 1");
        let gone = compare_rows_apply_diff(json!({"targetConnName":P_RW,"targetDb":"cmp_kx_tgt","table":"t",
            "pkCols":diff["pkCols"],"updates":diff["diffs"]})).await.unwrap();
        assert_eq!(gone["ok"], false, "{gone}");
        assert!(gone["log"].to_string().contains("0 row(s) with this key"), "{gone}");
        raw("DROP DATABASE cmp_kx_src"); raw("DROP DATABASE cmp_kx_tgt");
    }

    // Schema sync rebuilt a column from its type, NULL, default and EXTRA: MODIFY COLUMN changed a
    // latin1_bin column to the table's default collation and dropped its comment, and a generated
    // column could not be added at all.
    #[tokio::test]
    #[ignore]
    async fn schema_sync_keeps_each_columns_full_definition() {
        let Some(_guard) = setup_conns() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        for db in ["cmp_sd_src", "cmp_sd_tgt"] { raw(&format!("DROP DATABASE IF EXISTS {db}")); raw(&format!("CREATE DATABASE {db} DEFAULT CHARACTER SET utf8mb4")); }
        raw("CREATE TABLE cmp_sd_src.t (id INT PRIMARY KEY, name VARCHAR(10) CHARACTER SET latin1 COLLATE latin1_bin NOT NULL COMMENT 'customer name', a INT, g INT GENERATED ALWAYS AS (a * 2) VIRTUAL) DEFAULT CHARSET=utf8mb4");
        raw("CREATE TABLE cmp_sd_tgt.t (id INT PRIMARY KEY, name VARCHAR(10) CHARACTER SET latin1 COLLATE latin1_bin NULL, a INT) DEFAULT CHARSET=utf8mb4");
        let r = compare_schemas(json!({"sourceConnName":P_RW,"sourceDb":"cmp_sd_src","targetConnName":P_RW,"targetDb":"cmp_sd_tgt"})).await.unwrap();
        assert_eq!(r["ok"], true, "{r}");
        let stmts: Vec<Value> = r["tables"].as_array().unwrap().iter().flat_map(|t| t["sql"].as_array().cloned().unwrap_or_default())
            .filter(|s| s["checked"] == true).map(|s| s["stmt"].clone()).collect();
        assert_eq!(stmts.len(), 2, "one MODIFY and one ADD: {stmts:?}");
        let ap = compare_apply(json!({"targetConnName":P_RW,"targetDb":"cmp_sd_tgt","statements":stmts})).await.unwrap();
        assert!(!ap["log"].to_string().contains("FAILED"), "{ap}");
        assert_eq!(scalar("SELECT CONCAT_WS('|', COLLATION_NAME, IS_NULLABLE, COLUMN_COMMENT) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA='cmp_sd_tgt' AND COLUMN_NAME='name'"),
                   "latin1_bin|NO|customer name");
        assert_eq!(scalar("SELECT LOWER(GENERATION_EXPRESSION) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA='cmp_sd_tgt' AND COLUMN_NAME='g'").replace(['`', ' ', '(', ')'], ""), "a*2");
        let again = compare_schemas(json!({"sourceConnName":P_RW,"sourceDb":"cmp_sd_src","targetConnName":P_RW,"targetDb":"cmp_sd_tgt"})).await.unwrap();
        assert!(again["tables"].as_array().unwrap().iter().all(|t| t["status"] == "same"), "nothing left to sync: {again}");
        raw("DROP DATABASE cmp_sd_src"); raw("DROP DATABASE cmp_sd_tgt");
    }

    // Missing tables were created in name order: a child named before its parent failed on its
    // foreign key. A failed CREATE TABLE is tried again once the others are in.
    #[tokio::test]
    #[ignore]
    async fn schema_sync_creates_a_child_table_after_its_parent() {
        let Some(_guard) = setup_conns() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        for db in ["cmp_fk_src", "cmp_fk_tgt"] { raw(&format!("DROP DATABASE IF EXISTS {db}")); raw(&format!("CREATE DATABASE {db}")); }
        raw("CREATE TABLE cmp_fk_src.b_parent (id INT PRIMARY KEY) ENGINE=InnoDB");
        raw("CREATE TABLE cmp_fk_src.a_child (id INT PRIMARY KEY, p INT, FOREIGN KEY (p) REFERENCES b_parent(id)) ENGINE=InnoDB");
        let r = compare_schemas(json!({"sourceConnName":P_RW,"sourceDb":"cmp_fk_src","targetConnName":P_RW,"targetDb":"cmp_fk_tgt"})).await.unwrap();
        let stmts: Vec<Value> = r["tables"].as_array().unwrap().iter().flat_map(|t| t["sql"].as_array().cloned().unwrap_or_default())
            .filter(|s| s["checked"] == true).map(|s| s["stmt"].clone()).collect();
        let ap = compare_apply(json!({"targetConnName":P_RW,"targetDb":"cmp_fk_tgt","statements":stmts})).await.unwrap();
        assert!(!ap["log"].to_string().contains("FAILED"), "{ap}");
        assert_eq!(scalar("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA='cmp_fk_tgt'"), "2");
        raw("DROP DATABASE cmp_fk_src"); raw("DROP DATABASE cmp_fk_tgt");
    }

    // Keys, foreign keys and CHECK constraints were not compared: a table that differed only there
    // showed as the same. Missing ones are added, and one only the target has is offered, unticked.
    #[tokio::test]
    #[ignore]
    async fn schema_sync_compares_keys_foreign_keys_and_checks() {
        let Some(_guard) = setup_conns() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        for db in ["cmp_key_src", "cmp_key_tgt"] { raw(&format!("DROP DATABASE IF EXISTS {db}")); raw(&format!("CREATE DATABASE {db}")); }
        for db in ["cmp_key_src", "cmp_key_tgt"] { raw(&format!("CREATE TABLE {db}.p (id INT PRIMARY KEY) ENGINE=InnoDB")); }
        raw("CREATE TABLE cmp_key_src.c (id INT PRIMARY KEY, p INT, n INT, KEY ix_n (n), CONSTRAINT fk_p FOREIGN KEY (p) REFERENCES p (id), CONSTRAINT ck_n CHECK (n >= 0)) ENGINE=InnoDB");
        raw("CREATE TABLE cmp_key_tgt.c (id INT PRIMARY KEY, p INT, n INT, KEY ix_extra (p)) ENGINE=InnoDB");
        let r = compare_schemas(json!({"sourceConnName":P_RW,"sourceDb":"cmp_key_src","targetConnName":P_RW,"targetDb":"cmp_key_tgt"})).await.unwrap();
        let c = r["tables"].as_array().unwrap().iter().find(|t| t["name"] == "c").cloned().unwrap();
        assert_eq!(c["status"], "diff", "{c}");
        let all: Vec<Value> = c["sql"].as_array().unwrap().clone();
        assert!(all.iter().any(|s| s["checked"] == false && s["stmt"].as_str().unwrap().contains("DROP INDEX `ix_extra`")), "{c}");
        let stmts: Vec<Value> = all.iter().filter(|s| s["checked"] == true).map(|s| s["stmt"].clone()).collect();
        let ap = compare_apply(json!({"targetConnName":P_RW,"targetDb":"cmp_key_tgt","statements":stmts})).await.unwrap();
        assert!(!ap["log"].to_string().contains("FAILED"), "{ap}");
        let again = compare_schemas(json!({"sourceConnName":P_RW,"sourceDb":"cmp_key_src","targetConnName":P_RW,"targetDb":"cmp_key_tgt"})).await.unwrap();
        let c2 = again["tables"].as_array().unwrap().iter().find(|t| t["name"] == "c").cloned().unwrap();
        let left: Vec<&str> = c2["sql"].as_array().unwrap().iter().map(|s| s["stmt"].as_str().unwrap()).collect();
        assert!(left.iter().all(|s| s.contains("DROP INDEX `ix_extra`")), "only the target's own index is left to decide on: {left:?}");
        raw("DROP DATABASE cmp_key_src"); raw("DROP DATABASE cmp_key_tgt");
    }

    // Names that differ only in case: a column is the same column on every server, and a table the
    // same table where the target's lower_case_table_names is not 0. Both used to be offered as
    // missing, and the ADD COLUMN or CREATE TABLE then failed. The table half only shows on a server
    // that keeps the case it was given (lower_case_table_names=2, compat.yml's MySQL 9.4); elsewhere
    // the two names are stored alike and this checks the columns alone.
    #[tokio::test]
    #[ignore]
    async fn schema_sync_matches_names_that_differ_only_in_case() {
        let Some(_guard) = setup_conns() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        for db in ["cmp_case_src", "cmp_case_tgt"] { raw(&format!("DROP DATABASE IF EXISTS {db}")); raw(&format!("CREATE DATABASE {db}")); }
        raw("CREATE TABLE cmp_case_src.Users (id INT PRIMARY KEY, Name VARCHAR(10))");
        raw("CREATE TABLE cmp_case_tgt.users (id INT PRIMARY KEY, name VARCHAR(10))");
        println!("  lower_case_table_names={}", scalar("SELECT @@lower_case_table_names"));
        let r = compare_schemas(json!({"sourceConnName":P_RW,"sourceDb":"cmp_case_src","targetConnName":P_RW,"targetDb":"cmp_case_tgt"})).await.unwrap();
        let tables: Vec<(String, String)> = r["tables"].as_array().unwrap().iter()
            .map(|t| (t["name"].as_str().unwrap().to_string(), t["status"].as_str().unwrap().to_string())).collect();
        assert_eq!(tables.len(), 1, "one table, not one missing on each side: {tables:?}");
        assert_eq!(tables[0].1, "same", "a column named in another case is the same column: {r}");
        raw("DROP DATABASE cmp_case_src"); raw("DROP DATABASE cmp_case_tgt");
    }

    // EXTRA was not compared: a target column that had lost its ON UPDATE CURRENT_TIMESTAMP or its
    // AUTO_INCREMENT showed as the same, and nothing offered to put it back.
    #[tokio::test]
    #[ignore]
    async fn schema_sync_sees_a_lost_on_update_and_auto_increment() {
        let Some(_guard) = setup_conns() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        for db in ["cmp_ex_src", "cmp_ex_tgt"] { raw(&format!("DROP DATABASE IF EXISTS {db}")); raw(&format!("CREATE DATABASE {db}")); }
        raw("CREATE TABLE cmp_ex_src.t (id INT NOT NULL AUTO_INCREMENT PRIMARY KEY, upd TIMESTAMP NULL DEFAULT NULL ON UPDATE CURRENT_TIMESTAMP)");
        raw("CREATE TABLE cmp_ex_tgt.t (id INT NOT NULL PRIMARY KEY, upd TIMESTAMP NULL DEFAULT NULL)");
        let r = compare_schemas(json!({"sourceConnName":P_RW,"sourceDb":"cmp_ex_src","targetConnName":P_RW,"targetDb":"cmp_ex_tgt"})).await.unwrap();
        let stmts: Vec<Value> = r["tables"].as_array().unwrap().iter().flat_map(|t| t["sql"].as_array().cloned().unwrap_or_default())
            .filter(|s| s["checked"] == true).map(|s| s["stmt"].clone()).collect();
        assert_eq!(stmts.len(), 2, "a MODIFY for each column: {stmts:?}");
        let ap = compare_apply(json!({"targetConnName":P_RW,"targetDb":"cmp_ex_tgt","statements":stmts})).await.unwrap();
        assert!(!ap["log"].to_string().contains("FAILED"), "{ap}");
        let again = compare_schemas(json!({"sourceConnName":P_RW,"sourceDb":"cmp_ex_src","targetConnName":P_RW,"targetDb":"cmp_ex_tgt"})).await.unwrap();
        assert!(again["tables"].as_array().unwrap().iter().all(|t| t["status"] == "same"), "nothing left to sync: {again}");
        raw("DROP DATABASE cmp_ex_src"); raw("DROP DATABASE cmp_ex_tgt");
    }
}

#[cfg(test)]
mod dump_split_tests {
    use super::*;
    // Real dumps of one database (a table named a`b, a table with a trigger and a row whose text
    // reads like a section heading, and a view), by MariaDB's and MySQL's mysqldump.
    #[test]
    fn a_whole_database_dump_splits_into_files_that_each_restore_alone() {
        for fixture in ["dump-mariadb-12.sql", "dump-mysql-8.sql"] {
            let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures").join(fixture);
            let text = std::fs::read_to_string(&src).unwrap();
            let dir = tempfile::tempdir().unwrap();
            let mut seen = Vec::new();
            let files = split_dump_by_table(&src, &mut |name: &str| {
                seen.push(name.to_string());
                dir.path().join(format!("{}.sql", name.replace('`', "_"))).to_string_lossy().into_owned()
            }).unwrap();
            let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
            // Import runs a view's file after the others (dump_file_is_view); only the view's file is one.
            for (n, p) in &files { assert_eq!(dump_file_is_view(p), n == "v", "{fixture}: {n}"); }
            assert_eq!(names, ["a`b", "t2", "v"], "{fixture}");
            assert_eq!(seen.len(), 3, "one file per name, the view's two sections included: {fixture}");
            let head_end = text.find("--\n-- Table structure").or_else(|| text.find("--\r\n-- Table structure")).unwrap();
            let foot_start = text.rfind("/*!40103 SET TIME_ZONE=@OLD_TIME_ZONE */;").unwrap();
            let body = |i: usize| std::fs::read_to_string(&files[i].1).unwrap();
            for (name, path) in &files {
                let b = std::fs::read_to_string(path).unwrap();
                assert!(b.starts_with(&text[..head_end]), "{fixture} {name}: the dump's opening lines");
                assert!(b.ends_with(&text[foot_start..]), "{fixture} {name}: the dump's closing lines");
            }
            let (ab, t2, v) = (body(0), body(1), body(2));
            assert!(ab.contains("INSERT INTO `a``b` VALUES (1)") && !ab.contains("`t2`"), "{fixture}: a`b alone");
            assert!(t2.contains("-- Table structure for table `fake`') ") || t2.contains("-- Table structure for table `fake`');"),
                "{fixture}: a value that reads like a heading stays in its row");
            assert!(t2.contains("BEFORE INSERT ON") && !t2.contains("VIEW `v`"), "{fixture}: the trigger goes with its table");
            assert!(v.contains("structure for view `v`") && v.contains("Final view structure for view `v`") && v.contains("VIEW `v` AS select"),
                "{fixture}: both parts of the view");
            assert!(!v.contains("CREATE TABLE"), "{fixture}");
            // Every line of the dump's body is in exactly one file.
            let body_lines = text[head_end..foot_start].lines().filter(|l| !l.is_empty()).count();
            let split_lines: usize = (0..3).map(|i| { let b = body(i); b[head_end..b.len() - (text.len() - foot_start)].lines().filter(|l| !l.is_empty()).count() }).sum();
            assert_eq!(split_lines, body_lines, "{fixture}");
        }
    }

    // A data-only dump (--no-create-info) has no structure headings, only "Dumping data for
    // table" ones - each table's rows still go to that table's file.
    #[test]
    fn a_data_only_dump_splits_by_its_data_headings() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("data.sql");
        let open = "-- MySQL dump
/*!40101 SET NAMES utf8mb4 */;

";
        let close = "/*!40103 SET TIME_ZONE=@OLD_TIME_ZONE */;
/*!40101 SET SQL_MODE=@OLD_SQL_MODE */;
-- Dump completed
";
        std::fs::write(&src, format!("{open}--
-- Dumping data for table `a`
--

LOCK TABLES `a` WRITE;
INSERT INTO `a` VALUES (1);
UNLOCK TABLES;

--
-- Dumping data for table `b`
--

INSERT INTO `b` VALUES (2);

{close}")).unwrap();
        let files = split_dump_by_table(&src, &mut |n: &str| dir.path().join(format!("{n}.sql")).to_string_lossy().into_owned()).unwrap();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        let a = std::fs::read_to_string(&files[0].1).unwrap();
        let b = std::fs::read_to_string(&files[1].1).unwrap();
        assert!(a.starts_with(open) && a.ends_with(close) && a.contains("INSERT INTO `a` VALUES (1)") && !a.contains("`b`"), "{a}");
        assert!(b.starts_with(open) && b.ends_with(close) && b.contains("INSERT INTO `b` VALUES (2)") && !b.contains("`a`"), "{b}");
    }

    #[test]
    fn a_dump_without_tables_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("empty.sql");
        std::fs::write(&src, "-- MySQL dump\n/*!40101 SET NAMES utf8mb4 */;\n/*!40101 SET SQL_MODE=@OLD_SQL_MODE */;\n-- Dump completed\n").unwrap();
        let files = split_dump_by_table(&src, &mut |_n: &str| panic!("no file should be made")).unwrap();
        assert!(files.is_empty());
    }
}

#[cfg(test)]
mod column_definition_tests {
    use super::*;
    fn col(name: &str, ctype: &str, cs: Option<&str>, co: Option<&str>) -> ColumnDef {
        ColumnDef { name: name.into(), ctype: ctype.into(), nullable: "NO".into(), default: None, extra: String::new(),
                    charset: cs.map(String::from), collation: co.map(String::from), comment: String::new(), generation: String::new() }
    }
    #[test]
    fn a_column_is_written_as_the_server_defines_it() {
        let create = "CREATE TABLE `t` (\n  `id` int(11) NOT NULL,\n  `we``ird` varchar(10) CHARACTER SET latin1 COLLATE latin1_bin NOT NULL COMMENT 'x, y',\n  `n` varchar(5) DEFAULT 'a',\n  `g` int(11) GENERATED ALWAYS AS (`id` * 2) VIRTUAL,\n  PRIMARY KEY (`id`)\n) ENGINE=InnoDB";
        let defs = column_definitions(create);
        assert_eq!(defs.len(), 4, "{defs:?}");
        assert_eq!(col_definition(&col("we`ird", "varchar(10)", Some("latin1"), Some("latin1_bin")), &defs),
                   "`we``ird` varchar(10) CHARACTER SET latin1 COLLATE latin1_bin NOT NULL COMMENT 'x, y'");
        assert_eq!(col_definition(&col("n", "varchar(5)", Some("utf8mb4"), Some("utf8mb4_bin")), &defs),
                   "`n` varchar(5) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin DEFAULT 'a'", "a table-default character set is spelled out");
        assert_eq!(col_definition(&col("G", "int(11)", None, None), &defs), "`g` int(11) GENERATED ALWAYS AS (`id` * 2) VIRTUAL");
        assert_eq!(col_definition(&col("missing", "int", None, None), &defs), "`missing` int NOT NULL", "without a line, the old way");
    }
}

// ---------------------------------------------------------------------------
// SSL modes
// ---------------------------------------------------------------------------
// The four ssl settings are a security promise, and until now nothing checked that any of them
// did what its name says. These pin the two halves of that promise that can be asserted without
// knowing anything about the test server's certificate:
//
//   - "required" really encrypts. A silent fallback to plaintext is the dangerous failure here,
//     because nothing in the UI would look any different.
//   - "disabled" really does not, so the two settings are distinguishable rather than both
//     landing on the crate's default.
//   - "verify" is never WEAKER than "required". It may legitimately refuse (the MariaDB and MySQL
//     servers both auto-generate a self-signed certificate, which no trust store accepts), but it
//     must not connect in the clear, and if it refuses it has to say which setting did it.
//
// Ssl_cipher is read back from the server, so this is the wire state, not what the client believes
// it negotiated.
#[cfg(test)]
mod ssl_tests {
    use super::*;

    fn conn_json(mode: &str) -> Option<Value> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let p: Vec<&str> = dsn.split(':').collect();
        if p.len() != 4 { return None; }
        Some(json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":mode}))
    }

    // Connects in `mode` and returns the cipher the SERVER reports, or the connection error.
    fn cipher_for(mode: &str) -> Result<String, String> {
        let conn = conn_json(mode).ok_or("no dsn")?;
        let mut c = build_conn(&conn)?;
        let row: Option<(String, String)> = c.query_first("SHOW STATUS LIKE 'Ssl_cipher'")
            .map_err(|e| e.to_string())?;
        Ok(row.map(|t| t.1).unwrap_or_default())
    }

    #[test]
    fn required_actually_encrypts() {
        if !caps::tls_or_skip() { return; }
        if conn_json("required").is_none() { eprintln!("NOBS_TEST_DSN not set - skipping"); return; }
        let c = cipher_for("required").expect("\"required\" must be able to connect");
        assert!(!c.is_empty(), "ssl=required connected in PLAINTEXT - the server reported no cipher");
    }

    #[test]
    fn disabled_actually_does_not_encrypt() {
        if conn_json("disabled").is_none() { eprintln!("NOBS_TEST_DSN not set - skipping"); return; }
        let c = cipher_for("disabled").expect("\"disabled\" must be able to connect");
        assert!(c.is_empty(), "ssl=disabled negotiated TLS anyway (cipher {c}) - the setting did nothing");
    }

    #[test]
    fn verify_is_never_weaker_than_required() {
        if !caps::tls_or_skip() { return; }
        if conn_json("verify").is_none() { eprintln!("NOBS_TEST_DSN not set - skipping"); return; }
        match cipher_for("verify") {
            // Refusing is the expected outcome against a self-signed certificate. What must not
            // happen is refusing uninformatively - the raw TlsError names neither the setting
            // that caused it nor the thing to do about it.
            Err(e) => assert!(e.contains("\"verify\""),
                "ssl=verify refused without explaining which setting refused: {e}"),
            // If the server does have a trusted certificate, verify has to be at least as strong
            // as required - i.e. encrypted. Connecting in the clear would mean it fell back.
            Ok(c) => assert!(!c.is_empty(),
                "ssl=verify connected in PLAINTEXT - it fell back instead of verifying"),
        }
    }

    // The live test above cannot tell "verify genuinely validated a trusted certificate" from
    // "verify was quietly downgraded to accept anything" - both connect, both over TLS. This can.
    #[test]
    fn verify_validates_and_required_does_not_pretend_to() {
        let v = ssl_opts_for("verify", None).expect("verify must use TLS");
        assert!(!v.accept_invalid_certs(),
            "ssl=verify accepts invalid certificates - it encrypts but verifies nothing");
        assert!(!v.skip_domain_validation(),
            "ssl=verify skips hostname validation - the certificate could be for any host");

        // "required" is the deliberately unverified one; that is the whole difference between them.
        let r = ssl_opts_for("required", None).expect("required must use TLS");
        assert!(r.accept_invalid_certs(),
            "ssl=required would reject the self-signed certificate a default server install uses");

        // And the settings that mean "no TLS" must not quietly turn it on.
        assert!(ssl_opts_for("disabled", None).is_none());
        assert!(ssl_opts_for("default", None).is_none());
    }

    // A CA makes "verify" usable against a private or self-signed server, which is the ordinary
    // case - both MariaDB and MySQL generate a self-signed certificate when none is configured,
    // and no OS trust store will ever accept one of those.
    #[test]
    fn a_ca_is_used_for_verify_and_only_for_verify() {
        let v = ssl_opts_for("verify", Some(r"C:\certs\ca.pem")).expect("verify must use TLS");
        assert_eq!(v.root_cert_path().map(|p| p.to_string_lossy().to_string()),
            Some(r"C:\certs\ca.pem".to_string()), "the CA did not reach the connection");
        // Supplying a CA must not weaken anything else about verify.
        assert!(!v.accept_invalid_certs(), "a CA must not turn verification off");
        assert!(!v.skip_domain_validation(), "a CA must not turn off hostname checking");

        // "required" verifies nothing by definition, so a CA there would be set and then ignored.
        // Better to not apply it at all than to imply a check that is not happening.
        let r = ssl_opts_for("required", Some(r"C:\certs\ca.pem")).unwrap();
        assert!(r.root_cert_path().is_none(),
            "ssl=required accepts any certificate, so a CA there would be decoration");

        // And the no-TLS settings stay no-TLS whatever is configured alongside them.
        assert!(ssl_opts_for("disabled", Some(r"C:\certs\ca.pem")).is_none());
        assert!(ssl_opts_for("default", Some(r"C:\certs\ca.pem")).is_none());
    }

    #[test]
    fn only_verify_gets_the_certificate_note() {
        let tls = "TlsError { a root certificate which is not trusted }";
        assert!(explain_conn_error("verify", false, tls).contains("CA certificate"),
            "a verify failure with no CA should say to set one");
        assert!(explain_conn_error("verify", false, tls).contains("\"required\""),
            "...and point at the setting that would work without one");
        // Other modes, and non-TLS failures, must pass through untouched - a wrong password in
        // verify mode should not be answered with advice about certificates.
        assert_eq!(explain_conn_error("required", false, tls), tls);
        assert_eq!(explain_conn_error("verify", false, "Access denied for user 'x'"), "Access denied for user 'x'");
    }

    // PAM accounts: what the driver says is right and says nothing about what to do.
    #[test]
    fn a_pam_sign_in_that_cannot_happen_says_what_would_make_it() {
        let clear = explain_conn_error("disabled", false, "Driver error: `mysql_clear_password must be enabled on the client side'");
        assert!(clear.contains("not encrypted") && clear.contains("\"required\""), "{clear}");
        let switched = explain_conn_error("disabled", false, "DriverError { Unknown authentication protocol: `mysql_clear_password` }");
        assert!(switched.contains("not encrypted"), "{switched}");
        let dialog = explain_conn_error("default", false, "Driver error: `Unknown authentication protocol: `dialog`'");
        assert!(dialog.contains("pam_use_cleartext_plugin=ON"), "{dialog}");
    }

    // Once a CA has been supplied, "set a CA" is no longer useful advice. The real error texts below
    // are what Windows produced against MySQL 8's auto-generated certificate.
    #[test]
    fn a_failure_despite_a_ca_gives_different_advice() {
        let untrusted = "TlsError { A certificate chain processed, but terminated in a root certificate which is not trusted by the trust provider. (os error -2146762487) }";
        let msg = explain_conn_error("verify", true, untrusted);
        assert!(!msg.contains("Set \"CA certificate\""),
            "telling someone to set the CA they already set is not advice: {msg}");
        assert!(msg.contains("not signed by it"), "with a CA given, an untrusted root means the wrong CA: {msg}");
    }

    // The right CA, the wrong name. The chain was fine; blaming the CA file would send the user off
    // swapping files that were never the problem. What fixes it is a mode.
    #[test]
    fn a_name_mismatch_points_at_verify_ca_not_at_the_file() {
        let mismatch = "TlsError { The certificate's CN name does not match the passed value. (os error -2146762481) }";
        for has_ca in [true, false] {
            let msg = explain_conn_error("verify", has_ca, mismatch);
            assert!(msg.contains("\"verify-ca\""), "a name mismatch should point at verify-ca: {msg}");
            assert!(!msg.contains("not signed by it"), "the chain checked out - the CA is not the problem: {msg}");
        }
    }

    #[test]
    fn verify_ca_checks_the_chain_but_not_the_name() {
        let v = ssl_opts_for("verify-ca", Some(r"C:\certs\ca.pem")).expect("verify-ca must use TLS");
        // The one thing it relaxes.
        assert!(v.skip_domain_validation(), "verify-ca should not check the host name");
        // And the things it must not.
        assert!(!v.accept_invalid_certs(),
            "verify-ca accepts invalid certificates - that is not 'verify the CA', it is 'required'");
        assert_eq!(v.root_cert_path().map(|p| p.to_string_lossy().to_string()),
            Some(r"C:\certs\ca.pem".to_string()), "the CA did not reach verify-ca");
        // Without a CA it still verifies, against the system store.
        let bare = ssl_opts_for("verify-ca", None).unwrap();
        assert!(!bare.accept_invalid_certs() && bare.root_cert_path().is_none());
        // And its failures get the same certificate advice as verify's.
        assert!(explain_conn_error("verify-ca", false, "TlsError { not trusted }").contains("CA certificate"));
    }
}

// ---------------------------------------------------------------------------
// Losing the connection in the middle of applying changes
// ---------------------------------------------------------------------------
// Staged grid edits, a table-designer apply and a compare-apply all send several statements that
// only make sense together, wrapped in START TRANSACTION/COMMIT. That protects against a statement
// FAILING. It says nothing about the connection going away halfway through, which is the failure
// mode a laptop lid, a VPN drop or a server restart actually produces.
//
// These sever the connection for real - a second session KILLs the one running the batch - rather
// than simulating it, because what is being checked is the server's behaviour as much as ours.
#[cfg(test)]
mod conn_loss_tests {
    use super::*;

    fn dsn_json() -> Option<Value> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let p: Vec<&str> = dsn.split(':').collect();
        if p.len() != 4 { return None; }
        Some(json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"}))
    }

    fn exec(sql: &str) {
        let mut c = build_conn(&dsn_json().unwrap()).unwrap();
        c.query_drop(sql).unwrap();
    }
    fn count(sql: &str) -> i64 {
        let mut c = build_conn(&dsn_json().unwrap()).unwrap();
        c.query_first::<i64, _>(sql).unwrap().unwrap_or(-1)
    }

    // Kills whichever connection is running a statement containing `needle`, waiting for it to
    // show up. Returns false if it never did.
    fn kill_running(needle: &str) -> bool {
        let mut c = build_conn(&dsn_json().unwrap()).unwrap();
        for _ in 0..100 {
            let found: Vec<(u64, Option<String>)> = c
                .query("SELECT ID, INFO FROM information_schema.PROCESSLIST")
                .unwrap();
            for (id, info) in found {
                if info.as_deref().map(|s| s.contains(needle)).unwrap_or(false) {
                    let _ = c.query_drop(format!("KILL {id}"));
                    return true;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    #[tokio::test]
    #[ignore]
    async fn a_connection_lost_mid_batch_leaves_nothing_behind() {
        if dsn_json().is_none() { eprintln!("NOBS_TEST_DSN not set - skipping"); return }
        exec("CREATE DATABASE IF NOT EXISTS nobs_test");
        exec("DROP TABLE IF EXISTS nobs_test.tx_drop");
        exec("CREATE TABLE nobs_test.tx_drop (id INT PRIMARY KEY) ENGINE=InnoDB");

        // Two rows land, then the batch parks on a SLEEP long enough to be killed from outside,
        // then a third row that must never be reached. Exactly the shape of a grid apply that
        // loses its connection partway down the list.
        let sql = "INSERT INTO nobs_test.tx_drop VALUES (1);\n\
                   INSERT INTO nobs_test.tx_drop VALUES (2);\n\
                   SELECT SLEEP(30) /*nobs_kill_me*/;\n\
                   INSERT INTO nobs_test.tx_drop VALUES (3);";
        let req = json!({"sql": sql, "conn": dsn_json().unwrap(), "db": "nobs_test", "transaction": true});

        let killer = std::thread::spawn(|| kill_running("nobs_kill_me"));
        let res = script(req).await.unwrap();
        assert!(killer.join().unwrap(), "never found the batch in the processlist to kill it");

        // The two rows that HAD been inserted must be gone: the transaction never committed, and
        // the server rolls back an open transaction when its connection dies.
        let left = count("SELECT COUNT(*) FROM nobs_test.tx_drop");
        assert_eq!(left, 0, "{left} row(s) survived a connection lost mid-transaction - the apply was partial");

        // And it has to SAY so. Reporting ok:true here would be the worst outcome of the three:
        // the user closes the dialog believing their edits are saved.
        assert_eq!(res["ok"], false, "a batch whose connection was killed reported success: {res}");

        exec("DROP TABLE IF EXISTS nobs_test.tx_drop");
    }

    // The claim the error message makes has to be one we can actually stand behind.
    #[test]
    fn a_failed_commit_does_not_promise_a_rollback_it_cannot_verify() {
        // A statement that fails before COMMIT is genuinely undone - by ROLLBACK if the connection
        // is alive, by the server on disconnect if it is not - so that message is honest.
        // A COMMIT that fails is different: if the connection broke while it was in flight, the
        // server may well have committed and simply never got the answer back to us. Claiming
        // "No changes were applied" there is a guess presented as a fact.
        let msg = commit_failure_message("Lost connection to server during query");
        assert!(!msg.contains("No changes were applied"),
            "a failed COMMIT must not claim the changes were not applied - it cannot know that");
        assert!(msg.to_lowercase().contains("may") || msg.to_lowercase().contains("check"),
            "a failed COMMIT should say the outcome is uncertain and to go and check: {msg}");
    }
}

// ---------------------------------------------------------------------------
// Several things happening at once
// ---------------------------------------------------------------------------
// Every command opens its own connection and every cursor owns its own thread, so there is no
// shared connection to race on by construction. What IS shared is the cursor registry - one
// global HashMap keyed by a generated id - and the failure that would produce is the quiet kind:
// not a crash, but one grid being handed another grid's rows.
//
// These run real concurrent cursors over disjoint id ranges, so any cross-talk shows up as rows
// that simply do not belong to the range that asked for them.
#[cfg(test)]
mod concurrency_tests {
    use super::*;

    fn conn_json() -> Option<Value> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let p: Vec<&str> = dsn.split(':').collect();
        if p.len() != 4 { return None; }
        Some(json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":"default"}))
    }

    // Pages one cursor to exhaustion, returning every id it produced in order.
    async fn page_range(conn: &Value, lo: i64, hi: i64, page: usize) -> Vec<i64> {
        let sql = format!("SELECT id FROM bulk_rows WHERE id >= {lo} AND id < {hi} ORDER BY id");
        let first = query(json!({"sql": sql, "conn": conn, "db": "nobs_test", "pageSize": page})).await.unwrap();
        assert_eq!(first["ok"], true, "opening cursor for [{lo},{hi}) failed: {first}");
        let take = |v: &Value| -> Vec<i64> {
            v["rows"].as_array().cloned().unwrap_or_default().iter()
                .map(|r| r[0].as_str().unwrap_or("0").parse().unwrap_or(-1)).collect()
        };
        let mut got = take(&first);
        if !first["hasMore"].as_bool().unwrap_or(false) { return got; }
        let cid = first["cursorId"].as_str().unwrap_or("").to_string();
        assert!(!cid.is_empty(), "more pages but no cursorId for [{lo},{hi})");
        loop {
            let n = fetch_cursor_batch(json!({"cursorId": cid, "pageSize": page})).await.unwrap();
            assert_eq!(n["ok"], true, "fetch for [{lo},{hi}) failed: {n}");
            got.extend(take(&n));
            if !n["hasMore"].as_bool().unwrap_or(false) { break; }
        }
        got
    }

    // Eight cursors over disjoint ranges, all paging at the same time. Each must come back with
    // exactly its own range - a registry mix-up would hand one of them another's rows.
    #[tokio::test]
    #[ignore]
    async fn concurrent_cursors_do_not_hand_each_other_rows() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let ranges: Vec<(i64, i64)> = (0..8).map(|i| (i * 1000, i * 1000 + 1000)).collect();
        let tasks: Vec<_> = ranges.iter().map(|&(lo, hi)| {
            let c = conn.clone();
            tokio::spawn(async move { (lo, hi, page_range(&c, lo, hi, 137).await) })
        }).collect();
        for t in tasks {
            let (lo, hi, got) = t.await.unwrap();
            let want: Vec<i64> = (lo..hi).collect();
            assert_eq!(got.len(), want.len(), "range [{lo},{hi}) returned {} rows, expected {}", got.len(), want.len());
            assert_eq!(got, want, "range [{lo},{hi}) came back with rows that are not its own");
        }
    }

    // Cursor ids are minted from a counter plus a timestamp. Two cursors opening in the same
    // nanosecond must still differ, or one would evict the other from the registry and its owner
    // would silently start reading the other's result set.
    #[test]
    fn cursor_ids_are_unique_under_contention() {
        let threads: Vec<_> = (0..8).map(|_| {
            std::thread::spawn(|| (0..500).map(|_| next_cursor_id()).collect::<Vec<_>>())
        }).collect();
        let all: Vec<String> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
        let uniq: std::collections::HashSet<&String> = all.iter().collect();
        assert_eq!(uniq.len(), all.len(), "{} of {} cursor ids collided", all.len() - uniq.len(), all.len());
    }

    // Closing a cursor while another is mid-page must not disturb the one still reading, and
    // closing one twice must stay harmless - the frontend fires close defensively.
    #[tokio::test]
    #[ignore]
    async fn closing_one_cursor_leaves_the_others_alone() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let sql = "SELECT id FROM bulk_rows WHERE id < 5000 ORDER BY id";
        let a = query(json!({"sql": sql, "conn": conn, "db": "nobs_test", "pageSize": 100})).await.unwrap();
        let b = query(json!({"sql": sql, "conn": conn, "db": "nobs_test", "pageSize": 100})).await.unwrap();
        let (ca, cb) = (a["cursorId"].as_str().unwrap().to_string(), b["cursorId"].as_str().unwrap().to_string());
        assert_ne!(ca, cb, "two concurrently opened cursors were given the same id");

        close_cursor(json!({"cursorId": ca})).await.unwrap();
        // Twice - the UI closes defensively without checking whether anything is left to close.
        let again = close_cursor(json!({"cursorId": ca})).await.unwrap();
        assert_eq!(again["ok"], true, "closing an already-closed cursor should be harmless: {again}");

        // The survivor keeps working and keeps its own rows.
        let n = fetch_cursor_batch(json!({"cursorId": cb, "pageSize": 100})).await.unwrap();
        assert_eq!(n["ok"], true, "closing one cursor broke another: {n}");
        let ids: Vec<i64> = n["rows"].as_array().unwrap().iter()
            .map(|r| r[0].as_str().unwrap_or("0").parse().unwrap_or(-1)).collect();
        assert_eq!(ids, (100..200).collect::<Vec<i64>>(), "the surviving cursor lost its place");

        // And the closed one is genuinely gone rather than still answering.
        let dead = fetch_cursor_batch(json!({"cursorId": ca, "pageSize": 10})).await.unwrap();
        assert_eq!(dead["ok"], false, "a closed cursor still served rows: {dead}");
        close_cursor(json!({"cursorId": cb})).await.unwrap();
    }

    // Plain concurrent queries, no cursors: each opens its own connection, so the only way these
    // can go wrong is if something is shared that should not be.
    #[tokio::test]
    #[ignore]
    async fn concurrent_queries_each_get_their_own_answer() {
        let Some(conn) = conn_json() else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let tasks: Vec<_> = (0..12i64).map(|i| {
            let c = conn.clone();
            tokio::spawn(async move {
                let sql = format!("SELECT COUNT(*) FROM bulk_rows WHERE id < {}", i * 100);
                let r = query(json!({"sql": sql, "conn": c, "db": "nobs_test", "pageSize": 10})).await.unwrap();
                (i, r["rows"][0][0].as_str().unwrap_or("?").to_string())
            })
        }).collect();
        for t in tasks {
            let (i, got) = t.await.unwrap();
            assert_eq!(got, (i * 100).to_string(), "query {i} got another query's answer");
        }
    }
}

// ---------------------------------------------------------------------------
// What the CLI tools are actually told
// ---------------------------------------------------------------------------
// Export and import do not go through the native driver - they shell out to mysqldump/mysql with
// a generated [client] options file. Two things were missing from it, and both only showed up
// against a real MySQL 8 server.
#[cfg(test)]
mod cnf_tests {
    use super::*;

    // Mutually exclusive dialects: the wrong one is not a weaker connection, it is an unknown
    // option and no connection at all. Both directions verified against the real binaries -
    // MariaDB client 15.2 and MySQL client 8.0.46.
    #[test]
    fn each_client_is_given_the_option_names_it_understands() {
        assert_eq!(ssl_cnf_lines("disabled", true),  vec!["skip-ssl"]);
        assert_eq!(ssl_cnf_lines("required", true),  vec!["ssl", "skip-ssl-verify-server-cert"]);
        assert_eq!(ssl_cnf_lines("verify",   true),  vec!["ssl", "ssl-verify-server-cert"]);
        assert_eq!(ssl_cnf_lines("disabled", false), vec!["ssl-mode=DISABLED", "loose-get-server-public-key"]);
        assert_eq!(ssl_cnf_lines("required", false), vec!["ssl-mode=REQUIRED"]);
        assert_eq!(ssl_cnf_lines("verify",   false), vec!["ssl-mode=VERIFY_IDENTITY"]);
        // MySQL's client has an exact equivalent of verify-ca.
        assert_eq!(ssl_cnf_lines("verify-ca", false), vec!["ssl-mode=VERIFY_CA"]);
        // MariaDB's does not, and gets the STRICTER mapping rather than a weaker one: measured, it
        // checks the host name whenever a CA is supplied (bar loopback), and skipping its verify
        // flag neither relaxes that nor - importantly - is safe to rely on as "chain only".
        assert_eq!(ssl_cnf_lines("verify-ca", true), ssl_cnf_lines("verify", true),
            "verify-ca on the MariaDB client must map to full verification, never to less");

        // The two vocabularies must not overlap anywhere, or a mix-up could go unnoticed.
        for mode in ["disabled", "required", "verify", "verify-ca"] {
            let (m, y) = (ssl_cnf_lines(mode, true), ssl_cnf_lines(mode, false));
            assert!(m.iter().all(|l| !y.contains(l)), "dialects overlap for {mode}: {m:?} vs {y:?}");
        }
    }

    // "default" means leave it to the client, so it must write nothing rather than guess - except
    // that MariaDB's client (11.4+) would then check the certificate, which "default" does not ask for.
    #[test]
    fn default_and_unknown_modes_write_nothing() {
        for maria in [true, false] {
            if maria { assert_eq!(ssl_cnf_lines("default", maria), vec!["skip-ssl-verify-server-cert"]); }
            else { assert!(ssl_cnf_lines("default", maria).is_empty()); }
            assert!(ssl_cnf_lines("", maria).is_empty());
            assert!(ssl_cnf_lines("bogus", maria).is_empty());
        }
    }

    // The ssl setting used to stop at the native driver: a connection saved as "required" was
    // dumped over whatever the CLI happened to negotiate, and one saved as "disabled" likewise.
    #[test]
    fn the_options_file_carries_the_ssl_setting() {
        let conn = json!({"host":"h","port":"3306","user":"u","password":"p","ssl":"required"});
        let (_f, path) = cnf_file(&conn, "definitely-not-a-real-binary").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("\nssl\n") || body.ends_with("ssl\n"),
            "ssl=required did not reach the options file:\n{body}");
        assert!(body.contains("password=\"p\""), "the rest of the file is still written:\n{body}");
        assert!(body.contains("\ndefault-character-set=utf8mb4\n"), "the client character set is pinned:\n{body}");
        assert!(!body.contains("init-command"), "a MariaDB client asks for utf8mb4 in a way every server knows:\n{body}");
        // Any tool whose --version does not say MariaDB counts as MySQL's; cargo is always at hand.
        let (_f3, p3) = cnf_file(&conn, &std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())).unwrap();
        let b3 = std::fs::read_to_string(&p3).unwrap();
        assert!(b3.contains("\nloose-init-command=SET NAMES utf8mb4\n"), "a MySQL client says SET NAMES itself, which 5.7 understands:\n{b3}");
        // A PAM password goes as typed, so MySQL's client may send it only over TLS that checks the
        // server, or where the connection says so.
        assert!(!b3.contains("cleartext"), "ssl=required checks no certificate, so not unless asked:\n{b3}");
        let mut pam = conn.clone(); pam["clearPw"] = json!(true);
        let (_f5, p5) = cnf_file(&pam, &std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())).unwrap();
        assert!(std::fs::read_to_string(&p5).unwrap().contains("\nloose-enable-cleartext-plugin\n"), "ssl=required with clearPw allows a PAM sign-in");
        let mut ver = conn.clone(); ver["ssl"] = json!("verify-ca");
        let (_f6, p6) = cnf_file(&ver, &std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())).unwrap();
        assert!(std::fs::read_to_string(&p6).unwrap().contains("\nloose-enable-cleartext-plugin\n"), "a verify mode allows it without asking");
        let plain = json!({"host":"h","port":"3306","user":"u","ssl":"default"});
        let (_f4, p4) = cnf_file(&plain, &std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())).unwrap();
        assert!(!std::fs::read_to_string(&p4).unwrap().contains("cleartext"), "ssl=default may be plaintext, so it does not");

        // ...and "default" forces nothing on a connection that did not ask: no TLS switched on, no
        // verification. (A MariaDB client is told not to verify, which 11.4+ would otherwise do.)
        let (_f2, p2) = cnf_file(&plain, "definitely-not-a-real-binary").unwrap();
        let b2 = std::fs::read_to_string(&p2).unwrap();
        assert!(!b2.lines().any(|l| l == "ssl" || l.starts_with("ssl-mode") || l == "ssl-verify-server-cert"),
            "ssl=default should neither require nor verify:\n{b2}");
    }

    // A dump should go out under the same verification as everything else. For MySQL's client this
    // is not a refinement either: it REFUSES ssl-mode=VERIFY_* outright without a CA, with
    // "CA certificate is required if ssl-mode is VERIFY_CA or VERIFY_IDENTITY".
    #[test]
    fn the_options_file_carries_the_ca_when_verifying() {
        let with_ca = |ssl: &str| {
            let c = json!({"host":"h","port":"3306","user":"u","ssl":ssl,"sslCa":r"C:\certs\ca.pem"});
            let (_f, p) = cnf_file(&c, "definitely-not-a-real-binary").unwrap();
            std::fs::read_to_string(&p).unwrap()
        };
        // Backslashes are doubled, because the option-file parser treats them as escapes.
        assert!(with_ca("verify").contains(r"ssl-ca=C:\\certs\\ca.pem"),
            "the CA did not reach the options file:\n{}", with_ca("verify"));
        assert!(with_ca("verify-ca").contains(r"ssl-ca=C:\\certs\\ca.pem"),
            "verify-ca is the mode a CA matters most for, and it did not reach the options file:\n{}", with_ca("verify-ca"));

        // Only where it means something. "required" accepts any certificate, so writing a CA
        // there would imply a check that is not happening.
        for ssl in ["required", "disabled", "default"] {
            assert!(!with_ca(ssl).contains("ssl-ca"),
                "ssl={ssl} does not verify anything, so it should not carry a CA:\n{}", with_ca(ssl));
        }

        // And no CA configured writes no line, rather than an empty one the client would reject.
        let none = json!({"host":"h","port":"3306","user":"u","ssl":"verify","sslCa":""});
        let (_f, p) = cnf_file(&none, "definitely-not-a-real-binary").unwrap();
        assert!(!std::fs::read_to_string(&p).unwrap().contains("ssl-ca"),
            "an empty CA should be left out entirely, not written as ssl-ca=");
    }

    // MariaDB's client on "required" carried on in plaintext when the server had no TLS. The guard
    // goes in the options file as loose- (mariadb-dump has no init-command), and only for that client.
    #[test]
    fn a_mariadb_client_on_required_is_given_the_tls_guard() {
        let conn = json!({"host":"h","port":"3306","user":"u","ssl":"required"});
        let g = tls_guard_sql(true);
        let (_f, p) = cnf_file_with(&conn, "definitely-not-a-real-binary", Some(&g)).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains(&format!("\nloose-init-command={g}\n")), "{body}");
        assert!(g.contains("information_schema.SESSION_STATUS") && tls_guard_sql(false).contains("performance_schema.session_status"));
        // Not asked for: default, disabled and the verify modes (verifying already insists on TLS).
        for ssl in ["default", "disabled", "verify"] {
            assert_eq!(tls_guard_for(&json!({"host":"h","port":"1","ssl":ssl}), "definitely-not-a-real-binary").unwrap(), None, "{ssl}");
        }
    }

    // The guard itself, run by the real client: an encrypted session carries on untouched, and one
    // the server did not encrypt is refused naming the reason. compat.yml's MariaDB 10.2 has no TLS,
    // so between the servers CI runs, both halves are checked.
    #[test]
    #[ignore]
    fn the_tls_guard_refuses_exactly_the_sessions_that_are_not_encrypted() {
        let (Ok(dsn), Ok(mbin)) = (std::env::var("NOBS_TEST_DSN"), std::env::var("MYSQL_BIN")) else { eprintln!("NOBS_TEST_DSN or MYSQL_BIN not set - skipping"); return };
        if !client_is_mariadb(&mbin) { eprintln!("MYSQL_BIN is not MariaDB's client - skipping"); return; }
        let d: Vec<&str> = dsn.splitn(4, ':').collect();
        let conn = json!({"host":d[0],"port":d[1],"user":d[2],"password":d[3],"ssl":"required"});
        let cap = caps::of(&json!({"host":d[0],"port":d[1],"user":d[2],"password":d[3],"ssl":"default"}));
        let (_f, cnf) = cnf_file_with(&conn, &mbin, Some(&tls_guard_sql(cap.maria))).unwrap();
        let out = Command::new(&mbin).args([format!("--defaults-extra-file={cnf}"), "-N".into(), "-e".into(), "SHOW SESSION STATUS LIKE 'Ssl_cipher'".into()]).output().unwrap();
        let (said, err) = (String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string());
        if cap.tls {
            assert!(out.status.success(), "an encrypted session was refused: {err}");
            assert!(said.split_whitespace().nth(1).is_some(), "it said it was encrypted but reports no cipher: {said}");
        } else {
            assert!(!out.status.success(), "a session without TLS went ahead on \"required\": {said}");
            assert!(err.contains("NOT_ENCRYPTED_BUT_SSL_MODE_IS_REQUIRED"), "{err}");
        }
    }

    // A client from a real MariaDB or MySQL installation has its own lib/plugin next door and
    // finds the right plugins itself; redirecting it at another product's would break it.
    #[test]
    fn only_our_own_tools_are_pointed_at_our_plugin_directory() {
        assert!(tools_plugin_dir(r"C:\Program Files\MariaDB 12.3\bin\mysql.exe").is_none(),
            "a client from a full installation must not be redirected");
        assert!(tools_plugin_dir("mysql.exe").is_none(), "a bare name has no directory to judge");

        // Our own tools dir, but only once the plugins are actually there.
        let ours = tools_dir().join("mysql.exe");
        let plugin = tools_dir().join("plugin");
        let existed = plugin.exists();
        if !existed { assert!(tools_plugin_dir(&ours.to_string_lossy()).is_none(),
            "nothing should be claimed before the plugins are unpacked"); }
        std::fs::create_dir_all(&plugin).unwrap();
        assert_eq!(tools_plugin_dir(&ours.to_string_lossy()), Some(plugin.clone()),
            "our own client should be pointed at the plugins we unpacked");
        if !existed { let _ = std::fs::remove_dir(&plugin); }
    }
}

// ---------------------------------------------------------------------------
// The CA certificate is actually used, not just carried around
// ---------------------------------------------------------------------------
// Handing a CA path to the driver proves nothing by itself. "verify" refuses a self-signed server
// with no CA, with the wrong CA, and with a CA that is being silently ignored - all three look the
// same from outside. The only thing that tells them apart is the RIGHT CA succeeding where the
// wrong one fails, with nothing else changed. So the decisive test needs the server's real CA:
//
//   NOBS_TEST_SERVER_CA   path to the CA that signed the test server's certificate
//
// For a MySQL server with an auto-generated certificate that is ca.pem in its data directory -
// often readable only by an administrator. The server also sends it in the TLS handshake, so it
// can be taken off the wire without any special access:
//
//   echo | openssl s_client -starttls mysql -connect 127.0.0.1:3308 -showcerts //     | awk '/BEGIN CERT/{n++} n==2{print} /END CERT/ && n==2{exit}' > server-ca.pem
//
// Without it the tests that need it say so and pass, like every other live test here.
#[cfg(test)]
mod ssl_ca_tests {
    use super::*;

    // A genuine self-signed CA (CN=NOBS Test Bogus CA, basicConstraints CA:TRUE, valid 2026-2031).
    // It signed nothing, so no server certificate anywhere validates against it.
    const UNRELATED_CA: &str = r"-----BEGIN CERTIFICATE-----
MIICzzCCAbegAwIBAgIIVjZZKgNf6DswDQYJKoZIhvcNAQELBQAwHTEbMBkGA1UEAxMSTk9CUyBU
ZXN0IEJvZ3VzIENBMB4XDTI2MDkxNTE3MzEwMloXDTMxMDkxNjE3MzEwMlowHTEbMBkGA1UEAxMS
Tk9CUyBUZXN0IEJvZ3VzIENBMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA9yv5N93T
tLdgg7m1eeYfAsTgmdljah78mWt2wzLqoL+YhC/TAYprQmoU3mLWYoJctX0868xPZ5Ig7wJHsd6d
fYkeisZuz9NWvumcimRH5HR+9S9ByGCFbT5CdADePKkMVIPuqKOL3WP/4Pyu0u3JmozgR8kV5F5Q
nMxK9UXtqIYe3hMqKH7VVuCfCo16szABnoO7LgZwaD2KaJgM/zSHDfAMeG151/NWLd4gtiwhwfWx
kfP8+6sufhTHv0bZ6X9/MmSWhnK23wb+o6y5Z/W7c7qKLteEx+ZouGvG8l6jKAqFeZAj4opIYOsS
7RIIWaS+3797nBjYrV6dlYW/F6kAHQIDAQABoxMwETAPBgNVHRMBAf8EBTADAQH/MA0GCSqGSIb3
DQEBCwUAA4IBAQBkwMAf+HgW6MR/T2V16xr7Qr8b2KzviFOxmrSubMM7pRwQV6F4DySfXOfdnmlb
hKUAoG4m8MES+y7Q2bi2h+2xxBejExquVhzXjW20BL910qTQ3gXdB65IP98p+kyKIrKBF7ZxQugV
AnR5YrkT2imF+/5Exr0MeLZ8L2yLuE0o6jSXBgEeBI7zeEqqEvAk2x6fDJuzCPwLa8vRMfwwFA3X
EFlWtFN8E4G8dWsBF6ELCTRTDlhvJa4OVPVyjmtolmmeWFn+G2J9vOulYfoYUXLMAg4tK+GE0wSS
s+GblpHbDz1GCdRkiTPZKgv8QnJrmZQthFmp2EzeKamcSe1q+U4O
-----END CERTIFICATE-----
";

    fn conn_with(mode: &str, ca: &str) -> Option<Value> {
        let dsn = std::env::var("NOBS_TEST_DSN").ok()?;
        let p: Vec<&str> = dsn.split(':').collect();
        if p.len() != 4 { return None; }
        Some(json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":mode,"sslCa":ca}))
    }
    fn server_ca() -> Option<String> {
        std::env::var("NOBS_TEST_SERVER_CA").ok().filter(|p| std::path::Path::new(p).exists())
    }
    fn unrelated_ca_file() -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(".pem").tempfile().unwrap();
        f.write_all(UNRELATED_CA.as_bytes()).unwrap();
        f
    }
    fn cipher(mut c: Conn) -> String {
        let row: Option<(String, String)> = c.query_first("SHOW STATUS LIKE 'Ssl_cipher'").unwrap();
        row.map(|r| r.1).unwrap_or_default()
    }

    // The pair that proves the CA decides the outcome: same mode, same server, only the CA differs.
    #[test]
    fn verify_ca_accepts_the_servers_own_ca_and_refuses_any_other() {
        let Some(ca) = server_ca() else { eprintln!("NOBS_TEST_SERVER_CA not set - skipping"); return };
        let Some(right) = conn_with("verify-ca", &ca) else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let c = build_conn(&right).unwrap_or_else(|e| panic!("ssl=verify-ca refused the server's own CA: {e}"));
        assert!(!cipher(c).is_empty(), "ssl=verify-ca connected, but not over TLS");

        let wrong_file = unrelated_ca_file();
        let wrong = conn_with("verify-ca", &wrong_file.path().to_string_lossy()).unwrap();
        // verify-ca skips the host name check - and must skip ONLY that. If it had switched chain
        // validation off along with it, this would connect.
        assert!(build_conn(&wrong).is_err(),
            "ssl=verify-ca accepted a CA that never signed the server's certificate - the chain is not being checked");
    }

    // "verify" keeps checking the host name. Against a server whose certificate names something
    // else, the right CA is not enough - and the message has to point at the mode, not the file.
    #[test]
    fn verify_with_the_right_ca_explains_a_name_mismatch() {
        let Some(ca) = server_ca() else { eprintln!("NOBS_TEST_SERVER_CA not set - skipping"); return };
        let Some(conn) = conn_with("verify", &ca) else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        match build_conn(&conn) {
            // A server whose certificate really does name this host: nothing to explain.
            Ok(c) => assert!(!cipher(c).is_empty(), "ssl=verify connected, but not over TLS"),
            Err(e) => {
                if e.contains("CN name does not match") {
                    assert!(e.contains("verify-ca"),
                        "a host name mismatch with the right CA should point at verify-ca: {e}");
                    assert!(!e.contains("was not signed by it"),
                        "the CA was right - blaming it sends the user after the wrong problem: {e}");
                } else {
                    panic!("ssl=verify refused the server's own CA for a reason other than the host name: {e}");
                }
            }
        }
    }

    #[test]
    fn required_ignores_the_ca_and_still_connects() {
        if !caps::tls_or_skip() { return; }
        let wrong_file = unrelated_ca_file();
        let Some(conn) = conn_with("required", &wrong_file.path().to_string_lossy()) else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
        let c = build_conn(&conn).expect("ssl=required must connect whatever CA is configured - it verifies nothing");
        assert!(!cipher(c).is_empty(), "ssl=required connected but not over TLS");
    }

    // A CA file that is not there must fail as a missing FILE. That is also what shows the path is
    // actually being read: a CA that was being ignored would fail with the same untrusted-root error
    // as no CA at all, and never mention a file.
    #[test]
    fn a_missing_ca_file_is_reported_as_a_missing_file() {
        if !caps::tls_or_skip() { return; }
        for mode in ["verify", "verify-ca"] {
            let Some(conn) = conn_with(mode, r"C:\definitely\not\here\ca.pem") else { eprintln!("NOBS_TEST_DSN not set - skipping"); return };
            let e = build_conn(&conn).err().unwrap_or_else(|| panic!("ssl={mode} with a missing CA file connected anyway"));
            assert!(e.contains("IoError") || e.contains("cannot find"),
                "ssl={mode}: a missing CA file should fail as a missing file, which proves the path is read: {e}");
        }
    }
}

// PAM sign-in, against a MariaDB with auth_pam - compat.yml starts two in Docker on Linux, since
// there is no auth_pam on Windows. NOBS_TEST_PAM is host:port:user:password of an account
// IDENTIFIED VIA pam on a server with pam_use_cleartext_plugin=ON and TLS; NOBS_TEST_PAM_DIALOG
// the same on one without pam_use_cleartext_plugin, so PAM asks through the dialog plugin. The
// account has every privilege on the database nobs_pam. Run with --ignored.
#[cfg(test)]
mod pam_tests {
    use super::*;
    fn conn(var: &str, ssl: &str) -> Option<Value> {
        let d = std::env::var(var).ok()?;
        let p: Vec<&str> = d.splitn(4, ':').collect();
        Some(json!({"host":p[0],"port":p[1],"user":p[2],"password":p[3],"ssl":ssl,"clearPw":true}))
    }

    #[tokio::test]
    #[ignore]
    async fn a_pam_account_signs_in_over_tls_and_nowhere_else() {
        let Some(req) = conn("NOBS_TEST_PAM", "required") else { eprintln!("NOBS_TEST_PAM not set - skipping"); return };
        let mut c = build_conn(&req).expect("a PAM account signs in over TLS");
        let who: String = c.query_first("SELECT CURRENT_USER()").unwrap().unwrap();
        assert!(who.starts_with(&format!("{}@", req["user"].as_str().unwrap())), "signed in as {who}");
        let cipher: Option<(String, String)> = c.query_first("SHOW STATUS LIKE 'Ssl_cipher'").unwrap();
        assert!(cipher.map(|x| !x.1.is_empty()).unwrap_or(false), "the password went over a connection without TLS");
        // "default" takes TLS when the server offers it, so it signs in as well.
        build_conn(&conn("NOBS_TEST_PAM", "default").unwrap()).expect("ssl=default signs a PAM account in over TLS");
        // Over TLS that checks no certificate, only when the connection says so (clearPw).
        let mut unasked = req.clone(); unasked["clearPw"] = json!(false);
        let e = build_conn(&unasked).err().unwrap_or_else(|| panic!("ssl=required without clearPw must not send a PAM password"));
        assert!(e.contains("does not check the server's certificate"), "{e}");
        // Without TLS the password is not sent, and the message says why.
        let e = build_conn(&conn("NOBS_TEST_PAM", "disabled").unwrap()).err().unwrap_or_else(|| panic!("ssl=disabled must not send a PAM password"));
        assert!(e.contains("not encrypted"), "{e}");
        // PAM itself decides: a wrong password is refused. Without this, a server that let anyone
        // in would pass every check above.
        let mut bad = req.clone(); bad["password"] = json!("not-the-password");
        assert!(build_conn(&bad).is_err(), "a wrong password was accepted - PAM is not checking");

        // The client tools sign in the same way, through the options file.
        let Ok(mbin) = std::env::var("MYSQL_BIN") else { eprintln!("MYSQL_BIN not set - the client tools part is skipped"); return };
        let f = std::env::temp_dir().join("nobs-pam-import.sql");
        std::fs::write(&f, "DROP TABLE IF EXISTS pam_t;\nCREATE TABLE pam_t (id INT);\nINSERT INTO pam_t VALUES (1);\n").unwrap();
        let r = import_run(json!({"files":[f.to_string_lossy()], "targetDb":"nobs_pam", "conn":req}), mbin).await.unwrap();
        let log = r["log"].to_string();
        assert!(log.contains("OK "), "the import signed in as the PAM account: {log}");
        let n: Option<u32> = c.query_first("SELECT COUNT(*) FROM nobs_pam.pam_t").unwrap();
        assert_eq!(n, Some(1));
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    #[ignore]
    fn a_pam_account_behind_the_dialog_plugin_is_explained() {
        let Some(req) = conn("NOBS_TEST_PAM_DIALOG", "required") else { eprintln!("NOBS_TEST_PAM_DIALOG not set - skipping"); return };
        let e = build_conn(&req).err().unwrap_or_else(|| panic!("the dialog plugin cannot be answered, so this does not sign in"));
        assert!(e.contains("pam_use_cleartext_plugin=ON"), "{e}");
    }
}

#[cfg(test)]
mod dump_target_tests {
    use super::*;

    // Verbatim lines from dumps the app's own export produced (MySQL 8.0.46 and MariaDB 12.2).
    #[test]
    fn database_statements_are_recognised_in_every_form_the_dump_tools_write() {
        let cases: [(&str, &str); 9] = [
            ("/*!40000 DROP DATABASE IF EXISTS `shop`*/;", "shop"),
            ("CREATE DATABASE /*!32312 IF NOT EXISTS*/ `shop` /*!40100 DEFAULT CHARACTER SET utf8mb4 */;", "shop"),
            ("USE `shop`;", "shop"),
            ("use shop;", "shop"),
            ("DROP DATABASE IF EXISTS `we``ird`;", "we`ird"),
            ("CREATE DATABASE IF NOT EXISTS plain_name;", "plain_name"),
            ("  CREATE SCHEMA `s2`;", "s2"),
            ("DROP SCHEMA s3;", "s3"),
            ("USE `sp ace`;", "sp ace"),
        ];
        for (line, want) in cases {
            let got = dump_db_ident(line.as_bytes()).map(|x| x.2);
            assert_eq!(got.as_deref(), Some(want), "not recognised: {line}");
        }
    }

    // A row of data, a table statement or a comment must never be taken for one.
    #[test]
    fn nothing_else_is_touched() {
        for line in [
            "INSERT INTO `t` VALUES (1,'USE `shop`;');",
            "DROP TABLE IF EXISTS `shop`;",
            "CREATE TABLE `shop` (id INT);",
            "-- USE `shop`;",
            "/*!50001 CREATE VIEW `v` AS SELECT 1 */;",
            "USER `shop`;",
            "USED shop;",
            "",
        ] {
            assert!(dump_db_ident(line.as_bytes()).is_none(), "wrongly recognised: {line}");
            assert_eq!(dump_rewrite_line(line.as_bytes(), "shop", "copy"), line.as_bytes());
        }
    }

    #[test]
    fn only_the_identifier_changes() {
        let rw = |l: &str| String::from_utf8(dump_rewrite_line(l.as_bytes(), "shop", "shop_copy")).unwrap();
        assert_eq!(rw("/*!40000 DROP DATABASE IF EXISTS `shop`*/;\n"), "/*!40000 DROP DATABASE IF EXISTS `shop_copy`*/;\n");
        assert_eq!(rw("CREATE DATABASE /*!32312 IF NOT EXISTS*/ `shop` /*!40100 DEFAULT CHARACTER SET utf8mb4 */;\r\n"),
                   "CREATE DATABASE /*!32312 IF NOT EXISTS*/ `shop_copy` /*!40100 DEFAULT CHARACTER SET utf8mb4 */;\r\n");
        assert_eq!(rw("USE shop;\n"), "USE `shop_copy`;\n");
        // A different database is left alone.
        assert_eq!(rw("USE `other`;\n"), "USE `other`;\n");
        // A target with a backtick is quoted properly.
        let q = String::from_utf8(dump_rewrite_line(b"USE `shop`;", "shop", "a`b")).unwrap();
        assert_eq!(q, "USE `a``b`;");
        // Bytes that are not UTF-8 pass through untouched.
        let raw = b"INSERT INTO t VALUES (0x\xff\xfe);\n";
        assert_eq!(dump_rewrite_line(raw, "shop", "x"), raw.to_vec());
    }

    #[test]
    fn the_plan_follows_what_the_file_contains() {
        let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(matches!(dump_plan(&n(&["shop"]), ""), DumpPlan::AsIs), "no target: restore where the file says");
        assert!(matches!(dump_plan(&n(&[]), "copy"), DumpPlan::AsIs), "a per-table dump just uses the target");
        assert!(matches!(dump_plan(&n(&["copy"]), "copy"), DumpPlan::AsIs));
        assert!(matches!(dump_plan(&n(&["shop"]), "copy"), DumpPlan::Rename(ref f) if f == "shop"));
        match dump_plan(&n(&["shop", "crm"]), "copy") {
            DumpPlan::Refuse(why) => assert!(why.contains("shop, crm") && why.contains("Target database"), "{why}"),
            _ => panic!("a file with two databases must not be squashed into one target"),
        }
    }

    #[test]
    fn names_are_collected_from_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("d.sql");
        std::fs::write(&p, "/*!40000 DROP DATABASE IF EXISTS `a`*/;\nCREATE DATABASE `a`;\nUSE `a`;\nINSERT INTO t VALUES (1);\n\
USE `a`;\n-- later, a second database\nUSE `b`;\n").unwrap();
        assert_eq!(dump_db_names(p.to_str().unwrap()).unwrap(), vec!["a".to_string(), "b".to_string()]);
    }
}

#[cfg(test)]
mod transfer_hex_tests {
    use super::*;
    #[test]
    fn the_hex_setting_is_used_only_where_it_exists() {
        for (v, want) in [("8.0.46", true), ("8.0.17", true), ("8.4.2-commercial", true), ("9.1.0", true),
                          ("8.0.16", false), ("5.7.44-log", false), ("12.2.2-MariaDB", false),
                          ("10.11.8-MariaDB-log", false), ("", false), ("garbage", false)] {
            assert_eq!(mysql_version_has_hex_identified(v), want, "{v}");
        }
    }
}
