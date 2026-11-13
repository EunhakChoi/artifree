//! Behavioral tests of files, descriptors and network (decision 0056 rule 14, row R02ae, amendment
//! of 2026-11-13 "files, descriptors and network"; decision 0042 rule 4 and the matrix rows "Host
//! files and processes" and "Network").
//!
//! Every test is a `job_*` test ignored in `cargo test`. `scripts/enforcement/run.sh` selects the
//! scenario and runs one test by its exact name. The payload's error numbers are reported, the
//! host-side readings are the evidence.

use super::super::bootstrap::PAYLOAD;
use super::super::launcher::TOOL_ROOT;
use super::super::report::sha256_hex;
use super::mapping_tests::{StopOnDrop, line_with, process_ended};
use super::resource_tests::{evidence_of, field_number, stdout_text, timed_launch};
use super::tests::{assert_no_attempt_groups, attempt_groups, refusal_of};
use super::{AttemptEvidence, AttemptFailure, AttemptPlan, AttemptRefusal, NotStartedReason};

use rustix::io::{FdFlags, fcntl_setfd};

use std::env;
use std::error::Error;
use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::fd::AsFd;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{SocketAddr as UnixSocketAddr, UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

/// The TCP port of the loopback listeners on `127.0.0.1` and on `[::1]`, which the payload tries.
const LOOPBACK_TCP_PORT: u16 = 47001;
/// The TCP port of the listener on every `IPv4` address, which the payload reaches at the runner's
/// address.
const HOST_TCP_PORT: u16 = 47002;
/// The TCP port of the listener that the leaked-descriptor test starts on `127.0.0.1`.
const LEAK_TCP_PORT: u16 = 47004;
/// The UDP port of the listener on `127.0.0.1`, which the payload's datagram tries target.
const LOOPBACK_UDP_PORT: u16 = 47053;
/// The pathname of the host's Unix socket, which the payload's `unix-path` try connects to.
const HOST_SOCKET_PATH: &str = "/tmp/artifree-enforcement-host.sock";
/// The abstract name of the host's Unix socket, which the payload's `unix-abstract` try names.
const HOST_ABSTRACT_NAME: &str = "artifree-enforcement-host";
/// The file under the tool root that holds the runner's primary `IPv4` address, on one line.
const HOST_ADDRESS_FILE: &str = "etc/host-address";
/// The network tries of the payload, each of which prints one `network <name>` line.
const NETWORK_TRIES: [&str; 11] = [
    "tcp-loopback4",
    "tcp-loopback6",
    "tcp-host",
    "udp-loopback",
    "udp-resolver",
    "udp-azure-dns",
    "tcp-metadata",
    "tcp-external",
    "tcp6-external",
    "unix-path",
    "unix-abstract",
];
/// The positive controls of the payload's network scenario, which must succeed inside the attempt.
const NETWORK_CONTROLS: [&str; 2] = ["local-tcp", "local-abstract"];
/// The host directory of the canary, which no attempt mounts.
const CANARY_DIR: &str = "/var/tmp/artifree-enforcement-canary";
/// The canary file that the payload must not read.
const CANARY_FILE: &str = "/var/tmp/artifree-enforcement-canary/secret";
/// The runner account's home on GitHub's hosted runners, which holds the second canary.
const HOME_CANARY_DIR: &str = "/home/runner/artifree-enforcement-canary";
/// The second canary file, under the runner account's home.
const HOME_CANARY_FILE: &str = "/home/runner/artifree-enforcement-canary/secret";
/// The directory under the canary that the payload must not create files in.
const DROP_DIR: &str = "/var/tmp/artifree-enforcement-canary/drop";
/// The fixed bytes of both canary files.
const CANARY_BYTES: &[u8] = b"artifree-enforcement-canary\n";
/// The paths that the payload reads, as the payload names them. Each one must be missing.
const HOST_READ_PATHS: [&str; 5] = [
    "/var/tmp/artifree-enforcement-canary/secret",
    "/home/runner/artifree-enforcement-canary/secret",
    "/tools/../var/tmp/artifree-enforcement-canary/secret",
    "/proc/1/root/var/tmp/artifree-enforcement-canary/secret",
    "/proc/self/cwd/../var/tmp/artifree-enforcement-canary/secret",
];
/// The paths that the payload creates, with the error number that each must report: `ENOENT` for
/// the path under a directory that the attempt does not have, and `EROFS` for the read-only root
/// and tools.
const HOST_CREATE_PATHS: [(&str, u64); 3] = [
    ("/var/tmp/artifree-enforcement-canary/drop/created", 2),
    ("/created", 30),
    ("/tools/created", 30),
];
/// The `/proc/sys` files whose values the host must keep unchanged across the launch.
const PROC_VALUES: [&str; 3] = [
    "/proc/sys/kernel/hostname",
    "/proc/sys/kernel/core_pattern",
    "/proc/sys/kernel/sysrq",
];
/// The names of the payload's `/proc/sys` writes, each of which must report an error.
const PROC_WRITE_NAMES: [&str; 5] = [
    "hostname",
    "drop-caches",
    "core-pattern",
    "sysrq",
    "ns-last-pid",
];
/// The log that the preload canary appends to, with one line for each program that loads it.
const PRELOAD_LOG: &str = "/tmp/artifree-enforcement-preload.log";
/// The name of the preload canary, which must appear in `LD_PRELOAD` of this test process.
const PRELOAD_CANARY_NAME: &str = "preload-canary";
/// The environment variable that names the environment record, which `run.sh` sets for each job
/// test of this module. The record is an artifact of the job (decision 0056 rule 13).
const RECORD_VARIABLE: &str = "ARTIFREE_ENFORCEMENT_RECORD";
/// The most processes that may answer a signal-zero probe inside the attempt. Only the attempt's
/// own processes answer, so no host process does.
const MAX_ANSWERED: u64 = 4;
/// How long a host listener may take to count the arrival of its self-check.
const SELF_CHECK_WITHIN: Duration = Duration::from_secs(5);
/// The interval between two polls of a listener, a watcher or a process.
const POLL: Duration = Duration::from_millis(10);
/// The size of one read from an accepted TCP stream.
const READ_CHUNK_BYTES: usize = 4096;
/// The size of the buffer that receives one datagram.
const DATAGRAM_BYTES: usize = 2048;
/// The number of header lines of a `/proc/<pid>/net/dev` text.
const NET_DEV_HEADER_LINES: usize = 2;

/// A listener that the test process runs on the host. A thread counts the connections or datagrams
/// that arrive, and reads what the accepted TCP streams send. Dropping it stops the thread.
struct HostListener {
    /// The name that the payload uses for this listener, which the failure messages name.
    name: &'static str,
    /// The number of accepted connections or received datagrams.
    count: Arc<AtomicUsize>,
    /// The number of bytes read from the accepted TCP streams.
    bytes: Arc<AtomicUsize>,
    /// Set to stop the thread.
    stop: Arc<AtomicBool>,
    /// The thread, which ends with the error of its listener, if any.
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl HostListener {
    /// Starts a thread that runs `serve` with the two counters and the stop flag.
    fn start<F>(name: &'static str, serve: F) -> Self
    where
        F: FnOnce(&AtomicUsize, &AtomicUsize, &AtomicBool) -> io::Result<()> + Send + 'static,
    {
        let count = Arc::new(AtomicUsize::new(0));
        let bytes = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let (count, bytes, stop) = (Arc::clone(&count), Arc::clone(&bytes), Arc::clone(&stop));
            thread::spawn(move || serve(&count, &bytes, &stop))
        };
        Self {
            name,
            count,
            bytes,
            stop,
            thread: Some(handle),
        }
    }

    /// A TCP listener on `address`, which counts each accepted connection.
    fn tcp(name: &'static str, address: SocketAddr) -> Result<Self, Box<dyn Error>> {
        let listener = TcpListener::bind(address)
            .map_err(|error| format!("the host listener {name} cannot bind {address}: {error}"))?;
        Ok(Self::start(name, move |count, bytes, stop| {
            serve_tcp(&listener, count, bytes, stop)
        }))
    }

    /// A UDP socket on `address`, which counts each received datagram.
    fn udp(name: &'static str, address: SocketAddr) -> Result<Self, Box<dyn Error>> {
        let socket = UdpSocket::bind(address)
            .map_err(|error| format!("the host listener {name} cannot bind {address}: {error}"))?;
        Ok(Self::start(name, move |count, _bytes, stop| {
            serve_udp(&socket, count, stop)
        }))
    }

    /// A Unix listener on the pathname `path`, which first replaces a stale socket file.
    fn unix_path(name: &'static str, path: &str) -> Result<Self, Box<dyn Error>> {
        remove_file_if_present(path)?;
        let listener = UnixListener::bind(path)
            .map_err(|error| format!("the host listener {name} cannot bind {path}: {error}"))?;
        Ok(Self::start(name, move |count, _bytes, stop| {
            serve_unix(&listener, count, stop)
        }))
    }

    /// A Unix listener on the abstract name `HOST_ABSTRACT_NAME`.
    fn unix_abstract(name: &'static str) -> Result<Self, Box<dyn Error>> {
        let address = abstract_address(HOST_ABSTRACT_NAME)?;
        let listener = UnixListener::bind_addr(&address)
            .map_err(|error| format!("the host listener {name} cannot bind: {error}"))?;
        Ok(Self::start(name, move |count, _bytes, stop| {
            serve_unix(&listener, count, stop)
        }))
    }

    /// The number of arrivals that the listener has counted.
    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    /// The number of bytes that the listener has read from its accepted TCP streams.
    fn bytes(&self) -> usize {
        self.bytes.load(Ordering::SeqCst)
    }

    /// Requires that the listener's thread still runs, since a stopped listener counts nothing.
    fn ensure_running(&self) -> Result<(), Box<dyn Error>> {
        match &self.thread {
            Some(handle) if !handle.is_finished() => Ok(()),
            _ => Err(format!("the host listener {} stopped running", self.name).into()),
        }
    }

    /// Waits until the listener has counted `wanted` arrivals, and fails when that takes longer
    /// than `SELF_CHECK_WITHIN`.
    fn wait_for(&self, wanted: usize) -> Result<(), Box<dyn Error>> {
        let started = Instant::now();
        while self.count() != wanted {
            self.ensure_running()?;
            if started.elapsed() >= SELF_CHECK_WITHIN {
                return Err(format!(
                    "the host listener {} counted {} arrivals, not {wanted}, within {SELF_CHECK_WITHIN:?}",
                    self.name,
                    self.count()
                )
                .into());
            }
            thread::sleep(POLL);
        }
        Ok(())
    }

    /// Makes one connection or datagram from this process with `poke`, and waits until the
    /// listener has counted it.
    fn self_check(&self, poke: impl FnOnce() -> io::Result<()>) -> Result<(), Box<dyn Error>> {
        poke().map_err(|error| format!("the self-check of {} failed: {error}", self.name))?;
        self.wait_for(1)
    }
}

impl Drop for HostListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            // A listener that stopped during the test is caught by `ensure_running`, so the
            // thread's own result is not reported a second time here.
            drop(handle.join());
        }
    }
}

