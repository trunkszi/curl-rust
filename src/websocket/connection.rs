use crate::event_loop::Error;
use crate::websocket::{CloseCode, Message, Options};
use std::collections::VecDeque;
use std::time::Instant;

#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameMeta {
    pub flags: i32,
    pub bytes_left: i64,
}

pub(crate) trait WebSocketIo {
    /// None means that the nonblocking transport needs another readiness event.
    fn recv(&mut self, buffer: &mut [u8]) -> Result<Option<(usize, FrameMeta)>, Error>;
    fn send(&mut self, data: &[u8], flags: u32, frame_size: i64) -> Result<Option<usize>, Error>;
}

pub(crate) enum TimerOutcome {
    Continue,
    Close(CloseCode, &'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InboundFrame<'a> {
    Message(Message<'a>),
    PeerClose { code: CloseCode, reason: &'a str },
}

pub(crate) struct RecvBuffers {
    pub(crate) scratch: Box<[u8; 65536]>,
    pub(crate) fragmented_buf: Vec<u8>,
    pub(crate) fragmented_state: Option<bool>,
    pub(crate) control_buf: Vec<u8>,
    pub(crate) control_flags: Option<i32>,
}

impl Default for RecvBuffers {
    fn default() -> Self {
        Self {
            scratch: vec![0u8; 65536]
                .into_boxed_slice()
                .try_into()
                .expect("64 KiB scratch buffer"),
            fragmented_buf: Vec::new(),
            fragmented_state: None,
            control_buf: Vec::new(),
            control_flags: None,
        }
    }
}

pub(crate) struct Connection {
    options: Options,
    last_received: Instant,
    buffers: Option<RecvBuffers>,
    outgoing: VecDeque<PendingFrame>,
    queued_bytes: usize,
    close_deadline: Option<Instant>,
    next_ping_at: Option<Instant>,
}

struct PendingFrame {
    payload: Vec<u8>,
    flags: u32,
    sent: usize,
    continuation: bool,
}

impl Connection {
    pub(crate) fn new(options: Options, now: Instant) -> Self {
        let next_ping_at = options.ping_interval.map(|d| now + d);
        Self {
            options,
            last_received: now,
            buffers: Some(RecvBuffers::default()),
            outgoing: VecDeque::new(),
            queued_bytes: 0,
            close_deadline: None,
            next_ping_at,
        }
    }

    pub(crate) fn on_open(&mut self, now: Instant) {
        self.last_received = now;
        self.next_ping_at = self.options.ping_interval.map(|d| now + d);
    }

    pub(crate) fn take_buffers(&mut self) -> RecvBuffers {
        self.buffers.take().unwrap_or_default()
    }

    pub(crate) fn put_buffers(&mut self, buffers: RecvBuffers) {
        self.buffers = Some(buffers);
    }

    fn encode_frame<'a>(
        message: Message<'a>,
        close_scratch: &'a mut Vec<u8>,
    ) -> (&'a [u8], u32) {
        match message {
            Message::Text(s) => (s.as_bytes(), curl_sys::CURLWS_TEXT as u32),
            Message::Binary(b) => (b, curl_sys::CURLWS_BINARY as u32),
            Message::Ping(b) => (b, curl_sys::CURLWS_PING as u32),
            Message::Pong(b) => (b, curl_sys::CURLWS_PONG as u32),
            Message::Close { code, reason } => {
                close_scratch.clear();
                if code != CloseCode::NO_STATUS_RECEIVED || !reason.is_empty() {
                    let wire_code = if code == CloseCode::NO_STATUS_RECEIVED {
                        CloseCode::NORMAL
                    } else {
                        code
                    };
                    close_scratch.reserve(2 + reason.len());
                    close_scratch.extend_from_slice(&wire_code.0.to_be_bytes());
                    close_scratch.extend_from_slice(reason.as_bytes());
                }
                (close_scratch.as_slice(), curl_sys::CURLWS_CLOSE as u32)
            }
        }
    }

    pub(crate) fn enqueue(&mut self, message: Message<'_>) -> Result<(), Error> {
        let mut close_scratch = Vec::new();
        let (payload, flags) = Self::encode_frame(message, &mut close_scratch);
        self.enqueue_raw(payload, flags, false)
    }

    fn enqueue_raw(
        &mut self,
        payload: &[u8],
        flags: u32,
        continuation: bool,
    ) -> Result<(), Error> {
        if payload.len()
            > self
                .options
                .max_send_buffer
                .saturating_sub(self.queued_bytes)
        {
            return Err(Error::SendBufferFull);
        }
        self.queued_bytes += payload.len();
        self.outgoing.push_back(PendingFrame {
            payload: payload.to_vec(),
            flags,
            sent: 0,
            continuation,
        });
        Ok(())
    }

    /// Optimistically transmits `message` directly over `io` when the outbound queue is empty.
    /// If `CURLE_AGAIN` or a partial write occurs, queues only the unwritten tail.
    pub(crate) fn send_direct(
        &mut self,
        io: &mut impl WebSocketIo,
        message: Message<'_>,
    ) -> Result<(), Error> {
        if !self.outgoing.is_empty() {
            return self.enqueue(message);
        }
        let mut close_scratch = Vec::new();
        let (payload, flags) = Self::encode_frame(message, &mut close_scratch);
        // libcurl may retain an unfinished frame even when it reports EAGAIN.
        // Ensure any unwritten tail fits before changing the transport state.
        if payload.len() > self.options.max_send_buffer {
            return Err(Error::SendBufferFull);
        }
        match io.send(payload, flags, payload.len() as i64)? {
            Some(sent) if sent == payload.len() => Ok(()),
            Some(sent) if sent > payload.len() => Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "transport returned excessive send count",
            ))),
            Some(sent) => {
                let tail = &payload[sent..];
                self.enqueue_raw(tail, flags, sent > 0)
            }
            None => self.enqueue_raw(payload, flags, false),
        }
    }

