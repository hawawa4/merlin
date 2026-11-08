//! Realtime priority for one thread through RealtimeKit (`rtkit`), the system service that
//! PipeWire and PulseAudio use where users have no realtime limit of their own.
//!
//! On such a system `RLIMIT_RTPRIO` is 0: a process cannot give any of its threads a realtime
//! policy, only ask `rtkit-daemon` to do it. PipeWire does that for the app's audio thread, and the
//! plugin host processes have to do the same for theirs, or the audio thread that waits for a
//! plugin runs realtime while the plugin itself is an ordinary thread, preempted by anything busy.
//!
//! This is the little part of D-Bus that takes: connect to the system bus, authenticate with the
//! user id, say hello, and make a few method calls. No library, no async runtime.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SERVICE: &str = "org.freedesktop.RealtimeKit1";
const OBJECT: &str = "/org/freedesktop/RealtimeKit1";
const INTERFACE: &str = "org.freedesktop.RealtimeKit1";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// The most a request waits for the bus and the service, all told.
const BUDGET: Duration = Duration::from_millis(1500);

/// Why rtkit did not do what was asked.
#[derive(Debug)]
pub enum Refused {
    /// No bus, no service, or it did not answer: nothing to ask.
    Unavailable(String),
    /// The service answered no (the D-Bus error name and message).
    Denied(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(why) => write!(f, "rtkit is not available ({why})"),
            Self::Denied(why) => write!(f, "rtkit said no ({why})"),
        }
    }
}

impl From<io::Error> for Refused {
    fn from(e: io::Error) -> Self {
        Self::Unavailable(e.to_string())
    }
}

// ---------------------------------------------------------------------------------------------
// The wire format

/// Builds a message body or header field list: values aligned to their own size, from the start
/// of the buffer (a D-Bus message keeps the same alignment whether or not it is split).
#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn align(&mut self, to: usize) {
        while !self.0.len().is_multiple_of(to) {
            self.0.push(0);
        }
    }

    fn byte(&mut self, v: u8) {
        self.0.push(v);
    }

    fn u32(&mut self, v: u32) {
        self.align(4);
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.align(8);
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    /// A string or object path: length, bytes, nul.
    fn string(&mut self, v: &str) {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v.as_bytes());
        self.byte(0);
    }

    /// A signature: one length byte, characters, nul.
    fn signature(&mut self, v: &str) {
        self.byte(v.len() as u8);
        self.0.extend_from_slice(v.as_bytes());
        self.byte(0);
    }
}

const METHOD_CALL: u8 = 1;
const METHOD_RETURN: u8 = 2;
const ERROR: u8 = 3;

/// The part of a message this client looks at.
#[derive(Debug, Default)]
struct Message {
    kind: u8,
    reply_serial: Option<u32>,
    error_name: Option<String>,
    signature: String,
    body: Vec<u8>,
}

impl Message {
    /// The first string in the body (an error message, the bus' unique name).
    fn first_string(&self) -> Option<String> {
        if !self.signature.starts_with('s') {
            return None;
        }
        let len = u32::from_le_bytes(self.body.get(..4)?.try_into().ok()?) as usize;
        Some(String::from_utf8_lossy(self.body.get(4..4 + len)?).into_owned())
    }
}

/// A method call, ready to send.
fn encode_call(
    serial: u32,
    destination: &str,
    path: &str,
    interface: &str,
    member: &str,
    signature: &str,
    body: &[u8],
) -> Vec<u8> {
    // The header fields are an array of (code, variant) structs, each starting on 8 bytes.
    let mut fields = Out::default();
    let mut field = |code: u8, kind: &str, value: &dyn Fn(&mut Out)| {
        fields.align(8);
        fields.byte(code);
        fields.signature(kind);
        value(&mut fields);
    };
    field(1, "o", &|o| o.string(path));
    field(2, "s", &|o| o.string(interface));
    field(3, "s", &|o| o.string(member));
    field(6, "s", &|o| o.string(destination));
    if !signature.is_empty() {
        field(8, "g", &|o| o.signature(signature));
    }

    let mut msg = Out::default();
    msg.byte(b'l');
    msg.byte(METHOD_CALL);
    msg.byte(0); // flags
    msg.byte(1); // protocol version
    msg.u32(body.len() as u32);
    msg.u32(serial);
    msg.u32(fields.0.len() as u32);
    msg.0.extend_from_slice(&fields.0);
    msg.align(8);
    msg.0.extend_from_slice(body);
    msg.0
}