/// Accepts TCP connections until `stop` is set, counts each one, and reads what the accepted
/// streams send. A stream that ends or fails is dropped.
fn serve_tcp(
    listener: &TcpListener,
    count: &AtomicUsize,
    bytes: &AtomicUsize,
    stop: &AtomicBool,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let mut streams: Vec<TcpStream> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _address)) => {
                stream.set_nonblocking(true)?;
                count.fetch_add(1, Ordering::SeqCst);
                streams.push(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        streams.retain_mut(|stream| read_available(stream, bytes));
        thread::sleep(POLL);
    }
    Ok(())
}

/// Reads what `stream` holds now and adds it to `bytes`. It returns whether the stream stays open.
/// A stream that ended or failed is dropped, and its arrival was counted when it was accepted.
fn read_available(stream: &mut TcpStream, bytes: &AtomicUsize) -> bool {
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return false,
            Ok(read) => {
                bytes.fetch_add(read, Ordering::SeqCst);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
    }
}

/// Receives datagrams until `stop` is set, and counts each one.
fn serve_udp(socket: &UdpSocket, count: &AtomicUsize, stop: &AtomicBool) -> io::Result<()> {
    socket.set_read_timeout(Some(POLL))?;
    let mut datagram = [0_u8; DATAGRAM_BYTES];
    while !stop.load(Ordering::SeqCst) {
        match socket.recv_from(&mut datagram) {
            Ok(_) => {
                count.fetch_add(1, Ordering::SeqCst);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Accepts connections on a Unix listener until `stop` is set, and counts each one.
fn serve_unix(listener: &UnixListener, count: &AtomicUsize, stop: &AtomicBool) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((_stream, _address)) => {
                count.fetch_add(1, Ordering::SeqCst);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        thread::sleep(POLL);
    }
    Ok(())
}

/// The `IPv4` loopback address on `port`.
fn local_v4(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// The `IPv6` loopback address on `port`.
fn local_v6(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv6Addr::LOCALHOST, port))
}

/// The abstract Unix address of `name`.
fn abstract_address(name: &str) -> io::Result<UnixSocketAddr> {
    UnixSocketAddr::from_abstract_name(name)
}

/// One connection from this process to a TCP address.
fn connect_once(address: SocketAddr) -> io::Result<()> {
    TcpStream::connect(address).map(|_stream| ())
}

/// One datagram from this process to a UDP address.
fn datagram_once(address: SocketAddr) -> io::Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    socket.send_to(b"self-check", address).map(|_sent| ())
}

/// One connection from this process to the host's Unix socket at its pathname.
fn unix_path_once() -> io::Result<()> {
    UnixStream::connect(HOST_SOCKET_PATH).map(|_stream| ())
}

/// One connection from this process to the host's Unix socket at its abstract name.
fn unix_abstract_once() -> io::Result<()> {
    UnixStream::connect_addr(&abstract_address(HOST_ABSTRACT_NAME)?).map(|_stream| ())
}

/// Removes the file at `path`, where a file that is not there is not an error.
fn remove_file_if_present(path: &str) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Removes the directory tree at `path`, where a tree that is not there is not an error.
fn remove_tree_if_present(path: &str) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Appends one `key=value` line to the file at `path`, which is created when it is missing.
fn append_record(path: &Path, key: &str, value: impl Display) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{key}={value}")
}