    pub(crate) fn wants_write(&self) -> bool {
        !self.outgoing.is_empty()
    }

    pub(crate) fn flush(&mut self, io: &mut impl WebSocketIo) -> Result<(), Error> {
        while let Some(frame) = self.outgoing.front_mut() {
            let continuation = frame.continuation || frame.sent != 0;
            let flags = if continuation {
                frame.flags | curl_sys::CURLWS_OFFSET as u32
            } else {
                frame.flags
            };
            let size = if continuation {
                0
            } else {
                frame.payload.len() as i64
            };
            let Some(sent) = io.send(&frame.payload[frame.sent..], flags, size)? else {
                break;
            };
            if sent > frame.payload.len() - frame.sent {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "transport returned excessive send count",
                )));
            }
            frame.sent += sent;
            self.queued_bytes -= sent;
            if frame.sent == frame.payload.len() {
                self.outgoing.pop_front();
            } else if sent == 0 {
                break;
            }
        }
        Ok(())
    }

    pub(crate) fn close(
        &mut self,
        code: CloseCode,
        reason: &str,
        now: Instant,
    ) -> Result<(), Error> {
        if self.close_deadline.is_none() {
            let result = self.enqueue(Message::Close { code, reason });
            self.close_deadline = Some(if result.is_ok() {
                now + self.options.close_timeout
            } else {
                now
            });
            result?;
        }
        Ok(())
    }

    fn send_protocol_close(
        &mut self,
        io: &mut impl WebSocketIo,
        code: CloseCode,
        reason: &str,
    ) {
        if self.close_deadline.is_some() {
            return;
        }
        self.close_deadline = Some(self.last_received);
        let mut payload = Vec::with_capacity(2 + reason.len());
        payload.extend_from_slice(&code.0.to_be_bytes());
        payload.extend_from_slice(reason.as_bytes());
        if self.outgoing.is_empty() {
            let _ = io.send(
                &payload,
                curl_sys::CURLWS_CLOSE as u32,
                payload.len() as i64,
            );
        } else if self
            .enqueue_raw(&payload, curl_sys::CURLWS_CLOSE as u32, false)
            .is_ok()
        {
            let _ = self.flush(io);
        }
    }

    pub(crate) fn on_timer(&mut self, now: Instant) -> Result<TimerOutcome, Error> {
        if self.close_deadline.is_some_and(|deadline| now >= deadline) {
            return Err(Error::CloseTimeout);
        }
        if self
            .options
            .idle_timeout
            .is_some_and(|timeout| now >= self.last_received + timeout)
        {
            return Ok(TimerOutcome::Close(CloseCode(1006), "idle timeout"));
        }
        if self.next_ping_at.is_some_and(|at| now >= at) {
            if let Err(Error::SendBufferFull) = self.enqueue(Message::Ping(&[])) {
            } else {
                self.next_ping_at = self.options.ping_interval.map(|d| now + d);
            }
        }
        Ok(TimerOutcome::Continue)
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        [
            self.close_deadline,
            self.next_ping_at,
            self.options.idle_timeout.map(|d| self.last_received + d),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn receive_step<'a>(
        &mut self,
        buffers: &'a mut RecvBuffers,
        io: &mut impl WebSocketIo,
        now: Instant,
    ) -> Result<Option<InboundFrame<'a>>, Error> {
        if buffers.fragmented_state.is_none() && !buffers.fragmented_buf.is_empty() {
            buffers.fragmented_buf.clear();
        }
        if buffers.control_flags.is_none() && !buffers.control_buf.is_empty() {
            buffers.control_buf.clear();
        }
        loop {
            let (size, meta) = match io.recv(&mut *buffers.scratch) {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(None),
                Err(error) => return Err(error),
            };
            self.last_received = now;
            self.next_ping_at = self.options.ping_interval.map(|d| now + d);

            if meta.bytes_left < 0 {
                self.send_protocol_close(io, CloseCode::PROTOCOL_ERROR, "protocol error");
                return Err(Error::Curl(crate::Error::new(
                    curl_sys::CURLE_FTP_WEIRD_SERVER_REPLY,
                )));
            }

            let control_bits =
                curl_sys::CURLWS_PING | curl_sys::CURLWS_PONG | curl_sys::CURLWS_CLOSE;
            let is_control_chunk =
                (meta.flags & control_bits != 0) || buffers.control_flags.is_some();
            if is_control_chunk {
                let flags = buffers.control_flags.unwrap_or(meta.flags);
                let is_close = flags & curl_sys::CURLWS_CLOSE != 0;
                let accum = buffers.control_buf.len();
                let total_control = accum
                    .saturating_add(size)
                    .saturating_add(meta.bytes_left as usize);
                if total_control > 125
                    || (meta.flags & curl_sys::CURLWS_CONT != 0)
                    || (buffers.control_flags.is_some()
                        && (meta.flags & control_bits != 0)
                        && (meta.flags & control_bits != (flags & control_bits)))
                {
                    buffers.control_flags = None;
                    buffers.control_buf.clear();
                    self.send_protocol_close(io, CloseCode::PROTOCOL_ERROR, "protocol error");
                    return Err(Error::Curl(crate::Error::new(
                        curl_sys::CURLE_FTP_WEIRD_SERVER_REPLY,
                    )));
                }

                if meta.bytes_left > 0 {
                    buffers.control_flags = Some(flags);
                    buffers.control_buf.extend_from_slice(&buffers.scratch[..size]);
                    continue;
                }

                let payload: &'a [u8] = if buffers.control_flags.take().is_some() {
                    buffers.control_buf.extend_from_slice(&buffers.scratch[..size]);
                    &buffers.control_buf
                } else {
                    &buffers.scratch[..size]
                };

                if is_close && payload.len() == 1 {
                    self.send_protocol_close(io, CloseCode::PROTOCOL_ERROR, "protocol error");
                    return Err(Error::Curl(crate::Error::new(
                        curl_sys::CURLE_FTP_WEIRD_SERVER_REPLY,
                    )));
                }

                if flags & curl_sys::CURLWS_PING != 0 {
                    return Ok(Some(InboundFrame::Message(Message::Ping(payload))));
                } else if flags & curl_sys::CURLWS_PONG != 0 {
                    return Ok(Some(InboundFrame::Message(Message::Pong(payload))));
                } else {
                    let code = if payload.len() >= 2 {
                        CloseCode(u16::from_be_bytes([payload[0], payload[1]]))
                    } else {
                        CloseCode(1005)
                    };
                    if payload.len() > 2 && std::str::from_utf8(&payload[2..]).is_err() {
                        self.send_protocol_close(io, CloseCode(1007), "invalid utf-8");
                        return Err(Error::InvalidUtf8);
                    }
                    let reason_temp = if payload.len() > 2 {
                        std::str::from_utf8(&payload[2..]).unwrap()
                    } else {
                        ""
                    };
                    if self.close(code, reason_temp, now).is_ok() {
                        let _ = self.flush(io);
                    }
                    let reason = if payload.len() > 2 {
                        std::str::from_utf8(&payload[2..]).unwrap()
                    } else {
                        ""
                    };
                    return Ok(Some(InboundFrame::PeerClose { code, reason }));
                }
            }

            let text = meta.flags & curl_sys::CURLWS_TEXT != 0;
            let bytes_left = meta.bytes_left as usize;
            let current_accum = if buffers.fragmented_state.is_some() {
                buffers.fragmented_buf.len()
            } else {
                0
            };
            let projected = current_accum
                .saturating_add(size)
                .saturating_add(bytes_left);
            if projected > self.options.max_message_size {
                buffers.fragmented_state = None;
                buffers.fragmented_buf.clear();
                self.send_protocol_close(io, CloseCode(1009), "message too large");
                return Err(Error::MessageTooLarge);
            }

            if let Some(is_text) = buffers.fragmented_state {
                let has_data_opcode =
                    meta.flags & (curl_sys::CURLWS_TEXT | curl_sys::CURLWS_BINARY) != 0;
                if has_data_opcode && (text != is_text) {
                    buffers.fragmented_state = None;
                    buffers.fragmented_buf.clear();
                    self.send_protocol_close(io, CloseCode::PROTOCOL_ERROR, "protocol error");
                    return Err(Error::Curl(crate::Error::new(
                        curl_sys::CURLE_FTP_WEIRD_SERVER_REPLY,
                    )));
                }
                buffers.fragmented_buf.extend_from_slice(&buffers.scratch[..size]);
                if meta.bytes_left == 0 && (meta.flags & curl_sys::CURLWS_CONT == 0) {
                    buffers.fragmented_state = None;
                    if is_text {
                        if std::str::from_utf8(&buffers.fragmented_buf).is_err() {
                            buffers.fragmented_buf.clear();
                            self.send_protocol_close(io, CloseCode(1007), "invalid utf-8");
                            return Err(Error::InvalidUtf8);
                        }
                        let s = std::str::from_utf8(&buffers.fragmented_buf).unwrap();
                        return Ok(Some(InboundFrame::Message(Message::Text(s))));
                    } else {
                        return Ok(Some(InboundFrame::Message(Message::Binary(
                            &buffers.fragmented_buf,
                        ))));
                    }
                }
            } else if meta.bytes_left != 0 || (meta.flags & curl_sys::CURLWS_CONT != 0) {
                buffers.fragmented_buf.clear();
                buffers.fragmented_buf.extend_from_slice(&buffers.scratch[..size]);
                buffers.fragmented_state = Some(text);
            } else if text {
                let Ok(s) = std::str::from_utf8(&buffers.scratch[..size]) else {
                    self.send_protocol_close(io, CloseCode(1007), "invalid utf-8");
                    return Err(Error::InvalidUtf8);
                };
                return Ok(Some(InboundFrame::Message(Message::Text(s))));
            } else {
                return Ok(Some(InboundFrame::Message(Message::Binary(
                    &buffers.scratch[..size],
                ))));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Connection, FrameMeta, InboundFrame, TimerOutcome, WebSocketIo};
    use crate::event_loop::Error;
    use crate::websocket::{CloseCode, Message, Options};
    use std::collections::VecDeque;
    use std::time::Instant;

    #[derive(Default)]
    struct MemoryIo {
        incoming: VecDeque<(Vec<u8>, FrameMeta)>,
        sent: Vec<(Vec<u8>, u32, i64)>,
        send_results: VecDeque<Option<usize>>,
    }

    impl WebSocketIo for MemoryIo {
        fn recv(&mut self, buffer: &mut [u8]) -> Result<Option<(usize, FrameMeta)>, Error> {
            match self.incoming.pop_front() {
                Some((data, meta)) => {
                    buffer[..data.len()].copy_from_slice(&data);
                    Ok(Some((data.len(), meta)))
                }
                None => Ok(None),
            }
        }

        fn send(
            &mut self,
            data: &[u8],
            flags: u32,
            frame_size: i64,
        ) -> Result<Option<usize>, Error> {
            self.sent.push((data.to_vec(), flags, frame_size));
            Ok(self.send_results.pop_front().unwrap_or(Some(data.len())))
        }
    }

    #[test]
    fn text_frame_is_delivered_as_one_borrowed_message() {
        let now = Instant::now();
        let mut connection = Connection::new(Options::default(), now);
        let mut buffers = connection.take_buffers();
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            b"hello".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_TEXT,
                bytes_left: 0,
            },
        ));
        let frame = connection
            .receive_step(&mut buffers, &mut io, now)
            .unwrap()
            .unwrap();
        assert_eq!(frame, InboundFrame::Message(Message::Text("hello")));
        assert!(
            connection
                .receive_step(&mut buffers, &mut io, now)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn fragmented_text_is_reassembled_and_reuses_buffer_capacity() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut buffers = c.take_buffers();
        let mut io = MemoryIo::default();
        for part in [b"he".as_slice(), b"ll".as_slice()] {
            io.incoming.push_back((
                part.to_vec(),
                FrameMeta {
                    flags: curl_sys::CURLWS_TEXT | curl_sys::CURLWS_CONT,
                    bytes_left: 0,
                },
            ));
        }
        io.incoming.push_back((
            b"o".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_TEXT,
                bytes_left: 0,
            },
        ));
        {
            let frame = c.receive_step(&mut buffers, &mut io, now).unwrap().unwrap();
            assert_eq!(frame, InboundFrame::Message(Message::Text("hello")));
        }
        let cap_after_first = buffers.fragmented_buf.capacity();
        assert!(cap_after_first >= 5);

        // Next step clears length while retaining capacity
        assert!(c.receive_step(&mut buffers, &mut io, now).unwrap().is_none());
        assert_eq!(buffers.fragmented_buf.len(), 0);
        assert_eq!(buffers.fragmented_buf.capacity(), cap_after_first);
    }

    #[test]
    fn binary_frame_is_delivered() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut buffers = c.take_buffers();
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            vec![1, 2],
            FrameMeta {
                flags: 0,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive_step(&mut buffers, &mut io, now).unwrap(),
            Some(InboundFrame::Message(Message::Binary(&[1, 2])))
        );
    }

    #[test]
    fn ping_is_visible() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut buffers = c.take_buffers();
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            b"x".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_PING,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive_step(&mut buffers, &mut io, now).unwrap(),
            Some(InboundFrame::Message(Message::Ping(b"x")))
        );
    }

    #[test]
    fn invalid_utf8_sends_1007_close_and_fails() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut buffers = c.take_buffers();
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            vec![0xff],
            FrameMeta {
                flags: curl_sys::CURLWS_TEXT,
                bytes_left: 0,
            },
        ));
        assert!(matches!(
            c.receive_step(&mut buffers, &mut io, now),
            Err(Error::InvalidUtf8)
        ));
        assert_eq!(io.sent.len(), 1);
        assert_eq!(&io.sent[0].0[..2], &1007u16.to_be_bytes());
        assert_ne!(io.sent[0].1 & curl_sys::CURLWS_CLOSE as u32, 0);
    }

    #[test]
    fn first_chunk_bytes_left_exceeding_max_message_size_is_rejected_with_1009() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default().max_message_size(10), now);
        let mut buffers = c.take_buffers();
        let mut io = MemoryIo::default();
        // First chunk is only 4 bytes, but bytes_left announces 8 more (4 + 8 = 12 > 10)
        io.incoming.push_back((
            b"abcd".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_TEXT | curl_sys::CURLWS_CONT,
                bytes_left: 8,
            },
        ));
        assert!(matches!(
            c.receive_step(&mut buffers, &mut io, now),
            Err(Error::MessageTooLarge)
        ));
        assert_eq!(buffers.fragmented_buf.capacity(), 0);
        assert_eq!(io.sent.len(), 1);
        assert_eq!(&io.sent[0].0[..2], &1009u16.to_be_bytes());
        assert_ne!(io.sent[0].1 & curl_sys::CURLWS_CLOSE as u32, 0);
    }

    #[test]
    fn malformed_control_frame_sends_1002_close_and_fails() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut buffers = c.take_buffers();
        let mut io = MemoryIo::default();
        // 1-byte close payload is invalid under RFC 6455
        io.incoming.push_back((
            vec![0x03],
            FrameMeta {
                flags: curl_sys::CURLWS_CLOSE,
                bytes_left: 0,
            },
        ));
        assert!(c.receive_step(&mut buffers, &mut io, now).is_err());
        assert_eq!(io.sent.len(), 1);
        assert_eq!(&io.sent[0].0[..2], &1002u16.to_be_bytes());
    }

    #[test]
    fn peer_close_frame_acks_and_yields_peer_close_event() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut buffers = c.take_buffers();
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            vec![3, 232, b'b', b'y', b'e'],
            FrameMeta {
                flags: curl_sys::CURLWS_CLOSE,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive_step(&mut buffers, &mut io, now).unwrap(),
            Some(InboundFrame::PeerClose {
                code: CloseCode(1000),
                reason: "bye"
            })
        );
        // Verify close-ack was automatically flushed
        assert_eq!(io.sent.len(), 1);
        assert_eq!(io.sent[0].0, vec![3, 232, b'b', b'y', b'e']);
    }

    #[test]
    fn empty_close_uses_1005() {
        let n = Instant::now();
        let mut c = Connection::new(Options::default(), n);
        let mut buffers = c.take_buffers();
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            vec![],
            FrameMeta {
                flags: curl_sys::CURLWS_CLOSE,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive_step(&mut buffers, &mut io, n).unwrap(),
            Some(InboundFrame::PeerClose {
                code: CloseCode(1005),
                reason: ""
            })
        );
        // RFC 6455 §7.4.1: status code 1005 MUST NOT be set as a status code in a Close control frame;
        // the close-ack must have an empty (0-byte) payload on the wire.
        assert_eq!(io.sent.len(), 1);
        assert!(io.sent[0].0.is_empty());
        assert_ne!(io.sent[0].1 & curl_sys::CURLWS_CLOSE as u32, 0);
    }

    #[test]
    fn unfragmented_control_frame_split_across_tcp_reads_is_reassembled() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut buffers = c.take_buffers();
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            b"pi".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_PING,
                bytes_left: 2,
            },
        ));
        io.incoming.push_back((
            b"ng".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_PING,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive_step(&mut buffers, &mut io, now).unwrap(),
            Some(InboundFrame::Message(Message::Ping(b"ping")))
        );
    }

    #[test]
    fn continuation_opcode_mismatch_is_rejected_with_1002() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut buffers = c.take_buffers();
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            b"hel".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_TEXT | curl_sys::CURLWS_CONT,
                bytes_left: 0,
            },
        ));
        io.incoming.push_back((
            b"lo".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_BINARY,
                bytes_left: 0,
            },
        ));
        assert!(c.receive_step(&mut buffers, &mut io, now).is_err());
        assert_eq!(io.sent.len(), 1);
        assert_eq!(&io.sent[0].0[..2], &1002u16.to_be_bytes());
    }

    #[test]
    fn oversized_direct_send_is_rejected_before_touching_transport() {
        for result in [Some(1), None] {
            let mut c = Connection::new(Options::default().max_send_buffer(4), Instant::now());
            let mut io = MemoryIo::default();
            io.send_results.push_back(result);
            assert!(matches!(
                c.send_direct(&mut io, Message::Text("too large")),
                Err(Error::SendBufferFull)
            ));
            assert!(io.sent.is_empty());
            assert!(!c.wants_write());

            // A rejected frame must leave the transport ready for the next message.
            c.send_direct(&mut io, Message::Text("next")).unwrap();
            c.flush(&mut io).unwrap();
            assert!(!c.wants_write());
        }
    }

    #[test]
    fn send_direct_writes_immediately_and_queues_tail_on_partial_or_eagain() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut io = MemoryIo::default();
        // 1. Full immediate write
        io.send_results.push_back(Some(5));
        c.send_direct(&mut io, Message::Text("hello")).unwrap();
        assert!(!c.wants_write());
        assert_eq!(io.sent.len(), 1);

        // 2. Partial immediate write (2 of 5 bytes sent), then EAGAIN on flush, then remaining 3 bytes
        io.sent.clear();
        io.send_results.extend([Some(2), None, Some(3)]);
        c.send_direct(&mut io, Message::Text("world")).unwrap();
        assert!(c.wants_write());
        c.flush(&mut io).unwrap();
        assert!(c.wants_write());
        c.flush(&mut io).unwrap();
        assert!(!c.wants_write());

        let first = curl_sys::CURLWS_TEXT as u32;
        let continuation = (curl_sys::CURLWS_TEXT | curl_sys::CURLWS_OFFSET) as u32;
        assert_eq!(
            io.sent,
            vec![
                (b"world".to_vec(), first, 5),
                (b"rld".to_vec(), continuation, 0),
                (b"rld".to_vec(), continuation, 0),
            ]
        );
    }

    #[test]
    fn eagain_preserves_pending_frame_and_offset() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut io = MemoryIo::default();
        io.send_results.extend([None, Some(2), None, Some(3)]);
        c.enqueue(Message::Text("hello")).unwrap();
        c.flush(&mut io).unwrap();
        assert!(c.wants_write());
        c.flush(&mut io).unwrap();
        assert!(c.wants_write());
        c.flush(&mut io).unwrap();
        assert!(!c.wants_write());
        let first = curl_sys::CURLWS_TEXT as u32;
        let continuation = (curl_sys::CURLWS_TEXT | curl_sys::CURLWS_OFFSET) as u32;
        assert_eq!(
            io.sent,
            vec![
                (b"hello".to_vec(), first, 5),
                (b"hello".to_vec(), first, 5),
                (b"llo".to_vec(), continuation, 0),
                (b"llo".to_vec(), continuation, 0)
            ]
        );
    }

    #[test]
    fn close_timeout_expires_at_deadline() {
        let now = Instant::now();
        let mut c = Connection::new(
            Options::default().close_timeout(std::time::Duration::from_secs(2)),
            now,
        );
        let mut io = MemoryIo::default();
        c.close(CloseCode::NORMAL, "bye", now).unwrap();
        c.flush(&mut io).unwrap();
        assert_eq!(io.sent[0].0, vec![3, 232, b'b', b'y', b'e']);
        assert!(matches!(
            c.on_timer(now + std::time::Duration::from_secs(1)).unwrap(),
            TimerOutcome::Continue
        ));
        assert!(matches!(
            c.on_timer(now + std::time::Duration::from_secs(2)),
            Err(Error::CloseTimeout)
        ));
    }

    #[test]
    fn idle_timeout_is_refreshed_by_receive() {
        let now = Instant::now();
        let mut c = Connection::new(
            Options::default().idle_timeout(Some(std::time::Duration::from_secs(2))),
            now,
        );
        let mut buffers = c.take_buffers();
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            b"alive".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_TEXT,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive_step(&mut buffers, &mut io, now + std::time::Duration::from_secs(1))
                .unwrap(),
            Some(InboundFrame::Message(Message::Text("alive")))
        );
        assert!(matches!(
            c.on_timer(now + std::time::Duration::from_secs(2)).unwrap(),
            TimerOutcome::Continue
        ));
        assert!(matches!(
            c.on_timer(now + std::time::Duration::from_secs(3)).unwrap(),
            TimerOutcome::Close(CloseCode(1006), _)
        ));
    }

    #[test]
    fn ping_due_queues_empty_frame() {
        let n = Instant::now();
        let mut c = Connection::new(
            Options::default().ping_interval(Some(std::time::Duration::from_secs(1))),
            n,
        );
        assert!(matches!(
            c.on_timer(n + std::time::Duration::from_secs(1)).unwrap(),
            TimerOutcome::Continue
        ));
        assert!(c.wants_write());
        let mut io = MemoryIo::default();
        c.flush(&mut io).unwrap();
        assert_eq!(io.sent[0].0, Vec::<u8>::new());
        assert_ne!(io.sent[0].1 & curl_sys::CURLWS_PING as u32, 0);
    }

    #[test]
    fn ping_not_due_does_not_queue() {
        let n = Instant::now();
        let mut c = Connection::new(
            Options::default().ping_interval(Some(std::time::Duration::from_secs(1))),
            n,
        );
        c.on_timer(n + std::time::Duration::from_millis(500))
            .unwrap();
        assert!(!c.wants_write());
    }

    #[test]
    fn no_deadline_when_options_empty() {
        let n = Instant::now();
        assert_eq!(Connection::new(Options::default(), n).next_deadline(), None);
    }

    #[test]
    fn deadline_is_minimum() {
        let n = Instant::now();
        let o = Options::default()
            .ping_interval(Some(std::time::Duration::from_secs(2)))
            .idle_timeout(Some(std::time::Duration::from_secs(3)));
        let mut c = Connection::new(o, n);
        c.close(CloseCode::NORMAL, "", n).unwrap();
        assert_eq!(
            c.next_deadline(),
            Some(n + std::time::Duration::from_secs(2))
        );
    }

    #[test]
    fn repeated_ping_timer_does_not_duplicate() {
        let n = Instant::now();
        let mut c = Connection::new(
            Options::default().ping_interval(Some(std::time::Duration::from_secs(1))),
            n,
        );
        c.on_timer(n + std::time::Duration::from_secs(1)).unwrap();
        c.on_timer(n + std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(c.outgoing.len(), 1);
    }

    #[test]
    fn ping_advances_deadline() {
        let n = Instant::now();
        let d = std::time::Duration::from_secs(1);
        let mut c = Connection::new(Options::default().ping_interval(Some(d)), n);
        c.on_timer(n + d).unwrap();
        assert_eq!(c.next_deadline(), Some(n + d + d));
    }
}