/// Reads one message. `deadline` bounds the whole wait.
fn read_message(stream: &mut UnixStream, deadline: Instant) -> Result<Message, Refused> {
    let mut fixed = [0u8; 16];
    read_exact(stream, &mut fixed, deadline)?;
    if fixed[0] != b'l' {
        return Err(Refused::Unavailable(
            "the bus sent a big-endian message".into(),
        ));
    }
    let word = |at: usize| u32::from_le_bytes(fixed[at..at + 4].try_into().unwrap()) as usize;
    let (body_len, fields_len) = (word(4), word(12));
    if body_len > 1 << 20 || fields_len > 1 << 20 {
        return Err(Refused::Unavailable(
            "the bus sent an absurd message".into(),
        ));
    }
    // The fields start at byte 16 and the body at the next multiple of 8 after them.
    let padded = (fields_len + 7) & !7;
    let mut rest = vec![0u8; padded + body_len];
    read_exact(stream, &mut rest, deadline)?;

    let mut message = Message {
        kind: fixed[1],
        body: rest[padded..].to_vec(),
        ..Message::default()
    };
    // Walk the fields; `at` counts from the start of the message so alignment holds.
    let fields = &rest[..fields_len];
    let mut at = 0usize;
    while at < fields.len() {
        at = (at + 7) & !7;
        if at + 4 > fields.len() {
            break;
        }
        let code = fields[at];
        let sig_len = fields[at + 1] as usize;
        let kind = fields.get(at + 2..at + 2 + sig_len).unwrap_or_default();
        at += 3 + sig_len;
        let bad = || Refused::Unavailable("a malformed message from the bus".into());
        match kind {
            b"u" => {
                at = (at + 3) & !3;
                let v =
                    u32::from_le_bytes(fields.get(at..at + 4).ok_or_else(bad)?.try_into().unwrap());
                at += 4;
                if code == 5 {
                    message.reply_serial = Some(v);
                }
            }
            b"s" | b"o" => {
                at = (at + 3) & !3;
                let len =
                    u32::from_le_bytes(fields.get(at..at + 4).ok_or_else(bad)?.try_into().unwrap())
                        as usize;
                let text = fields.get(at + 4..at + 4 + len).ok_or_else(bad)?;
                at += 4 + len + 1;
                if code == 4 {
                    message.error_name = Some(String::from_utf8_lossy(text).into_owned());
                }
            }
            b"g" => {
                let len = *fields.get(at).ok_or_else(bad)? as usize;
                let text = fields.get(at + 1..at + 1 + len).ok_or_else(bad)?;
                at += 1 + len + 1;
                if code == 8 {
                    message.signature = String::from_utf8_lossy(text).into_owned();
                }
            }
            _ => return Err(bad()),
        }
    }
    Ok(message)
}