/// Appends one observation to the environment record that `run.sh` names in `RECORD_VARIABLE`. A
/// job test without that variable fails, so that no observation is dropped silently.
fn record(key: &str, value: impl Display) -> io::Result<()> {
    let path = env::var_os(RECORD_VARIABLE).ok_or_else(|| {
        io::Error::other(format!(
            "{RECORD_VARIABLE} must name the environment record, which run.sh sets for this test"
        ))
    })?;
    append_record(Path::new(&path), key, value)
}

/// Records the arrivals that each host listener counted before the launch and after it.
fn record_listener_counts(listeners: &[HostListener], before: &[usize]) -> io::Result<()> {
    for (listener, wanted) in listeners.iter().zip(before) {
        record(
            &format!("listener.{}.arrivals_before_launch", listener.name),
            wanted,
        )?;
        record(
            &format!("listener.{}.arrivals_after_launch", listener.name),
            listener.count(),
        )?;
    }
    Ok(())
}

/// Removes the host's Unix socket file when it is dropped. The test declares it before its
/// listeners, so that the listeners stop first.
struct HostSocketFile;

impl Drop for HostSocketFile {
    fn drop(&mut self) {
        let removed = remove_file_if_present(HOST_SOCKET_PATH);
        // A failed removal fails the test, unless the test is already unwinding.
        assert!(
            removed.is_ok() || thread::panicking(),
            "could not remove the host socket {HOST_SOCKET_PATH}: {removed:?}"
        );
    }
}

