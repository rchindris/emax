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

use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;

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
    Attach { id: SessionId, size: PtySize },
    Detach { id: SessionId },
    Write { id: SessionId, data: Vec<u8> },
    Resize { id: SessionId, size: PtySize },
    Kill { id: SessionId },
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
    Hello { version: u32, daemon_pid: u32 },
    /// The answer to request `req`; `Err` carries a message.
    Reply { req: u64, result: Result<Reply, String> },
    Output { id: SessionId, data: Vec<u8> },
    Exited { id: SessionId, status: ExitStatus },
}

/// Write one framed message.
pub fn write_frame<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let body = postcard::to_stdvec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "message too large"));
    }
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(&body);
    w.write_all(&buf)?;
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
    }
}
