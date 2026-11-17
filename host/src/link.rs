// SPDX-License-Identifier: GPL-3.0-or-later
//! A connected byte stream to the tablet, independent of the transport
//! (TCP through `adb reverse`, or raw USB bulk endpoints with AOA).

use std::{
    io::{BufReader, Read, Write},
    net::{Shutdown, TcpStream},
    sync::Arc,
    time::Duration,
};

/// The tablet answers pings at 2 Hz; this much silence means it's gone.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Link {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    /// Unblocks the reader and fails further I/O (callable from any thread).
    pub close: Arc<dyn Fn() + Send + Sync>,
    /// AOA: the nonce of the SYNC this session started with.
    pub sync: Option<u64>,
}

impl Link {
    pub fn tcp(stream: TcpStream) -> std::io::Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
        let closer = stream.try_clone()?;
        Ok(Self {
            reader: Box::new(BufReader::with_capacity(64 * 1024, stream.try_clone()?)),
            writer: Box::new(stream),
            close: Arc::new(move || {
                let _ = closer.shutdown(Shutdown::Both);
            }),
            sync: None,
        })
    }
}