/// Starts the host listeners of the network test, in the order of the payload's tries. Each one
/// is checked by one connection or datagram from this process before the next one starts.
fn start_host_listeners(host: Ipv4Addr) -> Result<Vec<HostListener>, Box<dyn Error>> {
    let mut running = Vec::new();
    let listener = HostListener::tcp("tcp-loopback4", local_v4(LOOPBACK_TCP_PORT))?;
    listener.self_check(|| connect_once(local_v4(LOOPBACK_TCP_PORT)))?;
    running.push(listener);
    let listener = HostListener::tcp("tcp-loopback6", local_v6(LOOPBACK_TCP_PORT))?;
    listener.self_check(|| connect_once(local_v6(LOOPBACK_TCP_PORT)))?;
    running.push(listener);
    let listener = HostListener::tcp(
        "tcp-host",
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, HOST_TCP_PORT)),
    )?;
    listener.self_check(|| connect_once(SocketAddr::from((host, HOST_TCP_PORT))))?;
    running.push(listener);
    let listener = HostListener::udp("udp-loopback", local_v4(LOOPBACK_UDP_PORT))?;
    listener.self_check(|| datagram_once(local_v4(LOOPBACK_UDP_PORT)))?;
    running.push(listener);
    let listener = HostListener::unix_path("unix-path", HOST_SOCKET_PATH)?;
    listener.self_check(unix_path_once)?;
    running.push(listener);
    let listener = HostListener::unix_abstract("unix-abstract")?;
    listener.self_check(unix_abstract_once)?;
    running.push(listener);
    Ok(running)
}

/// The runner's primary `IPv4` address, which `prepare.sh` writes under the tool root.
fn host_address() -> Result<Ipv4Addr, Box<dyn Error>> {
    let text = fs::read_to_string(Path::new(TOOL_ROOT).join(HOST_ADDRESS_FILE))?;
    let address: Ipv4Addr = text.trim().parse()?;
    Ok(address)
}

/// What the host reads of the payload's network namespace through `/proc/<pid>/net`.
struct OutsideReading {
    /// The text of `net/dev`, which lists the interfaces.
    net_dev: String,
    /// The text of `net/route`, which lists the `IPv4` routes.
    route: String,
    /// The text of `net/ipv6_route`, which lists the `IPv6` routes.
    ipv6_route: String,
}

/// The interface names of a `/proc/<pid>/net/dev` text, which has two header lines. A line without
/// a colon is taken whole, so that an unexpected line shows up as an interface.
fn net_dev_interfaces(text: &str) -> Vec<&str> {
    text.lines()
        .skip(NET_DEV_HEADER_LINES)
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.split_once(':')
                .map_or(line, |(name, _counters)| name)
                .trim()
        })
        .collect()
}

/// The non-blank lines of a `/proc/<pid>/net/route` text after its one header line.
fn route_entries(text: &str) -> Vec<&str> {
    text.lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

/// The device of each route in a `/proc/<pid>/net/ipv6_route` text, which has no header: the last
/// field of each non-blank line.
fn ipv6_route_devices(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.split_whitespace().last())
        .collect()
}

