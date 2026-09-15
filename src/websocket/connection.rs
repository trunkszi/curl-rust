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
    Close(CloseCode, String),
}

pub(crate) struct Connection {
    options: Options,
    last_received: Instant,
    fragmented: Option<(bool, Vec<u8>)>,
    outgoing: VecDeque<PendingFrame>,
    queued_bytes: usize,
    close_deadline: Option<Instant>,
    next_ping_at: Option<Instant>,
    receive_error: Option<Error>,
}

struct PendingFrame {
    payload: Vec<u8>,
    flags: u32,
    sent: usize,
}

impl Connection {
    pub(crate) fn new(options: Options, now: Instant) -> Self {
        let next_ping_at = options.ping_interval.map(|d| now + d);
        Self {
            options,
            last_received: now,
            fragmented: None,
            outgoing: VecDeque::new(),
            queued_bytes: 0,
            close_deadline: None,
            next_ping_at,
            receive_error: None,
        }
    }

    pub(crate) fn enqueue(&mut self, message: Message) -> Result<(), Error> {
        let (payload, flags) = match message {
            Message::Text(s) => (s.into_bytes(), curl_sys::CURLWS_TEXT),
            Message::Binary(b) => (b, curl_sys::CURLWS_BINARY),
            Message::Ping(b) => (b, curl_sys::CURLWS_PING),
            Message::Pong(b) => (b, curl_sys::CURLWS_PONG),
            Message::Close { code, reason } => {
                let mut b = code.0.to_be_bytes().to_vec();
                b.extend_from_slice(reason.as_bytes());
                (b, curl_sys::CURLWS_CLOSE)
            }
        };
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
            payload,
            flags: flags as u32,
            sent: 0,
        });
        Ok(())
    }

    pub(crate) fn wants_write(&self) -> bool {
        !self.outgoing.is_empty()
    }

    pub(crate) fn flush(&mut self, io: &mut impl WebSocketIo) -> Result<(), Error> {
        while let Some(frame) = self.outgoing.front_mut() {
            let continuation = frame.sent != 0;
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
            let result = self.enqueue(Message::Close {
                code,
                reason: reason.into(),
            });
            self.close_deadline = Some(if result.is_ok() {
                now + self.options.close_timeout
            } else {
                now
            });
            result?;
        }
        Ok(())
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
            return Ok(TimerOutcome::Close(CloseCode(1006), "idle timeout".into()));
        }
        if self.next_ping_at.is_some_and(|at| now >= at) {
            if let Err(Error::SendBufferFull) = self.enqueue(Message::Ping(Vec::new())) {
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

    pub(crate) fn receive(
        &mut self,
        io: &mut impl WebSocketIo,
        now: Instant,
    ) -> Result<Vec<Message>, Error> {
        if let Some(error) = self.receive_error.take() {
            return Err(error);
        }
        let mut messages = Vec::new();
        let mut buffer = [0; 65536];
        loop {
            let (size, meta) = match io.recv(&mut buffer) {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) if messages.is_empty() => return Err(error),
                Err(error) => {
                    self.receive_error = Some(error);
                    break;
                }
            };
            self.last_received = now;
            self.next_ping_at = self.options.ping_interval.map(|d| now + d);
            if size > self.options.max_message_size {
                return Err(Error::MessageTooLarge);
            }
            let text = meta.flags & curl_sys::CURLWS_TEXT != 0;
            let control = meta.flags
                & (curl_sys::CURLWS_PING | curl_sys::CURLWS_PONG | curl_sys::CURLWS_CLOSE)
                != 0;
            if control {
                let data = buffer[..size].to_vec();
                if meta.flags & curl_sys::CURLWS_PING != 0 {
                    messages.push(Message::Ping(data));
                } else if meta.flags & curl_sys::CURLWS_PONG != 0 {
                    messages.push(Message::Pong(data));
                } else {
                    let code = if data.len() >= 2 {
                        u16::from_be_bytes([data[0], data[1]])
                    } else {
                        CloseCode(1005).0
                    };
                    let reason = if data.len() > 2 {
                        String::from_utf8(data[2..].to_vec()).map_err(|_| Error::InvalidUtf8)?
                    } else {
                        String::new()
                    };
                    messages.push(Message::Close {
                        code: CloseCode(code),
                        reason,
                    });
                }
                continue;
            }
            if let Some((is_text, ref mut data)) = self.fragmented {
                data.extend_from_slice(&buffer[..size]);
                if data.len() > self.options.max_message_size {
                    return Err(Error::MessageTooLarge);
                }
                if meta.bytes_left == 0 && meta.flags & curl_sys::CURLWS_CONT == 0 {
                    let out = std::mem::take(data);
                    self.fragmented = None;
                    if is_text {
                        messages.push(Message::Text(
                            String::from_utf8(out).map_err(|_| Error::InvalidUtf8)?,
                        ));
                    } else {
                        messages.push(Message::Binary(out));
                    }
                }
            } else if meta.bytes_left != 0 || meta.flags & curl_sys::CURLWS_CONT != 0 {
                self.fragmented = Some((text, buffer[..size].to_vec()));
            } else if text {
                messages.push(Message::Text(
                    String::from_utf8(buffer[..size].to_vec()).map_err(|_| Error::InvalidUtf8)?,
                ));
            } else {
                messages.push(Message::Binary(buffer[..size].to_vec()));
            }
        }
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::{Connection, FrameMeta, TimerOutcome, WebSocketIo};
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
    fn text_frame_is_delivered_as_one_message() {
        let now = Instant::now();
        let mut connection = Connection::new(Options::default(), now);
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            b"hello".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_TEXT,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            connection.receive(&mut io, now).unwrap(),
            vec![Message::Text("hello".into())]
        );
    }

    #[test]
    fn fragmented_text_is_reassembled() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
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
        assert_eq!(
            c.receive(&mut io, now).unwrap(),
            vec![Message::Text("hello".into())]
        );
    }
    #[test]
    fn binary_frame_is_delivered() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            vec![1, 2],
            FrameMeta {
                flags: 0,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive(&mut io, now).unwrap(),
            vec![Message::Binary(vec![1, 2])]
        );
    }
    #[test]
    fn ping_is_visible() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            b"x".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_PING,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive(&mut io, now).unwrap(),
            vec![Message::Ping(b"x".to_vec())]
        );
    }
    #[test]
    fn invalid_utf8_fails() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            vec![0xff],
            FrameMeta {
                flags: curl_sys::CURLWS_TEXT,
                bytes_left: 0,
            },
        ));
        assert!(matches!(c.receive(&mut io, now), Err(Error::InvalidUtf8)));
    }
    #[test]
    fn message_limit_is_enforced() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default().max_message_size(1), now);
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            b"xx".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_TEXT,
                bytes_left: 0,
            },
        ));
        assert!(matches!(
            c.receive(&mut io, now),
            Err(Error::MessageTooLarge)
        ));
    }
    #[test]
    fn close_frame_is_parsed() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            vec![0x03, 0xe8],
            FrameMeta {
                flags: curl_sys::CURLWS_CLOSE,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive(&mut io, now).unwrap(),
            vec![Message::Close {
                code: CloseCode(1000),
                reason: String::new()
            }]
        );
    }
    #[test]
    fn close_reason_is_parsed() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            vec![3, 232, b'b', b'y', b'e'],
            FrameMeta {
                flags: curl_sys::CURLWS_CLOSE,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive(&mut io, now).unwrap(),
            vec![Message::Close {
                code: CloseCode(1000),
                reason: "bye".into()
            }]
        );
    }
    #[test]
    fn eagain_preserves_pending_frame_and_offset() {
        let now = Instant::now();
        let mut c = Connection::new(Options::default(), now);
        let mut io = MemoryIo::default();
        io.send_results.extend([None, Some(2), None, Some(3)]);
        c.enqueue(Message::Text("hello".into())).unwrap();
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
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            b"alive".to_vec(),
            FrameMeta {
                flags: curl_sys::CURLWS_TEXT,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive(&mut io, now + std::time::Duration::from_secs(1))
                .unwrap(),
            vec![Message::Text("alive".into())]
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
    fn empty_close_uses_1005() {
        let n = Instant::now();
        let mut c = Connection::new(Options::default(), n);
        let mut io = MemoryIo::default();
        io.incoming.push_back((
            vec![],
            FrameMeta {
                flags: curl_sys::CURLWS_CLOSE,
                bytes_left: 0,
            },
        ));
        assert_eq!(
            c.receive(&mut io, n).unwrap(),
            vec![Message::Close {
                code: CloseCode(1005),
                reason: String::new()
            }]
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