fn read_exact(stream: &mut UnixStream, buf: &mut [u8], deadline: Instant) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_read_timeout(Some(left))?;
        match stream.read(&mut buf[done..]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                return Err(io::ErrorKind::TimedOut.into());
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The connection

/// The socket of the system bus: `DBUS_SYSTEM_BUS_ADDRESS` (`unix:path=...`) or the usual place.
fn system_bus_path() -> PathBuf {
    std::env::var("DBUS_SYSTEM_BUS_ADDRESS")
        .ok()
        .and_then(|addr| {
            addr.split(',')
                .find_map(|part| part.strip_prefix("unix:path=").map(PathBuf::from))
        })
        .unwrap_or_else(|| "/run/dbus/system_bus_socket".into())
}

/// A connection to a bus, past authentication and `Hello`.
pub struct Bus {
    stream: UnixStream,
    serial: u32,
    deadline: Instant,
}

impl Bus {
    /// Connects to the system bus.
    pub fn system() -> Result<Self, Refused> {
        Self::connect(&system_bus_path())
    }

    pub fn connect(path: &std::path::Path) -> Result<Self, Refused> {
        let deadline = Instant::now() + BUDGET;
        let mut stream = UnixStream::connect(path)?;
        stream.set_write_timeout(Some(BUDGET))?;
        // SAFETY: plain syscall.
        let uid = unsafe { libc::geteuid() };
        let hex: String = uid
            .to_string()
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect();
        stream.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())?;
        // The answer is one line: "OK <server guid>".
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while !line.ends_with(b"\r\n") {
            read_exact(&mut stream, &mut byte, deadline)?;
            line.push(byte[0]);
            if line.len() > 256 {
                return Err(Refused::Unavailable(
                    "a long answer to authentication".into(),
                ));
            }
        }
        if !line.starts_with(b"OK") {
            return Err(Refused::Unavailable(format!(
                "authentication refused: {}",
                String::from_utf8_lossy(&line).trim()
            )));
        }
        stream.write_all(b"BEGIN\r\n")?;
        let mut bus = Self {
            stream,
            serial: 0,
            deadline,
        };
        bus.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            "",
            &[],
        )?;
        Ok(bus)
    }

    /// Calls a method and waits for its answer. An error answer is [`Refused::Denied`].
    fn call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        signature: &str,
        body: &[u8],
    ) -> Result<Message, Refused> {
        self.serial += 1;
        let serial = self.serial;
        let msg = encode_call(
            serial,
            destination,
            path,
            interface,
            member,
            signature,
            body,
        );
        self.stream.write_all(&msg)?;
        loop {
            let reply = read_message(&mut self.stream, self.deadline)?;
            // Signals (NameAcquired) and anything else not about this call go by.
            if reply.reply_serial != Some(serial) || !matches!(reply.kind, METHOD_RETURN | ERROR) {
                continue;
            }
            if reply.kind == ERROR {
                let name = reply.error_name.clone().unwrap_or_default();
                let text = reply.first_string().unwrap_or_default();
                return Err(match name.as_str() {
                    // The service is not installed or not running: nothing to ask.
                    "org.freedesktop.DBus.Error.ServiceUnknown"
                    | "org.freedesktop.DBus.Error.NameHasNoOwner" => Refused::Unavailable(name),
                    _ => Refused::Denied(format!("{name}: {text}")),
                });
            }
            return Ok(reply);
        }
    }
}

/// RealtimeKit's side: what it allows, and the two requests.
pub struct Rtkit {
    bus: Bus,
}

impl Rtkit {
    pub fn connect() -> Result<Self, Refused> {
        Ok(Self {
            bus: Bus::system()?,
        })
    }

    /// An integer property of the service (`MaxRealtimePriority`, `MinNiceLevel`: int32;
    /// `RTTimeUSecMax`: int64).
    fn property(&mut self, name: &str) -> Result<i64, Refused> {
        let mut body = Out::default();
        body.string(INTERFACE);
        body.string(name);
        let reply = self
            .bus
            .call(SERVICE, OBJECT, PROPERTIES, "Get", "ss", &body.0)?;
        // A variant: its signature, then the value.
        let bad = || Refused::Unavailable(format!("an odd answer for {name}"));
        let sig = reply.body.get(1..2).ok_or_else(bad)?;
        match sig {
            b"i" => Ok(i64::from(i32::from_le_bytes(
                reply.body.get(4..8).ok_or_else(bad)?.try_into().unwrap(),
            ))),
            b"x" => Ok(i64::from_le_bytes(
                reply.body.get(8..16).ok_or_else(bad)?.try_into().unwrap(),
            )),
            _ => Err(bad()),
        }
    }