/// Whether the process `pid` runs the payload, which its `cmdline` shows by the path that it was
/// started with. A process that ended does not run it.
fn runs_payload(pid: u32) -> Result<bool, Box<dyn Error>> {
    match fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(cmdline) => Ok(cmdline.split(|byte| *byte == 0).next() == Some(PAYLOAD.as_bytes())),
        Err(error) if process_ended(&error) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Reads the network namespace of the process `pid` from outside the attempt.
fn read_outside(pid: u32) -> io::Result<OutsideReading> {
    Ok(OutsideReading {
        net_dev: fs::read_to_string(format!("/proc/{pid}/net/dev"))?,
        route: fs::read_to_string(format!("/proc/{pid}/net/route"))?,
        ipv6_route: fs::read_to_string(format!("/proc/{pid}/net/ipv6_route"))?,
    })
}

/// Reads the payload's network namespace when the group `group` lists the payload. It returns
/// `None` when the group does not list it, or when the group has gone.
fn read_payload_network(group: &Path) -> Result<Option<OutsideReading>, Box<dyn Error>> {
    let listing = match fs::read_to_string(group.join("cgroup.procs")) {
        Ok(listing) => listing,
        Err(error) if process_ended(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    for line in listing.lines() {
        let pid: u32 = line.trim().parse()?;
        if runs_payload(pid)? {
            return Ok(Some(read_outside(pid)?));
        }
    }
    Ok(None)
}

/// Polls the attempt groups until one of them lists the payload, and reads the payload's network
/// namespace from outside, once. It gives up when `stop` is set.
fn watch_payload_network(stop: &AtomicBool) -> Result<OutsideReading, Box<dyn Error>> {
    let mut last = String::from("no attempt group was listed");
    while !stop.load(Ordering::SeqCst) {
        match attempt_groups() {
            Ok(groups) => {
                for group in groups {
                    if let Some(reading) = read_payload_network(&group)? {
                        return Ok(reading);
                    }
                }
            }
            Err(error) => last = error.to_string(),
        }
        thread::sleep(POLL);
    }
    Err(
        format!("the launch ended before the payload held its network; last listing: {last}")
            .into(),
    )
}

/// Requires what the host reads of the payload's network namespace: `lo` alone in `net/dev`, no
/// route in `net/route`, and only routes through `lo` in `net/ipv6_route`.
fn require_loopback_only(outside: &OutsideReading) {
    let interfaces = net_dev_interfaces(&outside.net_dev);
    assert!(
        interfaces == ["lo"],
        "the payload's network namespace must list `lo` alone in net/dev, got {interfaces:?}; \
         net/dev was:\n{}",
        outside.net_dev
    );
    let routes = route_entries(&outside.route);
    assert!(
        routes.is_empty(),
        "net/route must have no route line, got {routes:?}"
    );
    let devices = ipv6_route_devices(&outside.ipv6_route);
    assert!(
        devices.iter().all(|device| *device == "lo"),
        "net/ipv6_route must have only routes through lo, got devices {devices:?}; \
         net/ipv6_route was:\n{}",
        outside.ipv6_route
    );
}

/// Requires that every host listener still runs and has counted as many arrivals as it had after
/// its self-check.
fn require_listeners_unchanged(
    listeners: &[HostListener],
    before: &[usize],
) -> Result<(), Box<dyn Error>> {
    for (listener, wanted) in listeners.iter().zip(before) {
        listener.ensure_running()?;
        assert_eq!(
            listener.count(),
            *wanted,
            "the host listener {} counted a connection or a datagram during the launch",
            listener.name
        );
    }
    Ok(())
}

/// Names what a launch produced, for a message about another check.
fn outcome_summary(outcome: &Result<AttemptEvidence, AttemptRefusal>) -> String {
    match outcome {
        Ok(evidence) => format!("exited with {:?}", evidence.exit),
        Err(refusal) => format!("was refused: {:?}", refusal.failure),
    }
}

/// The payload's standard output, which must be UTF-8, once the payload is known to have exited
/// with status 0.
fn payload_output(evidence: &AttemptEvidence) -> Result<&str, Box<dyn Error>> {
    let text = stdout_text(evidence)?;
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the payload must exit with status 0, got {:?}; its output was:\n{text}",
        evidence.exit
    );
    Ok(text)
}

/// Requires that the payload printed `expected` as one whole line.
fn require_exact_line(text: &str, expected: &str) {
    assert!(
        text.lines().any(|line| line == expected),
        "the payload must print the line `{expected}`; its output was:\n{text}"
    );
}

/// Requires that the payload printed a line that starts with `prefix`.
fn require_line_starting(text: &str, prefix: &str) {
    assert!(
        text.lines().any(|line| line.starts_with(prefix)),
        "the payload must print a line that starts with `{prefix}`; its output was:\n{text}"
    );
}

/// Requires that the payload reaches no host listener. The checks run in this order: no counter of
/// a host listener moves during the launch; from outside the attempt, the payload's network
/// namespace lists `lo` alone in `net/dev`, has no route in `net/route`, and routes only through
/// `lo` in `net/ipv6_route`; then the payload prints every try and both controls. The payload's
/// error numbers are reported in the messages and are not asserted.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_the_network_reaches_no_host_listener() -> Result<(), Box<dyn Error>> {
    let _socket_file = HostSocketFile;
    let running = start_host_listeners(host_address()?)?;
    let before: Vec<usize> = running.iter().map(HostListener::count).collect();
    let stop = AtomicBool::new(false);
    let (launched, watched) = thread::scope(|scope| {
        let watcher =
            scope.spawn(|| watch_payload_network(&stop).map_err(|error| error.to_string()));
        let launched = {
            let _stop_watcher = StopOnDrop(&stop);
            timed_launch("network", &AttemptPlan::fixture())
        };
        (launched, watcher.join())
    });
    let (outcome, _elapsed) = launched?;
    // Recorded before the checks, so that the counters survive a failed check.
    record_listener_counts(&running, &before)?;
    require_listeners_unchanged(&running, &before)?;
    let outside = watched
        .map_err(|_| "the network watcher panicked")?
        .map_err(|error| format!("{error}; the launch {}", outcome_summary(&outcome)))?;
    require_loopback_only(&outside);
    let evidence = evidence_of(outcome)?;
    let text = payload_output(&evidence)?;
    for name in NETWORK_TRIES {
        require_line_starting(text, &format!("network {name} "));
    }
    for name in NETWORK_CONTROLS {
        require_exact_line(text, &format!("network {name} ok"));
    }
    require_exact_line(text, "network done");
    assert_no_attempt_groups()
}

/// The variable that names the canary process. `run.sh` starts the process on the host, outside
/// the test's unit, because the provider refuses a group that holds another process
/// (`OccupiedCgroup`), and stops it after the test.
const CANARY_PID_VARIABLE: &str = "ARTIFREE_CANARY_PID";

/// The command line of the canary process that `run.sh` starts, as `/proc` shows it.
const CANARY_COMMAND: &[u8] = b"sleep\x00300\x00";

/// The process number of the canary process, from `CANARY_PID_VARIABLE`.
fn canary_pid() -> Result<u32, Box<dyn Error>> {
    let value = env::var(CANARY_PID_VARIABLE).map_err(|_| {
        format!("{CANARY_PID_VARIABLE} must name the canary process; run.sh sets it")
    })?;
    Ok(value.trim().parse()?)
}

/// Whether the canary process still runs: `/proc` lists its number with the canary's command line.
fn canary_running(pid: u32) -> bool {
    fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|command| command == CANARY_COMMAND)
}

