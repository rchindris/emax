//! The session daemon's wire protocol.
//!
//! A connection is a Unix stream socket. Every message is one frame: a
//! 4-byte big-endian length followed by that many bytes of `postcard`-encoded
//! [`ClientMsg`] (client to daemon) or [`DaemonMsg`] (daemon to client).
//! Frames larger than [`MAX_FRAME`] are a protocol error.
//!
//! The client starts with [`ClientMsg::Hello`]; the daemon answers
//! [`DaemonMsg::Hello`] (or closes the connection on a version mismatch).
//! Then the client sends requests, each with a `req` number; the daemon
//! answers each with a [`DaemonMsg::Reply`] carrying the same number, except
//! that `req == 0` asks for no reply (used for `Write` and `Resize`, the
//! high-rate operations). Output and exits of the sessions this connection
//! is attached to arrive at any time as [`DaemonMsg::Output`] and
//! [`DaemonMsg::Exited`].

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::engine::pty::{ExitStatus, PtySize, SessionId, SessionInfo, SpawnSpec};

/// Bumped on any incompatible change to the messages below.
pub const PROTOCOL_VERSION: u32 = 1;

/// The largest frame either side accepts (16 MiB).
pub const MAX_FRAME: usize = 16 << 20;

/// The largest output chunk the daemon sends in one `Output` message.
pub const MAX_OUTPUT_CHUNK: usize = 64 << 10;

/// One operation of [`crate::engine::pty::PtyBackend`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Op {
    Spawn(SpawnSpec),
    Attach {
        id: SessionId,
        size: PtySize,
    },
    Detach {
        id: SessionId,
    },
    Write {
        id: SessionId,
        data: Vec<u8>,
    },
    Resize {
        id: SessionId,
        size: PtySize,
    },
    Kill {
        id: SessionId,
    },
    List,
    /// Kill every session and exit the daemon.
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello { version: u32, client_pid: u32 },
    Request { req: u64, op: Op },
}

/// A successful reply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    Ok,
    Spawned(SessionId),
    Sessions(Vec<SessionInfo>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DaemonMsg {
    Hello {
        version: u32,
        daemon_pid: u32,
    },
    /// The answer to request `req`; `Err` carries a message.
    Reply {
        req: u64,
        result: Result<Reply, String>,
    },
    Output {
        id: SessionId,
        data: Vec<u8>,
    },
    Exited {
        id: SessionId,
        status: ExitStatus,
    },
}

/// One message as the bytes of a frame (length prefix included), for
/// writers that queue frames and write them elsewhere.
pub fn encode_frame<T: Serialize>(msg: &T) -> io::Result<Vec<u8>> {
    // Encode straight after a placeholder length, so the frame is one allocation.
    let buf = postcard::to_extend(msg, vec![0u8; 4]).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = buf.len() - 4;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "message too large"));
    }
    let mut buf = buf;
    buf[..4].copy_from_slice(&(len as u32).to_be_bytes());
    Ok(buf)
}

/// Write one framed message.
pub fn write_frame<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    w.write_all(&encode_frame(msg)?)?;
    w.flush()
}