    /// The highest realtime priority it grants.
    pub fn max_realtime_priority(&mut self) -> Result<i32, Refused> {
        Ok(self.property("MaxRealtimePriority")? as i32)
    }

    /// The longest a realtime thread may run without blocking, microseconds: rtkit insists the
    /// process has set `RLIMIT_RTTIME` to no more than this before it grants anything.
    pub fn rttime_usec_max(&mut self) -> Result<u64, Refused> {
        Ok(self.property("RTTimeUSecMax")?.max(0) as u64)
    }

    /// The lowest nice level it grants.
    pub fn min_nice_level(&mut self) -> Result<i32, Refused> {
        Ok(self.property("MinNiceLevel")? as i32)
    }

    /// Makes thread `tid` realtime (`SCHED_RR`) at `priority`.
    pub fn make_realtime(&mut self, tid: u64, priority: u32) -> Result<(), Refused> {
        let mut body = Out::default();
        body.u64(tid);
        body.u32(priority);
        self.bus
            .call(
                SERVICE,
                OBJECT,
                INTERFACE,
                "MakeThreadRealtime",
                "tu",
                &body.0,
            )
            .map(drop)
    }

    /// Gives thread `tid` a nice level below zero.
    pub fn make_high_priority(&mut self, tid: u64, nice: i32) -> Result<(), Refused> {
        let mut body = Out::default();
        body.u64(tid);
        body.u32(nice as u32);
        self.bus
            .call(
                SERVICE,
                OBJECT,
                INTERFACE,
                "MakeThreadHighPriority",
                "ti",
                &body.0,
            )
            .map(drop)
    }
}

/// How [`prepare_thread`] ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Granted {
    Realtime(u32),
    Raised(i32),
}