/// The canary directories, which are removed when this guard is dropped, so that a failed
/// assertion leaves none behind. Its errors are ignored, as those of `CanaryProcess` are.
struct CanaryDirs;

impl Drop for CanaryDirs {
    fn drop(&mut self) {
        let _ = remove_tree_if_present(CANARY_DIR);
        let _ = remove_tree_if_present(HOME_CANARY_DIR);
    }
}

/// Writes the canary files on the host and creates the drop directory with mode 0o777. Directories
/// left by an earlier run are removed first.
fn write_canary_files() -> Result<(), Box<dyn Error>> {
    remove_tree_if_present(CANARY_DIR)?;
    remove_tree_if_present(HOME_CANARY_DIR)?;
    fs::create_dir_all(CANARY_DIR)?;
    fs::write(CANARY_FILE, CANARY_BYTES)?;
    fs::create_dir_all(HOME_CANARY_DIR)?;
    fs::write(HOME_CANARY_FILE, CANARY_BYTES)?;
    fs::create_dir(DROP_DIR)?;
    fs::set_permissions(DROP_DIR, fs::Permissions::from_mode(0o777))?;
    Ok(())
}

/// The path, bytes and modification time of one canary file, as the host reads them.
type CanaryFile = (&'static str, Vec<u8>, SystemTime);

/// The path, bytes and modification time of each canary file, as the host reads them.
fn canary_files() -> Result<Vec<CanaryFile>, Box<dyn Error>> {
    let mut files = Vec::new();
    for path in [CANARY_FILE, HOME_CANARY_FILE] {
        let modified = fs::metadata(path)?.modified()?;
        files.push((path, fs::read(path)?, modified));
    }
    Ok(files)
}

/// Describes the state of a canary file as one record value: the SHA-256 of its bytes and its
/// modification time in nanoseconds since the Unix epoch.
fn canary_state(bytes: &[u8], modified: SystemTime) -> Result<String, Box<dyn Error>> {
    let nanos = modified.duration_since(SystemTime::UNIX_EPOCH)?.as_nanos();
    Ok(format!("sha256:{} modified_ns:{nanos}", sha256_hex(bytes)))
}

/// Records the state of each canary file before the launch and after it.
fn record_canary_files(before: &[CanaryFile], after: &[CanaryFile]) -> Result<(), Box<dyn Error>> {
    for ((path, bytes, modified), (_, bytes_after, modified_after)) in before.iter().zip(after) {
        record(
            &format!("canary.{path}.before_launch"),
            canary_state(bytes, *modified)?,
        )?;
        record(
            &format!("canary.{path}.after_launch"),
            canary_state(bytes_after, *modified_after)?,
        )?;
    }
    Ok(())
}

/// Requires that the payload reaches no host file and no host process. Every read and every
/// creation fails. Read from outside, the canary files keep their bytes and modification times,
/// the drop directory stays empty and the canary process keeps running. The payload's signal
/// probes are answered by at most `MAX_ANSWERED` processes, and its signal to the process group
/// ends only its own child.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_host_files_and_processes_stay_intact() -> Result<(), Box<dyn Error>> {
    let _dirs = CanaryDirs;
    write_canary_files()?;
    let canary = canary_pid()?;
    assert!(
        canary_running(canary),
        "the canary process {canary} must run before the launch"
    );
    let files_before = canary_files()?;
    let (outcome, _elapsed) = timed_launch("host", &AttemptPlan::fixture())?;
    // Recorded before the checks, so that the observations survive a failed check.
    let files_after = canary_files()?;
    record_canary_files(&files_before, &files_after)?;
    let entries = fs::read_dir(DROP_DIR)?.count();
    record("canary.drop-directory.entries_after_launch", entries)?;
    let running = canary_running(canary);
    record("canary.sleep-process.running_after_launch", running)?;
    assert_eq!(
        files_after, files_before,
        "the canary files must keep their bytes and modification times across the launch"
    );
    assert_eq!(entries, 0, "the drop directory {DROP_DIR} must stay empty");
    assert!(
        running,
        "the canary process must still run after the launch"
    );
    let evidence = evidence_of(outcome)?;
    let text = payload_output(&evidence)?;
    for path in HOST_READ_PATHS {
        require_exact_line(text, &format!("host read {path} error=2"));
    }
    for (path, errno) in HOST_CREATE_PATHS {
        require_exact_line(text, &format!("host create {path} error={errno}"));
    }
    let answered = field_number(line_with(text, "host signal-probe ")?, "answered")?;
    record("signal-probe.answered", answered)?;
    assert!(
        (1..=MAX_ANSWERED).contains(&answered),
        "the signal probe must be answered by the payload's own processes only, \
         answered={answered}; its output was:\n{text}"
    );
    require_exact_line(text, "host hold ended signal=9");
    assert_no_attempt_groups()
}