/// Read one framed message. `Ok(None)` on a clean end of stream (between frames).
pub fn read_frame<T: DeserializeOwned>(r: &mut impl Read) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {len} bytes is too large")));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    postcard::from_bytes(&body).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let msgs = vec![
            ClientMsg::Hello { version: PROTOCOL_VERSION, client_pid: 42 },
            ClientMsg::Request {
                req: 1,
                op: Op::Spawn(SpawnSpec {
                    program: "/bin/sh".into(),
                    args: vec!["-l".into()],
                    cwd: Some("/tmp".into()),
                    env: vec![("TERM".into(), "xterm-256color".into())],
                    size: PtySize::new(120, 40),
                }),
            },
            ClientMsg::Request { req: 0, op: Op::Write { id: SessionId(3), data: (0..=255).collect() } },
            ClientMsg::Request { req: 2, op: Op::List },
        ];
        let mut wire = Vec::new();
        for m in &msgs {
            write_frame(&mut wire, m).unwrap();
        }
        let mut r = wire.as_slice();
        for m in &msgs {
            assert_eq!(read_frame::<ClientMsg>(&mut r).unwrap().as_ref(), Some(m));
        }
        assert_eq!(read_frame::<ClientMsg>(&mut r).unwrap(), None);

        let replies = vec![
            DaemonMsg::Reply { req: 1, result: Ok(Reply::Spawned(SessionId(9))) },
            DaemonMsg::Reply { req: 2, result: Err("no such session".into()) },
            DaemonMsg::Output { id: SessionId(9), data: b"\x1b[31mred\xff".to_vec() },
            DaemonMsg::Exited { id: SessionId(9), status: ExitStatus { code: 0, signal: Some("Hangup".into()) } },
        ];
        let mut wire = Vec::new();
        for m in &replies {
            write_frame(&mut wire, m).unwrap();
        }
        let mut r = wire.as_slice();
        for m in &replies {
            assert_eq!(read_frame::<DaemonMsg>(&mut r).unwrap().as_ref(), Some(m));
        }
    }

    #[test]
    fn bad_frames_are_errors() {
        // Truncated length.
        assert!(read_frame::<ClientMsg>(&mut &[0u8, 0][..]).is_err());
        // Truncated body.
        assert!(read_frame::<ClientMsg>(&mut &[0u8, 0, 0, 9, 1][..]).is_err());
        // Too large.
        let huge = ((MAX_FRAME + 1) as u32).to_be_bytes();
        assert_eq!(read_frame::<ClientMsg>(&mut &huge[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // Garbage body.
        assert!(read_frame::<ClientMsg>(&mut &[0u8, 0, 0, 2, 0xff, 0xff][..]).is_err());
        // A length at the limit is accepted (then the body is missing).
        let at_limit = (MAX_FRAME as u32).to_be_bytes();
        assert_eq!(read_frame::<ClientMsg>(&mut &at_limit[..]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    /// A reader that hands out at most `step` bytes per read and fails every
    /// other read with `Interrupted`, like a slow socket.
    struct Trickle<'a> {
        data: &'a [u8],
        step: usize,
        interrupt: bool,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.interrupt = !self.interrupt;
            if self.interrupt {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let n = self.step.min(buf.len()).min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    #[test]
    fn partial_and_interrupted_reads_reassemble_frames() {
        let msgs: Vec<DaemonMsg> = (0..20u64)
            .map(|i| DaemonMsg::Output { id: SessionId(i), data: (0..i * 37).map(|b| b as u8).collect() })
            .chain([DaemonMsg::Hello { version: PROTOCOL_VERSION, daemon_pid: 7 }])
            .collect();
        let mut wire = Vec::new();
        for m in &msgs {
            write_frame(&mut wire, m).unwrap();
        }
        for step in [1, 2, 3, 5, 64, 4096] {
            let mut r = Trickle { data: &wire, step, interrupt: false };
            for m in &msgs {
                assert_eq!(read_frame::<DaemonMsg>(&mut r).unwrap().as_ref(), Some(m), "step {step}");
            }
            assert_eq!(read_frame::<DaemonMsg>(&mut r).unwrap(), None);
        }
        // A stream cut anywhere inside a frame is an error, never a message.
        for cut in 1..wire.len().min(200) {
            let mut r = &wire[..cut];
            let mut results = Vec::new();
            loop {
                match read_frame::<DaemonMsg>(&mut r) {
                    Ok(Some(m)) => results.push(m),
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            assert!(results.iter().zip(&msgs).all(|(a, b)| a == b), "cut {cut}");
        }
    }

    #[test]
    fn oversized_messages_are_refused_when_writing() {
        let big = ClientMsg::Request { req: 1, op: Op::Write { id: SessionId(1), data: vec![0; MAX_FRAME + 1] } };
        let mut wire = Vec::new();
        assert_eq!(write_frame(&mut wire, &big).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert!(wire.is_empty(), "nothing is written for a refused message");
        // Just under the limit goes through and reads back.
        let ok = ClientMsg::Request { req: 1, op: Op::Write { id: SessionId(1), data: vec![7; MAX_FRAME - 64] } };
        write_frame(&mut wire, &ok).unwrap();
        assert_eq!(read_frame::<ClientMsg>(&mut wire.as_slice()).unwrap(), Some(ok));
    }

    #[test]
    fn encode_frame_matches_write_frame() {
        let m = DaemonMsg::Reply { req: 3, result: Ok(Reply::Ok) };
        let mut wire = Vec::new();
        write_frame(&mut wire, &m).unwrap();
        let f = encode_frame(&m).unwrap();
        assert_eq!(f, wire);
        assert_eq!(u32::from_be_bytes(f[..4].try_into().unwrap()) as usize, f.len() - 4);
    }
}