/// Asks rtkit for realtime priority `want` (or the most it grants) for thread `tid`, else for
/// a raised nice level. Sets `RLIMIT_RTTIME` for the process first, which rtkit insists on: a
/// realtime thread that runs this long without blocking is stopped by the kernel, so a plugin
/// stuck in a loop cannot freeze the machine.
pub fn prepare_thread(tid: u64, want: u32) -> Result<Granted, Refused> {
    let mut rtkit = Rtkit::connect()?;
    if let Ok(limit) = rtkit.rttime_usec_max() {
        // SAFETY: plain syscalls on our own limits; this only ever lowers them.
        unsafe {
            let mut now: libc::rlimit = std::mem::zeroed();
            if libc::getrlimit(libc::RLIMIT_RTTIME, &mut now) == 0 {
                let capped = |v: libc::rlim_t| {
                    if v == libc::RLIM_INFINITY || v > limit {
                        limit
                    } else {
                        v
                    }
                };
                let lowered = libc::rlimit {
                    rlim_cur: capped(now.rlim_cur),
                    rlim_max: capped(now.rlim_max),
                };
                if (lowered.rlim_cur, lowered.rlim_max) != (now.rlim_cur, now.rlim_max) {
                    libc::setrlimit(libc::RLIMIT_RTTIME, &lowered);
                }
            }
        }
    }
    let priority = match rtkit.max_realtime_priority() {
        Ok(max) if max > 0 => want.min(max as u32),
        _ => want,
    };
    match rtkit.make_realtime(tid, priority) {
        Ok(()) => return Ok(Granted::Realtime(priority)),
        // Not realtime, then at least not at the back of the queue.
        Err(Refused::Denied(_)) => {}
        Err(e) => return Err(e),
    }
    let nice = rtkit.min_nice_level().unwrap_or(-15).clamp(-20, -1);
    rtkit.make_high_priority(tid, nice)?;
    Ok(Granted::Raised(nice))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// The encoded call of the spec's own example shapes: header fields start on 8, the body on 8.
    #[test]
    fn a_call_is_laid_out_as_the_spec_says() {
        let mut body = Out::default();
        body.u64(0x0102030405060708);
        body.u32(20);
        let msg = encode_call(
            7,
            "org.freedesktop.RealtimeKit1",
            "/o",
            "i.f",
            "M",
            "tu",
            &body.0,
        );
        assert_eq!(&msg[..4], [b'l', METHOD_CALL, 0, 1]);
        let word = |at: usize| u32::from_le_bytes(msg[at..at + 4].try_into().unwrap());
        assert_eq!(word(4), 12, "body length");
        assert_eq!(word(8), 7, "serial");
        let fields_len = word(12) as usize;
        // The body begins on a multiple of 8 after the fields, and is what we gave.
        let body_at = (16 + fields_len + 7) & !7;
        assert_eq!(body_at % 8, 0);
        assert_eq!(msg.len(), body_at + 12);
        assert_eq!(
            &msg[body_at..body_at + 8],
            &0x0102030405060708u64.to_le_bytes()
        );
        assert_eq!(&msg[body_at + 8..], &20u32.to_le_bytes());
        // The first field is the path: code 1, signature "o", then "/o".
        assert_eq!(&msg[16..20], [1, 1, b'o', 0]);
        assert_eq!(word(20), 2);
        assert_eq!(&msg[24..27], b"/o\0");
    }

    /// Encodes a reply the way a bus would (for the fake server below).
    fn encode_reply(
        kind: u8,
        serial: u32,
        reply_to: u32,
        error: Option<&str>,
        text: Option<&str>,
    ) -> Vec<u8> {
        let mut fields = Out::default();
        fields.byte(5);
        fields.signature("u");
        fields.u32(reply_to);
        if let Some(name) = error {
            fields.align(8);
            fields.byte(4);
            fields.signature("s");
            fields.string(name);
        }
        let mut body = Out::default();
        if let Some(t) = text {
            fields.align(8);
            fields.byte(8);
            fields.signature("g");
            fields.signature("s");
            body.string(t);
        }
        let mut msg = Out::default();
        for b in [b'l', kind, 0, 1] {
            msg.byte(b);
        }
        msg.u32(body.0.len() as u32);
        msg.u32(serial);
        msg.u32(fields.0.len() as u32);
        msg.0.extend_from_slice(&fields.0);
        msg.align(8);
        msg.0.extend_from_slice(&body.0);
        msg.0
    }

    /// A bus that accepts anyone, welcomes them, and answers each call by `answer(member)`.
    fn fake_bus(
        answer: impl Fn(&str) -> (u8, Option<&'static str>, Option<&'static str>) + Send + 'static,
    ) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "merlin-rtkit-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bus");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            // "\0AUTH EXTERNAL <hex>\r\n", answered with OK; then BEGIN.
            let mut seen = Vec::new();
            let mut b = [0u8; 1];
            while !seen.ends_with(b"\r\n") {
                s.read_exact(&mut b).unwrap();
                seen.push(b[0]);
            }
            assert!(String::from_utf8_lossy(&seen).contains("AUTH EXTERNAL"));
            s.write_all(b"OK 0123456789abcdef\r\n").unwrap();
            let mut begin = [0u8; 7];
            s.read_exact(&mut begin).unwrap();
            assert_eq!(&begin, b"BEGIN\r\n");
            let far = Instant::now() + Duration::from_secs(5);
            loop {
                let Ok(call) = read_call(&mut s, far) else {
                    return;
                };
                let (kind, error, text) = answer(&call);
                let msg = encode_reply(kind, 100, call_serial(&call), error, text);
                let _ = s.write_all(&msg);
            }
        });
        path
    }

    // The fake server remembers calls as "member#serial".
    fn call_serial(call: &str) -> u32 {
        call.rsplit('#').next().unwrap().parse().unwrap()
    }

    fn read_call(s: &mut UnixStream, deadline: Instant) -> Result<String, Refused> {
        let mut fixed = [0u8; 16];
        read_exact(s, &mut fixed, deadline)?;
        let word = |at: usize| u32::from_le_bytes(fixed[at..at + 4].try_into().unwrap()) as usize;
        let (body_len, fields_len, serial) = (word(4), word(12), word(8));
        let padded = (fields_len + 7) & !7;
        let mut rest = vec![0u8; padded + body_len];
        read_exact(s, &mut rest, deadline)?;
        // The member is the field with code 3.
        let fields = &rest[..fields_len];
        let mut at = 0;
        while at < fields.len() {
            at = (at + 7) & !7;
            let (code, sig_len) = (fields[at], fields[at + 1] as usize);
            let kind = fields[at + 2];
            at += 3 + sig_len;
            if kind == b'g' {
                at += 1 + fields[at] as usize + 1;
            } else {
                at = (at + 3) & !3;
                let len = u32::from_le_bytes(fields[at..at + 4].try_into().unwrap()) as usize;
                if code == 3 {
                    let member =
                        String::from_utf8_lossy(&fields[at + 4..at + 4 + len]).into_owned();
                    return Ok(format!("{member}#{serial}"));
                }
                at += 4 + len + 1;
            }
        }
        Err(Refused::Unavailable("no member".into()))
    }

    #[test]
    fn a_denied_request_says_why_and_a_missing_service_is_unavailable() {
        let path = fake_bus(|call| match call.split('#').next().unwrap() {
            "Hello" => (METHOD_RETURN, None, Some(":1.9")),
            "MakeThreadRealtime" => (
                ERROR,
                Some("org.freedesktop.DBus.Error.AccessDenied"),
                Some("no canary"),
            ),
            _ => (
                ERROR,
                Some("org.freedesktop.DBus.Error.ServiceUnknown"),
                None,
            ),
        });
        let mut rtkit = Rtkit {
            bus: Bus::connect(&path).unwrap(),
        };
        match rtkit.make_realtime(1, 10) {
            Err(Refused::Denied(why)) => assert!(
                why.contains("AccessDenied") && why.contains("no canary"),
                "{why}"
            ),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            rtkit.make_high_priority(1, -15),
            Err(Refused::Unavailable(_))
        ));
    }

    #[test]
    fn a_granted_request_is_ok() {
        let path = fake_bus(|call| match call.split('#').next().unwrap() {
            "Hello" => (METHOD_RETURN, None, Some(":1.9")),
            _ => (METHOD_RETURN, None, None),
        });
        let mut rtkit = Rtkit {
            bus: Bus::connect(&path).unwrap(),
        };
        rtkit.make_realtime(1, 10).unwrap();
    }

    /// Against the machine's real bus, where there is one: the bus accepts our framing and
    /// authentication, and either has the service (a grant or a refusal) or says it has not.
    #[test]
    fn the_real_system_bus_understands_us() {
        let path = system_bus_path();
        if !path.exists() {
            eprintln!("skipped: no system bus at {}", path.display());
            return;
        }
        let mut bus = Bus::connect(&path).expect("hello on the real bus");
        let reply = bus.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetNameOwner",
            "s",
            &{
                let mut o = Out::default();
                o.string("org.freedesktop.DBus");
                o.0
            },
        );
        // The bus' own name has an owner: its answer is the name itself.
        assert_eq!(
            reply.unwrap().first_string().as_deref(),
            Some("org.freedesktop.DBus")
        );
        // A service nobody provides: an error we read, not a hang or a broken connection.
        let missing = bus.call(
            "org.merlin.NoSuchService",
            "/",
            "org.merlin.NoSuchService",
            "Nothing",
            "",
            &[],
        );
        assert!(
            matches!(missing, Err(Refused::Unavailable(ref n)) if n.contains("ServiceUnknown")),
            "{missing:?}"
        );
    }
}