/// The host values that the proc-dev test requires to be unchanged across the launch: the text of
/// each file in `PROC_VALUES`, and the sorted names in `/dev/pts`.
fn host_kernel_state() -> Result<(Vec<String>, Vec<String>), Box<dyn Error>> {
    let mut values = Vec::new();
    for path in PROC_VALUES {
        values.push(fs::read_to_string(path)?);
    }
    let mut pts_names = Vec::new();
    for entry in fs::read_dir("/dev/pts")? {
        pts_names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    pts_names.sort_unstable();
    Ok((values, pts_names))
}

/// Records the values of the host's `/proc/sys` files and the names in `/dev/pts` at `phase`, which
/// names the moment of the observation, such as `before_launch`.
fn record_host_kernel_state(phase: &str, state: &(Vec<String>, Vec<String>)) -> io::Result<()> {
    let (values, pts_names) = state;
    for (path, value) in PROC_VALUES.iter().zip(values) {
        record(&format!("canary.{path}.{phase}"), value.trim_end())?;
    }
    record(&format!("canary.dev-pts.{phase}"), pts_names.join(" "))
}

/// Requires that the payload reaches no `/proc/sys` value and no pseudo-terminal of the host. The
/// host values are unchanged across the launch, every `/proc/sys` write reports an error, and the
/// open of `/dev/ptmx` fails with `ENOENT`, because the bootstrap's empty mount covers `/dev/pts`.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_proc_and_dev_pts_stay_inside() -> Result<(), Box<dyn Error>> {
    let before = host_kernel_state()?;
    record_host_kernel_state("before_launch", &before)?;
    let (outcome, _elapsed) = timed_launch("proc-dev", &AttemptPlan::fixture())?;
    let after = host_kernel_state()?;
    // Recorded before the checks, so that the values survive a failed check.
    record_host_kernel_state("after_launch", &after)?;
    assert_eq!(
        after, before,
        "the host values of /proc/sys and /dev/pts must not change across the launch"
    );
    let evidence = evidence_of(outcome)?;
    let text = payload_output(&evidence)?;
    for name in PROC_WRITE_NAMES {
        require_line_starting(text, &format!("proc-dev {name} error="));
    }
    require_exact_line(text, "proc-dev ptmx error=2");
    let pts = line_with(text, "proc-dev pts")?;
    assert!(
        pts.strip_prefix("proc-dev pts")
            .is_some_and(|names| names.trim().is_empty()),
        "the payload must list no name in /dev/pts, got {pts:?}; its output was:\n{text}"
    );
    assert_no_attempt_groups()
}

/// Sets the close-on-exec flag of `fd` to `flags`.
fn set_descriptor_flags(fd: &impl AsFd, flags: FdFlags) -> Result<(), Box<dyn Error>> {
    fcntl_setfd(fd, flags)?;
    Ok(())
}

/// Requires that the bootstrap refuses a descriptor that the launch leaks. The test opens, without
/// close-on-exec, a directory, a connection to a listener of its own and a pipe, and then launches.
/// The bootstrap's check of the descriptors fails before READY, so the readiness read ends without
/// the record and the attempt counts as `NoReadiness`. The payload is then never released, prints
/// nothing, and the listener receives no bytes beyond the test's own connection.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_leaked_descriptor_is_refused() -> Result<(), Box<dyn Error>> {
    let listener = HostListener::tcp("leak", local_v4(LEAK_TCP_PORT))?;
    let directory = File::open("/tmp")?;
    let connection = TcpStream::connect(local_v4(LEAK_TCP_PORT))?;
    let (pipe_reader, pipe_writer) = io::pipe()?;
    set_descriptor_flags(&directory, FdFlags::empty())?;
    set_descriptor_flags(&connection, FdFlags::empty())?;
    set_descriptor_flags(&pipe_reader, FdFlags::empty())?;
    set_descriptor_flags(&pipe_writer, FdFlags::empty())?;
    listener.wait_for(1)?;
    let launched = timed_launch("leak", &AttemptPlan::fixture());
    // Restored before any assertion, so that the descriptors are close-on-exec again whatever the
    // result of the checks below.
    set_descriptor_flags(&directory, FdFlags::CLOEXEC)?;
    set_descriptor_flags(&connection, FdFlags::CLOEXEC)?;
    set_descriptor_flags(&pipe_reader, FdFlags::CLOEXEC)?;
    set_descriptor_flags(&pipe_writer, FdFlags::CLOEXEC)?;
    let (outcome, _elapsed) = launched?;
    // Recorded before the checks, so that the counters survive a failed check.
    record("listener.leak.arrivals_after_launch", listener.count())?;
    record("listener.leak.bytes_after_launch", listener.bytes())?;
    let failure = refusal_of(outcome)?;
    assert_eq!(
        failure,
        AttemptFailure::NotStarted(NotStartedReason::NoReadiness),
        "the bootstrap must refuse the leaked descriptors before READY"
    );
    assert_eq!(
        listener.count(),
        1,
        "the leaked connection must be the only connection that the listener accepts"
    );
    assert_eq!(
        listener.bytes(),
        0,
        "the listener must receive no bytes from the leaked descriptors"
    );
    assert_no_attempt_groups()
}

/// Requires that this process carries the injection that the job's unit sets: `LD_PRELOAD` names
/// the preload canary, `LD_LIBRARY_PATH` and `BASH_ENV` are set, and the canary logged this
/// executable when the test started.
fn require_injection_is_live() -> Result<(), Box<dyn Error>> {
    assert!(
        env::var_os("LD_PRELOAD")
            .is_some_and(|value| value.to_string_lossy().contains(PRELOAD_CANARY_NAME)),
        "LD_PRELOAD must name the preload canary; run.sh sets it on the unit"
    );
    assert!(
        env::var_os("LD_LIBRARY_PATH").is_some(),
        "LD_LIBRARY_PATH must be set; run.sh sets it on the unit"
    );
    assert!(
        env::var_os("BASH_ENV").is_some(),
        "BASH_ENV must be set; run.sh sets it on the unit"
    );
    let executable = env::current_exe()?.display().to_string();
    let log = fs::read_to_string(PRELOAD_LOG)?;
    assert!(
        log.lines().any(|line| line == executable),
        "the preload canary must have logged {executable} when the test started; the log holds {log:?}"
    );
    Ok(())
}

/// Requires that the payload starts with an empty environment, and that no program the provider
/// starts loads the preload canary. The stub, the launcher, the bootstrap and the payload add no
/// entry to the preload log, and the payload reports no variable.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_the_environment_reaches_the_payload_empty() -> Result<(), Box<dyn Error>> {
    require_injection_is_live()?;
    let log_before = fs::read_to_string(PRELOAD_LOG)?;
    let (outcome, _elapsed) = timed_launch("environment", &AttemptPlan::fixture())?;
    let log_after = fs::read_to_string(PRELOAD_LOG)?;
    // Recorded before the checks, so that the entry counts survive a failed check.
    record(
        "canary.preload-log.entries_before_launch",
        log_before.lines().count(),
    )?;
    record(
        "canary.preload-log.entries_after_launch",
        log_after.lines().count(),
    )?;
    assert_eq!(
        log_after, log_before,
        "the preload log gained an entry during the launch, so a program the provider starts \
         loaded the canary; the log before was {log_before:?}"
    );
    let evidence = evidence_of(outcome)?;
    let text = payload_output(&evidence)?;
    require_exact_line(text, "environment count=0");
    let listed = text
        .lines()
        .filter(|line| line.starts_with("environment var="))
        .count();
    assert_eq!(
        listed, 0,
        "the payload must report no variable; its output was:\n{text}"
    );
    assert_no_attempt_groups()
}

/// Requires that no file of an attempt outlives it. Each of two launches finds no marker in the
/// scratch, writes one there, and fails to create a file under `/dev/shm` with `EROFS`.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_no_file_outlives_an_attempt() -> Result<(), Box<dyn Error>> {
    for label in ["marker-first", "marker-second"] {
        let (outcome, _elapsed) = timed_launch(label, &AttemptPlan::fixture())?;
        let evidence = evidence_of(outcome)?;
        let text = payload_output(&evidence)?;
        require_exact_line(text, "marker found=no");
        require_exact_line(text, "marker wrote ok");
        require_exact_line(text, "marker dev-shm error=30");
    }
    assert_no_attempt_groups()
}

/// The `net/dev` parser names the interfaces of a kernel text, skipping its two header lines, and
/// returns `lo` alone for a namespace that has no other interface.
#[test]
fn net_dev_lists_only_its_interfaces() {
    let header = "Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
";
    let lo = "    lo:    1234      12    0    0    0     0          0         0     1234      12    0    0    0     0       0          0
";
    let eth0 = "  eth0:  987654    4321    0    0    0     0          0         0   123456    3210    0    0    0     0       0          0
";
    let cases: [(String, &[&str]); 2] = [
        (format!("{header}{lo}"), &["lo"]),
        (format!("{header}{lo}{eth0}"), &["lo", "eth0"]),
    ];
    for (text, expected) in cases {
        assert_eq!(net_dev_interfaces(&text), expected, "net/dev was:\n{text}");
    }
}

/// The `net/route` parser returns no entry for a header alone, and one entry for each route line
/// after the header.
#[test]
fn route_header_alone_has_no_entry() {
    let header =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n";
    let eth0 = "eth0\t00000000\t0100000A\t0003\t0\t0\t0\t00000000\t0\t0\t0";
    let cases: [(String, &[&str]); 2] = [
        (header.to_owned(), &[]),
        (format!("{header}{eth0}\n"), &[eth0]),
    ];
    for (text, expected) in cases {
        assert_eq!(route_entries(&text), expected, "net/route was:\n{text}");
    }
}

/// The `net/ipv6_route` parser names the device of each route, which is the last field of its
/// line.
#[test]
fn ipv6_routes_name_their_devices() {
    let lo_routes = "00000000000000000000000000000001 80 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 80200001       lo
ff000000000000000000000000000000 08 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001       lo
";
    let eth0_route = "fe800000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001      eth0
";
    let cases: [(&str, &[&str]); 2] = [(lo_routes, &["lo", "lo"]), (eth0_route, &["eth0"])];
    for (text, expected) in cases {
        assert_eq!(
            ipv6_route_devices(text),
            expected,
            "net/ipv6_route was:\n{text}"
        );
    }
}

/// The record writer appends one `key=value` line per observation, so that a later observation
/// keeps the earlier ones. The environment record is the job's artifact of its observations.
#[test]
fn record_appends_one_line_per_observation() -> Result<(), Box<dyn Error>> {
    let path = env::temp_dir().join(format!("artifree-record-test-{}.txt", std::process::id()));
    fs::write(&path, b"")?;
    append_record(&path, "listener.tcp-loopback4.arrivals_after_launch", 1)?;
    append_record(&path, "canary.sleep-process.running_after_launch", true)?;
    let text = fs::read_to_string(&path)?;
    fs::remove_file(&path)?;
    let expected = "listener.tcp-loopback4.arrivals_after_launch=1\n\
                    canary.sleep-process.running_after_launch=true\n";
    assert_eq!(
        text, expected,
        "each observation must be appended as one key=value line"
    );
    Ok(())
}
